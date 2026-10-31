use crate::api::Shared;
use anyhow::{Context, Result};
use serde_json::{json, Value};

pub const QWEN3_ABLITERATED: &str = "richardyoung/qwen3-14b-abliterated:latest";

// Official Ollama Qwen3 ChatML template (MIT), including tools and /no_think.
// https://ollama.com/library/qwen3:14b/blobs/ae370d884f10
const QWEN3_TEMPLATE: &str = include_str!("qwen3-tools.gotmpl");

pub async fn prepare(app: &Shared, model: &str) -> Result<()> {
    if model != QWEN3_ABLITERATED {
        return Ok(());
    }
    let _setup = app.model_setup.lock().await;
    let endpoint = crate::engine::ollama_url();
    let show: Value = app
        .client
        .post(format!("{endpoint}/api/show"))
        .json(&json!({"model":model}))
        .send()
        .await?
        .error_for_status()
        .context("Сначала скачайте Qwen3 14B Abliterated в настройках")?
        .json()
        .await?;
    let template = show["template"].as_str().unwrap_or("");
    if template.contains(".Tools") && template.contains(".ToolCalls") {
        return Ok(());
    }
    let backup = app.data.join("logs/qwen3-original-template.json");
    if !backup.exists() {
        tokio::fs::write(backup, serde_json::to_vec_pretty(&json!({
            "model":model,"template":show["template"],"parameters":show["parameters"],"system":show["system"]
        }))?).await?;
    }
    // Reuse the existing GGUF blobs; only request-format metadata changes.
    let result: Value = app
        .client
        .post(format!("{endpoint}/api/create"))
        .json(&json!({"model":model,"from":model,"template":QWEN3_TEMPLATE,"stream":false}))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    if let Some(error) = result["error"].as_str() {
        anyhow::bail!("Настройка Qwen3: {error}");
    }
    anyhow::ensure!(
        result["status"] == "success",
        "Не удалось подключить инструменты Qwen3: {result}"
    );
    Ok(())
}
