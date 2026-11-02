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
            let mut value=memory_bounds(system.total_memory(),system.available_memory(),system.cgroup_limits().map(|limit|(limit.total_memory,limit.free_memory)));
            value["swap_used_bytes"]=json!(system.used_swap());value
        }
        Err(error) => json!({"error":error.to_string()}),
    }
}
fn memory_bounds(total:u64,available:u64,cgroup:Option<(u64,u64)>)->Value{
    let cgroup=cgroup.filter(|(total,_)|*total>0);
    let effective_total=cgroup.map_or(total,|(limit,_)|total.min(limit));
    let effective_available=cgroup.map_or(available,|(_,free)|available.min(free)).min(effective_total);
    json!({"total_bytes":effective_total,"available_bytes":effective_available,"host_total_bytes":total,"host_available_bytes":available,"cgroup_limit_bytes":cgroup.map(|v|v.0),"scope":if cgroup.is_some(){"host_and_cgroup"}else{"host"}})
}
/// Client CPU/affinity facts apply only to an engine owned by this app.
pub fn thread_plan(requested:usize,owned:bool,available:Option<usize>,physical:Option<usize>)->Result<Value>{
    if !owned{return Ok(json!({"policy":if requested==0{"server_auto"}else{"manual"},"requested_threads":requested,"effective_threads":if requested==0{None}else{Some(requested)},"available_logical_cpus":null,"physical_cores":null}));}
    let available=available.filter(|n|*n>0);let physical=physical.filter(|n|*n>0);
    if let Some(available)=available{anyhow::ensure!(requested<=available,"Указано {requested} потоков CPU, процессу доступно {available}. Уменьшите ручное значение или выберите Авто; запрос сохранён");}
    let cores=physical.or(available).unwrap_or(1).min(available.unwrap_or(usize::MAX)).max(1);
    let threads=if requested==0{cores.saturating_sub(1).max(1)}else{requested};
    Ok(json!({"policy":if requested==0{"auto_reduced_threads"}else{"manual"},"requested_threads":requested,"effective_threads":threads,"available_logical_cpus":available,"physical_cores":physical,"estimated":available.is_none()||physical.is_none(),"note":"Число вычислительных потоков, передаваемое движку; ядра не закрепляются, служебные потоки и планирование ОС независимы"}))
}
pub fn cpu_threads(requested:usize,owned:bool)->Result<Value>{
    if !owned{return thread_plan(requested,false,None,None);}
    thread_plan(requested,true,std::thread::available_parallelism().ok().map(|n|n.get()),sysinfo::System::physical_core_count().ok())
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
    let per_token=kv_bytes_per_token(show).filter(|n|*n>0);
    if !cpu{return Ok(json!({"estimated":per_token.is_some(),"kv_bytes_per_token":per_token,"memory":memory,"placement":"GPU/Auto: RAM admission estimate not applied"}));}
    let total=memory["total_bytes"].as_u64().unwrap_or(0);let available=memory["available_bytes"].as_u64().unwrap_or(0);
    if total==0{return Ok(json!({"estimated":false,"reason":"memory_measurement_unavailable"}));}
    let reserve=(total/10).max(2*1024*1024*1024).min(total/2);
    let parameter_count=show["model_info"]["general.parameter_count"].as_u64();
    let quant=show["details"]["quantization_level"].as_str().unwrap_or("");
    let bits=match quant{"F16"|"BF16"=>Some(16),"F32"=>Some(32),_=>quant.strip_prefix('Q').or_else(||quant.strip_prefix("IQ")).and_then(|q|q.split('_').next()?.parse::<u64>().ok())};
    let weights=parameter_count.zip(bits).map(|(n,b)|n.saturating_mul(b).div_ceil(8).saturating_mul(12)/10);
    anyhow::ensure!(available>reserve&&weights.is_none_or(|bytes|bytes<available-reserve),"Недостаточно свободной RAM для модели: доступно {} МиБ, резерв ОС {} МиБ, оценка весов {} МиБ. Запрос сохранён. Освободите память или выберите меньшую модель; оценка приблизительная",available/(1024*1024),reserve/(1024*1024),weights.unwrap_or(0)/(1024*1024));
    let Some(per_token)=per_token else{return Ok(json!({"estimated":false,"reason":"architecture_metadata_missing","weights_bytes":weights,"os_reserve_bytes":reserve,"memory":memory}));};
    let budget=available.saturating_sub(reserve).saturating_sub(weights.unwrap_or(0));let safe=(budget/per_token).min(usize::MAX as u64) as usize;
    let trained=crate::context::model_limit(show);
    let required=if config.window_tokens==0{4096.min(trained)}else{config.window_tokens.min(trained)};
    anyhow::ensure!(safe>=required,"Контекст {} превышает оценку доступной RAM (около {safe} токенов). Запрос сохранён; выберите Авто или уменьшите окно",required);
    if config.window_tokens==0{config.auto_max_tokens=config.auto_max_tokens.min(safe);}
    Ok(json!({"estimated":true,"kv_bytes_per_token":per_token,"weights_bytes":weights,"os_reserve_bytes":reserve,"safe_window_tokens":safe,"memory":memory}))

}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn physical_memory_limits_auto_context_but_never_silently_changes_manual_window() {
        let show = json!({"model_info":{"x.block_count":32,"x.attention.head_count":32,"x.attention.head_count_kv":8,"x.embedding_length":4096,"x.context_length":262144}});
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
    #[test]
    fn cgroup_headroom_never_uses_host_ram_beyond_the_container_quota(){
        let value=memory_bounds(64u64<<30,50u64<<30,Some((2u64<<30,700u64<<20)));
        assert_eq!(value["total_bytes"],2u64<<30);assert_eq!(value["available_bytes"],700u64<<20);
        let host=memory_bounds(4,3,None);assert_eq!(host["available_bytes"],3);
        assert_eq!(memory_bounds(4,3,Some((0,0)))["available_bytes"],3);
        assert_eq!(memory_bounds(4,3,Some((8,5)))["available_bytes"],3);
    }
    #[test]
    fn thread_policy_respects_manual_affinity_and_unknown_remote_hardware(){
        assert_eq!(thread_plan(0,true,Some(16),Some(8)).unwrap()["effective_threads"],7);
        assert_eq!(thread_plan(0,true,Some(2),Some(8)).unwrap()["effective_threads"],1);
        assert_eq!(thread_plan(0,true,Some(1),Some(1)).unwrap()["effective_threads"],1);
        assert_eq!(thread_plan(0,true,None,None).unwrap()["effective_threads"],1);
        assert_eq!(thread_plan(16,true,Some(16),Some(8)).unwrap()["effective_threads"],16);
        assert!(thread_plan(17,true,Some(16),Some(8)).is_err());
        let remote=thread_plan(0,false,Some(1),Some(1)).unwrap();assert!(remote["effective_threads"].is_null());assert!(remote["physical_cores"].is_null());
        assert_eq!(thread_plan(128,false,Some(1),Some(1)).unwrap()["effective_threads"],128);
    }
    #[test]
    fn cpu_weights_are_checked_without_kv_metadata_and_manual_native_window_is_used(){
        let memory=json!({"total_bytes":32u64<<30,"available_bytes":4u64<<30});
        let show=json!({"model_info":{"general.parameter_count":30_000_000_000u64},"details":{"quantization_level":"Q4_K_M"}});
        assert!(fit_context(&mut crate::context::Config::default(),&show,&memory,true).is_err());
        for quant in ["F16","BF16","F32","IQ4_XS"]{
            let show=json!({"model_info":{"general.parameter_count":30_000_000_000u64},"details":{"quantization_level":quant}});
            assert!(fit_context(&mut crate::context::Config::default(),&show,&memory,true).is_err());
        }
        let show=json!({"model_info":{"x.block_count":32,"x.attention.head_count":32,"x.attention.head_count_kv":8,"x.embedding_length":4096,"x.context_length":4096}});
        let mut config=crate::context::Config{window_tokens:65536,..Default::default()};
        fit_context(&mut config,&show,&memory,true).unwrap();assert_eq!(config.window_tokens,65536);
        assert!(fit_context(&mut config,&show,&json!({"total_bytes":32u64<<30,"available_bytes":1u64<<30}),true).is_err());
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
