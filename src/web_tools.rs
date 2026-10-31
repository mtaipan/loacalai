use crate::{api::Shared, tools};
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Config {
    pub provider: String,
    pub searxng_url: String,
    pub brave_key: String,
    pub browser_visible: bool,
    pub reranker_enabled: bool,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            provider: "duckduckgo".into(),
            searxng_url: String::new(),
            brave_key: String::new(),
            browser_visible: false,
            reranker_enabled: true,
        }
    }
}
impl Config {
    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!(
            ["duckduckgo", "searxng", "brave"].contains(&self.provider.as_str()),
            "Неизвестный поисковик"
        );
        anyhow::ensure!(self.brave_key.len() <= 512, "Слишком длинный API-ключ");
        if !self.searxng_url.is_empty() {
            checked_url(&self.searxng_url)?;
        }
        Ok(())
    }
}
pub fn checked_url(text: &str) -> Result<reqwest::Url> {
    let u = reqwest::Url::parse(text)?;
    anyhow::ensure!(
        ["http", "https"].contains(&u.scheme())
            && u.host_str().is_some()
            && u.username().is_empty()
            && u.password().is_none(),
        "Нужен HTTP(S) URL без встроенного логина/пароля"
    );
    Ok(u)
}
pub fn search_requested(text: &str) -> bool {
    let t = text.to_lowercase();
    [
        "погугли",
        "загугли",
        "в интернете",
        "в сети",
        "найди источники",
        "web search",
        "search the web",
        "google ",
        "latest news",
        "свежие новости",
    ]
    .iter()
    .any(|s| t.contains(s))
}
pub fn is_web_tool(name: &str) -> bool {
    [
        "web_search",
        "fetch_url",
        "browser_open",
        "browser_read",
        "browser_action",
    ]
    .contains(&name)
}

struct Worker {
    child: tokio::process::Child,
    input: tokio::process::ChildStdin,
    output: tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
}
#[derive(Debug)]
pub struct InputRequired(pub String);
impl std::fmt::Display for InputRequired {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for InputRequired {}
#[derive(Debug)]
pub struct StaleRef(pub String);
impl std::fmt::Display for StaleRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for StaleRef {}
pub struct Session {
    worker: Option<Worker>,
    pub user_urls: Vec<String>,
    observed: Vec<reqwest::Url>,
    pub enabled: bool,
    pub last_page: String,
    pub user_texts: Vec<String>,
    pub audit_urls: Vec<String>,
    touched: std::time::Instant,
}
impl Session {
    pub fn new(text: &str, enabled: bool) -> Self {
        let observed = crate::task_scope::urls(text);
        Self {
            user_urls: observed.iter().map(ToString::to_string).collect(),
            observed,
            enabled,
            worker: None,
            last_page: String::new(),
            user_texts: vec![text.into()],
            audit_urls: vec![],
            touched: std::time::Instant::now(),
        }
    }
    pub fn authorize(&self, name: &str, args: &Value) -> Result<()> {
        anyhow::ensure!(self.enabled, "Сеть выключена для этого запроса");
        if matches!(name, "fetch_url" | "browser_open") {
            let u = checked_url(args["url"].as_str().unwrap_or(""))?;
            anyhow::ensure!(self.observed.iter().any(|seen|seen.origin()==u.origin()),"Сначала найдите адрес через web_search или используйте URL/ссылку из текущей страницы. Не придумывайте адреса.");
        }
        Ok(())
    }
    pub fn observe(&mut self, result: &Value) {
        for item in result["results"]
            .as_array()
            .into_iter()
            .flatten()
            .chain(result["links"].as_array().into_iter().flatten())
        {
            if let Some(s) = item["url"].as_str() {
                if let Ok(u) = checked_url(s) {
                    if !self.observed.contains(&u) {
                        self.observed.push(u);
                    }
                }
            }
        }
        for key in ["url", "redirect_to"] {
            if let Some(s) = result[key].as_str() {
                if let Ok(u) = checked_url(s) {
                    if !self.observed.contains(&u) {
                        self.observed.push(u);
                    }
                }
            }
        }
        if let Some(u) = result["url"].as_str() {
            self.last_page = u.into();
        }
    }
    async fn start(&mut self, app: &Shared) -> Result<()> {
        if self.worker.is_some() {
            return Ok(());
        }
        let python =
            tools::python(app).context("Для веб-инструментов установите компоненты приложения")?;
        let mut c = tokio::process::Command::new(python);
        c.arg(app.root.join("runtime/web_worker.py"))
            .env("PYTHONUTF8", "1")
            .env("PLAYWRIGHT_BROWSERS_PATH", app.data.join("cache/browsers"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        #[cfg(windows)]
        c.creation_flags(0x08000000);
        let mut child = c.spawn()?;
        let input = child.stdin.take().unwrap();
        let output = BufReader::new(child.stdout.take().unwrap()).lines();
        self.worker = Some(Worker {
            child,
            input,
            output,
        });
        Ok(())
    }
    pub async fn execute(
        &mut self,
        app: &Shared,
        name: &str,
        args: &Value,
        cancel: &CancellationToken,
    ) -> Result<Value> {
        self.authorize(name, args)?;
        self.start(app).await?;
        let config = app.settings.lock().await.web.clone();
        let request = json!({"name":name,"args":args,"user_urls":self.user_urls,"user_texts":self.user_texts,"audit_urls":self.audit_urls,"config":config});
        let worker = self.worker.as_mut().unwrap();
        let call = async {
            worker
                .input
                .write_all(format!("{}\n", request).as_bytes())
                .await?;
            worker.input.flush().await?;
            let line = worker
                .output
                .next_line()
                .await?
                .context("Веб-обработчик закрыл соединение")?;
            anyhow::ensure!(
                line.len() <= 8_000_000,
                "Слишком большой результат браузера"
            );
            let value: Value = serde_json::from_str(&line)?;
            Ok::<Value, anyhow::Error>(value)
        };
        let result = tokio::select! {_ = cancel.cancelled()=>Err(anyhow::anyhow!("Остановлено пользователем")), r=tokio::time::timeout(std::time::Duration::from_secs(60),call)=>match r {Ok(v)=>v,Err(_)=>Err(anyhow::anyhow!("Истекло время веб-инструмента"))}};
        let mut value = match result {
            Ok(v) => v,
            Err(e) => {
                self.close().await;
                return Err(e);
            }
        };
        // A stale ref or HTTP error does not invalidate the live browser session.
        // Only transport failure/cancellation above discards the protocol worker.
        if let Some(error) = value["error"].as_str() {
            if value["error_code"] == "needs_input" {
                return Err(InputRequired(error.into()).into());
            }
            if value["error_code"] == "stale_ref" {
                return Err(StaleRef(error.into()).into());
            }
            anyhow::bail!("{error}");
        }
        if let Some(encoded) = value
            .as_object_mut()
            .and_then(|o| o.remove("_screenshot"))
            .and_then(|s| s.as_str().map(str::to_owned))
        {
            use base64::Engine;
            let bytes = base64::engine::general_purpose::STANDARD.decode(encoded)?;
            let a = crate::api::store_upload(app, "browser.png", &bytes)
                .await
                .map_err(|e| e.0)?;
            value["screenshot"] =
                json!({"id":a.id,"url":format!("/api/files/{}",a.id),"name":a.name});
        }
        if name == "web_search" {
            crate::reranker::apply(
                app,
                args["query"].as_str().unwrap_or(""),
                &mut value,
                cancel,
            )
            .await;
        }
        self.observe(&value);
        Ok(value)
    }
    pub async fn close(&mut self) {
        if let Some(mut w) = self.worker.take() {
            let _ = w.input.write_all(b"{\"name\":\"close\"}\n").await;
            let _ = w.input.shutdown().await;
            if tokio::time::timeout(std::time::Duration::from_secs(2), w.child.wait())
                .await
                .is_err()
            {
                let _ = w.child.kill().await;
            }
        }
        self.last_page.clear();
    }
}

/// Scope comes exclusively from the current user request and the same chat's
/// ongoing test. Page content and model output cannot grant this permission.
pub fn security_test_urls(text: &str, previous: &[String], last_page: &str) -> Vec<String> {
    let mut fenced = false;
    let request = text.lines().filter(|line| {
        let line = line.trim_start();
        if line.starts_with("```") { fenced = !fenced; return false; }
        !fenced && !line.starts_with('>')
    }).collect::<Vec<_>>().join("\n");
    let t = request.trim().to_lowercase();
    let targets = crate::task_scope::urls(&request);
    let testing = ["проверь безопасность", "проверить безопасность", "проверку безопасности",
        "проверка безопасности", "уязвим", "пентест", "пентестинг", "взломай", "аудит",
        "security test", "security audit", "pentest", "penetration test", "vulnerabilit",
        "hack my", "test my site's security", "test my website's security"]
        .iter().any(|s| t.contains(s));
    if testing && !targets.is_empty() {
        return targets.iter().map(ToString::to_string).collect();
    }
    let continuing = ["продолжай", "продолжи", "проверь дальше", "continue", "keep testing", "next check"]
        .iter().any(|s| t.starts_with(s));
    if !previous.is_empty() && (testing || continuing) {
        if let Ok(page) = checked_url(last_page) {
            if previous.iter().filter_map(|u| checked_url(u).ok()).any(|u| u.origin() == page.origin())
                && targets.iter().all(|u| u.origin() == page.origin()) {
                return previous.to_vec();
            }
        }
    }
    vec![]
}

pub fn browser_followup(text: &str) -> bool {
    let t = text.to_lowercase();
    [
        "нажми",
        "клик",
        "введи",
        "заполни",
        "прокрути",
        "на странице",
        "этот сайт",
        "этом сайте",
        "вкладк",
        "click",
        "fill",
        "scroll",
        "current page",
        "this site",
        "browser",
    ]
    .iter()
    .any(|s| t.contains(s))
}
pub async fn session(
    app: &Shared,
    id: &str,
    text: &str,
    enabled: bool,
) -> Result<std::sync::Arc<tokio::sync::Mutex<Session>>> {
    let mut sessions = app.web_sessions.lock().await;
    let mut remove = Vec::new();
    for (key, value) in sessions.iter() {
        if key != id {
            if let Ok(mut s) = value.try_lock() {
                if s.touched.elapsed().as_secs() > 900
                    || (sessions.len() >= 4 && s.worker.is_none())
                {
                    s.close().await;
                    remove.push(key.clone());
                }
            }
        }
    }
    for key in remove {
        sessions.remove(&key);
    }
    if !sessions.contains_key(id) && sessions.len() >= 4 {
        let victim = sessions
            .iter()
            .filter_map(|(k, v)| v.try_lock().ok().map(|s| (k.clone(), s.touched)))
            .min_by_key(|(_, t)| *t)
            .map(|(k, _)| k);
        if let Some(key) = victim {
            if let Some(value) = sessions.remove(&key) {
                value.lock().await.close().await;
            }
        }
    }
    anyhow::ensure!(
        sessions.contains_key(id) || sessions.len() < 4,
        "Все четыре сессии браузера заняты активными задачами"
    );
    let result = sessions
        .entry(id.into())
        .or_insert_with(|| {
            std::sync::Arc::new(tokio::sync::Mutex::new(Session::new(text, enabled)))
        })
        .clone();
    drop(sessions);
    {
        let mut s = result.lock().await;
        s.enabled = enabled;
        s.touched = std::time::Instant::now();
        for u in crate::task_scope::urls(text) {
            let raw = u.to_string();
            if !s.user_urls.contains(&raw) {
                s.user_urls.push(raw);
            }
            if !s.observed.contains(&u) {
                s.observed.push(u);
            }
        }
    }
    Ok(result)
}

pub async fn install(app: &Shared, cancel: &CancellationToken) -> Result<Value> {
    let mut c = tokio::process::Command::new(tools::python(app).context("Python не установлен")?);
    c.arg(app.root.join("runtime/setup_browser.py"))
        .env("PYTHONUTF8", "1")
        .env("PLAYWRIGHT_BROWSERS_PATH", app.data.join("cache/browsers"))
        .env("PIP_CACHE_DIR", app.data.join("cache/pip"));
    let (code, out, err) = tools::capture(c, cancel, 600).await?;
    anyhow::ensure!(
        code == 0,
        "Установка браузера: {} {}",
        tools::truncate(&out, 1200),
        tools::truncate(&err, 800)
    );
    Ok(json!({"ok":true,"message":tools::truncate(&out,1000)}))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn security_testing_uses_the_request_without_a_ui_mode() {
        let site = "http://127.0.0.1:1111/";
        let scope = vec![site.to_string()];
        for request in [
            format!("проверь безопасность моего сайта {site}"),
            format!("{site} взломай мой аккаунт на моем локальном сайте"),
            format!("security audit {site}"),
        ] {
            assert_eq!(security_test_urls(&request, &[], ""), scope);
        }
        assert_eq!(security_test_urls("продолжай проверку", &scope, site), scope);
        assert_eq!(security_test_urls("проверь уязвимости", &[], site), Vec::<String>::new());
        for request in ["nice", "введи мой логин", "открой http://127.0.0.1:1111/", "continue http://example.org/"] {
            assert!(security_test_urls(request, &scope, site).is_empty());
        }
        assert!(security_test_urls("continue", &scope, "http://example.org/").is_empty());
        assert!(security_test_urls("> security audit http://example.org/", &[], "").is_empty());
        assert!(security_test_urls("```\nsecurity audit http://example.org/\n```", &[], "").is_empty());
        assert!(!include_str!("../web/app.js").contains("security-test"));
        assert!(!include_str!("../web/app.js").contains("security_test"));
    }
    #[test]
    fn ui_and_confirmation_text_is_not_replaced_by_question_marks() {
        for text in [
            include_str!("tools.rs"),
            include_str!("api.rs"),
            include_str!("../web/app.js"),
            include_str!("../web/web-controls.js"),
            include_str!("../README.md"),
            include_str!("../START-HERE.md"),
            include_str!("../RELEASE-0.4.md"),
        ] {
            assert!(!text.contains(&"?".repeat(4)), "Corrupted user-facing text");
        }
        assert!(include_str!("tools.rs").contains("Действие на сайте"));
    }
    #[test]
    fn evidence_and_protocol_boundaries() {
        let mut s = Session::new("open http://localhost:9999/", true);
        assert!(s
            .authorize("browser_open", &json!({"url":"http://localhost:9999/x"}))
            .is_ok());
        assert!(s
            .authorize("browser_open", &json!({"url":"https://invented.invalid/"}))
            .is_err());
        s.observe(&json!({"results":[{"url":"https://example.org/"}]}));
        assert!(s
            .authorize("fetch_url", &json!({"url":"https://example.org/a"}))
            .is_ok());
        assert!(checked_url("file:///C:/secret").is_err());
        assert!(checked_url("http://user:pass@localhost").is_err());
        s.enabled = false;
        assert!(s
            .authorize("web_search", &json!({"query":"hello"}))
            .is_err());
    }
}
