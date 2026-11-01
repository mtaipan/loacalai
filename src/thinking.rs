use crate::api::Shared;
use anyhow::Result;
use serde_json::{json, Value};

pub fn choices(show: &Value) -> Vec<Value> {
    if let Some(a) = show["thinking"]["values"].as_array() {
        return a
            .iter()
            .filter(|v| v.is_boolean() || v.is_string())
            .cloned()
            .collect();
    }
    if show["capabilities"]
        .as_array()
        .is_some_and(|a| a.iter().any(|v| v == "thinking"))
    {
        return vec![json!("auto")];
    }
    vec![json!(false)]
}
pub fn resolve(selection: &str, show: &Value) -> Result<Value> {
    let values = choices(show);
    if selection == "auto" {
        return Ok(Value::Null);
    }
    let requested = match selection {
        "off" => json!(false),
        "on" => json!(true),
        level => json!(level),
    };
    anyhow::ensure!(
        values.contains(&requested),
        "Эта модель не поддерживает режим рассуждений «{selection}». Доступно: {values:?}"
    );
    Ok(requested)
}
pub async fn show(app: &Shared, model: &str) -> Result<Value> {
    crate::engine::ensure_ollama(app).await?;
    crate::model_adapter::prepare(app, model).await?;
    let mut v: Value = app
        .client
        .post(format!(
            "{}/api/show",
            crate::model_storage::endpoint(app, model).await?
        ))
        .json(&json!({"model":model}))
        .timeout(std::time::Duration::from_secs(20))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    if crate::specialists::adapted(model) {
        v["thinking"] = json!({"values":[false]});
    }
    Ok(v)
}
pub async fn show_cancellable(
    app: &Shared,
    model: &str,
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<Value> {
    tokio::select! {_=cancel.cancelled()=>anyhow::bail!("Остановлено пользователем"),value=show(app,model)=>value}
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn controls_are_model_specific() {
        let q = json!({"thinking":{"values":[false,true],"default":true}});
        assert_eq!(resolve("on", &q).unwrap(), true);
        assert_eq!(resolve("off", &q).unwrap(), false);
        assert!(resolve("high", &q).is_err());
        let g = json!({"thinking":{"values":["low","medium","high"]}});
        assert!(resolve("off", &g).is_err());
        assert_eq!(resolve("high", &g).unwrap(), "high");
        assert!(resolve("on", &json!({})).is_err());
        assert_eq!(resolve("auto", &q).unwrap(), Value::Null);
    }
}
