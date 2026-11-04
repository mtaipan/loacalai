//! Installation ownership belongs to a durable job, never a browser connection.
use anyhow::{Context,Result};
use rusqlite::{params,Connection,OptionalExtension};
use serde_json::{json,Value};
use crate::api::Shared;
use tokio_util::sync::CancellationToken;

pub fn install(db:&Connection)->Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS installation_jobs(id TEXT PRIMARY KEY,spec TEXT NOT NULL,status TEXT NOT NULL,attempt INTEGER NOT NULL DEFAULT 1,created TEXT NOT NULL,updated TEXT NOT NULL,error TEXT);
        CREATE TABLE IF NOT EXISTS installation_requests(request_id TEXT PRIMARY KEY,job TEXT NOT NULL REFERENCES installation_jobs(id) ON DELETE CASCADE);
        CREATE INDEX IF NOT EXISTS installation_queue ON installation_jobs(status,created,id);
        CREATE TABLE IF NOT EXISTS installation_events(sequence INTEGER PRIMARY KEY AUTOINCREMENT,job TEXT NOT NULL REFERENCES installation_jobs(id) ON DELETE CASCADE,payload TEXT NOT NULL);
        CREATE INDEX IF NOT EXISTS installation_event_page ON installation_events(job,sequence);")?;
    Ok(())
}
pub fn recover(db:&Connection)->Result<()> {
    db.execute("UPDATE installation_jobs SET status='interrupted',error='Приложение закрылось. Повторите установку: сохранённые части будут проверены перед использованием.',updated=?1 WHERE status IN ('queued','running','stopping')",[crate::api::now()])?;
    Ok(())
}
fn row(db:&Connection,id:&str)->Result<Value> {
    let id=db.query_row("SELECT job FROM installation_requests WHERE request_id=?1",[id],|r|r.get::<_,String>(0)).optional()?.unwrap_or_else(||id.to_string());
    Ok(db.query_row("SELECT spec,status,attempt,created,updated,error FROM installation_jobs WHERE id=?1",[&id],|r|Ok(json!({"id":id,"spec":serde_json::from_str::<Value>(&r.get::<_,String>(0)?).unwrap_or(Value::Null),"status":r.get::<_,String>(1)?,"attempt":r.get::<_,i64>(2)?,"created":r.get::<_,String>(3)?,"updated":r.get::<_,String>(4)?,"error":r.get::<_,Option<String>>(5)?}))).optional()?.ok_or(crate::task_store::MissingResource("Установка не найдена"))?)
}
impl crate::task_store::Store {
    pub async fn installation_create(&self,id:String,spec:Value)->Result<Value> {self.access(move|db|{
        uuid::Uuid::parse_str(&id)?;let encoded=serde_json::to_string(&spec)?;let tx=db.transaction()?;
        if let Some(previous)=tx.query_row("SELECT spec FROM installation_jobs WHERE id=COALESCE((SELECT job FROM installation_requests WHERE request_id=?1),?1)",[&id],|r|r.get::<_,String>(0)).optional()?{anyhow::ensure!(previous==encoded,"Этот ID установки уже относится к другим параметрам");return row(&tx,&id);}
        let canonical=if let Some(existing)=tx.query_row("SELECT id FROM installation_jobs WHERE spec=?1 AND status IN ('queued','running','stopping') ORDER BY created,id LIMIT 1",[&encoded],|r|r.get::<_,String>(0)).optional()?{existing}else{
            let now=crate::api::now();tx.execute("INSERT INTO installation_jobs(id,spec,status,created,updated) VALUES(?1,?2,'queued',?3,?3)",params![id,encoded,now])?;id.clone()
        };
        tx.execute("INSERT INTO installation_requests VALUES(?1,?2)",params![id,canonical])?;let result=row(&tx,&canonical)?;tx.commit()?;Ok(result)
    }).await}
    pub async fn installation_get(&self,id:String)->Result<Value>{self.access(move|db|row(db,&id)).await}
    pub async fn installation_list(&self,before:Option<(String,String)>)->Result<Value>{self.access(move|db|{
        let mut q=db.prepare("SELECT id,created FROM installation_jobs WHERE ?1 IS NULL OR (created,id)<(?1,?2) ORDER BY created DESC,id DESC LIMIT 51")?;
        let mut keys=q.query_map(params![before.as_ref().map(|v|&v.0),before.as_ref().map(|v|&v.1)],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
        let more=keys.len()>50;keys.truncate(50);let next=if more{keys.last().map(|(id,created)|json!({"before_created":created,"before_id":id}))}else{None};
        let mut items=Vec::new();for(id,_)in keys{items.push(row(db,&id)?);}Ok(json!({"items":items,"next":next}))
    }).await}
    pub async fn installation_events(&self,id:String,after:i64)->Result<Value>{self.access(move|db|{
        anyhow::ensure!(after>=0,"Курсор не может быть отрицательным");let state=row(db,&id)?;let id=state["id"].as_str().unwrap().to_string();let mut q=db.prepare("SELECT sequence,payload FROM installation_events WHERE job=?1 AND sequence>?2 ORDER BY sequence LIMIT 64")?;
        let items=q.query_map(params![id,after],|r|Ok(json!({"sequence":r.get::<_,i64>(0)?,"event":serde_json::from_str::<Value>(&r.get::<_,String>(1)?).unwrap_or(Value::Null)})))?.collect::<rusqlite::Result<Vec<_>>>()?;
        let next=items.last().and_then(|v|v["sequence"].as_i64()).unwrap_or(after);let more:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM installation_events WHERE job=?1 AND sequence>?2)",params![id,next],|r|r.get(0))?;
        Ok(json!({"job":state,"items":items,"next_after":next,"has_more":more}))
    }).await}
    pub async fn installation_event(&self,id:String,event:Value)->Result<()> {self.access(move|db|{db.execute("INSERT INTO installation_events(job,payload) VALUES(?1,?2)",params![id,event.to_string()])?;Ok(())}).await}
    async fn installation_claim(&self)->Result<Option<Value>> {self.access(|db|{
        let tx=db.transaction()?;let next=tx.query_row("SELECT id FROM installation_jobs WHERE status='queued' ORDER BY created,id LIMIT 1",[],|r|r.get::<_,String>(0)).optional()?;
        let Some(id)=next else{return Ok(None);};tx.execute("UPDATE installation_jobs SET status='running',updated=?1,error=NULL WHERE id=?2 AND status='queued'",params![crate::api::now(),id])?;let result=row(&tx,&id)?;tx.commit()?;Ok(Some(result))
    }).await}
    pub async fn installation_cancel(&self,id:String)->Result<Value>{self.access(move|db|{
        let value=row(db,&id)?;let id=value["id"].as_str().unwrap();db.execute("UPDATE installation_jobs SET status=CASE WHEN status='queued' THEN 'cancelled' ELSE 'stopping' END,updated=?1 WHERE id=?2 AND status IN ('queued','running')",params![crate::api::now(),id])?;row(db,&id)
    }).await}
    pub async fn installation_retry(&self,id:String)->Result<Value>{self.access(move|db|{
        let old=row(db,&id)?;let id=old["id"].as_str().unwrap();anyhow::ensure!(["failed","cancelled","interrupted"].contains(&old["status"].as_str().unwrap_or("")),"Повтор доступен после ошибки, остановки или перезапуска приложения");
        let active:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM installation_jobs WHERE spec=?1 AND status IN ('queued','running','stopping'))",[old["spec"].to_string()],|r|r.get(0))?;anyhow::ensure!(!active,"Такая установка уже выполняется");
        db.execute("UPDATE installation_jobs SET status='queued',attempt=attempt+1,updated=?1,error=NULL WHERE id=?2",params![crate::api::now(),id])?;row(db,&id)
    }).await}
    async fn installation_finish(&self,id:String,result:Result<()>,cancelled:bool)->Result<()> {self.access(move|db|{
        let(status,error)=match result{Ok(())=>("succeeded",None),Err(e) if cancelled=>("cancelled",Some(format!("{e:#}"))),Err(e)=>("failed",Some(format!("{e:#}")))};
        db.execute("UPDATE installation_jobs SET status=?1,error=?2,updated=?3 WHERE id=?4 AND status IN ('running','stopping')",params![status,error,crate::api::now(),id])?;Ok(())
    }).await}
}

pub fn validate(spec:&Value)->Result<()> {
    anyhow::ensure!(spec.is_object(),"Укажите параметры установки");
    match spec["kind"].as_str().unwrap_or("") {
        "models"=>{let models=spec["models"].as_array().context("Укажите модели")?;anyhow::ensure!(!models.is_empty(),"Укажите модели");for model in models{let model=model.as_str().context("ID модели должен быть строкой")?;anyhow::ensure!(crate::models::catalog().as_array().unwrap().iter().any(|m|m["id"]==model),"Модель отсутствует в каталоге");}if let Some(directory)=spec.get("directory"){anyhow::ensure!(directory.is_string(),"Папка модели должна быть строкой");}},
        "gpu"=>{anyhow::ensure!(cfg!(windows),"Модули GPU на этой платформе устанавливаются вместе с системным Ollama");anyhow::ensure!(crate::engine::owns_engine(),"Компоненты внешнего сервера устанавливаются отдельно");anyhow::ensure!(["cuda","vulkan","rocm"].contains(&spec["backend"].as_str().unwrap_or("")),"Неизвестный модуль GPU");},
        "browser"=>{},_=>anyhow::bail!("Неизвестная установка")
    }Ok(())
}

// The registry contains cancellation handles only. All visible state and events live in SQLite.
#[derive(Default)]pub struct Runtime {driver:std::sync::atomic::AtomicBool,active:tokio::sync::Mutex<std::collections::HashMap<String,CancellationToken>>}
pub async fn start(app:&Shared,id:String,spec:Value)->Result<Value>{validate(&spec)?;let job=app.tasks.installation_create(id,spec).await?;wake(app);Ok(job)}
pub async fn cancel(app:&Shared,id:String)->Result<Value>{let value=app.tasks.installation_cancel(id.clone()).await?;if let Some(token)=app.installations.active.lock().await.get(value["id"].as_str().unwrap()){token.cancel();}Ok(value)}
pub async fn retry(app:&Shared,id:String)->Result<Value>{let job=app.tasks.installation_get(id.clone()).await?;validate(&job["spec"])?;let job=app.tasks.installation_retry(id).await?;wake(app);Ok(job)}
pub fn wake(app:&Shared){
    use std::sync::atomic::Ordering;
    if app.installations.driver.compare_exchange(false,true,Ordering::AcqRel,Ordering::Acquire).is_err(){return;}
    let app=app.clone();tokio::spawn(async move{
        loop {
            match app.tasks.installation_claim().await {
                Ok(Some(job))=>{let id=job["id"].as_str().unwrap().to_string();let token=app.shutdown.child_token();app.installations.active.lock().await.insert(id.clone(),token.clone());
                    // A cancellation can precede registration of its handle.
                    if app.tasks.installation_get(id.clone()).await.is_ok_and(|v|v["status"]=="stopping"){token.cancel();}
                    let (tx,mut rx)=tokio::sync::mpsc::channel(32);let store=app.tasks.clone();let key=id.clone();let relay=tokio::spawn(async move{while let Some(event)=rx.recv().await{store.installation_event(key.clone(),event).await?;}Ok::<(),anyhow::Error>(())});
                    let outcome=execute(&app,&job["spec"],&tx,&token).await;drop(tx);let recording=relay.await;let outcome=match recording{Ok(Ok(()))=>outcome,Ok(Err(e))=>Err(e.context("Не удалось сохранить ход установки")),Err(e)=>Err(e.into())};
                    let _=app.tasks.installation_finish(id.clone(),outcome,token.is_cancelled()).await;app.installations.active.lock().await.remove(&id);
                },Ok(None)|Err(_)=>break
            }
        }
        app.installations.driver.store(false,Ordering::Release);
        // Close admission-vs-idle race without holding a browser subscription or a long DB transaction.
        if app.tasks.access(|db|Ok(db.query_row("SELECT EXISTS(SELECT 1 FROM installation_jobs WHERE status='queued')",[],|r|r.get::<_,bool>(0))?)).await.unwrap_or(false){wake(&app);}
    });
}
async fn execute(app:&Shared,spec:&Value,tx:&tokio::sync::mpsc::Sender<Value>,cancel:&CancellationToken)->Result<()> {
    validate(spec)?;anyhow::ensure!(!cancel.is_cancelled(),"Установка остановлена");
    let kind=spec["kind"].as_str().unwrap();
    if kind=="browser"{let result=crate::web_tools::install(app,cancel).await?;crate::engine::emit(tx,json!({"type":"progress","message":result["message"]})).await?;return Ok(());}
    if kind=="gpu"{crate::engine::emit(tx,json!({"type":"progress","message":"Ожидание завершения работы с моделями…"})).await?;let _guard=tokio::select!{_=cancel.cancelled()=>anyhow::bail!("Установка остановлена"),guard=app.model_lifecycle.write()=>guard};return crate::compute::install_package_cancel(app,spec["backend"].as_str().unwrap(),tx,cancel).await;}
    let models=spec["models"].as_array().unwrap().iter().map(|m|m.as_str().unwrap().to_string()).collect::<Vec<_>>();
    // Changing a model's destination needs exclusive admission. No query uses half-updated routing.
    if let Some(directory)=spec["directory"].as_str().filter(|s|!s.is_empty()){
        let _guard=tokio::select!{_=cancel.cancelled()=>anyhow::bail!("Установка остановлена"),guard=app.model_lifecycle.write()=>guard};
        for model in &models{let installed=crate::models::installed(model,&crate::model_storage::tags(app).await["models"]);if installed&&!crate::model_storage::locations(app)?.contains_key(model)&&model!="whisper-small"{anyhow::ensure!(dunce::canonicalize(directory).ok().is_some_and(|requested|dunce::canonicalize(app.data.join("models")).ok()==Some(requested)),"Модель уже установлена в основной папке. Удалите её перед выбором другого диска");}crate::model_storage::set_location(app,model,directory,installed)?;}
        return crate::engine::pull_models_cancel(app,&models,tx,cancel).await;
    }
    let _guard=tokio::select!{_=cancel.cancelled()=>anyhow::bail!("Установка остановлена"),guard=app.model_lifecycle.read()=>guard};
    crate::engine::pull_models_cancel(app,&models,tx,cancel).await
}

#[cfg(test)]mod tests {
    use super::*;
    #[tokio::test]
    async fn queue_idempotency_pagination_cancellation_and_restart_keep_the_journal(){
        let root=std::env::temp_dir().join(format!("install-jobs-{}",uuid::Uuid::new_v4()));std::fs::create_dir_all(&root).unwrap();let store=crate::task_store::Store::open(&root).unwrap();
        let id=uuid::Uuid::new_v4().to_string();let spec=json!({"kind":"browser"});let first=store.installation_create(id.clone(),spec.clone()).await.unwrap();assert_eq!(first["status"],"queued");
        let equivalent_id=uuid::Uuid::new_v4().to_string();assert_eq!(store.installation_create(equivalent_id.clone(),spec.clone()).await.unwrap()["id"],id);assert_eq!(store.installation_get(equivalent_id.clone()).await.unwrap()["id"],id);assert_eq!(store.installation_events(equivalent_id.clone(),0).await.unwrap()["job"]["id"],id);assert!(store.installation_create(equivalent_id,json!({"kind":"gpu"})).await.is_err());
        assert_eq!(store.installation_create(id.clone(),spec.clone()).await.unwrap()["attempt"],1);assert_eq!(store.installation_create(uuid::Uuid::new_v4().to_string(),spec.clone()).await.unwrap()["id"],id);
        assert!(store.installation_create(id.clone(),json!({"kind":"models","models":[]})).await.is_err());
        for number in 0..130{store.installation_event(id.clone(),json!({"type":"progress","message":format!("step {number}")})).await.unwrap();}
        let mut after=0;let mut seen=0;loop{let page=store.installation_events(id.clone(),after).await.unwrap();assert!(page["items"].as_array().unwrap().len()<=64);seen+=page["items"].as_array().unwrap().len();after=page["next_after"].as_i64().unwrap();if page["has_more"]==false{break;}}assert_eq!(seen,130);
        assert_eq!(store.installation_claim().await.unwrap().unwrap()["id"],id);assert_eq!(store.installation_cancel(id.clone()).await.unwrap()["status"],"stopping");store.installation_finish(id.clone(),Err(anyhow::anyhow!("stopped")),true).await.unwrap();assert_eq!(store.installation_get(id.clone()).await.unwrap()["status"],"cancelled");
        assert_eq!(store.installation_retry(id.clone()).await.unwrap()["attempt"],2);assert_eq!(store.installation_claim().await.unwrap().unwrap()["id"],id);
        drop(store);let store=crate::task_store::Store::open(&root).unwrap();assert_eq!(store.installation_get(id.clone()).await.unwrap()["status"],"interrupted");assert_eq!(store.installation_events(id.clone(),0).await.unwrap()["items"].as_array().unwrap().len(),64);assert!(store.installation_claim().await.unwrap().is_none());
        store.installation_retry(id.clone()).await.unwrap();store.installation_claim().await.unwrap();store.installation_cancel(id.clone()).await.unwrap();store.installation_finish(id.clone(),Ok(()),true).await.unwrap();assert_eq!(store.installation_get(id.clone()).await.unwrap()["status"],"succeeded");assert!(store.installation_retry(id).await.is_err());
        for number in 0..121{let id=uuid::Uuid::new_v4().to_string();store.installation_create(id.clone(),json!({"kind":"browser","fixture":number})).await.unwrap();store.installation_cancel(id).await.unwrap();}
        let mut cursor=None;let mut ids=std::collections::HashSet::new();loop{let page=store.installation_list(cursor).await.unwrap();assert!(page["items"].as_array().unwrap().len()<=50);for row in page["items"].as_array().unwrap(){assert!(ids.insert(row["id"].as_str().unwrap().to_string()));}if page["next"].is_null(){break;}cursor=Some((page["next"]["before_created"].as_str().unwrap().to_string(),page["next"]["before_id"].as_str().unwrap().to_string()));}assert_eq!(ids.len(),122);
        drop(store);std::fs::remove_dir_all(root).unwrap();
    }
}
