//! One inference at a time across every model store. Cancellation owns cleanup.
use anyhow::Result;
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::sync::{Mutex, OwnedMutexGuard};
use tokio_util::sync::CancellationToken;

#[derive(Default)]
pub struct Broker {
    pub gate: Arc<Mutex<()>>,
    pub failure: Arc<std::sync::Mutex<Option<Value>>>,
}
pub struct Lease {
    guard: Option<OwnedMutexGuard<()>>,
    client: reqwest::Client,
    endpoint: Option<String>,
    model: String,
    completed: bool,
    failure: Arc<std::sync::Mutex<Option<Value>>>,
}
impl Broker {
    pub async fn acquire(
        &self,
        client: reqwest::Client,
        model: &str,
        cancel: &CancellationToken,
    ) -> Result<Lease> {
        let guard = tokio::select! {_=cancel.cancelled()=>anyhow::bail!("Остановлено пользователем"),guard=self.gate.clone().lock_owned()=>guard};
        anyhow::ensure!(self.failure.lock().unwrap().is_none(),"Освобождение предыдущей модели не подтверждено. Откройте вычисления и повторите запуск движка; запрос сохранён.");
        Ok(Lease {
            guard: Some(guard),
            client,
            endpoint: None,
            model: model.into(),
            completed: false,
            failure: self.failure.clone(),
        })
    }
}
impl Lease {
    pub fn endpoint(&mut self, endpoint: String) {
        self.endpoint = Some(endpoint);
    }
    pub fn completed(&mut self) {
        self.completed = true;
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        if self.completed || self.endpoint.is_none() {
            return;
        }
        let endpoint = self.endpoint.take().unwrap();
        let model = self.model.clone();
        let client = self.client.clone();
        let guard = self.guard.take();
        let failure = self.failure.clone();
        // Keep the global gate until cleanup finishes, including when an outer
        // user deadline drops the inference future instead of returning it.
        tokio::spawn(async move {
            let _guard = guard;
            if let Err(error) = unload(&client, &endpoint, &model).await {
                eprintln!("Model cleanup {model}: {error}");
                *failure.lock().unwrap() =
                    Some(json!({"endpoint":endpoint,"model":model,"error":error.to_string()}));
            }
        });
    }
}
impl Broker {
    pub async fn recover(&self, client: &reqwest::Client) -> Result<()> {
        let _guard = self.gate.lock().await;
        let value = self.failure.lock().unwrap().clone();
        if let Some(value) = value {
            unload(
                client,
                value["endpoint"].as_str().unwrap_or(""),
                value["model"].as_str().unwrap_or(""),
            )
            .await?;
            *self.failure.lock().unwrap() = None;
        }
        Ok(())
    }
}
async fn unload(client: &reqwest::Client, endpoint: &str, model: &str) -> Result<()> {
    let value: Value = client
        .post(format!("{endpoint}/api/generate"))
        .json(&json!({"model":model,"keep_alive":0,"stream":false}))
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    anyhow::ensure!(
        value["error"].is_null() && value["done"] == true,
        "Движок не подтвердил освобождение модели: {value}"
    );
    Ok(())
}
pub fn memory() -> Value {
    match sysinfo::System::new() {
        Ok(mut system) => {
            system.refresh_memory();
            json!({"total_bytes":system.total_memory(),"available_bytes":system.available_memory(),"swap_used_bytes":system.used_swap()})
        }
        Err(error) => json!({"error":error.to_string()}),
    }
}
/// Conservative f16 KV estimate for ordinary attention. Missing architecture
/// data stays unknown; it is never replaced with a fabricated hardware limit.
pub fn kv_bytes_per_token(show: &Value) -> Option<u64> {
    let info = show["model_info"].as_object()?;
    let field = |suffix: &str| {
        info.iter()
            .find(|(key, _)| key.ends_with(suffix))
            .and_then(|(_, v)| v.as_u64())
    };
    let layers = field(".block_count")?;
    let heads = field(".attention.head_count")?;
    let kv_heads = field(".attention.head_count_kv").unwrap_or(heads);
    let width = field(".attention.key_length")
        .or_else(|| field(".embedding_length").and_then(|n| n.checked_div(heads)))?;
    layers
        .checked_mul(kv_heads)?
        .checked_mul(width)?
        .checked_mul(4)
}
pub fn fit_context(
    config: &mut crate::context::Config,
    show: &Value,
    memory: &Value,
    cpu: bool,
) -> Result<Value> {
    let Some(per_token) = kv_bytes_per_token(show).filter(|n| *n > 0) else {
        return Ok(
            json!({"estimated":false,"reason":"architecture_metadata_missing","memory":memory}),
        );
    };
    if !cpu {
        return Ok(
            json!({"estimated":true,"kv_bytes_per_token":per_token,"memory":memory,"placement":"GPU/Auto: RAM admission estimate not applied"}),
        );
    }
    let total = memory["total_bytes"].as_u64().unwrap_or(0);
    let available = memory["available_bytes"].as_u64().unwrap_or(0);
    if total == 0 {
        return Ok(json!({"estimated":false,"reason":"memory_measurement_unavailable"}));
    }
    let reserve = (total / 10).max(2 * 1024 * 1024 * 1024).min(total / 2);
    let parameter_count = show["model_info"]["general.parameter_count"].as_u64();
    let quant = show["details"]["quantization_level"].as_str().unwrap_or("");
    let bits = quant
        .strip_prefix('Q')
        .and_then(|q| q.split('_').next()?.parse::<u64>().ok());
    let weights = parameter_count
        .zip(bits)
        .map(|(n, b)| n.saturating_mul(b).div_ceil(8).saturating_mul(12) / 10);
    let budget = available
        .saturating_sub(reserve)
        .saturating_sub(weights.unwrap_or(0));
    let safe = (budget / per_token).min(usize::MAX as u64) as usize;
    anyhow::ensure!(safe>=8192,"Недостаточно свободной RAM для модели и контекста: доступно {} МиБ, резерв ОС {} МиБ. Запрос сохранён. Освободите память или выберите меньшую модель; оценка приблизительная.",available/(1024*1024),reserve/(1024*1024));
    if config.window_tokens == 0 {
        config.auto_max_tokens = config.auto_max_tokens.min(safe);
    } else {
        anyhow::ensure!(config.window_tokens<=safe,"Контекст {} превышает оценку доступной RAM (около {safe} токенов). Запрос сохранён; выберите Авто или уменьшите окно.",config.window_tokens);
    }
    Ok(
        json!({"estimated":true,"kv_bytes_per_token":per_token,"weights_bytes":weights,"os_reserve_bytes":reserve,"safe_window_tokens":safe,"memory":memory}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn physical_memory_limits_auto_context_but_never_silently_changes_manual_window() {
        let show = json!({"model_info":{"x.block_count":32,"x.attention.head_count":32,"x.attention.head_count_kv":8,"x.embedding_length":4096}});
        assert_eq!(kv_bytes_per_token(&show), Some(131072));
        let memory = json!({"total_bytes":32u64<<30,"available_bytes":8u64<<30});
        let mut config = crate::context::Config {
            auto_max_tokens: 262144,
            ..Default::default()
        };
        let estimate = fit_context(&mut config, &show, &memory, true).unwrap();
        assert!(config.auto_max_tokens < 65536);
        assert_eq!(estimate["estimated"], true);
        config.window_tokens = 262144;
        assert!(fit_context(&mut config, &show, &memory, true).is_err());
        assert_eq!(config.window_tokens, 262144);
        assert!(fit_context(&mut config, &json!({}), &memory, true).is_ok());
    }
    #[tokio::test]
    async fn inference_is_serial_and_queued_request_can_be_cancelled() {
        let broker = Broker::default();
        let client = reqwest::Client::new();
        let token = CancellationToken::new();
        let first = broker.acquire(client.clone(), "a", &token).await.unwrap();
        let wait = broker.acquire(client, "b", &token);
        tokio::pin!(wait);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), &mut wait)
                .await
                .is_err()
        );
        token.cancel();
        assert!(wait.await.is_err());
        drop(first);
        assert!(broker
            .acquire(reqwest::Client::new(), "c", &CancellationToken::new())
            .await
            .is_ok());
    }
    #[tokio::test]
    async fn failed_cleanup_blocks_inference_until_recovery_is_confirmed() {
        let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint=format!("http://{}",listener.local_addr().unwrap());
        let router=axum::Router::new().route("/api/generate",axum::routing::post(||async{axum::http::StatusCode::INTERNAL_SERVER_ERROR}));
        let server=tokio::spawn(async move{axum::serve(listener,router).await.unwrap()});
        let broker=Broker::default();let client=reqwest::Client::builder().no_proxy().build().unwrap();
        let mut lease=broker.acquire(client.clone(),"a",&CancellationToken::new()).await.unwrap();lease.endpoint(endpoint);drop(lease);
        let result=tokio::time::timeout(std::time::Duration::from_secs(2),broker.acquire(client.clone(),"b",&CancellationToken::new())).await.unwrap();
        assert!(result.is_err());assert!(broker.failure.lock().unwrap().is_some());
        assert!(broker.recover(&client).await.is_err());assert!(broker.failure.lock().unwrap().is_some());
        server.abort();
    }
}
