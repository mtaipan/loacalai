//! Bounded presentation windows over a durable, unbounded run journal.
use anyhow::Result;
use rusqlite::{params,Connection,OptionalExtension};
use serde_json::{json,Value};
pub const TOOL_WINDOW:usize=48;
pub fn window(tools:&mut Vec<Value>){if tools.len()>TOOL_WINDOW{tools.drain(..tools.len()-TOOL_WINDOW);}}
pub fn tail(text:&mut String,limit:usize)->usize{
    if text.len()<=limit{return 0;}
    let mut cut=text.len()-limit;while !text.is_char_boundary(cut){cut+=1;}
    let removed=text[..cut].chars().count();text.drain(..cut);removed
}
pub fn install(conn:&Connection)->Result<()>{
    conn.execute_batch("CREATE TABLE IF NOT EXISTS run_tools(sequence INTEGER PRIMARY KEY AUTOINCREMENT,run TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE,tool_id TEXT NOT NULL,payload TEXT NOT NULL,UNIQUE(run,tool_id));
    CREATE INDEX IF NOT EXISTS events_run_kind ON events(run,json_extract(payload,'$.type'),sequence);
    CREATE INDEX IF NOT EXISTS run_tools_page ON run_tools(run,sequence);
    CREATE TABLE IF NOT EXISTS run_views(run TEXT PRIMARY KEY REFERENCES runs(id) ON DELETE CASCADE,sequence INTEGER NOT NULL,state TEXT NOT NULL);")?;Ok(())
}
pub fn record(conn:&Connection,run:&str,event:&Value)->Result<()>{
    let mut record=match event["type"].as_str().unwrap_or(""){
        "tool_start"=>json!({"id":event["id"],"name":event["name"],"arguments":event["arguments"],"arguments_artifact":event["arguments_artifact"],"status":"running"}),
        "tool_result"=>event["tool"].clone(),
        _=>return Ok(())
    };
    crate::tools::public_record(&mut record);
    if let Some(id)=record["id"].as_str(){
        conn.execute("INSERT INTO run_tools(run,tool_id,payload) VALUES (?1,?2,?3) ON CONFLICT(run,tool_id) DO UPDATE SET payload=excluded.payload WHERE ?4 OR json_extract(run_tools.payload,'$.status')='running'",params![run,id,record.to_string(),event["type"]=="tool_result"])?;
        if let Err(error)=crate::task_requirements::observe(conn,run,&record){eprintln!("Task mutation index: {error:#}");}
    }Ok(())
}
pub fn checkpoint(conn:&Connection,run:&str,sequence:i64,event:&Value)->Result<()>{
    let old:Option<(i64,String)>=conn.query_row("SELECT sequence,state FROM run_views WHERE run=?1",[run],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    let after=old.as_ref().map_or(0,|row|row.0);
    if sequence-after<128&&matches!(event["type"].as_str(),Some("token"|"thinking")){return Ok(());}
    let state=fold(conn,run,sequence,old)?;
    conn.execute("INSERT INTO run_views VALUES (?1,?2,?3) ON CONFLICT(run) DO UPDATE SET sequence=excluded.sequence,state=excluded.state",params![run,sequence,serde_json::to_string(&state)?])?;Ok(())
}
fn fold(conn:&Connection,run:&str,cursor:i64,old:Option<(i64,String)>)->Result<crate::task_store::Fold>{
    let(after,mut state)=match old{Some((seq,text))=>(seq,serde_json::from_str(&text)?),None=>(0,crate::task_store::Fold::default())};
    let mut statement=conn.prepare("SELECT payload FROM events WHERE run=?1 AND sequence>?2 AND sequence<=?3 ORDER BY sequence")?;
    for row in statement.query_map(params![run,after,cursor],|r|r.get::<_,String>(0))?{let mut value:Value=serde_json::from_str(&row?)?;crate::tools::public_event(&mut value);state.consume(&value);}
    Ok(state)
}
pub fn snapshot(conn:&Connection,run:&str,cursor:i64)->Result<Value>{
    let old=conn.query_row("SELECT sequence,state FROM run_views WHERE run=?1",[run],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    Ok(fold(conn,run,cursor,old)?.value())
}
impl crate::task_store::Store{
    pub async fn persist_tool(&self,run:String,tool:Value)->Result<()>{self.access(move|conn|record(conn,&run,&json!({"type":"tool_result","tool":tool}))).await}
    pub async fn text_page(&self,run:String,kind:String,before:Option<i64>)->Result<Value>{
        self.access(move|conn|{
            anyhow::ensure!(kind=="thinking"||kind=="content","Неизвестный раздел журнала");
            anyhow::ensure!(before.is_none_or(|value|value>0),"Курсор должен быть положительным");
            let exists:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM runs WHERE id=?1)",[&run],|r|r.get(0))?;
            if !exists{return Err(crate::task_store::MissingRun.into());}
            let mut query=conn.prepare("SELECT sequence,payload FROM events WHERE run=?1 AND (?2 IS NULL OR sequence<?2) AND json_extract(payload,'$.type') IN (?3,?4) ORDER BY sequence DESC LIMIT 33")?;
            let mut records=query.query_map(params![run,before,if kind=="thinking"{"thinking"}else{"token"},if kind=="thinking"{"thinking"}else{"replace_content"}],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
            let more=records.len()>32;records.truncate(32);let next=records.last().map(|row|row.0);records.reverse();
            let items=records.into_iter().map(|(sequence,text)|{let event:Value=serde_json::from_str(&text)?;Ok(json!({"sequence":sequence,"type":event["type"],"text":event["text"]}))}).collect::<Result<Vec<_>>>()?;
            Ok(json!({"run":run,"kind":kind,"items":items,"next_before":if more{next}else{None}}))
        }).await
    }
    pub async fn tool_page(&self,run:String,before:Option<i64>)->Result<Value>{
        self.access(move|conn|{
            anyhow::ensure!(before.is_none_or(|value|value>0),"Курсор должен быть положительным");
            let exists:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM runs WHERE id=?1)",[&run],|r|r.get(0))?;
            if !exists{return Err(crate::task_store::MissingRun.into());}
            let mut query=conn.prepare("SELECT sequence,payload FROM run_tools WHERE run=?1 AND (?2 IS NULL OR sequence<?2) ORDER BY sequence DESC LIMIT 33")?;
            let mut records=query.query_map(params![run,before],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
            let more=records.len()>32;records.truncate(32);let next=records.last().map(|row|row.0);records.reverse();
            let tools=records.into_iter().map(|(sequence,text)|{let mut tool:Value=serde_json::from_str(&text)?;crate::tools::public_record(&mut tool);Ok(json!({"sequence":sequence,"tool":tool}))}).collect::<Result<Vec<_>>>()?;
            Ok(json!({"run":run,"tools":tools,"next_before":if more{next}else{None}}))
        }).await
    }
}
pub fn mutation_key(tool:&Value)->Option<String>{
    use sha2::{Digest,Sha256};
    let name=tool["name"].as_str()?;
    if !["write_file","edit_file","copy_file","move_file","delete_file","translate_file"].contains(&name){return None;}
    if let Some(hash)=tool["arguments_artifact"]["sha256"].as_str(){return Some(format!("{name}:{hash}"));}
    let args=tool["arguments"].as_object()?;
    if args.is_empty(){return None;}
    Some(format!("{name}:{:x}",Sha256::digest(tool["arguments"].to_string().as_bytes())))
}
/// Preserve every unresolved operation intent, not merely the final mutation.
/// Only a receipt for the same exact intent can supersede an earlier attempt.
pub fn fold_mutation(receipts:&mut Vec<Value>,tool:&Value){
    if !["write_file","edit_file","copy_file","move_file","delete_file","translate_file"].contains(&tool["name"].as_str().unwrap_or("")){return;}
    let key=mutation_key(tool);
    receipts.retain(|old|old["status"]!="succeeded"&&!(key.is_some()&&mutation_key(old)==key));
    receipts.push(tool.clone());
}
#[derive(Default)]
pub struct Evidence{
    pub actions:u64,pub succeeded:u64,pub failed:u64,pub stopped:u64,
    pub opened:usize,pub read:usize,pub browser_actions:usize,
    pub mutation:Vec<Value>,pub file:Vec<Value>,pub mutation_partial:bool,
}
impl Evidence{
    pub fn observe(&mut self,tool:&Value){
        if !tool["status"].is_string(){return;}
        self.actions+=1;
        match tool["status"].as_str().unwrap_or(""){"succeeded"=>self.succeeded+=1,"failed"=>self.failed+=1,_=>self.stopped+=1}
        let name=tool["name"].as_str().unwrap_or("");
        if tool["status"]=="succeeded"{match name{"browser_open"=>self.opened+=1,"browser_read"|"fetch_url"=>self.read+=1,"browser_action"=>self.browser_actions+=1,_=>{}}}
        fold_mutation(&mut self.mutation,tool);
        if self.mutation.len()>32{self.mutation.drain(..self.mutation.len()-32);self.mutation_partial=true;}
        if self.mutation_partial{if let Some(first)=self.mutation.first_mut(){first["mutation_history_partial"]=json!(true);}}
        if ["read_file","write_file","edit_file","copy_file","move_file","delete_file"].contains(&name){self.file=vec![tool.clone()];}
    }
    pub fn report(&self,recent:&[Value],run:&str)->Value{
        let mut report=crate::agent::execution_report(recent);
        report["actions"]=json!(self.actions);report["succeeded"]=json!(self.succeeded);report["failed"]=json!(self.failed);report["stopped"]=json!(self.stopped);
        report["details_partial"]=json!(self.actions>recent.len() as u64);report["mutation_details_partial"]=json!(self.mutation_partial);report["run_id"]=json!(run);report
    }
}
#[cfg(test)]
mod tests{
    use super::*;
    #[tokio::test]
    async fn bounded_views_and_complete_cursor_journal_survive_checkpoints(){
        let root=std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());std::fs::create_dir_all(&root).unwrap();
        let store=crate::task_store::Store::open(&root).unwrap();store.begin("run".into(),"chat".into(),json!({})).await.unwrap();
        let accepted=json!({"mode":"agent","model":"accepted-model","web":"off","thinking":"auto","search":{"depth":"deep"},"settings_revision":7});
        store.append("run".into(),json!({"type":"start","accepted":accepted})).await.unwrap();
        let mut evidence=Evidence::default();let mut recent=vec![];
        for i in 0..120{
            store.append("run".into(),json!({"type":"tool_start","id":format!("t{i}"),"name":"read_file","arguments":{"path":format!("{i}.txt")}})).await.unwrap();
            let tool=json!({"id":format!("t{i}"),"name":"read_file","status":"succeeded","arguments":{},"result":"{}"});evidence.observe(&tool);recent.push(tool.clone());window(&mut recent);
            store.persist_tool("run".into(),tool.clone()).await.unwrap();
            store.append("run".into(),json!({"type":"tool_result","tool":tool})).await.unwrap();
            store.append("run".into(),json!({"type":"thinking","text":"reason 🦀 ".repeat(80)})).await.unwrap();
        }
        // A late replay of start must never replace a durably stored outcome.
        store.append("run".into(),json!({"type":"tool_start","id":"t0","name":"read_file","arguments":{}})).await.unwrap();
        let dashboard=store.dashboard().await.unwrap();let state=&dashboard["runs"][0]["state"];
        assert!(state["streamTools"].as_array().unwrap().len()<=48);assert!(state["streamThinking"].as_str().unwrap().len()<=16384);assert!(state["thinkingOffset"].as_u64().unwrap()>0);
        assert_eq!(state["acceptedOptions"],accepted);
        assert_eq!(evidence.report(&recent,"run")["actions"],120);
        let mut before=None;let mut seen=std::collections::HashSet::new();
        loop{let page=store.tool_page("run".into(),before).await.unwrap();for item in page["tools"].as_array().unwrap(){assert_eq!(item["tool"]["status"],"succeeded");assert!(seen.insert(item["tool"]["id"].as_str().unwrap().to_string()));}before=page["next_before"].as_i64();if before.is_none(){break;}}
        assert_eq!(seen.len(),120);let mut before=None;let mut total=0;
        loop{let page=store.text_page("run".into(),"thinking".into(),before).await.unwrap();total+=page["items"].as_array().unwrap().len();before=page["next_before"].as_i64();if before.is_none(){break;}}
        assert_eq!(total,120);store.delete_conversation("chat".into()).await.unwrap();assert!(store.tool_page("run".into(),None).await.is_err());
        drop(store);std::fs::remove_dir_all(root).unwrap();
    }
}
