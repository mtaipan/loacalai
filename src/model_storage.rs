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
    _group: crate::process_group::Group,
}
pub fn locations(app: &Shared) -> Result<BTreeMap<String, String>> {
    match std::fs::read(app.data.join("model-locations.json")) {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes)
            .context("Повреждён model-locations.json; исходный файл сохранён")?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
        Err(e) => Err(e.into()),
    }
}
pub fn set_location(app: &Shared, model: &str, directory: &str, installed: bool) -> Result<()> {
    anyhow::ensure!(
        std::env::var_os("LOCAL_GEMMA_OLLAMA").is_none(),
        "Внешняя Ollama управляет хранилищем самостоятельно"
    );
    let mut all = locations(app)?;
    let path = PathBuf::from(directory);
    anyhow::ensure!(path.is_absolute(), "Нужен абсолютный путь к папке");
    std::fs::create_dir_all(&path).context("Не удалось создать папку моделей")?;
    let path = dunce::canonicalize(path)?;
    std::fs::create_dir_all(app.data.join("models"))?;
    let default = dunce::canonicalize(app.data.join("models"))?;
    let value = path.to_string_lossy().to_string();
    if let Some(old) = all.get(model) {
        anyhow::ensure!(
            *old == value || !installed,
            "Модель установлена в {old}. Сначала удалите модель; автоматического переноса нет."
        );
    }
    if path != default {
        all.insert(model.into(), value);
    } else {
        all.remove(model);
    }
    crate::changes::replace(
        &app.data.join("model-locations.json"),
        &serde_json::to_vec_pretty(&all)?,
    )?;
    Ok(())
}
pub fn forget(app: &Shared, model: &str) -> Result<()> {
    let mut all = locations(app)?;
    all.remove(model);
    crate::changes::replace(
        &app.data.join("model-locations.json"),
        &serde_json::to_vec_pretty(&all)?,
    )?;
    Ok(())
}
pub async fn endpoint(app: &Shared, model: &str) -> Result<String> {
    let Some(path) = locations(app)?.get(model).cloned() else {
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
    let mut group = crate::process_group::Group::prepare(&mut cmd)?;
    let mut child = cmd.spawn()?;
    if let Err(error) = group.attach(&child) {
        let _ = child.kill().await;
        return Err(error);
    }
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
                    _group: group,
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
    let all = match locations(app) {
        Ok(all) => all,
        Err(error) => {
            return json!({"models":[],"locations":{},"errors":[{"error":format!("{error:#}")}],"online":false})
        }
    };
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
            let mut stores = app.model_stores.lock().await;
            let running = stores
                .get_mut(path)
                .filter(|store| store.child.id().is_some())
                .map(|store| store.url.clone());
            match running {
                Some(url) => endpoints.push((url, path.clone())),
                None => match installed_manifests(std::path::Path::new(path)) {
                    Ok(found) => models.extend(found),
                    Err(e) => errors.push(json!({"path":path,"error":e.to_string()})),
                },
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
fn installed_manifests(root: &std::path::Path) -> Result<Vec<Value>> {
    fn visit(
        root: &std::path::Path,
        dir: &std::path::Path,
        found: &mut Vec<Value>,
        depth: usize,
    ) -> Result<()> {
        if depth > 5 {
            return Ok(());
        }
        for item in std::fs::read_dir(dir)? {
            let item = item?;
            let kind = item.file_type()?;
            if kind.is_symlink() {
                continue;
            }
            if kind.is_dir() {
                visit(root, &item.path(), found, depth + 1)?;
                continue;
            }
            if !kind.is_file() {
                continue;
            }
            let path = item.path();
            if item.metadata()?.len() > 1024 * 1024 {
                continue;
            }
            let bytes = std::fs::read(&path)?;
            let manifest: Value = serde_json::from_slice(&bytes)?;
            let layers = manifest["layers"]
                .as_array()
                .context("Manifest без layers")?;
            let complete = layers
                .iter()
                .chain(std::iter::once(&manifest["config"]))
                .all(|layer| {
                    let Some(digest) = layer["digest"]
                        .as_str()
                        .and_then(|s| s.strip_prefix("sha256:"))
                    else {
                        return false;
                    };
                    if digest.len() != 64 || !digest.bytes().all(|b| b.is_ascii_hexdigit()) {
                        return false;
                    }
                    std::fs::metadata(root.join("blobs").join(format!("sha256-{digest}")))
                        .is_ok_and(|m| Some(m.len()) == layer["size"].as_u64())
                });
            if !complete {
                continue;
            }
            let relative = path.strip_prefix(root.join("manifests"))?;
            let parts: Vec<_> = relative
                .iter()
                .map(|p| p.to_string_lossy().to_string())
                .collect();
            if parts.len() < 3 {
                continue;
            }
            let tag = parts.last().unwrap();
            let name = if parts[0] == "registry.ollama.ai" && parts[1] == "library" {
                parts[2..parts.len() - 1].join("/")
            } else {
                parts[..parts.len() - 1].join("/")
            };
            let model = format!("{name}:{tag}");
            let size = layers
                .iter()
                .filter_map(|l| l["size"].as_u64())
                .sum::<u64>();
            found.push(json!({"name":model,"model":model,"size":size,"storage_path":root.to_string_lossy(),"source":"manifest"}));
        }
        Ok(())
    }
    anyhow::ensure!(
        root.is_dir(),
        "Диск или папка модели недоступны: {}",
        root.display()
    );
    let mut found = vec![];
    let manifests = root.join("manifests");
    if manifests.is_dir() {
        visit(root, &manifests, &mut found, 0)?;
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn offline_inventory_requires_complete_blobs_without_starting_an_engine() {
        let root = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        let manifests = root.join("manifests/registry.ollama.ai/library/qwen/test");
        std::fs::create_dir_all(manifests.parent().unwrap()).unwrap();
        std::fs::create_dir_all(root.join("blobs")).unwrap();
        let config = "0".repeat(64);
        let model = "1".repeat(64);
        std::fs::write(root.join("blobs").join(format!("sha256-{config}")), b"{}").unwrap();
        let blob = root.join("blobs").join(format!("sha256-{model}"));
        std::fs::write(&blob, b"abc").unwrap();
        std::fs::write(manifests,json!({"config":{"digest":format!("sha256:{config}"),"size":2},"layers":[{"digest":format!("sha256:{model}"),"size":3}]}).to_string()).unwrap();
        let found = installed_manifests(&root).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0]["name"], "qwen:test");
        std::fs::write(blob, b"a").unwrap();
        assert!(installed_manifests(&root).unwrap().is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }
}
