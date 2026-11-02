//! Durable runs and ordered events. The UI is a subscriber, never the owner of a run.
use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::{
    path::Path,
    sync::{Arc, Mutex},
};

#[derive(Clone)]
pub struct Store(Arc<Mutex<Connection>>);
#[derive(Debug)]
pub struct MissingRun;
impl std::fmt::Display for MissingRun {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Запуск не найден")
    }
}
impl std::error::Error for MissingRun {}
#[derive(Debug)]
pub struct MissingResource(pub &'static str);
impl std::fmt::Display for MissingResource {fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result{f.write_str(self.0)}}
impl std::error::Error for MissingResource {}
impl Store {
    pub async fn workspace_events(&self, after: i64) -> Result<Vec<Value>> {
        self.access(move|conn|{
            let mut statement=conn.prepare("SELECT e.sequence,e.payload,e.run,r.conversation FROM events e JOIN runs r ON r.id=e.run WHERE sequence>?1 ORDER BY sequence LIMIT 256")?;
            let rows=statement.query_map([after],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?)))?;
            rows.map(|row|{let(sequence,payload,run,conversation)=row?;let mut value:Value=serde_json::from_str(&payload)?;crate::tools::public_event(&mut value);value["sequence"]=json!(sequence);value["run_id"]=json!(run);value["conversation_id"]=json!(conversation);Ok(value)}).collect()
        }).await
    }
    pub async fn dashboard(&self) -> Result<Value> {
        self.access(|conn|{
            let tx=conn.transaction()?;
            let cursor:i64=tx.query_row("SELECT COALESCE(MAX(sequence),0) FROM events",[],|r|r.get(0))?;
            let mut query=tx.prepare("SELECT id,conversation,status,input,created FROM runs WHERE status IN ('running','queued','awaiting_approval','stopping') ORDER BY created")?;
            let rows=query.query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?)))?;
            let mut runs=vec![];
            for row in rows {
                let(id,conversation,status,input,created)=row?;
                let state=crate::timeline::snapshot(&tx,&id,cursor)?;
                let input:Value=serde_json::from_str(&input)?;
                runs.push(json!({"id":id,"conversation":conversation,"status":status,"input":input,"created":created,"state":state}));
            }
            Ok(json!({"cursor":cursor,"runs":runs}))
        }).await
    }
    pub fn open(data: &Path) -> Result<Self> {
        let mut conn = Connection::open(data.join("tasks.sqlite3"))?;
        let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        anyhow::ensure!(
            version <= 9,
            "База задач создана более новой версией приложения; исходный файл сохранён"
        );
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "FULL")?;
        conn.pragma_update(None, "foreign_keys", true)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS runs (
            id TEXT PRIMARY KEY, conversation TEXT NOT NULL, input TEXT NOT NULL,
            status TEXT NOT NULL, created TEXT NOT NULL, updated TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS events (
            sequence INTEGER PRIMARY KEY AUTOINCREMENT, run TEXT NOT NULL,
            payload TEXT NOT NULL, FOREIGN KEY(run) REFERENCES runs(id));
            CREATE INDEX IF NOT EXISTS events_run ON events(run,sequence);
            CREATE TABLE IF NOT EXISTS deleted_conversations (id TEXT PRIMARY KEY);
            ",
        )?;
        crate::web_authority::install(&conn)?;
        crate::task_state::install(&mut conn,version<3)?;
        crate::artifacts::install(&mut conn)?;
        crate::timeline::install(&conn)?;
        crate::materials::install(&conn)?;
        crate::legacy::install(&conn)?;
        crate::source_index::install(&conn)?;
        conn.pragma_update(None,"user_version",9)?;
        conn.execute("UPDATE material_ingests SET status='interrupted',error=COALESCE(error,'Приложение завершилось до окончания подготовки') WHERE status='ingesting'",[])?;
        // A crash leaves effects with unknown outcome. Never replay mutations automatically.
        conn.execute(
            "UPDATE runs SET status='interrupted', updated=?1 WHERE status IN ('running','queued','awaiting_approval','stopping')",
            [crate::api::now()],
        )?;
        Ok(Self(Arc::new(Mutex::new(conn))))
    }
    pub async fn is_deleted(&self, id: String) -> Result<bool> {
        self.access(move |conn| Ok(conn.query_row("SELECT EXISTS(SELECT 1 FROM deleted_conversations WHERE id=?1)", [id], |r|r.get(0))?)).await
    }
    /// A persistent tombstone closes the JSON/SQLite crash window. It contains only an ID.
    pub async fn delete_conversation(&self, id: String) -> Result<()> {
        self.access(move |conn| {
            let tx=conn.transaction()?;
            tx.execute("INSERT OR IGNORE INTO deleted_conversations VALUES (?1)", [&id])?;
            tx.execute("DELETE FROM material_ingests WHERE run IN (SELECT id FROM runs WHERE conversation=?1)",[&id])?;
            tx.execute("DELETE FROM artifacts WHERE run IN (SELECT id FROM runs WHERE conversation=?1)",[&id])?;
            tx.execute("DELETE FROM artifact_bodies WHERE NOT EXISTS (SELECT 1 FROM artifacts WHERE artifacts.hash=artifact_bodies.hash) AND NOT EXISTS (SELECT 1 FROM material_ingests WHERE material_ingests.body=artifact_bodies.hash)",[])?;
            tx.execute("DELETE FROM task_plans WHERE task IN (SELECT id FROM work_tasks WHERE conversation=?1)",[&id])?;
            tx.execute("DELETE FROM task_requests WHERE task IN (SELECT id FROM work_tasks WHERE conversation=?1)",[&id])?;
            tx.execute("DELETE FROM task_runs WHERE task IN (SELECT id FROM work_tasks WHERE conversation=?1)",[&id])?;
            tx.execute("DELETE FROM work_tasks WHERE conversation=?1",[&id])?;
            tx.execute("DELETE FROM events WHERE run IN (SELECT id FROM runs WHERE conversation=?1)", [&id])?;
            tx.execute("DELETE FROM runs WHERE conversation=?1", [&id])?;
            tx.commit()?;
            Ok(())
        }).await
    }
    pub fn recover_deletions(&self, data: &Path) -> Result<()> {
        let conn=self.0.lock().map_err(|_|anyhow::anyhow!("Task store lock poisoned"))?;
        let mut query=conn.prepare("SELECT id FROM deleted_conversations")?;
        for id in query.query_map([],|r|r.get::<_,String>(0))? {
            let id=id?;
            uuid::Uuid::parse_str(&id)?;
            match std::fs::remove_file(data.join("chats").join(format!("{id}.json"))) {
                Ok(())=>{}, Err(e) if e.kind()==std::io::ErrorKind::NotFound=>{}, Err(e)=>return Err(e.into())
            }
        }
        Ok(())
    }
    pub(crate) async fn access<T: Send + 'static>(
        &self,
        f: impl FnOnce(&mut Connection) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let conn = self.0.clone();
        tokio::task::spawn_blocking(move || {
            let mut guard = conn
                .lock()
                .map_err(|_| anyhow::anyhow!("Task store lock poisoned"))?;
            f(&mut guard)
        })
        .await?
    }
    pub async fn begin(&self, id: String, conversation: String, input: Value) -> Result<bool> {
        self.begin_in_project(id,conversation,input,String::new()).await
    }
    pub async fn begin_in_project(&self, id: String, conversation: String, input: Value, project:String) -> Result<bool> {
        self.access(move |conn| {
            let tx = conn.transaction()?;
            anyhow::ensure!(!tx.query_row("SELECT EXISTS(SELECT 1 FROM deleted_conversations WHERE id=?1)", [&conversation], |r|r.get::<_,bool>(0))?, "Чат удалён");
            let old: Option<String> = tx
                .query_row("SELECT input FROM runs WHERE id=?1", [&id], |r| r.get(0))
                .optional()?;
            if let Some(old) = old {
                anyhow::ensure!(
                    serde_json::from_str::<Value>(&old)? == input,
                    "Идентификатор запроса уже использован для другого сообщения"
                );
                return Ok(false);
            }
            tx.execute(
                "INSERT INTO runs VALUES (?1,?2,?3,'running',?4,?4)",
                params![id, conversation, input.to_string(), crate::api::now()],
            )?;
            crate::task_state::associate(&tx,&id,&conversation,&input,&project)?;
            tx.commit()?;
            Ok(true)
        })
        .await
    }
    pub async fn append(&self, id: String, event: Value) -> Result<()> {
        self.access(move |conn| {
            let tx = conn.transaction()?;
            tx.execute("INSERT INTO events(run,payload) VALUES (?1,?2)", params![id,event.to_string()])?;
            let sequence=tx.last_insert_rowid();
            crate::timeline::record(&tx,&id,&event)?;
            crate::timeline::checkpoint(&tx,&id,sequence,&event)?;
            let status = if event["type"]=="done" {Some(event["status"].as_str().unwrap_or("completed"))} else if event["type"]=="error" {Some(if event["cancelled"]==true {"cancelled"} else {"failed"})} else if event["type"]=="approval" {Some("awaiting_approval")} else if event["type"]=="approval_closed" {Some("running")} else {None};
            if let Some(status) = status {
                tx.execute("UPDATE runs SET status=CASE WHEN status='failed' THEN status ELSE ?1 END, updated=?2 WHERE id=?3", params![status,crate::api::now(),id])?;
            }
            tx.commit()?;
            Ok(())
        }).await
    }
    pub async fn events(&self, id: String, after: i64) -> Result<Vec<Value>> {
        self.access(move |conn| {
            let mut statement=conn.prepare("SELECT sequence,payload FROM events WHERE run=?1 AND sequence>?2 ORDER BY sequence LIMIT 256")?;
            let rows=statement.query_map(params![id,after], |row| Ok((row.get::<_,i64>(0)?,row.get::<_,String>(1)?)))?;
            rows.map(|row| {let (sequence,payload)=row?;let mut value:Value=serde_json::from_str(&payload)?;crate::tools::public_event(&mut value);value["sequence"]=json!(sequence);Ok(value)}).collect()
        }).await
    }
    pub async fn get(&self, id: String) -> Result<Value> {
        self.access(move |conn| {
            let (conversation,status,input): (String,String,String) = conn.query_row("SELECT conversation,status,input FROM runs WHERE id=?1", [&id], |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?.ok_or(MissingRun)?;
            let sequence:i64=conn.query_row("SELECT COALESCE(MAX(sequence),0) FROM events WHERE run=?1",[&id],|r|r.get(0))?;
            Ok(json!({"id":id,"conversation":conversation,"status":status,"input":serde_json::from_str::<Value>(&input)?,"last_sequence":sequence}))
        }).await
    }
    pub async fn list(&self) -> Result<Value> {
        self.access(|conn| {
            let mut statement=conn.prepare("SELECT id,conversation,status,created,updated FROM runs ORDER BY created DESC LIMIT 100")?;
            let rows=statement.query_map([], |r|Ok(json!({"id":r.get::<_,String>(0)?,"conversation":r.get::<_,String>(1)?,"status":r.get::<_,String>(2)?,"created":r.get::<_,String>(3)?,"updated":r.get::<_,String>(4)?})))?;
            Ok(Value::Array(rows.collect::<rusqlite::Result<Vec<_>>>()?))
        }).await
    }
    pub async fn task_for_run(&self, run:String)->Result<Option<Value>> {
        self.access(move|conn|{
            let id:Option<String>=conn.query_row("SELECT task FROM task_runs WHERE run=?1",[run],|r|r.get(0)).optional()?;
            id.map(|id|crate::task_state::snapshot(conn,&id,0)).transpose()
        }).await
    }
    pub async fn permission_task(&self,run:String)->Result<Option<(String,String)>>{
        self.access(move|conn|Ok(conn.query_row("SELECT t.task,substr(w.goal,1,180) FROM task_runs t JOIN work_tasks w ON w.id=t.task WHERE t.run=?1 AND w.state='open'",[run],|row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?))).optional()?)).await
    }
    pub async fn permission_task_titles(&self,ids:Vec<String>)->Result<serde_json::Map<String,Value>>{
        self.access(move|conn|{let mut query=conn.prepare("SELECT id,substr(goal,1,180) FROM work_tasks WHERE id IN (SELECT value FROM json_each(?1))")?;let rows=query.query_map([serde_json::to_string(&ids)?],|row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;Ok(rows.into_iter().map(|(id,title)|(id,json!(title))).collect())}).await
    }
    pub async fn work_task_ids(&self,conversation:String)->Result<Vec<String>>{
        self.access(move|conn|{let mut query=conn.prepare("SELECT id FROM work_tasks WHERE conversation=?1")?;let ids=query.query_map([conversation],|row|row.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;Ok(ids)}).await
    }
    pub async fn task(&self,id:String,after:i64)->Result<Value>{self.access(move|conn|crate::task_state::snapshot(conn,&id,after)).await}
    pub async fn task_request(&self,id:String,sequence:i64,offset:usize)->Result<Value>{
        self.task_request_slice(id,sequence,offset,16000).await
    }
    pub async fn task_request_slice(&self,id:String,sequence:i64,offset:usize,max_characters:usize)->Result<Value>{
        anyhow::ensure!((1..=16000).contains(&max_characters),"Размер страницы запроса от 1 до 16000 символов");
        self.access(move|conn|{
            let (text,total):(String,i64)=conn.query_row("SELECT substr(content,?3+1,?4),length(content) FROM task_requests WHERE task=?1 AND sequence=?2",params![id,sequence,i64::try_from(offset)?,i64::try_from(max_characters)?],|r|Ok((r.get(0)?,r.get(1)?))).optional()?.ok_or(MissingResource("Запрос задачи не найден"))?;
            let next=offset.saturating_add(text.chars().count());
            Ok(json!({"sequence":sequence,"text":text,"offset":offset,"next_offset":next,"characters":total,"has_more":(next as u64)<total as u64}))
        }).await
    }
    pub async fn task_list(&self,conversation:String)->Result<Value>{
        self.access(move|conn|{
            let mut query=conn.prepare("SELECT id,goal,project,state,updated FROM work_tasks WHERE conversation=?1 ORDER BY created DESC,id DESC")?;
            let rows=query.query_map([conversation],|r|Ok(json!({"id":r.get::<_,String>(0)?,"goal_preview":crate::tools::truncate(&r.get::<_,String>(1)?,300),"project":r.get::<_,String>(2)?,"state":r.get::<_,String>(3)?,"updated":r.get::<_,String>(4)?})))?;
            Ok(json!(rows.collect::<rusqlite::Result<Vec<_>>>()?))
        }).await
    }
    pub async fn save_task_plan(&self,run:String,plan:Value)->Result<()> {
        self.access(move|conn|{
            let task:Option<String>=conn.query_row("SELECT task FROM task_runs WHERE run=?1",[&run],|r|r.get(0)).optional()?;
            if let Some(task)=task {
                conn.execute("INSERT INTO task_plans VALUES (?1,?2,?3,1) ON CONFLICT(task) DO UPDATE SET plan=excluded.plan,source_run=excluded.source_run,revision=task_plans.revision+1",params![task,plan.to_string(),run])?;
            }
            Ok(())
        }).await
    }
}

#[derive(Default,serde::Serialize,serde::Deserialize)]
#[serde(default)]
pub(crate) struct Fold {
    text: String,
    thinking: String,
    text_offset:usize,
    thinking_offset:usize,
    tools_total:u64,
    notice: String,
    accepted: Value,
    tools: Vec<Value>,
    approvals: Vec<Value>,
}
impl Fold {
    pub(crate) fn consume(&mut self, event: &Value) {
        let text = event["text"].as_str().unwrap_or("");
        match event["type"].as_str().unwrap_or("") {
            "start"=>self.accepted=event["accepted"].clone(),
            "token"=>self.text.push_str(text),
            "replace_content"=>{self.text=text.into();self.text_offset=0;},
            "thinking"=>self.thinking.push_str(text),
            "notice"|"progress"=>self.notice=event["message"].as_str().unwrap_or("").into(),
            "tool_start"=>self.tools.push(json!({"id":event["id"],"name":event["name"],"arguments":event["arguments"],"arguments_artifact":event["arguments_artifact"],"status":"running"})),
            "tool_result"=>{if let Some(tool)=self.tools.iter_mut().find(|t|t["id"]==event["tool"]["id"]){*tool=event["tool"].clone();}else{self.tools_total+=1;self.tools.push(event["tool"].clone());}},
            "approval"=>{self.approvals.push(event.clone());if let Some(tool)=self.tools.iter_mut().find(|t|t["id"]==event["tool_id"]){tool["status"]=json!("awaiting_approval");}},
            "approval_closed"=>{self.approvals.retain(|a|a["id"]!=event["id"]);if let Some(tool)=self.tools.iter_mut().find(|t|t["id"]==event["tool_id"]){tool["status"]=event["status"].clone();}},
            "extracted"=>self.tools.push(json!({"id":event["id"],"name":event["name"],"arguments":{"file":event["name"]},"result":text,"material_artifact":event["material_artifact"],"status":"succeeded"})),
            _=>{}
        }
        if matches!(event["type"].as_str(),Some("tool_start"|"extracted")){self.tools_total+=1;}
        crate::timeline::window(&mut self.tools);
        self.text_offset+=crate::timeline::tail(&mut self.text,65536);
        self.thinking_offset+=crate::timeline::tail(&mut self.thinking,16384);
    }
    pub(crate) fn value(self) -> Value {
        json!({"acceptedOptions":self.accepted,"streamText":self.text,"streamThinking":self.thinking,"streamTools":self.tools,"approvals":self.approvals,"notice":self.notice,"textOffset":self.text_offset,"thinkingOffset":self.thinking_offset,"toolsTotal":self.tools_total})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn schema_two_runs_backfill_without_losing_original_goal() {
        let dir=std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        std::fs::create_dir(&dir).unwrap();
        let conn=Connection::open(dir.join("tasks.sqlite3")).unwrap();
        conn.execute_batch("CREATE TABLE runs(id TEXT PRIMARY KEY,conversation TEXT NOT NULL,input TEXT NOT NULL,status TEXT NOT NULL,created TEXT NOT NULL,updated TEXT NOT NULL); PRAGMA user_version=2;").unwrap();
        conn.execute("INSERT INTO runs VALUES ('old','chat',?1,'completed','2026-01-01','2026-01-01')",[json!({"mode":"agent","content":"ORIGINAL_REQUIREMENTS"}).to_string()]).unwrap();
        drop(conn);
        let store=Store::open(&dir).unwrap();
        let old=store.task_for_run("old".into()).await.unwrap().unwrap();
        store.begin_in_project("next".into(),"chat".into(),json!({"mode":"agent","content":"continue"}),"project".into()).await.unwrap();
        let next=store.task_for_run("next".into()).await.unwrap().unwrap();
        assert_eq!(next["id"],old["id"]);
        assert_eq!(next["goal_preview"],"ORIGINAL_REQUIREMENTS");
        assert_eq!(next["project"],"project");
        drop(store);std::fs::remove_dir_all(dir).unwrap();
    }
    #[tokio::test]
    async fn task_goal_plan_and_full_requests_survive_runs_restart_and_pagination() {
        let dir=std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        std::fs::create_dir(&dir).unwrap();
        let store=Store::open(&dir).unwrap();
        let original=format!("Build the project with all constraints {} END_OF_GOAL", "требование 🦀 ".repeat(2000));
        store.begin_in_project("first".into(),"chat".into(),json!({"mode":"agent","content":original,"attachments":["material"]}),"project".into()).await.unwrap();
        let task=store.task_for_run("first".into()).await.unwrap().unwrap();
        let id=task["id"].as_str().unwrap().to_string();
        store.save_task_plan("first".into(),json!([{"title":"Tests","status":"pending"}])).await.unwrap();
        drop(store);
        let store=Store::open(&dir).unwrap();
        store.begin_in_project("second".into(),"chat".into(),json!({"mode":"agent","content":"продолжай работу"}),"project".into()).await.unwrap();
        let continued=store.task_for_run("second".into()).await.unwrap().unwrap();
        assert_eq!(continued["id"],id);
        assert_eq!(continued["plan"]["steps"][0]["status"],"pending");
        assert_eq!(continued["completion_verified"],false);
        assert_eq!(continued["runs"][1]["status"],"interrupted");
        let sequence=continued["requests"][0]["sequence"].as_i64().unwrap();
        let mut offset=0;let mut text=String::new();
        loop {let page=store.task_request(id.clone(),sequence,offset).await.unwrap();text.push_str(page["text"].as_str().unwrap());if page["has_more"]!=true {break;}offset=page["next_offset"].as_u64().unwrap() as usize;}
        assert_eq!(text,original);
        assert!(store.begin_in_project("wrong-project".into(),"chat".into(),json!({"mode":"agent","content":"continue"}),"other".into()).await.is_err());
        assert!(store.get("wrong-project".into()).await.is_err());
        store.begin_in_project("new".into(),"chat".into(),json!({"mode":"agent","content":"Another goal","new_task":true}),"other".into()).await.unwrap();
        assert_ne!(store.task_for_run("new".into()).await.unwrap().unwrap()["id"],id);
        store.delete_conversation("chat".into()).await.unwrap();
        assert!(store.task(id,0).await.is_err());
        assert!(store.task_list("chat".into()).await.unwrap().as_array().unwrap().is_empty());
        drop(store);std::fs::remove_dir_all(dir).unwrap();
    }
    #[tokio::test]
    async fn deletion_removes_journal_and_recovers_the_json_crash_window() {
        let dir=std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        std::fs::create_dir_all(dir.join("chats")).unwrap();
        let chat=uuid::Uuid::new_v4().to_string();
        let store=Store::open(&dir).unwrap();
        store.begin("r".into(),chat.clone(),json!({"text":"private"})).await.unwrap();
        store.append("r".into(),json!({"type":"token","text":"private"})).await.unwrap();
        store.begin("other".into(),"other-chat".into(),json!({"text":"keep"})).await.unwrap();
        let path=dir.join("chats").join(format!("{chat}.json"));
        std::fs::write(&path,b"legacy JSON remaining at crash").unwrap();
        store.delete_conversation(chat.clone()).await.unwrap();
        assert!(store.get("r".into()).await.is_err());
        assert!(store.events("r".into(),0).await.unwrap().is_empty());
        assert!(store.begin("retry".into(),chat.clone(),json!({})).await.is_err());
        assert!(store.append("r".into(),json!({"type":"token"})).await.is_err());
        drop(store);
        let store=Store::open(&dir).unwrap();
        store.recover_deletions(&dir).unwrap();
        assert!(!path.exists());
        assert!(store.is_deleted(chat).await.unwrap());
        assert!(store.get("other".into()).await.is_ok());
        assert_eq!(store.workspace_events(0).await.unwrap().len(),0);
        drop(store);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[tokio::test]
    async fn replay_is_ordered_idempotent_and_crash_requires_review() {
        let dir = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        std::fs::create_dir(&dir).unwrap();
        let store = Store::open(&dir).unwrap();
        assert!(store
            .begin("r".into(), "c".into(), json!({"text":"task"}))
            .await
            .unwrap());
        assert!(!store
            .begin("r".into(), "c".into(), json!({"text":"task"}))
            .await
            .unwrap());
        assert!(store
            .begin("r".into(), "c".into(), json!({"text":"different"}))
            .await
            .is_err());
        store
            .append("r".into(), json!({"type":"token","text":"partial"}))
            .await
            .unwrap();
        let events = store.events("r".into(), 0).await.unwrap();
        assert_eq!(events[0]["text"], "partial");
        let dashboard = store.dashboard().await.unwrap();
        assert_eq!(dashboard["cursor"], events[0]["sequence"]);
        assert_eq!(dashboard["runs"][0]["state"]["streamText"], "partial");
        let shared = store.workspace_events(0).await.unwrap();
        assert_eq!(shared[0]["run_id"], "r");
        assert_eq!(shared[0]["conversation_id"], "c");
        assert!(store
            .events("r".into(), events[0]["sequence"].as_i64().unwrap())
            .await
            .unwrap()
            .is_empty());
        drop(store);
        let store = Store::open(&dir).unwrap();
        assert_eq!(
            store.get("r".into()).await.unwrap()["status"],
            "interrupted"
        );
        assert_eq!(store.events("r".into(), 0).await.unwrap().len(), 1);
        drop(store);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
