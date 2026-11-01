//! Additional model stores use separate private Ollama instances. No cross-volume
//! symlinks, administrator rights, or temporary copies of model weights required.
use crate::api::Shared;
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::{collections::BTreeMap, path::PathBuf};

pub struct Store {
    pub url: String,
    key: String,
    child: tokio::process::Child,
}
pub fn locations(app: &Shared) -> BTreeMap<String, String> {
    std::fs::read(app.data.join("model-locations.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}
pub fn set_location(app: &Shared, model: &str, directory: &str) -> Result<()> {
    anyhow::ensure!(
        std::env::var_os("LOCAL_GEMMA_OLLAMA").is_none(),
        "Внешняя Ollama управляет хранилищем самостоятельно"
    );
    let mut all = locations(app);
    let path = PathBuf::from(directory);
    anyhow::ensure!(path.is_absolute(), "Нужен абсолютный путь к папке");
    std::fs::create_dir_all(&path).context("Не удалось создать папку моделей")?;
    let path = dunce::canonicalize(path)?;
    std::fs::create_dir_all(app.data.join("models"))?;
    let default = dunce::canonicalize(app.data.join("models"))?;
    let value = path.to_string_lossy().to_string();
    if let Some(old) = all.get(model) {
        anyhow::ensure!(*old == value, "Место этой модели уже назначено: {old}. Сначала удалите модель; автоматического переноса нет.");
    }
    if path != default {
        all.insert(model.into(), value);
    }
    crate::changes::replace(&app.data.join("model-locations.json"),&serde_json::to_vec_pretty(&all)?)?;
    Ok(())
}
pub fn forget(app: &Shared, model: &str) -> Result<()> {
    let mut all = locations(app);
    all.remove(model);
    crate::changes::replace(&app.data.join("model-locations.json"),&serde_json::to_vec_pretty(&all)?)?;
    Ok(())
}
pub async fn endpoint(app: &Shared, model: &str) -> Result<String> {
    let Some(path) = locations(app).get(model).cloned() else {
        crate::engine::ensure_ollama(app).await?;
        return Ok(crate::engine::ollama_url());
    };
    let mut config = app.settings.lock().await.compute.clone();
    if crate::compute::force_cpu(app, &config).await {
        config.backend = "cpu".into();
        config.device.clear();
    }
    let key = config.key();
    let mut stores = app.model_stores.lock().await;
    if let Some(store) = stores.get_mut(&path) {
        if store.key == key && store.child.try_wait()?.is_none() {
            return Ok(store.url.clone());
        }
    }
    if let Some(mut old) = stores.remove(&path) {
        let _ = old.child.kill().await;
    }
    anyhow::ensure!(
        PathBuf::from(&path).is_dir(),
        "Диск или папка модели недоступны: {path}"
    );
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    drop(listener);
    let url = format!("http://127.0.0.1:{port}");
    let mut cmd = tokio::process::Command::new(crate::tools::ollama(app));
    cmd.arg("serve")
        .kill_on_drop(true)
        .env("OLLAMA_HOST", format!("127.0.0.1:{port}"))
        .env("OLLAMA_MODELS", &path)
        .env("OLLAMA_NO_CLOUD", "true")
        .env("OLLAMA_MAX_LOADED_MODELS", "1")
        .env("OLLAMA_NUM_PARALLEL", "1")
        .env("HOME", app.data.join("home"))
        .env("USERPROFILE", app.data.join("home"));
    for name in [
        "CUDA_VISIBLE_DEVICES",
        "HIP_VISIBLE_DEVICES",
        "ROCR_VISIBLE_DEVICES",
        "GGML_VK_VISIBLE_DEVICES",
        "OLLAMA_VULKAN",
        "OLLAMA_LLM_LIBRARY",
        "GPU_DEVICE_ORDINAL",
    ] {
        cmd.env_remove(name);
    }
    for (k, v) in config.environment() {
        cmd.env(k, v);
    }
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(app.data.join("logs/model-stores.log"))?;
    cmd.stdout(log.try_clone()?).stderr(log);
    #[cfg(windows)]
    cmd.creation_flags(0x08000000);
    let mut child = cmd.spawn()?;
    for _ in 0..100 {
        if let Some(code) = child.try_wait()? {
            anyhow::bail!("Движок хранилища завершился: {code}");
        }
        if app
            .client
            .get(format!("{url}/api/version"))
            .timeout(std::time::Duration::from_secs(1))
            .send()
            .await
            .is_ok_and(|r| r.status().is_success())
        {
            stores.insert(
                path,
                Store {
                    url: url.clone(),
                    key,
                    child,
                },
            );
            return Ok(url);
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    anyhow::bail!("Движок дополнительного хранилища не запустился")
}
pub async fn tags(app: &Shared) -> Value {
    let mut models = vec![];
    let mut errors = vec![];
    let all = locations(app);
    let mut seen = std::collections::HashSet::new();
    let mut endpoints = vec![(
        crate::engine::ollama_url(),
        app.data.join("models").to_string_lossy().to_string(),
    )];
    for (model, path) in &all {
        if model == "whisper-small" {
            continue;
        }
        if seen.insert(path.clone()) {
            match endpoint(app, model).await {
                Ok(url) => endpoints.push((url, path.clone())),
                Err(e) => errors.push(json!({"path":path,"error":e.to_string()})),
            }
        }
    }
    let mut online = false;
    for (url, path) in endpoints {
        if let Ok(r) = app
            .client
            .get(format!("{url}/api/tags"))
            .timeout(std::time::Duration::from_secs(3))
            .send()
            .await
        {
            if let Ok(v) = r.json::<Value>().await {
                if let Some(a) = v["models"].as_array() {
                    online = true;
                    for m in a {
                        let mut m = m.clone();
                        m["storage_path"] = json!(path);
                        models.push(m);
                    }
                }
            }
        }
    }
    json!({"models":models,"locations":all,"errors":errors,"online":online})
}
