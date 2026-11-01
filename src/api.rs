use crate::{engine, tools};
use axum::{
    body::Body,
    extract::{DefaultBodyLimit, Multipart, Path, State},
    http::{HeaderMap, Request, StatusCode},
    middleware::{self, Next},
    response::{
        sse::{Event, KeepAlive},
        IntoResponse, Response, Sse,
    },
    routing::{get, post},
    Json, Router,
};
use include_dir::{include_dir, Dir};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{collections::HashMap, convert::Infallible, path::PathBuf, sync::Arc};
use tokio::sync::{mpsc, Mutex};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

static WEB: Dir = include_dir!("$CARGO_MANIFEST_DIR/web");
pub type Shared = Arc<App>;
pub type ApiResult<T> = Result<Json<T>, ApiError>;
fn require(condition: bool, message: &str) -> Result<(), ApiError> {
    if condition {
        Ok(())
    } else {
        Err(ApiError(anyhow::anyhow!("{message}")))
    }
}
pub struct ApiError(pub anyhow::Error);
#[derive(Debug)]
struct HttpFailure(StatusCode, String);
impl std::fmt::Display for HttpFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.1)
    }
}
impl std::error::Error for HttpFailure {}
impl ApiError {
    fn status(status: StatusCode, message: &str) -> Self {
        Self(HttpFailure(status, message.into()).into())
    }
    fn status_code(&self) -> StatusCode {
        if let Some(error) = self.0.downcast_ref::<HttpFailure>() {
            return error.0;
        }
        if self.0.is::<crate::task_store::MissingRun>() {
            return StatusCode::NOT_FOUND;
        }
        if let Some(error) = self.0.downcast_ref::<reqwest::Error>() {
            return if error.is_timeout() {
                StatusCode::GATEWAY_TIMEOUT
            } else {
                StatusCode::BAD_GATEWAY
            };
        }
        if let Some(error) = self.0.downcast_ref::<std::io::Error>() {
            return match error.kind() {
                std::io::ErrorKind::NotFound => StatusCode::NOT_FOUND,
                std::io::ErrorKind::PermissionDenied => StatusCode::FORBIDDEN,
                std::io::ErrorKind::AlreadyExists => StatusCode::CONFLICT,
                std::io::ErrorKind::InvalidInput => StatusCode::BAD_REQUEST,
                _ => StatusCode::INTERNAL_SERVER_ERROR,
            };
        }
        if self.0.is::<rusqlite::Error>() || self.0.is::<serde_json::Error>() {
            return StatusCode::INTERNAL_SERVER_ERROR;
        }
        StatusCode::BAD_REQUEST
    }
}
impl<E: Into<anyhow::Error>> From<E> for ApiError {
    fn from(e: E) -> Self {
        Self(e.into())
    }
}
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = self.status_code();
        let code = match status {
            StatusCode::NOT_FOUND => "not_found",
            StatusCode::FORBIDDEN => "permission_denied",
            StatusCode::CONFLICT => "conflict",
            StatusCode::BAD_GATEWAY | StatusCode::GATEWAY_TIMEOUT => "upstream_error",
            StatusCode::INTERNAL_SERVER_ERROR => "internal_error",
            _ => "invalid_request",
        };
        (
            status,
            Json(json!({"code":code,"error":format!("{:#}",self.0)})),
        )
            .into_response()
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub revision: u64,
    #[serde(default = "default_permission_mode")]
    pub permission_mode: String,
    #[serde(default)]
    pub schema_version: u32,
    #[serde(default)]
    pub context: crate::context::Config,
    #[serde(default)]
    pub web: crate::web_tools::Config,
    #[serde(default)]
    pub assistant: crate::agent::AssistantConfig,
    #[serde(default = "default_actions")]
    pub agent_max_actions: usize,
    #[serde(default = "default_task_seconds")]
    pub agent_timeout_seconds: u64,
    #[serde(default = "crate::models::default_translation")]
    pub translation_model: String,
    #[serde(default)]
    pub quick_model: String,
    #[serde(default = "crate::models::default_assistant")]
    pub agent_model: String,
    #[serde(default = "crate::models::default_assistant")]
    pub team_model: String,
    #[serde(default = "crate::models::default_assistant")]
    pub chat_model: String,
    pub workspace: String,
    #[serde(default)]
    pub recent_projects: Vec<String>,
    pub source: String,
    pub target: String,
    pub theme: String,
    pub cpu_only: bool,
    #[serde(default)]
    pub compute: crate::compute::Config,
    #[serde(default = "default_true")]
    pub computer_access: bool,
    #[serde(default = "default_auto")]
    pub quick_source: String,
    #[serde(default = "default_hotkey")]
    pub quick_hotkey: String,
    #[serde(default = "default_area_hotkey")]
    pub quick_area_hotkey: String,
}
fn default_true() -> bool {
    true
}
fn default_permission_mode() -> String {
    "manual".into()
}
fn default_actions() -> usize {
    0
}
fn default_task_seconds() -> u64 {
    0
}
fn default_auto() -> String {
    "auto".into()
}
fn default_hotkey() -> String {
    if cfg!(target_os = "macos") {
        "Super+Shift+T"
    } else {
        "Control+Shift+T"
    }
    .into()
}
pub fn default_area_hotkey() -> String {
    if cfg!(target_os = "macos") {
        "Super+Shift+O"
    } else {
        "Control+Shift+O"
    }
    .into()
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Attachment {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub size: u64,
    pub path: String,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Message {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub material: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostics: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub id: String,
    pub role: String,
    pub content: String,
    #[serde(default)]
    pub attachments: Vec<Attachment>,
    #[serde(default)]
    pub tools: Vec<Value>,
    pub created: String,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Conversation {
    #[serde(default)]
    pub history_task: String,
    #[serde(default)]
    pub history_workspace: String,
    #[serde(default)]
    pub history_model: String,
    pub id: String,
    pub title: String,
    pub updated: String,
    pub mode: String,
    pub messages: Vec<Message>,
    #[serde(default)]
    pub history: Vec<Value>,
}
pub struct Run {
    pub cancel: CancellationToken,
    pub conversation: String,
}
pub struct App {
    pub processes: Mutex<HashMap<String,crate::processes::Active>>,
    pub process_recovery_errors:Vec<String>,
    pub settings_write: Mutex<()>,
    pub resources: crate::resources::Broker,
    pub tasks: crate::task_store::Store,
    pub root: PathBuf,
    pub data: PathBuf,
    pub client: reqwest::Client,
    pub token: String,
    pub settings: Mutex<Settings>,
    pub runs: Mutex<HashMap<String, Run>>,
    pub approvals: Mutex<HashMap<String, crate::permissions::Pending>>,
    pub grants: Mutex<crate::permissions::Grants>,
    pub storage: Mutex<()>,
    pub file_changes: Mutex<()>,
    pub startup: Mutex<()>,
    pub compute: Mutex<crate::compute::Runtime>,
    pub model_setup: Mutex<()>,
    pub reranker: Mutex<()>,
    pub model_stores: Mutex<HashMap<String, crate::model_storage::Store>>,
    pub web_sessions: Mutex<HashMap<String, Arc<Mutex<crate::web_tools::Session>>>>,
    pub model_lifecycle: Arc<tokio::sync::RwLock<()>>,
    pub shutdown: CancellationToken,
    pub quick: Mutex<Value>,
    pub quick_cancel: Mutex<Option<CancellationToken>>,
}
impl App {
    pub fn new(root: PathBuf) -> anyhow::Result<Self> {
        let data = root.join("data");
        for folder in ["chats", "uploads", "workspace", "logs"] {
            std::fs::create_dir_all(data.join(folder))?;
        }
        let defaults = Settings {
            revision: 0,
            permission_mode: default_permission_mode(),
            schema_version: 1,
            context: Default::default(),
            web: Default::default(),
            assistant: Default::default(),
            agent_max_actions: default_actions(),
            agent_timeout_seconds: default_task_seconds(),
            translation_model: crate::models::default_translation(),
            quick_model: String::new(),
            agent_model: crate::models::default_assistant(),
            team_model: crate::models::default_assistant(),
            chat_model: crate::models::default_assistant(),
            workspace: data.join("workspace").to_string_lossy().into(),
            recent_projects: vec![],
            source: "en".into(),
            target: "ru".into(),
            theme: "dark".into(),
            cpu_only: false,
            compute: crate::compute::Config {
                backend: "auto".into(),
                ..Default::default()
            },
            computer_access: true,
            quick_source: default_auto(),
            quick_hotkey: default_hotkey(),
            quick_area_hotkey: default_area_hotkey(),
        };
        let settings_path = data.join("settings.json");
        let mut settings: Settings = match std::fs::read(&settings_path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| anyhow::anyhow!("Не удалось прочитать настройки {}: {e}. Исходный файл сохранён; восстановите его из резервной копии.",settings_path.display()))?,
            Err(e) if e.kind()==std::io::ErrorKind::NotFound => defaults,
            Err(e) => return Err(anyhow::anyhow!("Не удалось открыть настройки {}: {e}",settings_path.display())),
        };
        if settings.schema_version == 0 {
            // Legacy mandatory defaults were not an explicit user quota.
            settings.agent_max_actions = 0;
            settings.agent_timeout_seconds = 0;
            settings.schema_version = 1;
        }
        settings.compute.normalize(settings.cpu_only);
        settings.cpu_only = settings.compute.backend == "cpu";
        let compute = crate::compute::Runtime::load(&data);
        let tasks = crate::task_store::Store::open(&data)?;
        tasks.recover_deletions(&data)?;
        let process_recovery_errors=crate::processes::recover(&data)?;
        Ok(Self {
            processes:Mutex::new(HashMap::new()),
            process_recovery_errors,
            settings_write: Mutex::new(()),
            resources: Default::default(),
            tasks,
            grants: Mutex::new(crate::permissions::Grants::load(&data)?),
            root,
            data,
            client: reqwest::Client::builder()
                .no_proxy()
                .connect_timeout(std::time::Duration::from_secs(15))
                .build()?,
            token: Uuid::new_v4().to_string(),
            settings: Mutex::new(settings),
            runs: Mutex::new(HashMap::new()),
            approvals: Mutex::new(HashMap::new()),
            storage: Mutex::new(()),
            file_changes: Mutex::new(()),
            startup: Mutex::new(()),
            compute: Mutex::new(compute),
            model_setup: Mutex::new(()),
            reranker: Mutex::new(()),
            model_stores: Mutex::new(HashMap::new()),
            web_sessions: Mutex::new(HashMap::new()),
            model_lifecycle: Arc::new(tokio::sync::RwLock::new(())),
            shutdown: CancellationToken::new(),
            quick: Mutex::new(json!({"status":"idle","text":"","translation":""})),
            quick_cancel: Mutex::new(None),
        })
    }
    pub fn chat_path(&self, id: &str) -> anyhow::Result<PathBuf> {
        Uuid::parse_str(id)?;
        Ok(self.data.join("chats").join(format!("{id}.json")))
    }
    pub async fn load(&self, id: &str) -> anyhow::Result<Conversation> {
        if self.tasks.is_deleted(id.into()).await? {
            return Err(std::io::Error::new(std::io::ErrorKind::NotFound, "Чат удалён").into());
        }
        Ok(serde_json::from_slice(
            &tokio::fs::read(self.chat_path(id)?).await?,
        )?)
    }
    pub async fn save(&self, chat: &Conversation) -> anyhow::Result<()> {
        let _guard = self.storage.lock().await;
        anyhow::ensure!(!self.tasks.is_deleted(chat.id.clone()).await?, "Чат удалён");
        let path = self.chat_path(&chat.id)?;
        let bytes = serde_json::to_vec_pretty(chat)?;
        tokio::task::spawn_blocking(move || crate::changes::replace(&path, &bytes)).await??;
        Ok(())
    }
}
pub fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}
pub fn router(app: Shared) -> Router {
    Router::new()
        .route("/api/bootstrap", get(bootstrap))
        .route("/api/status", get(status))
        .route("/api/settings", post(settings))
        .route("/api/assistant/config", get(assistant_config))
        .route("/api/thinking", get(thinking_options))
        .route("/api/web/install", post(install_browser))
        .route("/api/compute", get(compute_info))
        .route("/api/compute/refresh", post(compute_refresh))
        .route("/api/compute/retry", post(compute_retry))
        .route("/api/compute/install", post(compute_install))
        .route("/api/chats", get(chats).post(new_chat))
        .route(
            "/api/chats/{id}",
            get(chat).patch(rename).delete(delete_chat),
        )
        .route("/api/upload", post(upload))
        .route("/api/documents/read", post(document_read))
        .route("/api/documents/translate", post(document_translate))
        .route("/api/quick/area", post(quick_area))
        .route("/api/clipboard", post(clipboard))
        .route("/api/quick", get(quick_state).post(quick_translate))
        .route("/api/quick/stop", post(quick_stop))
        .route("/api/files/{id}", get(file))
        .route("/api/tree", get(tree))
        .route("/api/project/inspect", post(inspect_project))
        .route("/api/project/git", post(project_git))
        .route("/api/executions/{id}", get(execution))
        .route("/api/executions/{id}/logs/{stream}", get(execution_log))
        .route("/api/executions/{id}/stop", post(stop_execution))
        .route("/api/executions/{id}/input", post(process_input))
        .route("/api/changes", get(changes))
        .route("/api/changes/{id}/restore", post(restore_change))
        .route("/api/chat", post(send))
        .route("/api/runs", get(run_list).post(create_run))
        .route("/api/session", get(session_snapshot))
        .route("/api/events", get(workspace_events))
        .route("/api/runs/{id}", get(run_snapshot))
        .route("/api/runs/{id}/events", get(run_events))
        .route("/api/chats/{id}/tasks", get(conversation_tasks))
        .route("/api/tasks/{id}", get(task_state))
        .route("/api/tasks/{id}/requests/{sequence}", get(task_request))
        .route("/api/approve/{id}", post(approve))
        .route("/api/permissions", get(permission_grants))
        .route("/api/permissions/{id}/revoke", post(revoke_permission))
        .route("/api/stop/{id}", post(stop))
        .route("/api/media", post(media))
        .route("/api/models/pull", post(pull))
        .route("/api/models/delete", post(delete_model))
        .route("/api/shutdown", post(shutdown))
        .fallback(get(asset))
        .layer(DefaultBodyLimit::max(100 * 1024 * 1024))
        .layer(middleware::from_fn_with_state(app.clone(), guard))
        .with_state(app)
}
async fn quick_state(State(app): State<Shared>) -> Json<Value> {
    Json(app.quick.lock().await.clone())
}
async fn quick_stop(State(app): State<Shared>) -> Json<Value> {
    if let Some(cancel) = app.quick_cancel.lock().await.take() {
        cancel.cancel();
    }
    Json(json!({"ok":true}))
}
async fn quick_translate(State(app): State<Shared>, Json(value): Json<Value>) -> ApiResult<Value> {
    if let Some(error) = value["error"].as_str() {
        if let Some(previous) = app.quick_cancel.lock().await.take() {
            previous.cancel();
        }
        *app.quick.lock().await = json!({"id":Uuid::new_v4().to_string(),"status":"error","text":"","translation":"","error":error});
        return Ok(Json(json!({"ok":true})));
    }
    let text = value["text"].as_str().unwrap_or("").trim().to_string();
    require(
        !text.is_empty(),
        "Выделите текст или скопируйте его в буфер",
    )?;
    require(
        text.chars().count() <= 50000,
        "Выделение больше 50 000 символов",
    )?;
    let settings = app.settings.lock().await.clone();
    let selected = if settings.quick_model.is_empty() {
        settings.translation_model.clone()
    } else {
        settings.quick_model.clone()
    };
    let source = value["source"]
        .as_str()
        .unwrap_or(&settings.quick_source)
        .to_string();
    let source = crate::system_translate::source_language(&text, &source);
    let target = value["target"]
        .as_str()
        .unwrap_or(&settings.target)
        .to_string();
    let cancel = app.shutdown.child_token();
    if let Some(previous) = app.quick_cancel.lock().await.replace(cancel.clone()) {
        previous.cancel();
    }
    let id = Uuid::new_v4().to_string();
    *app.quick.lock().await = json!({"id":id,"status":"translating","text":text,"translation":"","source":source,"target":target,"model":selected});
    let job = id.clone();
    let model_guard = app.model_lifecycle.clone().read_owned().await;
    tokio::spawn(async move {
        let _model_guard = model_guard;
        let (tx, mut rx) = mpsc::channel::<Value>(128);
        let updates = app.clone();
        let update_id = job.clone();
        let relay = tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                let mut current = updates.quick.lock().await;
                if current["id"] != update_id {
                    continue;
                }
                if event["type"] == "token" {
                    let mut text = current["translation"].as_str().unwrap_or("").to_string();
                    text.push_str(event["text"].as_str().unwrap_or(""));
                    current["translation"] = json!(text);
                }
                if event["type"] == "notice" {
                    current["notice"] = event["message"].clone();
                }
            }
        });
        let result = async {
            if source == target {
                Ok(text.clone())
            } else {
                engine::ensure_ollama(&app).await?;
                engine::translate_with_model(&app, &text, &source, &target, &selected, &tx, &cancel)
                    .await
            }
        }
        .await;
        drop(tx);
        let _ = relay.await;
        let mut current = app.quick.lock().await;
        if current["id"] == job {
            match result {
                Ok(text) => {
                    current["status"] = json!("done");
                    current["translation"] = json!(text);
                }
                Err(error) => {
                    current["status"] = json!("error");
                    current["error"] = json!(format!("{error:#}"));
                }
            }
        }
    });
    Ok(Json(json!({"id":id})))
}
async fn shutdown(State(app): State<Shared>) -> Json<Value> {
    app.model_stores.lock().await.clear();
    for run in app.runs.lock().await.values() {
        run.cancel.cancel();
    }
    app.shutdown.cancel();
    for session in app.web_sessions.lock().await.values() {
        if let Ok(mut s) = session.try_lock() {
            s.close().await;
        }
    }
    Json(json!({"ok":true}))
}
async fn guard(State(app): State<Shared>, req: Request<Body>, next: Next) -> Response {
    let host = req
        .headers()
        .get("host")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    let valid_host = host
        .split(':')
        .next()
        .is_some_and(|s| s == "127.0.0.1" || s == "localhost");
    if !valid_host {
        return StatusCode::FORBIDDEN.into_response();
    }
    if let Some(origin) = req.headers().get("origin").and_then(|h| h.to_str().ok()) {
        if origin != format!("http://{host}") {
            return StatusCode::FORBIDDEN.into_response();
        }
    }
    if req.method() != axum::http::Method::GET && req.method() != axum::http::Method::HEAD {
        if req
            .headers()
            .get("x-studio-token")
            .and_then(|h| h.to_str().ok())
            != Some(app.token.as_str())
        {
            return StatusCode::FORBIDDEN.into_response();
        }
    }
    let mut response = next.run(req).await;
    response
        .headers_mut()
        .insert("x-content-type-options", "nosniff".parse().unwrap());
    response
        .headers_mut()
        .insert("x-frame-options", "DENY".parse().unwrap());
    response.headers_mut().insert("content-security-policy", "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' blob: data:; media-src 'self' blob:; connect-src 'self'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'".parse().unwrap());
    response
}
async fn bootstrap(State(app): State<Shared>) -> Json<Value> {
    Json(
        json!({"app":"Local Gemma Studio","root":app.root.to_string_lossy(),"version":env!("CARGO_PKG_VERSION"),"token":app.token,"settings":app.settings.lock().await.clone(),"platform":std::env::consts::OS,"local_inference":engine::local_inference(),"catalog":crate::models::catalog()}),
    )
}
async fn status(State(app): State<Shared>) -> Json<Value> {
    let hotkey = std::fs::read(app.root.join("data/hotkey-status.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());
    let models = crate::model_storage::tags(&app).await;
    let compute = crate::compute::summary(&app).await;
    Json(
        json!({"process_recovery_errors":app.process_recovery_errors,"compute":compute,"online":models["online"],"model_locations":models["locations"],"storage_errors":models["errors"],"models":models["models"],"python":tools::python(&app).map(|p|p.to_string_lossy().to_string()),"models_path":app.data.join("models").to_string_lossy(),"speech_ready":tools::speech_model(&app).join("model.bin").exists(),"workspace":app.settings.lock().await.workspace,"hotkey":hotkey,"active_tasks":app.runs.lock().await.len(),"models_busy":app.model_lifecycle.try_write().is_err(),"quick_busy":matches!(app.quick.lock().await["status"].as_str(),Some("translating"|"capturing"))}),
    )
}
async fn settings(
    State(app): State<Shared>,
    Json(mut value): Json<Settings>,
) -> ApiResult<Settings> {
    let _write = app.settings_write.lock().await;
    let revision = app.settings.lock().await.revision;
    if value.revision != revision {
        return Err(ApiError::status(StatusCode::CONFLICT,"Настройки изменились в другом окне. Введённые значения сохранены в окне; сбросьте их для загрузки новой версии."));
    }
    value.revision = revision
        .checked_add(1)
        .ok_or_else(|| anyhow::anyhow!("Переполнение версии настроек"))?;
    value.schema_version = 1;
    require(
        ["manual", "project", "full"].contains(&value.permission_mode.as_str()),
        "Неизвестный режим прав",
    )?;
    value.assistant.validate()?;
    crate::lifecycle::deadline(value.agent_timeout_seconds)?;
    value.context.validate()?;
    value.web.validate()?;
    crate::models::validate_settings(&value)?;
    #[cfg(feature = "desktop")]
    {
        let text_key: global_hotkey::hotkey::HotKey = value
            .quick_hotkey
            .parse()
            .map_err(|e| anyhow::anyhow!("Неверное сочетание клавиш: {e}"))?;
        let area_key: global_hotkey::hotkey::HotKey = value
            .quick_area_hotkey
            .parse()
            .map_err(|e| anyhow::anyhow!("Неверное сочетание области экрана: {e}"))?;
        require(
            text_key.id() != area_key.id(),
            "Для текста и области экрана нужны разные сочетания клавиш",
        )?;
    }
    let previous = app.settings.lock().await.clone();
    if value.compute == previous.compute && value.cpu_only != previous.cpu_only {
        value.compute.backend = if value.cpu_only { "cpu" } else { "auto" }.into();
        value.compute.device.clear();
    }
    value.compute.normalize(value.cpu_only);
    value.cpu_only = value.compute.backend == "cpu";
    let compute_changed = value.compute != previous.compute;
    let _compute_guard = if compute_changed {
        Some(app.model_lifecycle.try_write().map_err(|_|anyhow::anyhow!("Дождитесь завершения ответа, перевода или установки компонентов перед сменой устройства"))?)
    } else {
        None
    };
    require(
        app.runs.lock().await.is_empty()
            || (value.workspace == previous.workspace
                && value.cpu_only == previous.cpu_only
                && !compute_changed
                && value.computer_access == previous.computer_access
                && value.translation_model == previous.translation_model
                && value.agent_model == previous.agent_model
                && value.team_model == previous.team_model
                && value.chat_model == previous.chat_model
                && value.web == previous.web
                && value.assistant == previous.assistant
                && value.context == previous.context),
        "Остановите текущую задачу перед изменением настроек",
    )?;
    if compute_changed {
        crate::compute::reconfigure(&app, &value.compute).await?;
    }
    let root = PathBuf::from(&value.workspace);
    require(
        root.is_absolute(),
        "Укажите абсолютный путь к рабочей папке",
    )?;
    tokio::fs::create_dir_all(&root).await?;
    let root = dunce::canonicalize(root)?;
    let mut value = value;
    value.workspace = root.to_string_lossy().into();
    let mut recent = vec![value.workspace.clone(), previous.workspace.clone()];
    recent.extend(previous.recent_projects);
    value.recent_projects = vec![];
    for path in recent {
        if !value.recent_projects.contains(&path) {
            value.recent_projects.push(path);
        }
        if value.recent_projects.len() >= 12 {
            break;
        }
    }
    crate::changes::replace(
        &app.data.join("settings.json"),
        &serde_json::to_vec_pretty(&value)?,
    )?;
    *app.settings.lock().await = value.clone();
    crate::permissions::resolve_pending(&app).await;
    Ok(Json(value))
}
async fn compute_info(State(app): State<Shared>) -> Json<Value> {
    Json(crate::compute::info(&app, false).await)
}
async fn compute_refresh(State(app): State<Shared>) -> Json<Value> {
    Json(crate::compute::info(&app, true).await)
}
async fn compute_retry(State(app): State<Shared>) -> ApiResult<Value> {
    let _guard = app
        .model_lifecycle
        .try_write()
        .map_err(|_| anyhow::anyhow!("Дождитесь окончания текущей задачи"))?;
    require(
        app.runs.lock().await.is_empty(),
        "Остановите текущую задачу",
    )?;
    let config = app.settings.lock().await.compute.clone();
    if std::env::var_os("LOCAL_GEMMA_OLLAMA").is_some() {
        app.resources.recover(&app.client).await?;
    }
    if std::env::var_os("LOCAL_GEMMA_OLLAMA").is_none() {
        crate::compute::restart(&app, &config).await?;
        *app.resources.failure.lock().unwrap() = None;
    } else {
        crate::compute::reconfigure(&app, &config).await?;
    }
    Ok(Json(crate::compute::info(&app, false).await))
}
async fn compute_install(
    State(app): State<Shared>,
    Json(value): Json<Value>,
) -> Result<impl IntoResponse, ApiError> {
    let backend = value["backend"].as_str().unwrap_or("").to_string();
    require(
        ["cuda", "vulkan", "rocm"].contains(&backend.as_str()),
        "Неизвестный модуль",
    )?;
    require(
        cfg!(windows),
        "На этой платформе компоненты GPU устанавливаются вместе с системным движком Ollama",
    )?;
    require(
        app.runs.lock().await.is_empty(),
        "Дождитесь завершения текущей задачи",
    )?;
    let guard = app
        .model_lifecycle
        .clone()
        .try_write_owned()
        .map_err(|_| anyhow::anyhow!("Дождитесь окончания ответа, перевода или скачивания"))?;
    let (tx, mut rx) = mpsc::channel::<Value>(32);
    let worker = app.clone();
    tokio::spawn(async move {
        let _guard = guard;
        if let Err(e) = crate::compute::install_package(&worker, &backend, &tx).await {
            let _ = tx
                .send(json!({"type":"error","message":format!("{e:#}")}))
                .await;
        }
        let _ = tx.send(json!({"type":"done"})).await;
    });
    let stream = async_stream::stream! {while let Some(value)=rx.recv().await{yield Ok::<Event,Infallible>(Event::default().json_data(value).unwrap());}};
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}
async fn chats(State(app): State<Shared>) -> ApiResult<Value> {
    let mut items = vec![];
    let mut entries = tokio::fs::read_dir(app.data.join("chats")).await?;
    while let Some(entry) = entries.next_entry().await? {
        if entry.path().extension().is_some_and(|e| e == "json") {
            if let Ok(bytes) = tokio::fs::read(entry.path()).await {
                if let Ok(chat) = serde_json::from_slice::<Conversation>(&bytes) {
                    if app.tasks.is_deleted(chat.id.clone()).await? {continue;}
                    items.push(json!({"id":chat.id,"title":chat.title,"updated":chat.updated,"mode":chat.mode,"count":chat.messages.len()}));
                }
            }
        }
    }
    items.sort_by(|a, b| b["updated"].as_str().cmp(&a["updated"].as_str()));
    Ok(Json(json!(items)))
}
async fn new_chat(State(app): State<Shared>, Json(value): Json<Value>) -> ApiResult<Conversation> {
    let chat = Conversation {
        history_task:String::new(),
        history_workspace: String::new(),
        history_model: String::new(),
        id: Uuid::new_v4().to_string(),
        title: "Новый чат".into(),
        updated: now(),
        mode: value["mode"].as_str().unwrap_or("agent").into(),
        messages: vec![],
        history: vec![],
    };
    app.save(&chat).await?;
    Ok(Json(chat))
}
async fn chat(State(app): State<Shared>, Path(id): Path<String>) -> ApiResult<Conversation> {
    Ok(Json(app.load(&id).await?))
}
async fn rename(
    State(app): State<Shared>,
    Path(id): Path<String>,
    Json(value): Json<Value>,
) -> ApiResult<Conversation> {
    let runs = app.runs.lock().await;
    require(
        !runs.values().any(|r| r.conversation == id),
        "Дождитесь завершения ответа или остановите его",
    )?;
    let mut chat = app.load(&id).await?;
    chat.title = value["title"]
        .as_str()
        .unwrap_or("Чат")
        .chars()
        .take(100)
        .collect();
    app.save(&chat).await?;
    Ok(Json(chat))
}
async fn delete_chat(State(app): State<Shared>, Path(id): Path<String>) -> ApiResult<Value> {
    let runs = app.runs.lock().await;
    require(
        !runs.values().any(|r| r.conversation == id),
        "Дождитесь завершения ответа или остановите его",
    )?;
    require(!crate::processes::has_live_conversation(&app,&id).await,"Сначала остановите управляемые процессы в разделе задачи")?;
    if let Some(session) = app.web_sessions.lock().await.remove(&id) {
        session.lock().await.close().await;
    }
    let path=app.chat_path(&id)?;
    let _storage=app.storage.lock().await;
    // Verify existence before accepting deletion, then make its intent durable first.
    tokio::fs::metadata(&path).await?;
    app.tasks.delete_conversation(id).await?;
    tokio::fs::remove_file(path).await?;
    Ok(Json(json!({"ok":true})))
}
async fn upload(State(app): State<Shared>, mut form: Multipart) -> ApiResult<Attachment> {
    let field = form
        .next_field()
        .await?
        .ok_or_else(|| anyhow::anyhow!("Нет файла"))?;
    let original = field.file_name().unwrap_or("attachment.bin").to_string();
    let bytes = field.bytes().await?;
    Ok(Json(store_upload(&app, &original, &bytes).await?))
}
pub(crate) async fn store_upload(
    app: &App,
    original: &str,
    bytes: &[u8],
) -> Result<Attachment, ApiError> {
    let name = original
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("attachment.bin")
        .chars()
        .filter(|c| !c.is_control())
        .take(160)
        .collect::<String>();
    let kind = tools::file_kind(&name);
    require(bytes.len() <= 80 * 1024 * 1024, "Файл больше 80 МБ")?;
    let id = Uuid::new_v4().to_string();
    let extension = std::path::Path::new(&name)
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("bin");
    require(
        extension.chars().all(|c| c.is_ascii_alphanumeric()),
        "Некорректное расширение файла",
    )?;
    let path = app.data.join("uploads").join(format!("{id}.{extension}"));
    tokio::fs::write(&path, &bytes).await?;
    let attachment = Attachment {
        id: id.clone(),
        name,
        kind,
        size: bytes.len() as u64,
        path: path.to_string_lossy().into(),
    };
    tokio::fs::write(
        app.data.join("uploads").join(format!("{id}.meta.json")),
        serde_json::to_vec(&attachment)?,
    )
    .await?;
    Ok(attachment)
}
async fn clipboard(State(app): State<Shared>) -> ApiResult<Value> {
    #[cfg(feature = "desktop")]
    {
        let (text, files, image) = tokio::task::spawn_blocking(
            || -> anyhow::Result<(String, Vec<String>, Option<Vec<u8>>)> {
                use clipboard_rs::{common::RustImage, Clipboard, ClipboardContext};
                let clipboard =
                    ClipboardContext::new().map_err(|e| anyhow::anyhow!(e.to_string()))?;
                let files = clipboard.get_files().unwrap_or_default();
                if !files.is_empty() {
                    return Ok((String::new(), files, None));
                }
                if let Ok(image) = clipboard.get_image() {
                    if let Ok(bytes) = image.to_png() {
                        return Ok((String::new(), vec![], Some(bytes.get_bytes().to_vec())));
                    }
                }
                Ok((clipboard.get_text().unwrap_or_default(), vec![], None))
            },
        )
        .await??;
        let mut attachments = vec![];
        for file in files.into_iter().take(12) {
            let path = reqwest::Url::parse(&file)
                .ok()
                .and_then(|url| url.to_file_path().ok())
                .unwrap_or_else(|| PathBuf::from(file));
            if !path.is_file() {
                continue;
            }
            require(
                tokio::fs::metadata(&path).await?.len() <= 80 * 1024 * 1024,
                "Файл из буфера больше 80 МБ",
            )?;
            let name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            attachments.push(store_upload(&app, &name, &tokio::fs::read(&path).await?).await?);
        }
        if let Some(bytes) = image {
            attachments.push(store_upload(&app, "clipboard.png", &bytes).await?);
        }
        Ok(Json(json!({"text":text,"attachments":attachments})))
    }
    #[cfg(not(feature = "desktop"))]
    {
        let _ = app;
        Err(ApiError(anyhow::anyhow!(
            "Используйте Ctrl/⌘+V или перетащите файл"
        )))
    }
}
pub async fn attachment(app: &App, id: &str) -> anyhow::Result<Attachment> {
    Uuid::parse_str(id)?;
    Ok(serde_json::from_slice(
        &tokio::fs::read(app.data.join("uploads").join(format!("{id}.meta.json"))).await?,
    )?)
}
async fn file(State(app): State<Shared>, Path(id): Path<String>) -> Result<Response, ApiError> {
    let attachment = attachment(&app, &id).await?;
    let bytes = tokio::fs::read(&attachment.path).await?;
    let mime = match attachment.kind.as_str() {
        "image" => mime_guess::from_path(&attachment.name)
            .first_or_octet_stream()
            .to_string(),
        "audio" => mime_guess::from_path(&attachment.name)
            .first_or_octet_stream()
            .to_string(),
        _ => "application/octet-stream".into(),
    };
    let mut headers = HeaderMap::new();
    headers.insert("content-type", mime.parse()?);
    Ok((headers, bytes).into_response())
}
async fn tree(State(app): State<Shared>) -> ApiResult<Value> {
    let root = PathBuf::from(&app.settings.lock().await.workspace);
    Ok(Json(json!(tools::file_tree(&root)?)))
}
async fn inspect_project(State(app): State<Shared>, Json(value): Json<Value>) -> ApiResult<Value> {
    let root = crate::projects::directory(&app, value["path"].as_str()).await?;
    let cancel = app.shutdown.child_token();
    Ok(Json(
        tokio::task::spawn_blocking(move || crate::projects::inspect(&root, &cancel)).await??,
    ))
}
async fn project_git(State(app): State<Shared>, Json(value): Json<Value>) -> ApiResult<Value> {
    let root = crate::projects::directory(&app, value["path"].as_str()).await?;
    Ok(Json(
        crate::projects::git(
            &root,
            value["action"].as_str().unwrap_or("status"),
            value["staged"].as_bool().unwrap_or(false),
            &app.shutdown.child_token(),
        )
        .await?,
    ))
}
async fn execution(State(app): State<Shared>, Path(id): Path<String>,axum::extract::Query(q):axum::extract::Query<HashMap<String,String>>) -> ApiResult<Value> {
    if let Some(stream)=q.get("stream") {let offset=q.get("offset").map(|v|v.parse::<u64>()).transpose()?.unwrap_or(0);return Ok(Json(crate::processes::log(&app,&id,stream,offset).await?));}
    Ok(Json(crate::processes::view(&app,&id).await?))
}
async fn stop_execution(State(app):State<Shared>,Path(id):Path<String>)->ApiResult<Value>{crate::processes::stop(&app,&id).await?;Ok(Json(json!({"stop_requested":true})))}
async fn process_input(State(app):State<Shared>,Path(id):Path<String>,Json(value):Json<Value>)->ApiResult<Value>{
    let text=value["text"].as_str().ok_or_else(||anyhow::anyhow!("Не задан текст для stdin"))?;crate::processes::input(&app,&id,text,&app.shutdown.child_token()).await?;Ok(Json(json!({"bytes_written":text.len()})))
}
async fn execution_log(State(app):State<Shared>,Path((id,stream)):Path<(String,String)>)->Result<Response,ApiError>{
    use tokio::io::AsyncReadExt;
    let path=crate::processes::log_path(&app,&id,&stream)?;let file=tokio::fs::File::open(path).await?;let length=file.metadata().await?.len();let mut file=file.take(length);
    let bytes=async_stream::stream!{let mut buffer=vec![0u8;65536];loop{match file.read(&mut buffer).await {Ok(0)=>break,Ok(count)=>yield Ok::<_,std::io::Error>(buffer[..count].to_vec()),Err(error)=>{yield Err(error);break;}}}};
    let mut headers=HeaderMap::new();headers.insert("content-type","application/octet-stream".parse()?);headers.insert("content-disposition",format!("attachment; filename=\"execution-{id}-{stream}.log\"").parse()?);
    headers.insert("content-length",length.to_string().parse()?);
    Ok((headers,Body::from_stream(bytes)).into_response())
}
#[derive(Clone, Serialize, Deserialize)]
pub struct ChatInput {
    #[serde(default,skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(default,skip_serializing_if = "Option::is_none")]
    pub new_task: Option<bool>,
    #[serde(default)]
    pub request_id: Option<String>,
    #[serde(default = "thinking_off")]
    pub thinking: String,
    #[serde(default = "web_auto")]
    pub web: String,
    pub conversation: String,
    pub content: String,
    #[serde(default)]
    pub attachments: Vec<String>,
    pub mode: String,
    pub source: String,
    pub target: String,
    #[serde(default)]
    pub retry: bool,
}
fn thinking_off() -> String {
    "off".into()
}
fn web_auto() -> String {
    "auto".into()
}
async fn thinking_options(
    State(app): State<Shared>,
    axum::extract::Query(q): axum::extract::Query<HashMap<String, String>>,
) -> ApiResult<Value> {
    let model = q
        .get("model")
        .ok_or_else(|| anyhow::anyhow!("Укажите модель"))?;
    require(
        crate::models::valid(model, "assistant"),
        "Неизвестная модель ассистента",
    )?;
    let show = crate::thinking::show(&app, model).await?;
    Ok(Json(
        json!({"model":model,"values":crate::thinking::choices(&show),"default":show["thinking"]["default"]}),
    ))
}
async fn install_browser(State(app): State<Shared>) -> ApiResult<Value> {
    require(
        app.runs.lock().await.is_empty(),
        "Дождитесь завершения задач",
    )?;
    Ok(Json(
        crate::web_tools::install(&app, &app.shutdown.child_token()).await?,
    ))
}
async fn send(
    State(app): State<Shared>,
    Json(input): Json<ChatInput>,
) -> Result<impl IntoResponse, ApiError> {
    let id = start_run(app.clone(), input).await?;
    Ok(run_stream(app, id, 0))
}
async fn create_run(State(app): State<Shared>, Json(input): Json<ChatInput>) -> ApiResult<Value> {
    let conversation = input.conversation.clone();
    let id = start_run(app.clone(), input).await?;
    let run = app.tasks.get(id.clone()).await?;
    Ok(Json(
        json!({"accepted":true,"run_id":id,"conversation":conversation,"status":run["status"],"last_sequence":run["last_sequence"]}),
    ))
}
async fn start_run(app: Shared, input: ChatInput) -> Result<String, ApiError> {
    let _settings_guard = app.settings_write.lock().await;
    let _chat = app.load(&input.conversation).await?;
    require(
        ["agent", "team", "translate", "chat"].contains(&input.mode.as_str()),
        "Неизвестный режим работы",
    )?;
    require(["agent","team"].contains(&input.mode.as_str()) || (input.new_task!=Some(true)&&input.task_id.is_none()),"Выбор рабочей задачи доступен в режиме агента или команды")?;
    require(
        !input.content.trim().is_empty() || !input.attachments.is_empty(),
        "Введите сообщение или прикрепите материал",
    )?;
    require(
        ["auto", "off", "search", "research"].contains(&input.web.as_str()),
        "Неизвестный режим сети",
    )?;
    require(input.thinking.len() <= 32, "Некорректный режим рассуждений")?;
    require(
        input.content.len() <= crate::context::MAX_REQUEST_TEXT_BYTES,
        "Текст сообщения превышает 512 КиБ; приложите его файлом или разделите",
    )?;
    let model_guard = app.model_lifecycle.clone().try_read_owned().map_err(|_| {
        anyhow::anyhow!("Движок обновляется или меняет устройство. Дождитесь завершения операции.")
    })?;
    let cancel = app.shutdown.child_token();
    let run_id = input
        .request_id
        .clone()
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    Uuid::parse_str(&run_id)?;
    if app.tasks.get(run_id.clone()).await.is_err() {
        for id in &input.attachments {
            let file=attachment(&app,id).await?;
            require(tokio::fs::metadata(&file.path).await?.is_file(),"Вложение больше не доступно")?;
        }
    }
    {
        let mut runs = app.runs.lock().await;
        app.load(&input.conversation).await?;
        if app.tasks.get(run_id.clone()).await.is_ok() {
            // Reconnecting an accepted request must not execute it twice.
            app.tasks
                .begin(
                    run_id.clone(),
                    input.conversation.clone(),
                    serde_json::to_value(&input)?,
                )
                .await?;
            return Ok(run_id);
        }
        require(
            !runs.values().any(|r| r.conversation == input.conversation),
            "В этом чате уже идёт ответ",
        )?;
        app.tasks
            .begin_in_project(
                run_id.clone(),
                input.conversation.clone(),
                serde_json::to_value(&input)?,
                app.settings.lock().await.workspace.clone(),
            )
            .await?;
        runs.insert(
            run_id.clone(),
            Run {
                cancel: cancel.clone(),
                conversation: input.conversation.clone(),
            },
        );
    }
    let (tx, mut rx) = mpsc::channel::<Value>(128);
    let journal = app.clone();
    let journal_id = run_id.clone();
    // The receiver lives independently of any HTTP connection.
    tokio::spawn(async move {
        let mut writable=true;
        let mut terminal=false;
        while let Some(event) = rx.recv().await {
            if !writable {continue;}
            terminal |= event["type"]=="done";
            if let Err(error) = journal.tasks.append(journal_id.clone(), event).await {
                eprintln!("Cannot persist task {journal_id}: {error:#}");
                if let Some(run) = journal.runs.lock().await.get(&journal_id) {
                    run.cancel.cancel();
                }
                // Drain until the producer has stopped; deletion and resource changes
                // must not observe a finished run while its worker can still act.
                writable=false;
            }
        }
        if writable && !terminal {
            let _=journal.tasks.append(journal_id.clone(), json!({"type":"done","status":"interrupted"})).await;
        }
        journal.runs.lock().await.remove(&journal_id);
        journal.grants.lock().await.finish(&journal_id);
    });
    let worker = app.clone();
    let id = run_id.clone();
    tokio::spawn(async move {
        let _model_guard = model_guard;
        let _ = tx
            .send(json!({"type":"start","run_id":id,"mode":input.mode}))
            .await;
        let started = std::time::Instant::now();
        let conversation = input.conversation.clone();
        let mut final_status="completed";
        if let Err(error) = crate::permissions::TASK
            .scope(
                id.clone(),
                engine::run(worker.clone(), input, &tx, cancel.clone()),
            )
            .await
        {
            final_status=if cancel.is_cancelled(){"cancelled"}else{"failed"};
            // A failed attempt must not leave its managed processes mutating files.
            cancel.cancel();
            crate::processes::cancel_run(&worker,&id).await;
            let diagnostic = format!("{error:#}");
            if let Ok(mut chat) = worker.load(&conversation).await {
                if chat.messages.last().is_some_and(|m| m.role == "user") {
                    chat.messages.push(Message {
                        material: None,
                        diagnostics: None,
                        duration_ms: Some(
                            started.elapsed().as_millis().min(u64::MAX as u128) as u64
                        ),
                        model: None,
                        id: Uuid::new_v4().to_string(),
                        role: "assistant".into(),
                        content: format!("⚠ {diagnostic}"),
                        attachments: vec![],
                        tools: vec![],
                        created: now(),
                    });
                    chat.updated = now();
                    let _ = worker.save(&chat).await;
                    let _ = tx.send(json!({"type":"saved","conversation_id":chat.id,"updated":chat.updated,"message_count":chat.messages.len(),"ref_only":true})).await;
                }
            }
            eprintln!("Chat {conversation}: {diagnostic}");
            let _ = tx
                .send(
                    json!({"type":"error","message":diagnostic,"cancelled":final_status=="cancelled"}),
                )
                .await;
        }
        if cancel.is_cancelled() {crate::processes::cancel_run(&worker,&id).await;if final_status=="completed" {final_status="cancelled";}}
        let _ = tx.send(json!({"type":"done","status":final_status})).await;
    });
    Ok(run_id)
}
fn run_stream(
    app: Shared,
    id: String,
    mut cursor: i64,
) -> Sse<impl futures_util::Stream<Item = Result<Event, Infallible>>> {
    let stream = async_stream::stream! {
        loop {
            match app.tasks.events(id.clone(),cursor).await {
                Ok(events) if !events.is_empty() => {
                    for event in events {
                        cursor=event["sequence"].as_i64().unwrap_or(cursor);
                        let done=event["type"]=="done";
                        yield Ok(Event::default().id(cursor.to_string()).json_data(event).unwrap());
                        if done {return;}
                    }
                },
                Ok(_) => {
                    let terminal=app.tasks.get(id.clone()).await.ok().is_some_and(|r|["interrupted","completed","cancelled","failed"].iter().any(|s|r["status"]==*s)) && !app.runs.lock().await.contains_key(&id);
                    if terminal {
                        yield Ok(Event::default().json_data(json!({"type":"done","run_id":id})).unwrap());return;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                },
                Err(error) => {
                    yield Ok(Event::default().json_data(json!({"type":"error","message":format!("{error:#}")})).unwrap());return;
                }
            }
        }
    };
    Sse::new(stream).keep_alive(KeepAlive::default())
}
async fn run_list(State(app): State<Shared>) -> ApiResult<Value> {
    Ok(Json(app.tasks.list().await?))
}
async fn session_snapshot(State(app): State<Shared>) -> ApiResult<Value> {
    Ok(Json(app.tasks.dashboard().await?))
}
async fn workspace_events(
    State(app): State<Shared>,
    axum::extract::Query(q): axum::extract::Query<HashMap<String, String>>,
) -> Result<impl IntoResponse, ApiError> {
    let mut cursor = q
        .get("after")
        .map(|s| s.parse::<i64>())
        .transpose()?
        .unwrap_or(0);
    let stream = async_stream::stream! {
        loop {
            let events=match app.tasks.workspace_events(cursor).await {Ok(events)=>events,Err(error)=>{yield Ok::<Event,Infallible>(Event::default().json_data(json!({"type":"error","message":format!("{error:#}")})).unwrap());return;}};
            if events.is_empty(){tokio::select!{_=app.shutdown.cancelled()=>return,_=tokio::time::sleep(std::time::Duration::from_millis(200))=>{}};continue;}
            for event in events {cursor=event["sequence"].as_i64().unwrap_or(cursor);yield Ok(Event::default().id(cursor.to_string()).json_data(event).unwrap());}
        }
    };
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}
async fn run_snapshot(State(app): State<Shared>, Path(id): Path<String>) -> ApiResult<Value> {
    Ok(Json(app.tasks.get(id).await?))
}
async fn run_events(
    State(app): State<Shared>,
    Path(id): Path<String>,
    axum::extract::Query(q): axum::extract::Query<HashMap<String, String>>,
) -> Result<impl IntoResponse, ApiError> {
    app.tasks.get(id.clone()).await?;
    let cursor = q
        .get("after")
        .map(|s| s.parse::<i64>())
        .transpose()?
        .unwrap_or(0);
    Ok(run_stream(app, id, cursor))
}
async fn changes(State(app): State<Shared>) -> ApiResult<Value> {
    let _guard = app.file_changes.lock().await;
    Ok(Json(crate::changes::list(&app.data)?))
}
async fn restore_change(State(app): State<Shared>, Path(id): Path<String>) -> ApiResult<Value> {
    require(
        app.runs.lock().await.is_empty(),
        "Остановите задачу агента перед восстановлением файла",
    )?;
    let settings = app.settings.lock().await.clone();
    let _guard = app.file_changes.lock().await;
    Ok(Json(crate::changes::restore(
        &app.data,
        &id,
        &PathBuf::from(settings.workspace),
        settings.computer_access,
    )?))
}
async fn approve(
    State(app): State<Shared>,
    Path(id): Path<String>,
    Json(value): Json<Value>,
) -> ApiResult<Value> {
    let mut pending = app.approvals.lock().await;
    let item = pending
        .get(&id)
        .ok_or_else(|| anyhow::anyhow!("Подтверждение уже закрыто"))?;
    require(!item.sender.is_closed(), "Задача уже остановлена")?;
    let allowed = value["allow"].as_bool().unwrap_or(false);
    let duration = value["duration"].as_str().unwrap_or("once");
    require(
        ["once", "task", "project"].contains(&duration),
        "Неизвестный срок разрешения",
    )?;
    if allowed && duration != "once" {
        app.grants
            .lock()
            .await
            .add(&app.data, item.scope.clone(), duration)?;
    }
    let sender = pending.remove(&id).unwrap().sender;
    let _ = sender.send(allowed);
    drop(pending);
    if allowed && duration != "once" {
        crate::permissions::resolve_pending(&app).await;
    }
    Ok(Json(json!({"ok":true})))
}
async fn permission_grants(State(app): State<Shared>) -> ApiResult<Value> {
    Ok(Json(serde_json::to_value(
        &app.grants.lock().await.entries,
    )?))
}
async fn conversation_tasks(State(app):State<Shared>,Path(id):Path<String>)->ApiResult<Value>{
    app.load(&id).await?;
    Ok(Json(app.tasks.task_list(id).await?))
}
async fn task_state(State(app):State<Shared>,Path(id):Path<String>,axum::extract::Query(q):axum::extract::Query<HashMap<String,String>>)->ApiResult<Value>{
    let after=q.get("after").map(|v|v.parse::<i64>()).transpose()?.unwrap_or(0);
    require(after>=0,"Курсор должен быть неотрицательным")?;
    let mut task=app.tasks.task(id.clone(),after).await?;task["active_processes"]=json!(crate::processes::for_task(&app,&id).await?);Ok(Json(task))
}
async fn task_request(State(app):State<Shared>,Path((id,sequence)):Path<(String,i64)>,axum::extract::Query(q):axum::extract::Query<HashMap<String,String>>)->ApiResult<Value>{
    let offset=q.get("offset").map(|v|v.parse::<usize>()).transpose()?.unwrap_or(0);
    Ok(Json(app.tasks.task_request(id,sequence,offset).await?))
}
async fn revoke_permission(State(app): State<Shared>, Path(id): Path<String>) -> ApiResult<Value> {
    app.grants.lock().await.revoke(&app.data, &id)?;
    Ok(Json(json!({"ok":true})))
}
async fn stop(State(app): State<Shared>, Path(id): Path<String>) -> ApiResult<Value> {
    if let Some(run) = app.runs.lock().await.get(&id) {
        run.cancel.cancel();
    }
    Ok(Json(json!({"ok":true})))
}
async fn media(State(app): State<Shared>, Json(value): Json<Value>) -> ApiResult<Value> {
    let _model_guard = app.model_lifecycle.read().await;
    let cancel = app.shutdown.child_token();
    let action = value["action"].as_str().unwrap_or("ocr");
    let path = if action == "record" {
        None
    } else {
        Some(
            attachment(&app, value["attachment"].as_str().unwrap_or(""))
                .await?
                .path,
        )
    };
    let job = value["job"].as_str().map(str::to_string);
    if let Some(id) = &job {
        Uuid::parse_str(id)?;
        let mut runs = app.runs.lock().await;
        require(!runs.contains_key(id), "Эта запись уже запущена")?;
        runs.insert(
            id.clone(),
            Run {
                cancel: cancel.clone(),
                conversation: String::new(),
            },
        );
    }
    let result = tools::media(
        &app,
        action,
        path.as_deref(),
        value["language"].as_str(),
        value["seconds"].as_u64().unwrap_or(10),
        &cancel,
    )
    .await;
    if let Some(id) = &job {
        app.runs.lock().await.remove(id);
    }
    Ok(Json(result?))
}
async fn pull(
    State(app): State<Shared>,
    Json(value): Json<Value>,
) -> Result<impl IntoResponse, ApiError> {
    let models: Vec<String> = if let Some(model) = value.get("model") {
        vec![model
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Укажите модель"))?
            .to_string()]
    } else {
        vec![
            crate::models::default_translation(),
            crate::models::default_assistant(),
        ]
    };
    for model in &models {
        require(
            crate::models::catalog()
                .as_array()
                .unwrap()
                .iter()
                .any(|m| m["id"] == model.as_str()),
            "Модель отсутствует в каталоге",
        )?;
    }
    if let Some(directory) = value["directory"].as_str().filter(|s| !s.is_empty()) {
        let _guard = app
            .model_lifecycle
            .try_write()
            .map_err(|_| anyhow::anyhow!("Дождитесь завершения работы с моделями"))?;
        if models[0] != "whisper-small"
            && !crate::model_storage::locations(&app)?.contains_key(&models[0])
        {
            let tags = crate::model_storage::tags(&app).await;
            if crate::models::installed(&models[0], &tags["models"]) {
                let requested = dunce::canonicalize(directory).ok();
                let current = dunce::canonicalize(app.data.join("models")).ok();
                require(requested.is_some() && requested==current,"Модель уже установлена в основной папке. Удалите её перед скачиванием на другой диск")?;
            }
        }
        let installed = crate::models::installed(
            &models[0],
            &crate::model_storage::tags(&app).await["models"],
        );
        crate::model_storage::set_location(&app, &models[0], directory, installed)?;
    }
    let (tx, mut rx) = mpsc::channel::<Value>(64);
    let model_guard = app.model_lifecycle.clone().read_owned().await;
    tokio::spawn(async move {
        let _model_guard = model_guard;
        if let Err(error) = engine::pull_models(&app, &models, &tx).await {
            let _ = tx
                .send(json!({"type":"error","message":format!("{error:#}")}))
                .await;
        }
        let _ = tx.send(json!({"type":"done"})).await;
    });
    let stream = async_stream::stream! {while let Some(value)=rx.recv().await{yield Ok::<Event,Infallible>(Event::default().json_data(value).unwrap());}};
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}
async fn delete_model(State(app): State<Shared>, Json(value): Json<Value>) -> ApiResult<Value> {
    let _guard = app
        .model_lifecycle
        .try_write()
        .map_err(|_| anyhow::anyhow!("Дождитесь окончания ответа, записи или скачивания модели"))?;
    let model = value["model"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("Укажите модель"))?;
    if model == "whisper-small" {
        let path = tools::speech_model(&app);
        // Only remove the fixed model filenames, never an arbitrary user folder.
        for name in [
            "config.json",
            "model.bin",
            "tokenizer.json",
            "vocabulary.json",
            "vocabulary.txt",
        ] {
            let file = path.join(name);
            if file.is_file() {
                std::fs::remove_file(file)?;
            }
        }
        crate::model_storage::forget(&app, model)?;
        return Ok(Json(json!({"ok":true,"model":model})));
    }
    let endpoint = crate::model_storage::endpoint(&app, model).await?;
    let tags: Value = app
        .client
        .get(format!("{}/api/tags", endpoint))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    require(
        crate::models::installed(model, &tags["models"]),
        "Эта модель не установлена",
    )?;
    app.client
        .delete(format!("{}/api/delete", endpoint))
        .json(&json!({"model":model}))
        .send()
        .await?
        .error_for_status()?;
    crate::model_storage::forget(&app, model)?;
    Ok(Json(json!({"ok":true,"model":model})))
}
async fn asset(req: Request<Body>) -> Response {
    let path = req.uri().path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
    if let Some(file) = WEB.get_file(path) {
        let mime = mime_guess::from_path(path).first_or_octet_stream();
        (
            [
                ("content-type", mime.as_ref()),
                ("cache-control", "no-cache"),
            ],
            file.contents(),
        )
            .into_response()
    } else {
        StatusCode::NOT_FOUND.into_response()
    }
}

async fn assistant_config(State(app): State<Shared>) -> Json<Value> {
    let settings = app.settings.lock().await.clone();
    let mut result = json!({"agent":{"model":settings.agent_model,"system_prompt":engine::system_prompt(&settings,&settings.agent_model,true)},"chat":{"model":settings.chat_model,"system_prompt":engine::system_prompt(&settings,&settings.chat_model,false)},"sampling":settings.assistant,"application_content_filter":false});
    for role in ["agent", "chat"] {
        let model = result[role]["model"].as_str().unwrap_or("").to_owned();
        let endpoint = match crate::model_storage::endpoint(&app, &model).await {
            Ok(v) => v,
            Err(e) => {
                result[role]["error"] = json!(e.to_string());
                continue;
            }
        };
        let response = app
            .client
            .post(format!("{}/api/show", endpoint))
            .timeout(std::time::Duration::from_secs(5))
            .json(&json!({"model":model}))
            .send()
            .await;
        match response {
            Ok(response) if response.status().is_success() => {
                match response.json::<Value>().await {
                    Ok(show) => {
                        for field in [
                            "template",
                            "system",
                            "parameters",
                            "capabilities",
                            "details",
                        ] {
                            result[role][field] = show[field].clone();
                        }
                    }
                    Err(error) => result[role]["diagnostic_error"] = json!(error.to_string()),
                }
            }
            Ok(response) => {
                result[role]["diagnostic_error"] = json!(format!(
                    "Ollama: {}. Модель может быть не установлена.",
                    response.status()
                ))
            }
            Err(error) => {
                result[role]["diagnostic_error"] =
                    json!(format!("Метаданные модели недоступны: {error}"))
            }
        }
    }
    Json(result)
}

async fn document_read(State(app): State<Shared>, Json(value): Json<Value>) -> ApiResult<Value> {
    let item = attachment(&app, value["attachment"].as_str().unwrap_or("")).await?;
    require(item.kind == "document", "Ожидается PDF или DOCX")?;
    let id = value["job"]
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    Uuid::parse_str(&id)?;
    let cancel = app.shutdown.child_token();
    {
        let mut runs = app.runs.lock().await;
        require(!runs.contains_key(&id), "Задача уже запущена")?;
        runs.insert(
            id.clone(),
            Run {
                cancel: cancel.clone(),
                conversation: String::new(),
            },
        );
    }
    let result =
        crate::documents::extract(&app, std::path::Path::new(&item.path), &value, &cancel).await;
    app.runs.lock().await.remove(&id);
    Ok(Json(result?))
}

async fn document_translate(
    State(app): State<Shared>,
    Json(value): Json<Value>,
) -> Result<impl IntoResponse, ApiError> {
    let item = attachment(&app, value["attachment"].as_str().unwrap_or("")).await?;
    require(item.kind == "document", "Ожидается PDF или DOCX")?;
    let model_guard = app
        .model_lifecycle
        .clone()
        .try_read_owned()
        .map_err(|_| anyhow::anyhow!("Дождитесь подготовки движка"))?;
    let id = Uuid::new_v4().to_string();
    let cancel = app.shutdown.child_token();
    app.runs.lock().await.insert(
        id.clone(),
        Run {
            cancel: cancel.clone(),
            conversation: String::new(),
        },
    );
    let (tx, mut rx) = mpsc::channel::<Value>(128);
    tokio::spawn(async move {
        let _model_guard = model_guard;
        let _ = tx.send(json!({"type":"start","run_id":id})).await;
        let operation = async {
            let (bytes, extension, report) = crate::documents::translate(
                &app,
                std::path::Path::new(&item.path),
                &value,
                &tx,
                &cancel,
            )
            .await?;
            let stem = std::path::Path::new(&item.name)
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy();
            let result = store_upload(&app, &format!("{stem}-translated.{extension}"), &bytes)
                .await
                .map_err(|error| error.0)?;
            let _ = tx
                .send(json!({"type":"document_result","attachment":result,"report":report}))
                .await;
            Ok::<(), anyhow::Error>(())
        };
        let disconnect_tx = tx.clone();
        let disconnect_cancel = cancel.clone();
        let disconnect = tokio::spawn(async move {
            disconnect_tx.closed().await;
            disconnect_cancel.cancel();
        });
        let outcome = operation.await;
        disconnect.abort();
        if let Err(error) = outcome {
            let _ = tx
                .send(json!({"type":"error","message":format!("{error:#}")}))
                .await;
        }
        app.runs.lock().await.remove(&id);
        let _ = tx.send(json!({"type":"done"})).await;
    });
    let stream = async_stream::stream! {while let Some(value)=rx.recv().await{yield Ok::<Event,Infallible>(Event::default().json_data(value).unwrap());}};
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}
async fn quick_area(State(app): State<Shared>, Json(value): Json<Value>) -> ApiResult<Value> {
    let _guard = app
        .model_lifecycle
        .clone()
        .try_read_owned()
        .map_err(|_| anyhow::anyhow!("Дождитесь подготовки движка"))?;
    let settings = app.settings.lock().await.clone();
    let cancel = app.shutdown.child_token();
    if let Some(old) = app.quick_cancel.lock().await.replace(cancel.clone()) {
        old.cancel();
    }
    let id = Uuid::new_v4().to_string();
    *app.quick.lock().await =
        json!({"id":id,"status":"capturing","notice":"Выделите область экрана. Esc — отмена."});
    let language = value["language"]
        .as_str()
        .unwrap_or(if settings.quick_source == "ru" {
            "ru"
        } else {
            "en-US"
        });
    let capture = tools::media(&app, "capture_area", None, Some(language), 10, &cancel).await;
    let captured = match capture {
        Ok(v) if v["cancelled"] == true => {
            let mut current = app.quick.lock().await;
            if current["id"] == id {
                *current = json!({"id":id,"status":"idle"});
            }
            return Ok(Json(json!({"cancelled":true})));
        }
        Ok(v) => v,
        Err(_) if cancel.is_cancelled() => {
            let mut current = app.quick.lock().await;
            if current["id"] == id {
                *current = json!({"id":id,"status":"idle"});
            }
            return Ok(Json(json!({"cancelled":true})));
        }
        Err(error) => {
            let mut current = app.quick.lock().await;
            if current["id"] == id {
                *current = json!({"id":id,"status":"error","error":format!("{error:#}")});
            }
            return Err(error.into());
        }
    };
    if app.quick.lock().await["id"] != id || cancel.is_cancelled() {
        return Ok(Json(json!({"cancelled":true})));
    }
    let mut payload = json!({"text":captured["text"]});
    for field in ["source", "target"] {
        if let Some(text) = value[field].as_str() {
            payload[field] = json!(text);
        }
    }
    quick_translate(State(app), Json(payload)).await
}
