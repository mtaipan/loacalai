//! Provenance, actual check receipts and unresolved attempts are distinct facts.
//! Model assessments never become semantic verification or user dispositions.
use anyhow::{Context,Result};
use rusqlite::{params,Connection,OptionalExtension};
use serde::Deserialize;
use serde_json::{json,Value};
use sha2::{Digest,Sha256};

pub fn install(conn:&Connection)->Result<()>{
    conn.execute_batch("CREATE TABLE IF NOT EXISTS task_text_sources(id TEXT PRIMARY KEY,task TEXT NOT NULL REFERENCES work_tasks(id) ON DELETE CASCADE,request INTEGER REFERENCES task_requests(sequence) ON DELETE CASCADE,bytes INTEGER NOT NULL,characters INTEGER NOT NULL,sha256 TEXT NOT NULL);
    CREATE TABLE IF NOT EXISTS task_text_offsets(source TEXT NOT NULL REFERENCES task_text_sources(id) ON DELETE CASCADE,character INTEGER NOT NULL,byte INTEGER NOT NULL,PRIMARY KEY(source,character));
    CREATE TABLE IF NOT EXISTS task_requirements(
      sequence INTEGER PRIMARY KEY AUTOINCREMENT,id TEXT NOT NULL UNIQUE,task TEXT NOT NULL REFERENCES work_tasks(id) ON DELETE CASCADE,
      request INTEGER NOT NULL REFERENCES task_requests(sequence) ON DELETE CASCADE,start INTEGER NOT NULL,characters INTEGER NOT NULL,source_hash TEXT NOT NULL,
      title TEXT NOT NULL,criterion TEXT NOT NULL,assessment TEXT NOT NULL,note TEXT NOT NULL,revision INTEGER NOT NULL,
      disposition TEXT NOT NULL DEFAULT 'active',user_decision TEXT,created_run TEXT NOT NULL,updated_run TEXT NOT NULL,created TEXT NOT NULL,updated TEXT NOT NULL,
      UNIQUE(task,request,start,characters));
    CREATE INDEX IF NOT EXISTS requirements_page ON task_requirements(task,sequence);
    CREATE TABLE IF NOT EXISTS requirement_source_reviews(task TEXT NOT NULL REFERENCES work_tasks(id) ON DELETE CASCADE,request INTEGER NOT NULL REFERENCES task_requests(sequence) ON DELETE CASCADE,source_hash TEXT NOT NULL,source_characters INTEGER NOT NULL,note TEXT NOT NULL,run TEXT NOT NULL,PRIMARY KEY(task,request));
    CREATE TABLE IF NOT EXISTS requirement_checks(sequence INTEGER PRIMARY KEY AUTOINCREMENT,id TEXT NOT NULL UNIQUE,requirement TEXT NOT NULL REFERENCES task_requirements(id) ON DELETE CASCADE,requirement_revision INTEGER NOT NULL,task TEXT NOT NULL REFERENCES work_tasks(id) ON DELETE CASCADE,run TEXT NOT NULL,tool_id TEXT NOT NULL,receipt_hash TEXT NOT NULL,artifact TEXT,artifact_hash TEXT,predicate TEXT NOT NULL,expected TEXT NOT NULL,observed TEXT NOT NULL,verdict TEXT NOT NULL,note TEXT NOT NULL,mutation_revision INTEGER NOT NULL,created_run TEXT NOT NULL,created TEXT NOT NULL);
    CREATE INDEX IF NOT EXISTS requirement_checks_page ON requirement_checks(task,sequence);
    CREATE TABLE IF NOT EXISTS requirement_user_decisions(sequence INTEGER PRIMARY KEY AUTOINCREMENT,id TEXT NOT NULL UNIQUE,task TEXT NOT NULL REFERENCES work_tasks(id) ON DELETE CASCADE,requirement TEXT NOT NULL REFERENCES task_requirements(id) ON DELETE CASCADE,decision TEXT NOT NULL,replacement TEXT,note TEXT NOT NULL,created TEXT NOT NULL);
    CREATE TABLE IF NOT EXISTS task_mutation_intents(task TEXT NOT NULL REFERENCES work_tasks(id) ON DELETE CASCADE,intent TEXT NOT NULL,sequence INTEGER NOT NULL,run TEXT NOT NULL,tool_id TEXT NOT NULL,name TEXT NOT NULL,status TEXT NOT NULL,receipt_hash TEXT NOT NULL,PRIMARY KEY(task,intent));
    CREATE INDEX IF NOT EXISTS task_mutation_pending ON task_mutation_intents(task,status,sequence);
    CREATE TABLE IF NOT EXISTS task_mutation_state(task TEXT PRIMARY KEY REFERENCES work_tasks(id) ON DELETE CASCADE,revision INTEGER NOT NULL DEFAULT 0,index_after INTEGER NOT NULL DEFAULT 0);
    CREATE TABLE IF NOT EXISTS task_attempt_resolutions(sequence INTEGER PRIMARY KEY AUTOINCREMENT,id TEXT NOT NULL UNIQUE,task TEXT NOT NULL REFERENCES work_tasks(id) ON DELETE CASCADE,failed_run TEXT NOT NULL,failed_tool TEXT NOT NULL,replacement_run TEXT NOT NULL,replacement_tool TEXT NOT NULL,note TEXT NOT NULL,created_run TEXT NOT NULL,created TEXT NOT NULL,UNIQUE(task,failed_run,failed_tool));")?;
    conn.execute_batch("CREATE TRIGGER IF NOT EXISTS task_request_text_immutable BEFORE UPDATE OF content ON task_requests WHEN NEW.content IS NOT OLD.content BEGIN SELECT RAISE(ABORT,'Original task request is immutable; append a new request'); END;
        CREATE TRIGGER IF NOT EXISTS task_goal_text_immutable BEFORE UPDATE OF goal ON work_tasks WHEN NEW.goal IS NOT OLD.goal BEGIN SELECT RAISE(ABORT,'Original task goal is immutable; create a new task'); END;")?;
    Ok(())
}
fn task(conn:&Connection,run:&str)->Result<String>{
    conn.query_row("SELECT task FROM task_runs WHERE run=?1",[run],|r|r.get(0)).optional()?.context("Для этого запуска нет рабочей задачи")
}
fn short(text:&str,max:usize,field:&str)->Result<()>{anyhow::ensure!(text.chars().count()<=max,"Поле {field}: используйте короткую подпись; полный исходный запрос сохраняется отдельно");Ok(())}
// SQLite TEXT length/substr stop at NUL. Incremental BLOB reads support TEXT,
// preserve every UTF-8 scalar and avoid constructing a huge source in memory.
fn text_location(conn:&Connection,task:&str,request:Option<i64>)->Result<(String,&'static str,&'static str,i64)>{
    if let Some(request)=request{let row:i64=conn.query_row("SELECT rowid FROM task_requests WHERE task=?1 AND sequence=?2",params![task,request],|r|r.get(0)).optional()?.context("Исходный запрос не принадлежит этой задаче")?;Ok((format!("request:{request}"),"task_requests","content",row))}
    else{let row:i64=conn.query_row("SELECT rowid FROM work_tasks WHERE id=?1",[task],|r|r.get(0)).optional()?.context("Рабочая задача не найдена")?;Ok((format!("goal:{task}"),"work_tasks","goal",row))}
}
fn utf8_packets(mut reader:impl std::io::Read,mut sink:impl FnMut(u64,&str)->Result<bool>)->Result<()>{
    let mut block=[0u8;16384];let mut pending=Vec::with_capacity(16388);let mut consumed=0u64;
    loop{let n=reader.read(&mut block)?;if n==0{anyhow::ensure!(pending.is_empty(),"Исходный текст содержит незавершённый UTF-8");break;}pending.extend_from_slice(&block[..n]);let valid=match std::str::from_utf8(&pending){Ok(_)=>pending.len(),Err(error)=>{anyhow::ensure!(error.error_len().is_none(),"Исходный текст содержит некорректный UTF-8");error.valid_up_to()}};
        let text=std::str::from_utf8(&pending[..valid])?;if !sink(consumed,text)?{break;}consumed+=valid as u64;pending.drain(..valid);
    }Ok(())
}
pub fn text_prefix(conn:&Connection,task:&str,request:Option<i64>,max:usize)->Result<String>{
    let(_,table,column,row)=text_location(conn,task,request)?;let blob=conn.blob_open("main",table,column,row,true)?;let mut text=String::new();let mut count=0;utf8_packets(blob,|_,part|{for ch in part.chars(){if count>=max{return Ok(false);}text.push(ch);count+=1;}Ok(count<max)})?;Ok(text)
}
struct TextSavepoint<'a>{conn:&'a Connection,done:bool}
impl Drop for TextSavepoint<'_>{fn drop(&mut self){if !self.done{let _=self.conn.execute_batch("ROLLBACK TO task_source_index; RELEASE task_source_index");}}}
fn text_index(conn:&Connection,task:&str,request:Option<i64>)->Result<(String,i64,i64,String)>{
    let(id,table,column,row)=text_location(conn,task,request)?;let blob=conn.blob_open("main",table,column,row,true)?;let bytes=i64::try_from(blob.len())?;
    if let Some((saved_bytes,characters,hash))=conn.query_row("SELECT bytes,characters,sha256 FROM task_text_sources WHERE id=?1",[&id],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,i64>(1)?,r.get::<_,String>(2)?))).optional()?{if saved_bytes==bytes{return Ok((id,bytes,characters,hash));}}
    conn.execute_batch("SAVEPOINT task_source_index")?;let mut savepoint=TextSavepoint{conn,done:false};conn.execute("DELETE FROM task_text_sources WHERE id=?1",[&id])?;conn.execute("INSERT INTO task_text_sources VALUES(?1,?2,?3,?4,0,'')",params![id,task,request,bytes])?;
    let mut characters=0i64;let mut hash=Sha256::new();
    utf8_packets(blob,|byte_start,text|{hash.update(text.as_bytes());for(offset,_)in text.char_indices(){if characters%8192==0{conn.execute("INSERT INTO task_text_offsets VALUES(?1,?2,?3)",params![id,characters,i64::try_from(byte_start+offset as u64)?])?;}characters+=1;}Ok(true)})?;
    let hash=format!("{:x}",hash.finalize());conn.execute("UPDATE task_text_sources SET characters=?1,sha256=?2 WHERE id=?3",params![characters,hash,id])?;conn.execute_batch("RELEASE task_source_index")?;savepoint.done=true;Ok((id,bytes,characters,hash))
}
fn text_range(conn:&Connection,task:&str,request:Option<i64>,start:i64,count:i64,mut sink:impl FnMut(&str)->Result<()>)->Result<(i64,String)>{
    use std::io::{Seek,SeekFrom};let(id,_,total,hash)=text_index(conn,task,request)?;
    anyhow::ensure!(start>=0&&count>=0&&start<=total,"Диапазон выходит за исходный запрос");let end=start.saturating_add(count).min(total);if start==end{return Ok((total,hash));}
    let(character,byte):(i64,i64)=conn.query_row("SELECT character,byte FROM task_text_offsets WHERE source=?1 AND character<=?2 ORDER BY character DESC LIMIT 1",params![id,start],|r|Ok((r.get(0)?,r.get(1)?))).optional()?.context("Отсутствует индекс диапазона исходного текста")?;
    let(_,table,column,row)=text_location(conn,task,request)?;let mut blob=conn.blob_open("main",table,column,row,true)?;blob.seek(SeekFrom::Start(u64::try_from(byte)?))?;let mut scalar=character;
    utf8_packets(blob,|_,text|{let mut first=None;let mut last=0;for(offset,ch)in text.char_indices(){if scalar>=start&&scalar<end{first.get_or_insert(offset);last=offset+ch.len_utf8();}scalar+=1;if scalar>=end{break;}}if let Some(first)=first{sink(&text[first..last])?;}Ok(scalar<end)})?;
    anyhow::ensure!(scalar>=end,"В исходном тексте отсутствует ожидаемый диапазон");Ok((total,hash))
}
pub fn request_page(conn:&Connection,task:&str,request:i64,start:usize,max:usize)->Result<Value>{
    anyhow::ensure!((1..=16000).contains(&max),"Размер страницы запроса 1–16000 символов");let mut text=String::new();let(total,hash)=text_range(conn,task,Some(request),i64::try_from(start)?,i64::try_from(max)?,|part|{text.push_str(part);Ok(())})?;let next=start+text.chars().count();
    Ok(json!({"sequence":request,"text":text,"offset":start,"next_offset":next,"characters":total,"has_more":(next as i64)<total,"sha256":hash,"offset_unit":"unicode_scalar"}))
}
pub fn goal_preview(conn:&Connection,task:&str,max:usize)->Result<Value>{let mut text=String::new();let(total,hash)=text_range(conn,task,None,0,i64::try_from(max)?,|part|{text.push_str(part);Ok(())})?;Ok(json!({"text":text,"characters":total,"sha256":hash}))}
fn source_hash(conn:&Connection,task:&str,request:i64,start:i64,characters:i64)->Result<String>{
    anyhow::ensure!(characters>0,"Пустой диапазон требования");let mut hash=Sha256::new();let(total,_)=text_range(conn,task,Some(request),start,characters,|part|{hash.update(part.as_bytes());Ok(())})?;anyhow::ensure!(start.checked_add(characters).is_some_and(|end|end<=total),"Диапазон требования выходит за исходный запрос");Ok(format!("{:x}",hash.finalize()))
}
#[derive(Deserialize)]#[serde(deny_unknown_fields)]
struct RequirementInput{#[serde(default)]id:Option<String>,request:i64,start:i64,characters:i64,title:String,#[serde(default)]criterion:String,#[serde(default="pending")]assessment:String,#[serde(default)]note:String}
fn pending()->String{"pending".into()}
fn record_requirement(conn:&mut Connection,run:&str,args:Value)->Result<Value>{
    let input:RequirementInput=serde_json::from_value(args)?;anyhow::ensure!(!input.title.trim().is_empty(),"Нужна подпись требования");short(&input.title,400,"title")?;short(&input.criterion,4000,"criterion")?;short(&input.note,4000,"note")?;
    anyhow::ensure!(["pending","in_progress","blocked","reported_complete"].contains(&input.assessment.as_str()),"Оценка агента не может менять проверенность или решение пользователя");
    let tx=conn.transaction()?;let task=task(&tx,run)?;let hash=source_hash(&tx,&task,input.request,input.start,input.characters)?;let now=crate::api::now();
    let prior:Option<(String,i64,i64,i64)>=if let Some(id)=input.id.as_deref(){tx.query_row("SELECT id,request,start,characters FROM task_requirements WHERE task=?1 AND id=?2",params![task,id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?}else{tx.query_row("SELECT id,request,start,characters FROM task_requirements WHERE task=?1 AND request=?2 AND start=?3 AND characters=?4",params![task,input.request,input.start,input.characters],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?};
    if input.id.is_some(){anyhow::ensure!(prior.is_some(),"Требование не принадлежит этой задаче");}
    let id=if let Some((id,request,start,count))=prior{anyhow::ensure!((request,start,count)==(input.request,input.start,input.characters),"Источник существующего требования неизменяем. Создайте новое; supersede требует отдельного решения пользователя");tx.execute("UPDATE task_requirements SET title=?1,criterion=?2,assessment=?3,note=?4,revision=revision+CASE WHEN title!=?1 OR criterion!=?2 THEN 1 ELSE 0 END,updated_run=?5,updated=?6 WHERE id=?7",params![input.title,input.criterion,input.assessment,input.note,run,now,id])?;id}else{let id=uuid::Uuid::new_v4().to_string();tx.execute("INSERT INTO task_requirements(id,task,request,start,characters,source_hash,title,criterion,assessment,note,revision,created_run,updated_run,created,updated) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,1,?11,?11,?12,?12)",params![id,task,input.request,input.start,input.characters,hash,input.title,input.criterion,input.assessment,input.note,run,now])?;id};
    let revision:i64=tx.query_row("SELECT revision FROM task_requirements WHERE id=?1",[&id],|r|r.get(0))?;tx.commit()?;
    Ok(json!({"id":id,"revision":revision,"source":{"request":input.request,"start":input.start,"characters":input.characters,"sha256":hash,"offset_unit":"unicode_scalar"},"assessment":input.assessment,"semantic_verification":"unverified","completion_verified":false}))
}
/// Called after the actual tool row is persisted. Replays of an old receipt cannot
/// overwrite a newer attempt for the same exact intent. Data stays on disk.
pub fn observe(conn:&Connection,run:&str,tool:&Value)->Result<()>{
    let name=tool["name"].as_str().unwrap_or("");
    if !["write_file","edit_file","copy_file","move_file","delete_file","translate_file","run_command","start_process","write_stdin","browser_action"].contains(&name){return Ok(());}
    let Some(task)=conn.query_row("SELECT task FROM task_runs WHERE run=?1",[run],|r|r.get::<_,String>(0)).optional()? else{return Ok(());};
    let Some(id)=tool["id"].as_str()else{return Ok(());};
    let row:Option<(i64,String)>=conn.query_row("SELECT sequence,payload FROM run_tools WHERE run=?1 AND tool_id=?2",params![run,id],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    let Some((sequence,payload))=row else{return Ok(());};let actual:Value=serde_json::from_str(&payload)?;let status=actual["status"].as_str().context("Нет фактического статуса инструмента")?;
    let receipt_hash=format!("{:x}",Sha256::digest(payload.as_bytes()));
    // Private inputs have no persistent content fingerprint; separate calls must
    // never be considered the same secret value merely because both are masked.
    let intent=if ["browser_action","write_stdin"].contains(&name){format!("{name}:{run}:{id}")}else{crate::timeline::mutation_key(&actual).unwrap_or_else(||{
        if let Some(hash)=actual["arguments_artifact"]["sha256"].as_str(){format!("{name}:{hash}")}else if actual["arguments"].is_object(){format!("{name}:{:x}",Sha256::digest(actual["arguments"].to_string().as_bytes()))}else{format!("{name}:{run}:{id}")}
    })};
    let changed=conn.execute("INSERT INTO task_mutation_intents(task,intent,sequence,run,tool_id,name,status,receipt_hash) VALUES(?1,?2,?3,?4,?5,?6,?7,?8) ON CONFLICT(task,intent) DO UPDATE SET sequence=excluded.sequence,run=excluded.run,tool_id=excluded.tool_id,name=excluded.name,status=excluded.status,receipt_hash=excluded.receipt_hash WHERE excluded.sequence>=task_mutation_intents.sequence AND excluded.receipt_hash!=task_mutation_intents.receipt_hash",params![task,intent,sequence,run,id,name,status,receipt_hash])?;
    if changed>0{conn.execute("INSERT INTO task_mutation_state(task,revision) VALUES(?1,1) ON CONFLICT(task) DO UPDATE SET revision=revision+1",[task])?;}
    Ok(())
}
fn index_history(conn:&Connection,task:&str)->Result<bool>{
    conn.execute("INSERT OR IGNORE INTO task_mutation_state(task) VALUES(?1)",[task])?;
    let after:i64=conn.query_row("SELECT index_after FROM task_mutation_state WHERE task=?1",[task],|r|r.get(0))?;
    let mut query=conn.prepare("SELECT t.sequence,t.run,t.tool_id,json_extract(t.payload,'$.name') FROM run_tools t JOIN task_runs s ON s.run=t.run WHERE s.task=?1 AND t.sequence>?2 ORDER BY t.sequence LIMIT 128")?;
    let rows=query.query_map(params![task,after],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
    let mut next=after;for(sequence,run,id,name)in rows{observe(conn,&run,&json!({"id":id,"name":name}))?;next=sequence;}
    conn.execute("UPDATE task_mutation_state SET index_after=?1 WHERE task=?2",params![next,task])?;
    Ok(conn.query_row("SELECT EXISTS(SELECT 1 FROM run_tools t JOIN task_runs s ON s.run=t.run WHERE s.task=?1 AND t.sequence>?2)",params![task,next],|r|r.get(0))?)
}
fn revision(conn:&Connection,task:&str)->Result<i64>{Ok(conn.query_row("SELECT revision FROM task_mutation_state WHERE task=?1",[task],|r|r.get(0)).optional()?.unwrap_or(0))}
fn receipt(conn:&Connection,task:&str,run:&str,id:&str)->Result<(Value,String)>{
    let text:Option<String>=conn.query_row("SELECT t.payload FROM run_tools t JOIN task_runs s ON s.run=t.run WHERE s.task=?1 AND t.run=?2 AND t.tool_id=?3",params![task,run,id],|r|r.get(0)).optional()?;
    let text=text.context("Фактическая запись инструмента не принадлежит этой задаче")?;Ok((serde_json::from_str(&text)?,format!("{:x}",Sha256::digest(text.as_bytes()))))
}
#[derive(Deserialize)]#[serde(deny_unknown_fields)]
struct CheckInput{requirement:String,#[serde(default)]run:Option<String>,tool_id:String,predicate:String,expected:Value,#[serde(default)]note:String}
fn check(conn:&mut Connection,run:&str,args:Value)->Result<Value>{
    let input:CheckInput=serde_json::from_value(args)?;short(&input.note,4000,"note")?;let tx=conn.transaction()?;let task=task(&tx,run)?;let indexed=index_history(&tx,&task)?;
    let req_revision:i64=tx.query_row("SELECT revision FROM task_requirements WHERE task=?1 AND id=?2",params![task,input.requirement],|r|r.get(0)).optional()?.context("Требование не принадлежит этой задаче")?;
    let evidence_run=input.run.as_deref().unwrap_or(run);let(record,receipt_hash)=receipt(&tx,&task,evidence_run,&input.tool_id)?;let status=record["status"].as_str().unwrap_or("unknown");
    let mut observed=Value::Null;let mut verdict="unknown";
    match input.predicate.as_str(){
        "tool_status"=>{anyhow::ensure!(input.expected.as_str().is_some_and(|s|["succeeded","failed","denied","cancelled","timed_out","interrupted","needs_input"].contains(&s)),"Нужен конкретный ожидаемый статус инструмента");observed=json!(status);if status!="running"{verdict=if observed==input.expected{"passed"}else{"failed"};}},
        "exit_code"=>{anyhow::ensure!(record["name"]=="run_command"&&input.expected.as_i64().is_some(),"exit_code проверяет только фактический run_command с целым ожидаемым кодом");observed=record["exit_code"].clone();if observed.as_i64().is_some(){verdict=if observed==input.expected{"passed"}else{"failed"};}},
        "mutation_verified"=>{anyhow::ensure!(crate::timeline::mutation_key(&record).is_some()&&input.expected==true,"mutation_verified требует файловую операцию и expected=true");if record["result_preview_only"]!=true{let result:Value=serde_json::from_str(record["result"].as_str().unwrap_or("null")).unwrap_or(Value::Null);observed=result["verified"].clone();if status=="succeeded"&&observed.is_boolean(){verdict=if observed==input.expected{"passed"}else{"failed"};}else if status=="failed"{verdict="failed";}}},
        _=>anyhow::bail!("Неизвестный проверяемый предикат; смысловое выполнение требования не выводится из слов модели")
    }
    let artifact=record["result_artifact"]["id"].as_str();let artifact_hash=if let Some(id)=artifact{let hash:Option<String>=tx.query_row("SELECT hash FROM artifacts WHERE id=?1 AND (run=?2 OR EXISTS(SELECT 1 FROM task_runs s WHERE s.run=artifacts.run AND s.task=?3))",params![id,evidence_run,task],|r|r.get(0)).optional()?;Some(hash.context("Артефакт записи недоступен этой задаче")?)}else{None};
    let mutation_revision=revision(&tx,&task)?;let id=uuid::Uuid::new_v4().to_string();tx.execute("INSERT INTO requirement_checks(id,requirement,requirement_revision,task,run,tool_id,receipt_hash,artifact,artifact_hash,predicate,expected,observed,verdict,note,mutation_revision,created_run,created) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)",params![id,input.requirement,req_revision,task,evidence_run,input.tool_id,receipt_hash,artifact,artifact_hash,input.predicate,input.expected.to_string(),observed.to_string(),verdict,input.note,mutation_revision,run,crate::api::now()])?;tx.commit()?;
    Ok(json!({"id":id,"requirement":input.requirement,"predicate":input.predicate,"expected":input.expected,"observed":observed,"verdict":verdict,"receipt":{"run":evidence_run,"tool_id":input.tool_id,"sha256":receipt_hash,"artifact":artifact,"artifact_sha256":artifact_hash},"historical_index_pending":indexed,"semantic_verification":"unverified","completion_verified":false,"note":"Only this explicit predicate was checked. A tool success, exit code or written bytes do not prove arbitrary project requirements."}))
}
#[derive(Deserialize)]#[serde(deny_unknown_fields)]
struct ResolutionInput{#[serde(default)]failed_run:Option<String>,failed_tool:String,#[serde(default)]replacement_run:Option<String>,replacement_tool:String,note:String}
fn resolve(conn:&mut Connection,run:&str,args:Value)->Result<Value>{
    let input:ResolutionInput=serde_json::from_value(args)?;anyhow::ensure!(!input.note.trim().is_empty(),"Объясните, как исправленная попытка заменяет предыдущую");short(&input.note,4000,"note")?;
    let tx=conn.transaction()?;let task=task(&tx,run)?;let failed_run=input.failed_run.as_deref().unwrap_or(run);let replacement_run=input.replacement_run.as_deref().unwrap_or(run);
    let(failed,_)=receipt(&tx,&task,failed_run,&input.failed_tool)?;let(replacement,_)=receipt(&tx,&task,replacement_run,&input.replacement_tool)?;
    anyhow::ensure!(failed["status"]!="succeeded"&&failed["status"]!="running","Нужна завершённая неуспешная попытка");anyhow::ensure!(replacement["status"]=="succeeded","Замена должна ссылаться на реальную успешную запись инструмента");
    let id=uuid::Uuid::new_v4().to_string();tx.execute("INSERT INTO task_attempt_resolutions(id,task,failed_run,failed_tool,replacement_run,replacement_tool,note,created_run,created) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9) ON CONFLICT(task,failed_run,failed_tool) DO UPDATE SET replacement_run=excluded.replacement_run,replacement_tool=excluded.replacement_tool,note=excluded.note,created_run=excluded.created_run,created=excluded.created",params![id,task,failed_run,input.failed_tool,replacement_run,input.replacement_tool,input.note,run,crate::api::now()])?;tx.commit()?;
    Ok(json!({"assessment":"model_assessed_resolution","failed":{"run":failed_run,"tool_id":input.failed_tool},"replacement":{"run":replacement_run,"tool_id":input.replacement_tool},"requirement_disposition_unchanged":true,"semantic_verification":"unverified","note":"Both actual receipts remain available. The link is the agent's explanation, not a user waiver or verified semantic equivalence."}))
}
const UNRESOLVED:&str="m.status!='succeeded' AND NOT EXISTS(SELECT 1 FROM task_attempt_resolutions x WHERE x.task=m.task AND x.failed_run=m.run AND x.failed_tool=m.tool_id)";
fn summary(conn:&Connection,run:&str)->Result<Value>{let task=task(conn,run)?;summary_task(conn,&task)}
pub fn summary_task(conn:&Connection,task:&str)->Result<Value>{
    let task=task.to_owned();let indexing=index_history(conn,&task)?;let revision=revision(conn,&task)?;
    let mut counts=serde_json::Map::new();let mut query=conn.prepare("SELECT disposition,assessment,count(*) FROM task_requirements WHERE task=?1 GROUP BY disposition,assessment")?;for row in query.query_map([&task],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,i64>(2)?)))?{let(d,a,n)=row?;counts.insert(format!("{d}:{a}"),json!(n));}
    let requests:i64=conn.query_row("SELECT count(*) FROM task_requests WHERE task=?1",[&task],|r|r.get(0))?;let reviewed:i64=conn.query_row("SELECT count(*) FROM requirement_source_reviews WHERE task=?1",[&task],|r|r.get(0))?;
    let unresolved:i64=conn.query_row(&format!("SELECT count(*) FROM task_mutation_intents m WHERE m.task=?1 AND {UNRESOLVED}"),[&task],|r|r.get(0))?;
    let unknown:i64=conn.query_row(&format!("SELECT count(*) FROM task_mutation_intents m JOIN runs r ON r.id=m.run WHERE m.task=?1 AND {UNRESOLVED} AND m.status IN ('running','cancelled','timed_out','interrupted')"),[&task],|r|r.get(0))?;
    let mut checks=serde_json::Map::new();let mut query=conn.prepare("SELECT CASE WHEN c.mutation_revision!=?2 OR c.requirement_revision!=r.revision THEN 'stale' ELSE c.verdict END,count(*) FROM requirement_checks c JOIN task_requirements r ON r.id=c.requirement WHERE c.task=?1 GROUP BY 1")?;for row in query.query_map(params![task,revision],|r|Ok((r.get::<_,String>(0)?,r.get::<_,i64>(1)?)))?{let(key,n)=row?;checks.insert(key,json!(n));}
    Ok(json!({"task":task,"requirements":counts,"source_requests":requests,"agent_reviewed_requests":reviewed,"unreviewed_requests":requests-reviewed,"checks":checks,"unresolved_attempts":unresolved,"unknown_effects":unknown,"mutation_revision":revision,"historical_index_pending":indexing,"status":if indexing{"history_indexing"}else if unresolved>0{"attempts_unresolved"}else if requests>reviewed{"requirements_unreviewed"}else{"requirements_semantically_unverified"},"completion_verified":false,"semantic_verification":"unverified","note":"Requirements, failed attempts, actual predicates and explicit user decisions are separate. Model checkmarks do not prove project completion."}))
}
fn page(conn:&Connection,run:&str,section:&str,args:&Value)->Result<Value>{
    let task=task(conn,run)?;let indexing=index_history(conn,&task)?;let limit=args["limit"].as_i64().unwrap_or(8);anyhow::ensure!((1..=32).contains(&limit),"Размер страницы 1–32");
    let before=args["before_requirement"].as_i64().or_else(||args["before_check"].as_i64()).or_else(||args["before_attempt"].as_i64());anyhow::ensure!(before.is_none_or(|n|n>0),"Некорректный cursor");let current_revision=revision(conn,&task)?;
    let mut items=vec![];
    if section=="requirements"{let mut query=conn.prepare("SELECT r.sequence,r.id,r.request,r.start,r.characters,r.source_hash,r.title,r.criterion,r.assessment,r.note,r.revision,r.disposition,r.user_decision,substr(q.content,r.start+1,min(r.characters,400)) FROM task_requirements r JOIN task_requests q ON q.sequence=r.request WHERE r.task=?1 AND (?2 IS NULL OR r.sequence<?2) ORDER BY r.sequence DESC LIMIT ?3")?;for row in query.query_map(params![task,before,limit+1],|r|Ok(json!({"sequence":r.get::<_,i64>(0)?,"id":r.get::<_,String>(1)?,"source":{"request":r.get::<_,i64>(2)?,"start":r.get::<_,i64>(3)?,"characters":r.get::<_,i64>(4)?,"sha256":r.get::<_,String>(5)?,"preview":r.get::<_,String>(13)?,"offset_unit":"unicode_scalar"},"title":r.get::<_,String>(6)?,"criterion":r.get::<_,String>(7)?,"assessment":r.get::<_,String>(8)?,"note":r.get::<_,String>(9)?,"revision":r.get::<_,i64>(10)?,"disposition":r.get::<_,String>(11)?,"user_decision":r.get::<_,Option<String>>(12)?,"semantic_verification":"unverified"})))?{items.push(row?);}}
    else if section=="requirement_checks"{let mut query=conn.prepare("SELECT c.sequence,c.id,c.requirement,c.run,c.tool_id,c.receipt_hash,c.artifact,c.artifact_hash,c.predicate,c.expected,c.observed,c.verdict,c.note,c.mutation_revision,c.requirement_revision,r.revision FROM requirement_checks c JOIN task_requirements r ON r.id=c.requirement WHERE c.task=?1 AND (?2 IS NULL OR c.sequence<?2) ORDER BY c.sequence DESC LIMIT ?3")?;for row in query.query_map(params![task,before,limit+1],|r|Ok(json!({"sequence":r.get::<_,i64>(0)?,"id":r.get::<_,String>(1)?,"requirement":r.get::<_,String>(2)?,"receipt":{"run":r.get::<_,String>(3)?,"tool_id":r.get::<_,String>(4)?,"sha256":r.get::<_,String>(5)?,"artifact":r.get::<_,Option<String>>(6)?,"artifact_sha256":r.get::<_,Option<String>>(7)?},"predicate":r.get::<_,String>(8)?,"expected":serde_json::from_str::<Value>(&r.get::<_,String>(9)?).unwrap_or(Value::Null),"observed":serde_json::from_str::<Value>(&r.get::<_,String>(10)?).unwrap_or(Value::Null),"verdict":r.get::<_,String>(11)?,"note":r.get::<_,String>(12)?,"freshness":if r.get::<_,i64>(13)?!=current_revision||r.get::<_,i64>(14)?!=r.get::<_,i64>(15)?{"stale"}else{"receipt_snapshot"},"semantic_verification":"unverified"})))?{items.push(row?);}}
    else if section=="attempts"{let mut query=conn.prepare("SELECT m.sequence,m.run,m.tool_id,m.name,m.status,r.status,t.payload,x.replacement_run,x.replacement_tool,x.note FROM task_mutation_intents m JOIN runs r ON r.id=m.run JOIN run_tools t ON t.run=m.run AND t.tool_id=m.tool_id LEFT JOIN task_attempt_resolutions x ON x.task=m.task AND x.failed_run=m.run AND x.failed_tool=m.tool_id WHERE m.task=?1 AND m.status!='succeeded' AND (?2 IS NULL OR m.sequence<?2) AND (?4=0 OR x.sequence IS NULL) ORDER BY m.sequence DESC LIMIT ?3")?;for row in query.query_map(params![task,before,limit+1,args["unresolved_only"].as_bool().unwrap_or(false)],|r|{let record:Value=serde_json::from_str(&r.get::<_,String>(6)?).unwrap_or(Value::Null);Ok(json!({"sequence":r.get::<_,i64>(0)?,"run":r.get::<_,String>(1)?,"tool_id":r.get::<_,String>(2)?,"name":r.get::<_,String>(3)?,"status":r.get::<_,String>(4)?,"run_status":r.get::<_,String>(5)?,"arguments":crate::artifacts::context_preview(&record["arguments"],1200),"arguments_artifact":record["arguments_artifact"],"result_preview":crate::tools::truncate(record["result"].as_str().unwrap_or(""),1000),"result_artifact":record["result_artifact"],"resolution":if r.get::<_,Option<String>>(7)?.is_some(){json!({"assessment":"model_assessed_resolution","run":r.get::<_,Option<String>>(7)?,"tool_id":r.get::<_,Option<String>>(8)?,"note":r.get::<_,Option<String>>(9)?})}else{Value::Null}}))})?{items.push(row?);}}
    else{anyhow::bail!("Неизвестный раздел требований");}
    if section=="requirements"{for row in &mut items{let source=&row["source"];let mut text=String::new();text_range(conn,&task,Some(source["request"].as_i64().unwrap()),source["start"].as_i64().unwrap(),source["characters"].as_i64().unwrap().min(400),|part|{text.push_str(part);Ok(())})?;row["source"]["preview"]=json!(text);}}
    let more=items.len()>limit as usize;items.truncate(limit as usize);let next=if more{items.last().map(|v|v["sequence"].clone()).unwrap_or(Value::Null)}else{Value::Null};
    let mut result=json!({"section":section,"items":items,"next_before":next,"has_more":more,"mutation_revision":current_revision,"historical_index_pending":indexing,"completion_verified":false,"note":"Pages show current ledger state; newly observed effects can change freshness. Original receipts remain in read_run_log/read_artifact."});
    result[match section{"requirements"=>"next_before_requirement","requirement_checks"=>"next_before_check",_=>"next_before_attempt"}]=next;Ok(result)
}
impl crate::task_store::Store{
    pub async fn record_requirement(&self,run:String,args:Value)->Result<Value>{self.access(move|conn|record_requirement(conn,&run,args)).await}
    pub async fn record_requirement_check(&self,run:String,args:Value)->Result<Value>{self.access(move|conn|check(conn,&run,args)).await}
    pub async fn resolve_task_attempt(&self,run:String,args:Value)->Result<Value>{self.access(move|conn|resolve(conn,&run,args)).await}
    pub async fn requirement_page(&self,run:String,section:String,args:Value)->Result<Value>{self.access(move|conn|page(conn,&run,&section,&args)).await}
    pub async fn task_requirement_page(&self,task_id:String,section:String,args:Value)->Result<Value>{self.access(move|conn|{
        let run:String=conn.query_row("SELECT t.run FROM task_runs t JOIN runs r ON r.id=t.run WHERE t.task=?1 ORDER BY r.created DESC,r.id DESC LIMIT 1",[&task_id],|r|r.get(0)).optional()?.context("Задача не найдена")?;
        page(conn,&run,&section,&args)
    }).await}
    pub async fn requirement_source(&self,task_id:String,id:String,offset:usize)->Result<Value>{self.access(move|conn|{
        let(request,start,characters):(i64,i64,i64)=conn.query_row("SELECT request,start,characters FROM task_requirements WHERE task=?1 AND id=?2",params![task_id,id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?.context("Требование не принадлежит задаче")?;
        let offset=i64::try_from(offset)?;anyhow::ensure!(offset<=characters,"Смещение за пределами исходного фрагмента");let count=(characters-offset).min(8000);let mut text=String::new();
        text_range(conn,&task_id,Some(request),start+offset,count,|part|{text.push_str(part);Ok(())})?;
        Ok(json!({"id":id,"request":request,"text":text,"offset":offset,"next_offset":offset+count,"has_more":offset+count<characters,"characters":characters,"offset_unit":"unicode_scalar"}))
    }).await}

    pub async fn completion_state(&self,run:String)->Result<Value>{self.access(move|conn|{let mut state=summary(conn,&run)?;state["attempts"]=page(conn,&run,"attempts",&json!({"limit":8,"unresolved_only":true}))?;Ok(state)}).await}
    pub async fn review_requirement_source(&self,run:String,request:i64,note:String)->Result<Value>{self.access(move|conn|{
        short(&note,4000,"note")?;let tx=conn.transaction()?;let task=task(&tx,&run)?;let(_,_,count,hash)=text_index(&tx,&task,Some(request))?;
        tx.execute("INSERT INTO requirement_source_reviews VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(task,request) DO UPDATE SET source_hash=excluded.source_hash,source_characters=excluded.source_characters,note=excluded.note,run=excluded.run",params![task,request,hash,count,note,run])?;tx.commit()?;
        Ok(json!({"request":request,"characters":count,"sha256":hash,"assessment":"agent_reviewed","semantic_verification":"unverified","note":"The agent reports reviewing this requirement source. This does not verify complete extraction or fulfillment."}))
    }).await}
    /// Trusted user-interface entry point. Never exposed as a model tool. The
    /// caller must authenticate the explicit user decision; no quote heuristic.
    pub async fn user_requirement_decision(&self,task:String,id:String,decision:String,replacement:Option<String>,note:String)->Result<Value>{self.access(move|conn|{
        anyhow::ensure!(["active","waived","superseded"].contains(&decision.as_str()),"Неизвестное решение пользователя");short(&note,8000,"note")?;let tx=conn.transaction()?;
        anyhow::ensure!(tx.query_row("SELECT EXISTS(SELECT 1 FROM task_requirements WHERE task=?1 AND id=?2)",params![task,id],|r|r.get::<_,bool>(0))?,"Требование не принадлежит задаче");
        if decision=="superseded"{let target=replacement.as_deref().context("Укажите заменяющее требование")?;anyhow::ensure!(target!=id&&tx.query_row("SELECT EXISTS(SELECT 1 FROM task_requirements WHERE task=?1 AND id=?2)",params![task,target],|r|r.get::<_,bool>(0))?,"Замена должна быть другим требованием той же задачи");}else{anyhow::ensure!(replacement.is_none(),"Замена допускается только для superseded");}
        let action=uuid::Uuid::new_v4().to_string();tx.execute("INSERT INTO requirement_user_decisions(id,task,requirement,decision,replacement,note,created) VALUES(?1,?2,?3,?4,?5,?6,?7)",params![action,task,id,decision,replacement,note,crate::api::now()])?;tx.execute("UPDATE task_requirements SET disposition=?1,user_decision=?2 WHERE id=?3",params![decision,action,id])?;tx.commit()?;
        Ok(json!({"id":id,"decision":decision,"replacement":replacement,"origin":"explicit_user_decision","decision_id":action,"source_requirement_retained":true}))
    }).await}
}

#[cfg(test)]mod tests{
    use super::*;
    async fn fixture()->(std::path::PathBuf,crate::task_store::Store,String,i64,String){
        let root=std::env::temp_dir().join(format!("requirements-{}",uuid::Uuid::new_v4()));std::fs::create_dir_all(&root).unwrap();let store=crate::task_store::Store::open(&root).unwrap();store.access(|conn|install(conn)).await.unwrap();
        let source="Intro 🦀\0\r\nBuild and test the required feature.\r\nPreserve the original data.".to_owned();store.begin_in_project("a".into(),"chat".into(),json!({"mode":"agent","content":source}),"project".into()).await.unwrap();
        let(task,sequence)=store.access(|conn|Ok((task(conn,"a")?,conn.query_row("SELECT sequence FROM task_requests WHERE run='a'",[],|r|r.get::<_,i64>(0))?))).await.unwrap();(root,store,task,sequence,source)
    }
    async fn save(store:&crate::task_store::Store,run:&str,tool:Value){store.persist_tool(run.into(),tool.clone()).await.unwrap();let run=run.to_owned();store.access(move|conn|observe(conn,&run,&tool)).await.unwrap();}
    fn write(id:&str,path:&str,status:&str)->Value{json!({"id":id,"name":"write_file","arguments":{"path":path,"content":"required bytes"},"status":status,"result":if status=="succeeded"{"{\"verified\":true,\"changed\":true}"}else{"write unavailable"}})}
    #[tokio::test]async fn exact_unicode_request_ranges_survive_updates_and_user_decisions_do_not_erase_sources(){
        let(root,store,task,request,source)=fixture().await;let start=source.chars().position(|c|c=='B').unwrap() as i64;let text="Build and test the required feature.";let count=text.chars().count() as i64;
        let input=json!({"request":request,"start":start,"characters":count,"title":"Required feature","criterion":"The requested behavior must hold","assessment":"reported_complete"});let requirement=store.record_requirement("a".into(),input.clone()).await.unwrap();let id=requirement["id"].as_str().unwrap().to_owned();
        assert_eq!(requirement["source"]["sha256"],format!("{:x}",Sha256::digest(text.as_bytes())));assert_eq!(requirement["semantic_verification"],"unverified");
        for extra in [json!({"waived":true}),json!({"verified":true}),json!({"disposition":"superseded"})]{let mut bad=input.clone();bad.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());assert!(store.record_requirement("a".into(),bad).await.is_err());}
        let mut bad=input.clone();bad["assessment"]=json!("verified");assert!(store.record_requirement("a".into(),bad).await.is_err());
        let mut changed=input.clone();changed["id"]=json!(id);changed["start"]=json!(start+1);assert!(store.record_requirement("a".into(),changed).await.is_err());
        let decision=store.user_requirement_decision(task.clone(),id.clone(),"waived".into(),None,"Explicit user decision fixture".into()).await.unwrap();assert_eq!(decision["origin"],"explicit_user_decision");
        let page=store.requirement_page("a".into(),"requirements".into(),json!({})).await.unwrap();assert_eq!(page["items"][0]["source"]["preview"],text);assert_eq!(page["items"][0]["disposition"],"waived");assert_eq!(page["items"][0]["assessment"],"reported_complete");
        assert_eq!(store.task_request_slice(task,request,0,8000).await.unwrap()["text"],source);
        store.delete_conversation("chat".into()).await.unwrap();store.access(|conn|{for table in["task_requirements","requirement_user_decisions","requirement_checks","task_mutation_intents"]{assert_eq!(conn.query_row(&format!("SELECT count(*) FROM {table}"),[],|r|r.get::<_,i64>(0))?,0);}Ok(())}).await.unwrap();drop(store);std::fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]async fn task_scoped_ui_pages_and_source_reader_preserve_full_large_unicode_ranges(){
        let(root,store,task,_,_)=fixture().await;let source="prefix 🦀\0\r\n".to_owned()+&"Large required behavior 🦀\0\r\n".repeat(900)+"END";
        store.begin_in_project("large".into(),"chat".into(),json!({"mode":"agent","content":source}),"project".into()).await.unwrap();let request=store.access(|conn|Ok(conn.query_row("SELECT sequence FROM task_requests WHERE run='large'",[],|r|r.get::<_,i64>(0))?)).await.unwrap();
        let result=store.record_requirement("large".into(),json!({"request":request,"start":3,"characters":source.chars().count()-5,"title":"Large exact source"})).await.unwrap();let id=result["id"].as_str().unwrap().to_owned();
        let page=store.task_requirement_page(task.clone(),"requirements".into(),json!({})).await.unwrap();assert_eq!(page["items"].as_array().unwrap().len(),1);assert_eq!(page["items"][0]["id"],id);
        let mut all=String::new();let mut offset=0;loop{let part=store.requirement_source(task.clone(),id.clone(),offset).await.unwrap();assert!(part["text"].as_str().unwrap().chars().count()<=8000);all.push_str(part["text"].as_str().unwrap());if part["has_more"]!=true{break;}offset=part["next_offset"].as_u64().unwrap() as usize;}
        assert_eq!(all,source.chars().skip(3).take(source.chars().count()-5).collect::<String>());assert!(store.requirement_source("foreign".into(),id.clone(),0).await.is_err());assert!(store.requirement_source(task,id,usize::MAX).await.is_err());
        drop(store);std::fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]async fn real_receipts_verify_only_explicit_predicates_and_later_effects_make_checks_stale(){
        let(root,store,_task,request,source)=fixture().await;let requirement=store.record_requirement("a".into(),json!({"request":request,"start":0,"characters":source.chars().count(),"title":"Complete working project"})).await.unwrap();let id=requirement["id"].as_str().unwrap();
        save(&store,"a",json!({"id":"cmd","name":"run_command","arguments":{"command":"fixture actual exit"},"status":"succeeded","exit_code":0,"result":"exit-code receipt"})).await;
        let check_args=json!({"requirement":id,"tool_id":"cmd","predicate":"exit_code","expected":0});let checked=store.record_requirement_check("a".into(),check_args.clone()).await.unwrap();assert_eq!(checked["verdict"],"passed");assert_eq!(checked["completion_verified"],false);assert_eq!(checked["semantic_verification"],"unverified");
        store.begin_in_project("b".into(),"chat".into(),json!({"mode":"agent","content":"Continue checking"}),"project".into()).await.unwrap();let mut continued=check_args.clone();continued["run"]=json!("a");let checked=store.record_requirement_check("b".into(),continued).await;
        // Use the exact same persisted receipt across a continued run.
        assert!(checked.is_ok());
        save(&store,"b",write("write","feature.txt","succeeded")).await;
        let page=store.requirement_page("b".into(),"requirement_checks".into(),json!({})).await.unwrap();assert!(page["items"].as_array().unwrap().iter().all(|c|c["freshness"]=="stale"));
        let mut fabricated=check_args.clone();fabricated["tool_id"]=json!("not-real");assert!(store.record_requirement_check("a".into(),fabricated).await.is_err());
        store.begin_in_project("foreign".into(),"other".into(),json!({"mode":"agent","content":"Other user's task"}),"elsewhere".into()).await.unwrap();save(&store,"foreign",write("foreign-write","elsewhere.txt","succeeded")).await;
        assert!(store.record_requirement_check("a".into(),json!({"requirement":id,"run":"foreign","tool_id":"foreign-write","predicate":"mutation_verified","expected":true})).await.is_err());
        let state=store.completion_state("b".into()).await.unwrap();assert_eq!(state["completion_verified"],false);assert_eq!(state["semantic_verification"],"unverified");
        drop(store);std::fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]async fn failed_attempts_are_durable_pageable_and_only_explicit_real_replacements_resolve_different_intents(){
        let(root,store,_task,_request,_)=fixture().await;
        for n in 0..170{save(&store,"a",write(&format!("failed-{n}"),&format!("missing-{n}.txt"),"failed")).await;}
        save(&store,"a",write("unrelated","unrelated.txt","succeeded")).await;
        let mut state=store.completion_state("a".into()).await.unwrap();while state["historical_index_pending"]==true{state=store.completion_state("a".into()).await.unwrap();}assert_eq!(state["unresolved_attempts"],170);
        let mut args=json!({"limit":7});let mut seen=std::collections::HashSet::new();loop{let page=store.requirement_page("a".into(),"attempts".into(),args.clone()).await.unwrap();assert!(page["items"].as_array().unwrap().len()<=7);for item in page["items"].as_array().unwrap(){assert!(seen.insert(item["tool_id"].as_str().unwrap().to_owned()));}if page["has_more"]!=true{break;}args["before_attempt"]=page["next_before"].clone();}assert_eq!(seen.len(),170);
        assert!(store.resolve_task_attempt("a".into(),json!({"failed_tool":"failed-0","replacement_tool":"not-real","note":"corrected typo"})).await.is_err());
        assert!(store.resolve_task_attempt("a".into(),json!({"failed_tool":"failed-0","replacement_tool":"failed-1","note":"still failed"})).await.is_err());
        let link=store.resolve_task_attempt("a".into(),json!({"failed_tool":"failed-0","replacement_tool":"unrelated","note":"Agent explicitly states the replacement addresses its previous mistaken path"})).await.unwrap();assert_eq!(link["assessment"],"model_assessed_resolution");assert_eq!(link["requirement_disposition_unchanged"],true);
        assert_eq!(store.completion_state("a".into()).await.unwrap()["unresolved_attempts"],169);
        save(&store,"a",write("corrected-exact","missing-1.txt","succeeded")).await;assert_eq!(store.completion_state("a".into()).await.unwrap()["unresolved_attempts"],168);
        drop(store);let store=crate::task_store::Store::open(&root).unwrap();assert_eq!(store.completion_state("a".into()).await.unwrap()["unresolved_attempts"],168);assert_eq!(store.tool_page("a".into(),None).await.unwrap()["tools"].as_array().unwrap().len(),32);
        let preserved=store.access(|conn|Ok(conn.query_row("SELECT json_extract(payload,'$.status') FROM run_tools WHERE run='a' AND tool_id='failed-0'",[],|r|r.get::<_,String>(0))?)).await.unwrap();assert_eq!(preserved,"failed");
        store.delete_conversation("chat".into()).await.unwrap();drop(store);std::fs::remove_dir_all(root).unwrap();
    }
}
