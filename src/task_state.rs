//! A user's goal survives individual inference runs and context windows.
use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde_json::{json, Value};

pub fn install(conn: &mut Connection, backfill:bool) -> Result<()> {
    let tx=conn.transaction()?;
    tx.execute_batch("CREATE TABLE IF NOT EXISTS work_tasks (
        id TEXT PRIMARY KEY, conversation TEXT NOT NULL, project TEXT NOT NULL,
        goal TEXT NOT NULL, state TEXT NOT NULL, created TEXT NOT NULL, updated TEXT NOT NULL);
        CREATE INDEX IF NOT EXISTS tasks_conversation ON work_tasks(conversation,created);
        CREATE TABLE IF NOT EXISTS task_runs (run TEXT PRIMARY KEY REFERENCES runs(id), task TEXT NOT NULL REFERENCES work_tasks(id));
        CREATE TABLE IF NOT EXISTS task_requests (sequence INTEGER PRIMARY KEY AUTOINCREMENT,
        task TEXT NOT NULL REFERENCES work_tasks(id), run TEXT NOT NULL UNIQUE REFERENCES runs(id), content TEXT NOT NULL, attachments TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS task_plans (task TEXT PRIMARY KEY REFERENCES work_tasks(id), plan TEXT NOT NULL, source_run TEXT NOT NULL REFERENCES runs(id), revision INTEGER NOT NULL);")?;
    // Backfill old accepted runs once, in order. Their original inputs remain intact.
    if backfill {
        let mut query=tx.prepare("SELECT id,conversation,input FROM runs WHERE id NOT IN (SELECT run FROM task_runs) ORDER BY created,id")?;
        let rows=query.query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?)))?;
        for row in rows {let(run,conversation,input)=row?;associate(&tx,&run,&conversation,&serde_json::from_str(&input)?,"")?;}
    }
    tx.commit()?;
    Ok(())
}
pub fn associate(tx:&Transaction<'_>,run:&str,conversation:&str,input:&Value,project:&str)->Result<()> {
    crate::web_authority::record(tx,run,input["content"].as_str().unwrap_or(""))?;
    if !matches!(input["mode"].as_str(),Some("agent"|"team"|"search"|"chat")) {return Ok(());}
    if input["mode"]!="search"&&crate::agent::reply_only(input["content"].as_str().unwrap_or("")) {return Ok(());}
    anyhow::ensure!(input["new_task"]!=true || input["task_id"].is_null(),"Выберите существующую или новую задачу");
    let existing:Option<String>=if let Some(id)=input["task_id"].as_str() {
        let valid:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM work_tasks WHERE id=?1 AND conversation=?2)",params![id,conversation],|r|r.get(0))?;
        anyhow::ensure!(valid,"Задача не принадлежит этому чату");
        Some(id.into())
    } else if input["new_task"]==true {None} else {
        tx.query_row("SELECT id FROM work_tasks WHERE conversation=?1 AND state='open' ORDER BY updated DESC,id DESC LIMIT 1",params![conversation],|r|r.get(0)).optional()?
    };
    let task=existing.unwrap_or_else(||uuid::Uuid::new_v4().to_string());
    let old_project:Option<String>=tx.query_row("SELECT project FROM work_tasks WHERE id=?1",[&task],|r|r.get(0)).optional()?;
    anyhow::ensure!(matches!(input["mode"].as_str(),Some("chat"|"search")) || old_project.is_none_or(|old|old.is_empty()||project.is_empty()||old==project),"Рабочая папка отличается от сохранённой задачи. Верните её в настройках или начните новую задачу в чате");
    let now=crate::api::now();
    tx.execute("INSERT OR IGNORE INTO work_tasks VALUES (?1,?2,?3,?4,'open',?5,?5)",params![task,conversation,project,input["content"].as_str().unwrap_or(""),now])?;
    tx.execute("UPDATE work_tasks SET project=CASE WHEN project='' THEN ?1 ELSE project END,updated=?2 WHERE id=?3",params![project,now,task])?;
    tx.execute("INSERT OR IGNORE INTO task_runs VALUES (?1,?2)",params![run,task])?;
    tx.execute("INSERT OR IGNORE INTO task_requests(task,run,content,attachments) VALUES (?1,?2,?3,?4)",params![task,run,input["content"].as_str().unwrap_or(""),input["attachments"].as_array().map(|a|json!(a)).unwrap_or(json!([])).to_string()])?;
    Ok(())
}
pub fn snapshot(conn:&Connection,task:&str,after:i64)->Result<Value> {
    let (conversation,project,state):(String,String,String)=conn.query_row("SELECT conversation,project,state FROM work_tasks WHERE id=?1",[task],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?.ok_or(crate::task_store::MissingResource("Рабочая задача не найдена"))?;
    let goal=crate::task_requirements::goal_preview(conn,task,8000)?;
    let mut query=conn.prepare("SELECT sequence,run,attachments FROM task_requests WHERE task=?1 AND sequence>?2 ORDER BY sequence LIMIT 32")?;
    let mut requests=query.query_map(params![task,after],|r|Ok(json!({"sequence":r.get::<_,i64>(0)?,"run":r.get::<_,String>(1)?,"attachments":serde_json::from_str::<Value>(&r.get::<_,String>(2)?).unwrap_or(json!([]))})))?.collect::<rusqlite::Result<Vec<_>>>()?;drop(query);
    for row in &mut requests{let source=crate::task_requirements::request_page(conn,task,row["sequence"].as_i64().unwrap(),0,400)?;row["preview"]=source["text"].clone();row["characters"]=source["characters"].clone();row["sha256"]=source["sha256"].clone();}
    let next=requests.last().and_then(|r|r["sequence"].as_i64()).unwrap_or(after);
    let more:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM task_requests WHERE task=?1 AND sequence>?2)",params![task,next],|r|r.get(0))?;
    let plan:Option<(String,String,i64)>=conn.query_row("SELECT plan,source_run,revision FROM task_plans WHERE task=?1",[task],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
    let plan=match plan {Some((value,run,revision))=>json!({"steps":serde_json::from_str::<Value>(&value)?,"source_run":run,"revision":revision,"assessment":"agent_reported"}),None=>Value::Null};
    let mut runs=conn.prepare("SELECT r.id,r.status,r.created FROM runs r JOIN task_runs t ON r.id=t.run WHERE t.task=?1 ORDER BY r.created DESC LIMIT 32")?;
    let runs=runs.query_map([task],|r|Ok(json!({"id":r.get::<_,String>(0)?,"status":r.get::<_,String>(1)?,"created":r.get::<_,String>(2)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
    let legacy:Option<Value>=conn.query_row("SELECT request_count,original_workspace,created FROM legacy_task_imports WHERE task=?1",[task],|row|Ok(json!({"request_count":row.get::<_,i64>(0)?,"original_workspace":row.get::<_,String>(1)?,"imported":row.get::<_,String>(2)?,"origin":"legacy_json","grouping_verified":false,"effects_verified":false,"note":"Historical user requests, not verified execution receipts. Reconcile original grouping/workspace and actual effects before continuing. The current selected workspace governs project permissions."}))).optional()?;
    Ok(json!({"id":task,"conversation":conversation,"project":project,"goal_preview":goal["text"],"goal_characters":goal["characters"],"goal_sha256":goal["sha256"],"state":state,"requests":requests,"next_after":next,"has_more":more,"plan":plan,"runs":runs,"completion_verified":false,"requirements":crate::task_requirements::summary_task(conn,task)?,"legacy_context":legacy}))
}
