//! Genuine ranking-head inference through the bundled llama.cpp runner.
//! No text-generation proxy scores and no automatic weight downloads.
use crate::api::Shared;
use anyhow::{Context, Result};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;
pub const MODEL: &str = "hf.co/ggml-org/Qwen3-reranker-0.6B-Q8_0-GGUF:Q8_0";

fn model_path(app: &Shared) -> Result<std::path::PathBuf> {
    anyhow::ensure!(
        std::env::var_os("LOCAL_GEMMA_OLLAMA").is_none(),
        "Реранкеру нужен локальный файл весов, внешняя Ollama здесь не поддерживается"
    );
    let root = crate::model_storage::locations(app)
        .get(MODEL)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| app.data.join("models"));
    let manifest = root.join("manifests/hf.co/ggml-org/Qwen3-reranker-0.6B-Q8_0-GGUF/Q8_0");
    let value: Value = serde_json::from_slice(
        &std::fs::read(manifest)
            .context("Сначала скачайте Qwen3-Reranker 0.6B в каталоге моделей")?,
    )?;
    let digest = value["layers"]
        .as_array()
        .context("Нет слоёв модели")?
        .iter()
        .find(|l| l["mediaType"] == "application/vnd.ollama.image.model")
        .and_then(|l| l["digest"].as_str())
        .context("Нет файла GGUF")?;
    let hash = digest.strip_prefix("sha256:").context("Неверный digest")?;
    anyhow::ensure!(
        hash.len() == 64 && hash.bytes().all(|c| c.is_ascii_hexdigit()),
        "Неверный hash GGUF"
    );
    let blob = root.join("blobs").join(format!("sha256-{hash}"));
    anyhow::ensure!(blob.is_file(), "Файл реранкера недоступен");
    Ok(blob)
}
pub fn reorder(original: &Value, response: &Value) -> Result<Value> {
    let source = original.as_array().context("Нет результатов поиска")?;
    let ranked = response["results"]
        .as_array()
        .context("Ранкер не вернул results")?;
    anyhow::ensure!(
        ranked.len() == source.len(),
        "Ранкер вернул неполный список"
    );
    let mut seen = std::collections::HashSet::new();
    let mut pairs = vec![];
    for row in ranked {
        let index = row["index"].as_u64().context("Нет индекса источника")? as usize;
        let score = row["relevance_score"]
            .as_f64()
            .context("Нет оценки релевантности")?;
        anyhow::ensure!(
            index < source.len() && seen.insert(index) && score.is_finite(),
            "Недостоверный ответ реранкера"
        );
        pairs.push((index, score));
    }
    pairs.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    Ok(Value::Array(
        pairs
            .into_iter()
            .map(|(index, score)| {
                let mut item = source[index].clone();
                item["relevance_score"] = json!(score);
                item["search_position"] = json!(index + 1);
                item
            })
            .collect(),
    ))
}
async fn run(
    app: &Shared,
    query: &str,
    results: &Value,
    cancel: &CancellationToken,
) -> Result<Value> {
    let _guard = tokio::select! {_=cancel.cancelled()=>anyhow::bail!("Остановлено"),g=app.reranker.lock()=>g};
    let blob = model_path(app)?;
    let engine = crate::tools::ollama(app);
    let parent = engine.parent().context("Нет пути движка")?;
    let name = if cfg!(windows) {
        "llama-server.exe"
    } else {
        "llama-server"
    };
    let binary = [
        parent.join("lib/ollama").join(name),
        parent.join(name),
        app.root.join("runtime").join(name),
    ]
    .into_iter()
    .find(|p| p.is_file())
    .context("В составе движка не найден llama-server для переранжирования")?;
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    drop(listener);
    let key = uuid::Uuid::new_v4().to_string();
    let url = format!("http://127.0.0.1:{port}");
    let threads = app.settings.lock().await.compute.cpu_threads;
    let mut command = tokio::process::Command::new(binary);
    command
        .args(["--model"])
        .arg(blob)
        .args([
            "--host",
            "127.0.0.1",
            "--port",
            &port.to_string(),
            "--api-key",
            &key,
            "--reranking",
            "--embedding",
            "--pooling",
            "rank",
            "--ctx-size",
            "4096",
            "--parallel",
            "1",
            "--gpu-layers",
            "0",
            "--no-webui",
            "--log-disable",
        ])
        .kill_on_drop(true);
    if threads > 0 {
        command.args(["--threads", &threads.to_string()]);
    }
    command
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    command.creation_flags(0x08000000);
    let mut child = command.spawn()?;
    let job = async {
        for _ in 0..100 {
            if let Some(code) = child.try_wait()? {
                anyhow::bail!("Ранкер завершился при запуске: {code}");
            }
            if app
                .client
                .get(format!("{url}/health"))
                .bearer_auth(&key)
                .timeout(std::time::Duration::from_secs(1))
                .send()
                .await
                .is_ok_and(|r| r.status().is_success())
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        }
        let docs: Vec<_> = results
            .as_array()
            .context("Нет списка источников")?
            .iter()
            .take(8)
            .map(|r| {
                format!(
                    "{}\n{}",
                    r["title"].as_str().unwrap_or(""),
                    r["snippet"]
                        .as_str()
                        .or(r["content"].as_str())
                        .unwrap_or("")
                )
                .chars()
                .take(1800)
                .collect::<String>()
            })
            .collect();
        let value:Value=app.client.post(format!("{url}/reranking")).bearer_auth(&key).json(&json!({"query":query.chars().take(1000).collect::<String>(),"documents":docs,"top_n":docs.len(),"return_documents":false})).send().await?.error_for_status()?.json().await?;
        reorder(results, &value)
    };
    let result = tokio::select! {_=cancel.cancelled()=>Err(anyhow::anyhow!("Переранжирование остановлено")),r=tokio::time::timeout(std::time::Duration::from_secs(60),job)=>r.context("Ранкер превысил лимит 60 секунд")?};
    let _ = child.kill().await;
    let _ = child.wait().await;
    result
}
pub async fn apply(app: &Shared, query: &str, value: &mut Value, cancel: &CancellationToken) {
    if !app.settings.lock().await.web.reranker_enabled {
        return;
    }
    if value["results"].as_array().is_none_or(|a| a.len() < 2) {
        return;
    }
    let start = std::time::Instant::now();
    match run(app, query, &value["results"], cancel).await {
        Ok(r) => {
            value["results"] = r;
            value["reranking"] = json!({"model":MODEL,"applied":true,"execution_ms":start.elapsed().as_millis(),"device":"cpu"});
        }
        Err(e) => {
            value["reranking"] = json!({"model":MODEL,"applied":false,"reason":format!("{e:#}"),"original_order_preserved":true})
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ranking_preserves_evidence() {
        let input = json!([{"url":"https://a.example","title":"A"},{"url":"https://b.example","title":"B"}]);
        let result=reorder(&input,&json!({"results":[{"index":0,"relevance_score":0.1},{"index":1,"relevance_score":0.9}]})).unwrap();
        assert_eq!(result[0]["url"], "https://b.example");
        assert_eq!(result[0]["search_position"], 2);
        assert!(reorder(&input,&json!({"results":[{"index":0,"relevance_score":0.5},{"index":0,"relevance_score":0.9}]})).is_err());
    }
}
