//! Hardware selection is applied to the private engine, never to global OS settings.
use crate::{api::Shared, tools};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{collections::BTreeMap, path::Path};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Config {
    pub backend: String,
    pub device: String,
    pub cpu_threads: usize,
    pub fallback_to_cpu: bool,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            backend: "legacy".into(),
            device: String::new(),
            cpu_threads: 0,
            fallback_to_cpu: true,
        }
    }
}
impl Config {
    pub fn key(&self) -> String {
        format!("{}:{}", self.backend, self.device)
    }
    pub fn normalize(&mut self, cpu_only: bool) {
        if self.backend == "legacy" {
            self.backend = if cpu_only { "cpu" } else { "auto" }.into();
        }
    }
    pub fn environment(&self) -> BTreeMap<String, String> {
        let mut env = BTreeMap::new();
        if self.backend == "cpu" || self.backend == "vulkan" || self.backend == "rocm" {
            env.insert("CUDA_VISIBLE_DEVICES".into(), "-1".into());
        }
        if self.backend == "cpu" || self.backend == "vulkan" || self.backend == "cuda" {
            env.insert("HIP_VISIBLE_DEVICES".into(), "-1".into());
            env.insert("ROCR_VISIBLE_DEVICES".into(), "-1".into());
        }
        if ["cpu", "cuda", "rocm"].contains(&self.backend.as_str()) {
            env.insert("OLLAMA_VULKAN".into(), "0".into());
            env.insert("GGML_VK_VISIBLE_DEVICES".into(), "-1".into());
        }
        if ["auto", "vulkan"].contains(&self.backend.as_str()) {
            env.insert("OLLAMA_VULKAN".into(), "1".into());
        }
        if !self.device.is_empty() {
            if self.backend == "cuda" {
                env.insert("CUDA_VISIBLE_DEVICES".into(), self.device.clone());
            }
            if self.backend == "vulkan" {
                env.insert("GGML_VK_VISIBLE_DEVICES".into(), self.device.clone());
            }
        }
        env
    }
}
#[derive(Default)]
pub struct Runtime {
    pub hardware: Option<Value>,
    pub applied_key: Option<String>,
    pub fallback_key: Option<String>,
    pub last_error: Option<String>,
    pub child: Option<tokio::process::Child>,
    pub group: Option<crate::process_group::Group>,
    pub installing: bool,
    pub package_cleanup_error:Option<String>,
}
impl Runtime {
    pub fn load(data: &Path) -> Self {
        let mut state = Self::default();
        if let Ok(bytes) = std::fs::read(data.join("compute-fallback.json")) {
            if let Ok(v) = serde_json::from_slice::<Value>(&bytes) {
                state.fallback_key = v["key"].as_str().map(str::to_owned);
                state.last_error = v["error"].as_str().map(str::to_owned);
            }
        }
        state
    }
}
pub fn installed(root: &Path, backend: &str) -> bool {
    if ["cpu", "auto"].contains(&backend) {
        return true;
    }
    if cfg!(target_os = "macos") {
        return backend == "metal";
    }
    if cfg!(windows) {
        let marker = root
            .join("runtime/ollama")
            .join(format!("{backend}-package.json"));
        let Some(files) = std::fs::read(marker)
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            .and_then(|v| v["files"].as_array().cloned())
        else {
            return false;
        };
        if files.is_empty()
            || files.iter().any(|p| {
                p.as_str()
                    .is_none_or(|p| !root.join("runtime/ollama").join(p).is_file())
            })
        {
            return false;
        }
    }
    let lib = root.join("runtime/ollama/lib/ollama");
    let Ok(dirs) = std::fs::read_dir(lib) else {
        return false;
    };
    dirs.flatten().any(|dir| {
        let name = dir.file_name().to_string_lossy().into_owned();
        let prefix = match backend {
            "cuda" => "cuda",
            "vulkan" => "vulkan",
            "rocm" => "rocm",
            _ => return false,
        };
        name.starts_with(prefix)
            && dir.path().is_dir()
            && std::fs::read_dir(dir.path()).is_ok_and(|files| {
                files.flatten().any(|f| {
                    f.file_name().to_string_lossy().starts_with(match backend {
                        "cuda" => "ggml-cuda",
                        "rocm" => "ggml-hip",
                        _ => "ggml-vulkan",
                    })
                })
            })
    })
}
pub async fn hardware(app: &Shared, refresh: bool) -> Value {
    if !refresh {
        if let Some(value) = &app.compute.lock().await.hardware {
            return value.clone();
        }
    }
    let mut value = json!({"cpu_name":std::env::consts::ARCH,"architecture":std::env::consts::ARCH,"platform":std::env::consts::OS,"logical_cpus":std::thread::available_parallelism().map(|n|n.get()).unwrap_or(1),"devices":[],"errors":[]});
    if let Some(python) = tools::python(app) {
        let mut cmd = tokio::process::Command::new(python);
        cmd.arg(app.root.join("runtime/compute_probe.py"))
            .env("PYTHONUTF8", "1");
        match tools::capture(cmd, &app.shutdown.child_token(), 35).await {
            Ok((0, out, _)) => match serde_json::from_str::<Value>(out.trim()) {
                Ok(v) => value = v,
                Err(e) => value["errors"] = json!([format!("Hardware probe: {e}")]),
            },
            Ok((code, _, err)) => {
                value["errors"] = json!([format!(
                    "Hardware probe exited {code}: {}",
                    tools::truncate(&err, 500)
                )])
            }
            Err(e) => value["errors"] = json!([format!("Hardware probe: {e}")]),
        }
    } else {
        value["errors"] =
            json!(["Для подробного определения GPU установите компоненты приложения."]);
    }
    app.compute.lock().await.hardware = Some(value.clone());
    value
}
pub async fn validate(app: &Shared, config: &Config) -> Result<()> {
    anyhow::ensure!(
        ["auto", "cpu", "cuda", "rocm", "vulkan", "metal"].contains(&config.backend.as_str()),
        "Неизвестный вычислительный движок"
    );
    let hardware = hardware(app, false).await;
    let max = hardware["logical_cpus"].as_u64().unwrap_or(1).max(1) as usize;
    anyhow::ensure!(
        config.cpu_threads <= max,
        "Число потоков CPU: 0 (авто) или от 1 до {max}"
    );
    anyhow::ensure!(
        config.device.len() <= 100,
        "Неверный идентификатор устройства"
    );
    if ["cpu", "auto", "metal", "rocm"].contains(&config.backend.as_str()) {
        anyhow::ensure!(
            config.device.is_empty(),
            "Для этого режима устройство выбирается автоматически"
        );
    }
    if !["cpu", "auto"].contains(&config.backend.as_str()) {
        anyhow::ensure!(
            installed(&app.root, &config.backend),
            "Модуль {} не установлен. Скачайте его в разделе «Вычисления».",
            config.backend
        );
        let devices = hardware["devices"].as_array().cloned().unwrap_or_default();
        let found = if config.backend == "rocm" {
            devices.iter().any(|d| d["vendor"] == "AMD")
                || hardware["display_adapters"].as_array().is_some_and(|list| {
                    list.iter().any(|d| {
                        d["AdapterCompatibility"]
                            .as_str()
                            .unwrap_or("")
                            .contains("Advanced Micro Devices")
                    })
                })
        } else {
            devices.iter().any(|d| {
                d["backend"] == config.backend
                    && (config.device.is_empty() || d["device"] == config.device)
            })
        };
        anyhow::ensure!(
            found,
            "Устройство {} не обнаружено. Проверьте драйвер или выберите CPU.",
            config.backend
        );
    }
    Ok(())
}
pub async fn force_cpu(app: &Shared, config: &Config) -> bool {
    config.backend == "cpu"
        || (config.fallback_to_cpu
            && app.compute.lock().await.fallback_key.as_deref() == Some(config.key().as_str()))
}
pub async fn fallback(app: &Shared, config: &Config, error: &str) {
    let error = tools::truncate(error, 1000);
    let key = config.key();
    {
        let mut runtime = app.compute.lock().await;
        runtime.fallback_key = Some(key.clone());
        runtime.last_error = Some(error.clone());
    }
    if let Err(persist)=crate::changes::replace(
        &app.data.join("compute-fallback.json"),
        json!({"key":key,"error":error}).to_string().as_bytes(),
    ){
        app.compute.lock().await.last_error=Some(format!("{error}; резервный режим действует в текущем процессе, но не сохранён на диске: {persist:#}"));
    }
}
pub async fn clear_fallback(app:&Shared)->Result<()> {
    let mut runtime=app.compute.lock().await;
    match std::fs::remove_file(app.data.join("compute-fallback.json")){
        Ok(())=>{},Err(error) if error.kind()==std::io::ErrorKind::NotFound=>{},
        Err(error)=>return Err(anyhow::anyhow!("Не удалось сбросить резервный режим; прежнее состояние сохранено: {error}"))
    }
    runtime.fallback_key=None;runtime.last_error=None;Ok(())
}
pub fn gpu_error(error: &str) -> bool {
    let text = error.to_lowercase();
    [
        "cuda",
        "vulkan",
        "rocm",
        "hip error",
        "gpu",
        "runner process",
        "runner has",
        "unable to load model",
        "memory allocation",
        "out of memory",
        "device lost",
    ]
    .iter()
    .any(|s| text.contains(s))
}
async fn healthy(app: &Shared) -> bool {
    app.client
        .get(format!("{}/api/version", crate::engine::ollama_url()))
        .timeout(std::time::Duration::from_secs(2))
        .send()
        .await
        .is_ok_and(|r| r.status().is_success())
}
pub fn engine_port() -> u16 {
    std::env::var("LOCAL_GEMMA_ENGINE_PORT")
        .ok()
        .and_then(|v| v.parse::<u16>().ok())
        .filter(|p| *p > 0)
        .unwrap_or(11435)
}
async fn stop_private(app: &Shared) -> Result<()> {
    anyhow::ensure!(
        std::env::var_os("LOCAL_GEMMA_OLLAMA").is_none(),
        "Внешняя Ollama не перезапускается приложением"
    );
    let (tracked, group) = {
        let mut runtime = app.compute.lock().await;
        (runtime.child.take(), runtime.group.take())
    };
    if let Some(group) = group {
        group.terminate();
        if let Some(mut child) = tracked {
            let _ = child.kill().await;
            let _ = child.wait().await;
        }
        return Ok(());
    }
    if let Some(mut child) = tracked {
        if child.try_wait()?.is_none() {
            #[cfg(windows)]
            {
                let mut stop = tokio::process::Command::new("taskkill");
                stop.args([
                    "/PID",
                    &child.id().context("Нет PID движка")?.to_string(),
                    "/T",
                    "/F",
                ]);
                stop.creation_flags(0x08000000);
                let status = stop.status().await?;
                anyhow::ensure!(status.success(), "Не удалось остановить встроенный движок");
            }
            #[cfg(not(windows))]
            child.kill().await?;
            let _ = child.wait().await;
        }
        return Ok(());
    }
    if !healthy(app).await {
        return Ok(());
    }
    anyhow::bail!("Порт встроенного движка занят процессом, которым приложение не владеет. Выберите свободный LOCAL_GEMMA_ENGINE_PORT или подключите внешний сервер через LOCAL_GEMMA_OLLAMA; чужой процесс не остановлен")
}
async fn recover_packages(app:&Shared)->Result<()> {
    let directory=app.data.join("package-transactions");
    let pending=match std::fs::read_dir(&directory){
        Ok(entries)=>{let mut found=false;for entry in entries{let entry=entry?;let name=entry.file_name();if name.to_str().is_some_and(|name|uuid::Uuid::parse_str(name).is_ok_and(|id|id.to_string().eq_ignore_ascii_case(name))){found=true;}}found},
        Err(error) if error.kind()==std::io::ErrorKind::NotFound=>false,
        Err(error)=>return Err(error.into())
    };
    if !pending{app.compute.lock().await.package_cleanup_error=None;return Ok(());}
    let python=tools::python(app).context("Для восстановления прерванной установки нужен Python из комплекта приложения")?;
    let mut command=tokio::process::Command::new(python);
    command.arg(app.root.join("runtime/compute_packages.py")).arg("--root").arg(&app.root).arg("--recover").env("PYTHONUTF8","1");
    let(code,_,error)=tools::capture(command,&app.shutdown.child_token(),0).await?;
    anyhow::ensure!(code==0,"Не удалось восстановить прерванную установку компонентов. Исходные файлы и копии сохранены: {}",tools::truncate(&error,2400));
    app.compute.lock().await.package_cleanup_error=if error.trim().is_empty(){None}else{Some(tools::truncate(&error,2400))};Ok(())
}
pub async fn ensure(app: &Shared, config: &Config) -> Result<()> {
    let _guard = app.startup.lock().await;
    recover_packages(app).await?;
    let mut effective = config.clone();
    if force_cpu(app, config).await {
        effective.backend = "cpu".into();
        effective.device.clear();
    }
    let key = format!("{}|{}", config.key(), effective.key());
    if std::env::var_os("LOCAL_GEMMA_OLLAMA").is_some() {
        anyhow::ensure!(healthy(app).await, "Внешняя Ollama недоступна");
        return Ok(());
    }
    if healthy(app).await {
        let owned={
            let mut runtime=app.compute.lock().await;
            runtime.group.is_some()&&runtime.child.as_mut().is_some_and(|child|matches!(child.try_wait(),Ok(None)))&&runtime.applied_key.as_deref()==Some(&key)
        };
        if owned{return Ok(());}
        stop_private(app).await?;
    }
    stop_private(app).await?;
    match start_private(app, &effective, &key).await {
        Ok(()) => Ok(()),
        Err(error) if effective.backend != "cpu" && config.fallback_to_cpu => {
            fallback(
                app,
                config,
                &format!("Запуск {}: {error:#}", effective.backend),
            )
            .await;
            stop_private(app).await?;
            effective.backend = "cpu".into();
            effective.device.clear();
            start_private(
                app,
                &effective,
                &format!("{}|{}", config.key(), effective.key()),
            )
            .await
        }
        Err(error) => Err(error),
    }
}
async fn start_private(app: &Shared, config: &Config, key: &str) -> Result<()> {
    let mut cmd = tokio::process::Command::new(tools::ollama(app));
    let models = app.data.join("models");
    std::fs::create_dir_all(&models)?;
    cmd.arg("serve")
        .env("OLLAMA_HOST", format!("127.0.0.1:{}", engine_port()))
        .env("OLLAMA_MODELS", models)
        .env("HOME", app.data.join("home"))
        .env("USERPROFILE", app.data.join("home"))
        .env("OLLAMA_NO_CLOUD", "true")
        .env("OLLAMA_MAX_LOADED_MODELS", "1")
        .env("OLLAMA_NUM_PARALLEL", "1");
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
    for (name, value) in config.environment() {
        cmd.env(name, value);
    }
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(app.data.join("logs/ollama.log"))?;
    cmd.stdout(log.try_clone()?).stderr(log);
    #[cfg(windows)]
    cmd.creation_flags(0x08000000);
    let mut group = crate::process_group::Group::prepare(&mut cmd)?;
    let mut child = cmd.spawn().context("Не найден встроенный движок Ollama")?;
    if let Err(error) = group.attach(&child) {
        let _ = child.kill().await;
        return Err(error);
    }
    let pid = child.id();
    let mut ready_since = None;
    for _ in 0..80 {
        if let Some(code) = child.try_wait()? {
            anyhow::bail!("Движок завершился ({code}); см. data/logs/ollama.log");
        }
        if healthy(app).await {
            let since = ready_since.get_or_insert_with(std::time::Instant::now);
            if since.elapsed() < std::time::Duration::from_secs(1) {
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                continue;
            }
            crate::changes::replace(
                &app.data.join("engine-owner.json"),
                &serde_json::to_vec_pretty(
                    &json!({"pid":pid,"key":key,"port":engine_port(),"executable":tools::ollama(app)}),
                )?,
            )?;
            let mut runtime = app.compute.lock().await;
            runtime.applied_key = Some(key.to_owned());
            runtime.child = Some(child);
            runtime.group = Some(group);
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
    let _ = child.kill().await;
    anyhow::bail!("Встроенный движок не запустился за 20 секунд")
}
pub async fn validate_reconfiguration(app: &Shared, config: &Config) -> Result<()> {
    if std::env::var_os("LOCAL_GEMMA_OLLAMA").is_some() {
        anyhow::ensure!(["auto","cpu"].contains(&config.backend.as_str()),"При подключении внешней Ollama выбор конкретной видеокарты настраивается на её сервере");
    }
    validate(app, config).await
}
pub async fn reconfigure(app: &Shared, config: &Config) -> Result<()> {
    validate_reconfiguration(app,config).await?;
    clear_fallback(app).await?;
    app.model_stores.lock().await.clear();
    ensure(app, config).await?;
    Ok(())
}
pub async fn restart(app: &Shared, config: &Config) -> Result<()> {
    anyhow::ensure!(
        std::env::var_os("LOCAL_GEMMA_OLLAMA").is_none(),
        "Внешняя Ollama не перезапускается приложением"
    );
    clear_fallback(app).await?;
    app.model_stores.lock().await.clear();
    stop_private(app).await?;
    ensure(app, config).await
}
pub async fn summary(app: &Shared) -> Value {
    let config = app.settings.lock().await.compute.clone();
    let fallback = force_cpu(app, &config).await;
    let error = if fallback && config.backend != "cpu" {
        app.compute.lock().await.last_error.clone()
    } else {
        None
    };
    let mut urls = vec![crate::engine::ollama_url()];
    urls.extend(
        app.model_stores
            .lock()
            .await
            .values()
            .map(|s| s.url.clone()),
    );
    let mut models = vec![];
    for url in urls {
        if let Ok(response) = app
            .client
            .get(format!("{url}/api/ps"))
            .timeout(std::time::Duration::from_secs(2))
            .send()
            .await
        {
            if let Ok(value) = response.json::<Value>().await {
                models.extend(value["models"].as_array().into_iter().flatten().cloned());
            }
        }
    }
    let (installing,package_cleanup_error)={let runtime=app.compute.lock().await;(runtime.installing,runtime.package_cleanup_error.clone())};
    json!({"requested":config,"forced_cpu":fallback,"fallback_error":error,"loaded_models":models,"installing":installing,"memory":crate::resources::memory(),"inference_busy":app.resources.gate.try_lock().is_err(),"release_after_inference":true,"cleanup_error":app.resources.failure.lock().unwrap().clone(),"package_cleanup_error":package_cleanup_error})
}
pub async fn model_on_gpu(app: &Shared, model: &str) -> Option<bool> {
    let value = app
        .client
        .get(format!(
            "{}/api/ps",
            crate::model_storage::endpoint(app, model).await.ok()?
        ))
        .timeout(std::time::Duration::from_secs(2))
        .send()
        .await
        .ok()?
        .json::<Value>()
        .await
        .ok()?;
    let found = value["models"]
        .as_array()?
        .iter()
        .find(|m| m["name"] == model || m["model"] == model)?;
    Some(found["size_vram"].as_u64()? > 0)
}
pub async fn install_package(
    app: &Shared,
    backend: &str,
    tx: &tokio::sync::mpsc::Sender<Value>,
) -> Result<()> {
    anyhow::ensure!(
        std::env::var_os("LOCAL_GEMMA_OLLAMA").is_none(),
        "Установка модулей внешнего сервера выполняется отдельно"
    );
    let config = app.settings.lock().await.compute.clone();
    ensure(app, &config).await?;
    let version = app
        .client
        .get(format!("{}/api/version", crate::engine::ollama_url()))
        .send()
        .await?
        .json::<Value>()
        .await?["version"]
        .as_str()
        .context("Неизвестна версия встроенного движка")?
        .to_string();
    let python = tools::python(app)
        .context("Для скачивания компонентов нужен Python из комплекта приложения")?;
    {
        let _startup = app.startup.lock().await;
        app.model_stores.lock().await.clear();
        stop_private(app).await?;
    }
    app.compute.lock().await.installing = true;
    let _=tx.send(json!({"type":"progress","message":format!("Скачивание {backend} для Ollama {version}. Файлы сохраняются на диске приложения.")})).await;
    let mut cmd = tokio::process::Command::new(python);
    cmd.arg(app.root.join("runtime/compute_packages.py"))
        .arg("--root")
        .arg(&app.root)
        .arg("--backend")
        .arg(backend)
        .arg("--version")
        .arg(version)
        .env("PYTHONUTF8", "1");
    let result = tools::capture(cmd, &app.shutdown.child_token(), 0).await;
    app.compute.lock().await.installing = false;
    // Bring back the private service even when a download fails.
    let restart = ensure(app, &config).await;
    let (code, _, err) = result?;
    anyhow::ensure!(
        code == 0,
        "Компонент не установлен: {}",
        tools::truncate(&err, 1800)
    );
    restart?;
    app.compute.lock().await.hardware = None;
    let _=tx.send(json!({"type":"progress","message":"Модуль установлен. Теперь выберите устройство и сохраните настройки."})).await;
    Ok(())
}
pub async fn info(app: &Shared, refresh: bool) -> Value {
    let inventory = hardware(app, refresh).await;
    json!({"hardware":inventory,"runtime":summary(app).await,"packages":{"cuda":installed(&app.root,"cuda"),"rocm":installed(&app.root,"rocm"),"vulkan":installed(&app.root,"vulkan"),"metal":installed(&app.root,"metal")},"package_install_supported":cfg!(windows),"scope":"Выбор относится к LLM. OCR и распознавание речи используют отдельные компоненты."})
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn fallback_disk_failures_preserve_current_recovery_state(){
        let root=std::env::temp_dir().join(format!("compute-state-{}",uuid::Uuid::new_v4()));
        let app=std::sync::Arc::new(crate::api::App::new(root.clone()).unwrap());
        let path=app.data.join("compute-fallback.json");std::fs::create_dir(&path).unwrap();
        let config=Config{backend:"cuda".into(),device:"fixture".into(),..Default::default()};
        fallback(&app,&config,"GPU fixture failed").await;
        assert!(force_cpu(&app,&config).await);
        let before=app.compute.lock().await.last_error.clone().unwrap();assert!(before.contains("не сохранён на диске"));
        assert!(clear_fallback(&app).await.is_err());assert!(force_cpu(&app,&config).await);
        assert_eq!(app.compute.lock().await.last_error.as_deref(),Some(before.as_str()));
        std::fs::remove_dir(&path).unwrap();fallback(&app,&config,"GPU fixture failed").await;
        let recovered=Runtime::load(&app.data);assert_eq!(recovered.fallback_key,Some(config.key()));
        clear_fallback(&app).await.unwrap();assert!(!path.exists());assert!(!force_cpu(&app,&config).await);
        drop(app);std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn backend_filters_are_isolated_and_device_ids_are_preserved() {
        let cpu = Config {
            backend: "cpu".into(),
            ..Config::default()
        }
        .environment();
        assert_eq!(cpu["CUDA_VISIBLE_DEVICES"], "-1");
        assert_eq!(cpu["GGML_VK_VISIBLE_DEVICES"], "-1");
        let cuda = Config {
            backend: "cuda".into(),
            device: "GPU-fixture".into(),
            ..Config::default()
        }
        .environment();
        assert_eq!(cuda["CUDA_VISIBLE_DEVICES"], "GPU-fixture");
        assert_eq!(cuda["OLLAMA_VULKAN"], "0");
        let vk = Config {
            backend: "vulkan".into(),
            device: "2".into(),
            ..Config::default()
        }
        .environment();
        assert_eq!(vk["GGML_VK_VISIBLE_DEVICES"], "2");
        assert_eq!(vk["HIP_VISIBLE_DEVICES"], "-1");
        let auto = Config {
            backend: "auto".into(),
            ..Config::default()
        }
        .environment();
        assert!(!auto.contains_key("CUDA_VISIBLE_DEVICES"));
    }
}
