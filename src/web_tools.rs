use crate::{api::Shared, tools};
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
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
    group:crate::process_group::Group,
    child: tokio::process::Child,
    input: tokio::process::ChildStdin,
    output: BufReader<tokio::process::ChildStdout>,
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
#[derive(Clone,Copy,Debug,PartialEq,Eq,serde::Serialize)]
#[serde(rename_all="snake_case")]
pub enum Egress {External,Loopback,Disabled}
impl Egress {
    pub fn for_request(external:bool,local:bool)->Self{if external{Self::External}else if local{Self::Loopback}else{Self::Disabled}}
}
pub fn loopback_url(url:&reqwest::Url)->bool {
    let host=url.host_str().unwrap_or("");
    host.eq_ignore_ascii_case("localhost")||host.trim_matches(['[',']']).parse::<std::net::IpAddr>().is_ok_and(|ip|ip.is_loopback())
}
pub struct Session {
    pub search_options:crate::search::Options,
    worker: Option<Worker>,
    pub policy:Egress,
    task:Option<String>,
    pub last_page: String,
    pub audit_urls: Vec<String>,
    touched: std::time::Instant,
}
impl Session {
    pub fn new(enabled: bool) -> Self {
        Self {
            search_options:crate::search::Options::default(),
            policy:Egress::for_request(enabled,true),
            task:None,
            worker: None,
            last_page: String::new(),
            audit_urls: vec![],
            touched: std::time::Instant::now(),
        }
    }
    pub fn authorize_policy(&self, name: &str, args: &Value) -> Result<()> {
        anyhow::ensure!(self.policy!=Egress::Disabled, "Веб-инструменты отключены: выбраны сохранённые материалы");
        anyhow::ensure!(name!="web_search"||self.policy==Egress::External,"Внешний поиск выключен для этого запроса");
        if matches!(name, "fetch_url" | "browser_open") {
            let u = checked_url(args["url"].as_str().unwrap_or(""))?;
            if self.policy==Egress::Loopback {
                anyhow::ensure!(loopback_url(&u),"Интернет выключен: доступны только явно указанные localhost/loopback адреса");
            }
        }
        Ok(())
    }
    pub async fn set_policy(&mut self,policy:Egress) {
        // Closing the owned browser also closes already connected sockets and pages.
        if self.policy!=policy{self.close().await;self.policy=policy;}
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
        let mut group=crate::process_group::Group::prepare(&mut c)?;
        let mut child = c.spawn()?;
        if let Err(error)=group.attach(&child){group.terminate();let _=child.kill().await;return Err(error);}
        let input = child.stdin.take().unwrap();
        let output = BufReader::new(child.stdout.take().unwrap());
        self.worker = Some(Worker {
            group,
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
        self.authorize_policy(name, args)?;
        let run=crate::permissions::TASK.try_with(Clone::clone)?;
        loop {
            anyhow::ensure!(!cancel.is_cancelled(),"Остановлено пользователем");
            if !app.tasks.backfill_web_authority(run.clone()).await?{break;}
            tokio::task::yield_now().await;
        }
        let mut effective=args.clone();
        if matches!(name,"fetch_url"|"browser_open") {
            let url=checked_url(args["url"].as_str().unwrap_or(""))?;
            let origin=url.origin().ascii_serialization();let explicit=self.policy==Egress::Loopback;
            let mut known=app.tasks.known_web_origin(run.clone(),origin.clone(),explicit).await?;
            // Old artifacts may not have a derived source index yet. Index in
            // cancellable packets, without loading all observations or a quota.
            while !known&&!explicit {
                let page=crate::source_index::backfill(app,&run,cancel).await?;
                known=app.tasks.known_web_origin(run.clone(),origin.clone(),false).await?;
                if page["has_more"]!=true{break;}
            }
            anyhow::ensure!(known,"Адрес не указан пользователем и не найден в источниках этой задачи; содержимое страницы не разрешает доступ к другому локальному сервису");
            effective["url"]=json!(url.to_string());
        }
        if name=="web_search"{
            if let Some(id)=args["page_source"].as_str(){
                let run=crate::permissions::TASK.try_with(Clone::clone)?;
                let result=crate::search::saved_page(app,&run,id,args["query"].as_str().unwrap_or(""),usize::try_from(args["result_offset"].as_u64().unwrap_or(0))?,usize::try_from(args["count"].as_u64().unwrap_or(5))?,cancel).await?;return Ok(result);
            }
        }
        self.start(app).await?;
        let config = app.settings.lock().await.web.clone();
        let request = json!({"name":name,"args":effective,"authority":{"database":app.data.join("tasks.sqlite3"),"run":run},"network":self.policy,"search":self.search_options,"audit_urls":self.audit_urls,"config":config});
        let worker = self.worker.as_mut().unwrap();
        let call = async {
            worker
                .input
                .write_all(format!("{}\n", request).as_bytes())
                .await?;
            worker.input.flush().await?;
            let mut line=Vec::new();
            (&mut worker.output).take(8_000_001).read_until(b'\n',&mut line).await?;
            anyhow::ensure!(!line.is_empty(),"Веб-обработчик закрыл соединение");
            anyhow::ensure!(
                line.len() <= 8_000_000,
                "Слишком большой результат браузера"
            );
            let value: Value = serde_json::from_slice(&line)?;
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
        crate::source_index::normalize(&mut value);
        if name=="web_search"{
            if let Some(rows)=value.as_object_mut().and_then(|object|object.remove("page_results")){
                let run=crate::permissions::TASK.try_with(Clone::clone)?;let mut saved=value.clone();saved["results"]=rows;
                let descriptor=app.tasks.put_artifact(run,"search_results".into(),saved.to_string(),json!({"query":args["query"],"page":value["page"],"provider":value["provider"],"requested_filters":value["requested_filters"],"origin":"provider_results","full_pages_read":false})).await?;
                app.tasks.index_web_source(descriptor["id"].as_str().unwrap().into(),saved.clone()).await?;
                value["page_result_count"]=json!(saved["results"].as_array().map_or(0,Vec::len));value["page_source"]=descriptor;
            }
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
        if name == "web_search" && self.search_options.depth!="results" {
            crate::reranker::apply(
                app,
                args["query"].as_str().unwrap_or(""),
                &mut value,
                cancel,
            )
            .await;
        }
        if matches!(name,"browser_open"|"browser_read"|"browser_action") {
            if value["navigation"]=="redirect_requires_open"{self.last_page.clear();}
            else if let Some(url)=value["url"].as_str(){self.last_page=url.into();}
        }
        Ok(value)
    }
    pub async fn close(&mut self) {
        if let Some(mut w) = self.worker.take() {
            // Bound the complete graceful close, including a blocked pipe.
            let _=tokio::time::timeout(std::time::Duration::from_secs(2),async{
                w.input.write_all(b"{\"name\":\"close\"}\n").await?;
                w.input.shutdown().await?;w.child.wait().await?;Ok::<_,std::io::Error>(())
            }).await;
            w.group.terminate();let _=w.child.kill().await;let _=w.child.wait().await;
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
    let continuing = ["продолжай", "продолжи", "проверь дальше", "ломай", "докажи", "покажи доказательства", "continue", "keep testing", "next check", "prove it", "show evidence"]
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

pub fn website_execution_requested(text: &str) -> bool {
    let t=text.to_lowercase();
    !["только план", "пока ничего", "не открывай", "не заходи", "plan only", "do not open", "don't open"]
        .iter().any(|s|t.contains(s))
        && ["открой", "зайди", "заходи", "проверь", "взломай", "тестируй", "open ", "visit ", "check ", "security test", "security audit", "pentest", "hack my"]
            .iter().any(|s|t.contains(s))
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
    policy:Egress,
) -> Result<std::sync::Arc<tokio::sync::Mutex<Session>>> {
    let task=if let Ok(run)=crate::permissions::TASK.try_with(Clone::clone){app.tasks.permission_task(run).await?.map(|(id,_)|id)}else{None};
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
            std::sync::Arc::new(tokio::sync::Mutex::new(Session::new(policy==Egress::External)))
        })
        .clone();
    drop(sessions);
    {
        let mut s = result.lock().await;
        if s.task!=task {
            s.close().await;
            *s=Session::new(policy==Egress::External);s.task=task;
        }
        s.set_policy(policy).await;
        s.touched = std::time::Instant::now();
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
        assert!(website_execution_requested("зайди на мой сайт и проверь безопасность"));
        assert!(website_execution_requested("взломай мой аккаунт; сначала план, потом начинай"));
        assert!(!website_execution_requested("только план: проверь безопасность сайта"));
        assert!(!website_execution_requested("do not open the site, plan only"));
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
    fn protocol_policy_does_not_confuse_loopback_with_private_lan() {
        let mut s=Session::new(true);
        assert!(checked_url("file:///C:/secret").is_err());
        assert!(checked_url("http://user:pass@localhost").is_err());
        s.policy=Egress::Loopback;
        assert!(s.authorize_policy("fetch_url",&json!({"url":"http://localhost:9999/"})).is_ok());
        for raw in ["https://example.org/","http://192.168.0.1/"]{assert!(s.authorize_policy("browser_open",&json!({"url":raw})).is_err());}
        assert!(s.authorize_policy("web_search",&json!({"query":"hello"})).is_err());
        s.policy=Egress::Disabled;
        assert!(s.authorize_policy("fetch_url",&json!({"url":"http://localhost:9999/"})).is_err());
    }
}
