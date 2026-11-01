//! Immutable UTF-8 observations, independently paged and scoped to their source run.
use anyhow::{Context,Result};
use rusqlite::{params,Connection,OptionalExtension};
use serde_json::{json,Value};
use sha2::{Digest,Sha256};
use crate::task_store::{Store,MissingResource};

pub fn install(conn:&mut Connection)->Result<()> {
    conn.execute_batch("CREATE TABLE IF NOT EXISTS artifact_bodies(hash TEXT PRIMARY KEY,bytes INTEGER NOT NULL,characters INTEGER NOT NULL);
        CREATE TABLE IF NOT EXISTS artifact_chunks(id INTEGER PRIMARY KEY AUTOINCREMENT,hash TEXT NOT NULL REFERENCES artifact_bodies(hash) ON DELETE CASCADE,part INTEGER NOT NULL,char_start INTEGER NOT NULL,characters INTEGER NOT NULL,text TEXT NOT NULL,UNIQUE(hash,part));
        CREATE INDEX IF NOT EXISTS artifact_seek ON artifact_chunks(hash,char_start);
        CREATE TABLE IF NOT EXISTS artifacts(sequence INTEGER PRIMARY KEY AUTOINCREMENT,id TEXT NOT NULL UNIQUE,run TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE,kind TEXT NOT NULL,metadata TEXT NOT NULL,hash TEXT NOT NULL REFERENCES artifact_bodies(hash),created TEXT NOT NULL);
        CREATE INDEX IF NOT EXISTS artifacts_run ON artifacts(run,created);
        CREATE INDEX IF NOT EXISTS artifacts_hash ON artifacts(hash);
        CREATE VIRTUAL TABLE IF NOT EXISTS artifact_fts USING fts5(text,content='artifact_chunks',content_rowid='id',tokenize='unicode61');
        CREATE TRIGGER IF NOT EXISTS artifact_insert AFTER INSERT ON artifact_chunks BEGIN INSERT INTO artifact_fts(rowid,text) VALUES(new.id,new.text); END;
        CREATE TRIGGER IF NOT EXISTS artifact_delete AFTER DELETE ON artifact_chunks BEGIN INSERT INTO artifact_fts(artifact_fts,rowid,text) VALUES('delete',old.id,old.text); END;")?;
    Ok(())
}
impl Store {
    pub async fn put_artifact(&self,run:String,kind:String,text:String,metadata:Value)->Result<Value> {
        // Hash outside the connection lock. A large observation must not turn
        // CPU hashing into a lock on cancellation/status bookkeeping.
        let (text,hash)=tokio::task::spawn_blocking(move||{let hash=format!("{:x}",Sha256::digest(text.as_bytes()));(text,hash)}).await?;
        self.access(move|conn|{
            let tx=conn.transaction()?;
            let exists:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM runs WHERE id=?1)",[&run],|r|r.get(0))?;
            anyhow::ensure!(exists,"Источник артефакта не найден");
            let characters=text.chars().count();
            let inserted=tx.execute("INSERT OR IGNORE INTO artifact_bodies VALUES (?1,?2,?3)",params![hash,i64::try_from(text.len())?,i64::try_from(characters)?])?;
            if inserted>0 {
                let mut start=0;let mut char_start=0i64;let mut part=0i64;
                while start<text.len(){let mut end=(start+16384).min(text.len());while !text.is_char_boundary(end){end-=1;}let block=&text[start..end];let count=block.chars().count() as i64;
                    tx.execute("INSERT INTO artifact_chunks(hash,part,char_start,characters,text) VALUES (?1,?2,?3,?4,?5)",params![hash,part,char_start,count,block])?;
                    start=end;char_start+=count;part+=1;
                }
            }
            let id=uuid::Uuid::new_v4().to_string();let created=crate::api::now();
            tx.execute("INSERT INTO artifacts(id,run,kind,metadata,hash,created) VALUES (?1,?2,?3,?4,?5,?6)",params![id,run,kind,metadata.to_string(),hash,created])?;
            tx.commit()?;
            Ok(json!({"id":id,"run":run,"kind":kind,"sha256":hash,"bytes":text.len(),"characters":characters,"metadata":metadata,"created":created,"immutable":true}))
        }).await
    }
    pub async fn artifact(&self,id:String)->Result<Value>{
        self.access(move|conn|descriptor(conn,&id)).await
    }
    pub async fn artifact_page(&self,id:String,offset:usize)->Result<Value>{
        self.access(move|conn|{
            let descriptor=descriptor(conn,&id)?;let hash=descriptor["sha256"].as_str().context("Нет хеша артефакта")?;
            let offset=i64::try_from(offset)?;let mut text=String::new();
            let mut query=conn.prepare("SELECT char_start,text FROM artifact_chunks WHERE hash=?1 AND char_start>=COALESCE((SELECT char_start FROM artifact_chunks WHERE hash=?1 AND char_start<=?2 ORDER BY char_start DESC LIMIT 1),0) ORDER BY char_start LIMIT 5")?;
            let rows=query.query_map(params![hash,offset],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?)))?;
            let mut remaining=8000usize;
            for row in rows {let(start,block)=row?;let skip=(offset-start).max(0) as usize;let part:String=block.chars().skip(skip).take(remaining).collect();remaining-=part.chars().count();text.push_str(&part);if remaining==0{break;}}
            let next=(offset as u64).checked_add(text.chars().count() as u64).context("Переполнение смещения")?;
            Ok(json!({"artifact":descriptor,"text":text,"offset":offset,"next_offset":next,"has_more":next<descriptor["characters"].as_u64().unwrap_or(0),"offset_unit":"unicode_scalar"}))
        }).await
    }
    pub async fn artifact_owned(&self,id:String,run:String)->Result<bool>{
        self.access(move|conn|{
            Ok(conn.query_row("SELECT EXISTS(SELECT 1 FROM artifacts a WHERE a.id=?1 AND (a.run=?2 OR EXISTS(SELECT 1 FROM task_runs owner JOIN task_runs current ON owner.task=current.task WHERE owner.run=a.run AND current.run=?2)))",params![id,run],|r|r.get(0))?)
        }).await
    }
    pub async fn artifact_list(&self,run:String,before:Option<i64>)->Result<Value>{
        anyhow::ensure!(before.is_none_or(|value|value>0),"Курсор должен быть положительным");
        self.access(move|conn|{
            let mut statement=conn.prepare("SELECT a.sequence,a.id,a.kind,a.metadata,b.characters,a.hash FROM artifacts a JOIN artifact_bodies b ON b.hash=a.hash WHERE a.sequence<?2 AND a.kind NOT IN ('tool_arguments','derived_result') AND (a.run=?1 OR EXISTS(SELECT 1 FROM task_runs owner JOIN task_runs current ON owner.task=current.task WHERE owner.run=a.run AND current.run=?1)) ORDER BY a.sequence DESC LIMIT 33")?;
            let mut rows=statement.query_map(params![run,before.unwrap_or(i64::MAX)],|r|Ok(json!({"sequence":r.get::<_,i64>(0)?,"id":r.get::<_,String>(1)?,"kind":r.get::<_,String>(2)?,"metadata":serde_json::from_str::<Value>(&r.get::<_,String>(3)?).unwrap_or(Value::Null),"characters":r.get::<_,i64>(4)?,"sha256":r.get::<_,String>(5)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
            let more=rows.len()>32;if more{rows.truncate(32);}let next=rows.last().and_then(|value|value["sequence"].as_i64());
            Ok(json!({"items":rows,"has_more":more,"next_before":next}))
        }).await
    }
    pub async fn artifact_search(&self,run:String,query:String,after:i64,snapshot:Option<Value>)->Result<Value>{
        anyhow::ensure!(query.len()<=8192,"Слишком длинный запрос индекса; разделите его на отдельные вопросы");
        let terms:Vec<_>=query.split(|c:char|!c.is_alphanumeric()&&c!='_').filter(|part|!part.is_empty()).map(|part|format!("\"{part}\"")).collect();
        anyhow::ensure!(after>=0,"Курсор должен быть неотрицательным");
        anyhow::ensure!(!terms.is_empty(),"Введите слова для поиска");let expression=terms.join(" OR ");
        self.access(move|conn|{
            let snapshot=match snapshot {Some(value)=>value,None=>json!({"chunk":conn.query_row("SELECT COALESCE(MAX(id),0) FROM artifact_chunks",[],|r|r.get::<_,i64>(0))?,"reference":conn.query_row("SELECT COALESCE(MAX(sequence),0) FROM artifacts",[],|r|r.get::<_,i64>(0))?})};
            let chunk=snapshot["chunk"].as_i64().context("Неверная граница блоков")?;let reference=snapshot["reference"].as_i64().context("Неверная граница источников")?;
            anyhow::ensure!(chunk>=0&&reference>=0,"Граница индекса должна быть неотрицательной");
            let mut statement=conn.prepare("WITH accessible AS (SELECT a.* FROM artifacts a WHERE a.kind NOT IN ('tool_arguments','derived_result') AND a.sequence<=?5 AND (a.run=?4 OR EXISTS(SELECT 1 FROM task_runs owner JOIN task_runs current ON owner.task=current.task WHERE owner.run=a.run AND current.run=?4))), chosen AS (SELECT hash,MIN(id) AS id FROM accessible GROUP BY hash) SELECT c.id,a.id,c.part,c.char_start,snippet(artifact_fts,0,'','',' … ',48),bm25(artifact_fts),a.metadata FROM artifact_fts JOIN artifact_chunks c ON c.id=artifact_fts.rowid JOIN chosen selected ON selected.hash=c.hash JOIN artifacts a ON a.id=selected.id WHERE artifact_fts MATCH ?1 AND artifact_fts.rowid>?2 AND artifact_fts.rowid<=?3 ORDER BY c.id LIMIT 33")?;
            let mut rows=statement.query_map(params![expression,after,chunk,run,reference],|r|Ok(json!({"chunk":r.get::<_,i64>(0)?,"artifact_id":r.get::<_,String>(1)?,"part":r.get::<_,i64>(2)?,"offset":r.get::<_,i64>(3)?,"snippet":r.get::<_,String>(4)?,"score":r.get::<_,f64>(5)?,"metadata":serde_json::from_str::<Value>(&r.get::<_,String>(6)?).unwrap_or(Value::Null)})))?.collect::<rusqlite::Result<Vec<_>>>()?;
            let more=rows.len()>32;if more{rows.truncate(32);}
            let next=rows.last().and_then(|row|row["chunk"].as_i64()).unwrap_or(after);
            Ok(json!({"results":rows,"next_after":next,"snapshot":snapshot,"has_more":more,"order":"chunk_cursor","ranking":"bm25_hint","deduplicated_by":"sha256"}))
        }).await
    }
}
fn descriptor(conn:&Connection,id:&str)->Result<Value>{
    let row=conn.query_row("SELECT a.run,a.kind,a.metadata,a.hash,a.created,b.bytes,b.characters FROM artifacts a JOIN artifact_bodies b ON b.hash=a.hash WHERE a.id=?1",[id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,i64>(5)?,r.get::<_,i64>(6)?))).optional()?.ok_or(MissingResource("Сохранённый материал не найден"))?;
    Ok(json!({"id":id,"run":row.0,"kind":row.1,"metadata":serde_json::from_str::<Value>(&row.2)?,"sha256":row.3,"created":row.4,"bytes":row.5,"characters":row.6,"immutable":true}))
}

fn compact_value(value:&Value,budget:&mut usize)->Value {
    if *budget==0 {return json!("[Часть сохранена отдельно]");}
    match value {
        Value::String(text)=>{let limit=512.min(*budget);let part=crate::tools::truncate(text,limit);*budget=budget.saturating_sub(part.chars().count());json!(part)},
        Value::Array(items)=>{let mut result=vec![];for item in items.iter().take(32){if *budget==0 {break;}result.push(compact_value(item,budget));}json!(result)},
        Value::Object(object)=>{
            let mut result=serde_json::Map::new();
            // Identity and trusted mutation/process facts survive preview reduction.
            for key in ["status","tool_status","changed","verified","path","source","destination","change_id","execution_id","exit_code","start_line","end_line","has_more","next_start_line","next_offset","complete","url","title","format","results","coverage","live_testing"] {
                if let Some(value)=object.get(key){let value=if ["path","source","destination","url","change_id","execution_id"].contains(&key)&&value.is_string(){value.clone()}else{compact_value(value,budget)};result.insert(key.into(),value);}
            }
            for(key,value)in object {if result.contains_key(key){continue;}if *budget==0{break;}result.insert(key.clone(),compact_value(value,budget));}
            Value::Object(result)
        },
        _=>{*budget=budget.saturating_sub(8);value.clone()}
    }
}
pub async fn archive_record(app:&crate::api::Shared,run:&str,mut record:Value)->Result<Value>{
    let name=record["name"].as_str().unwrap_or("").to_string();let text=record["result"].as_str().unwrap_or("").to_string();
    let derived=["read_task","search_artifacts","update_plan","security_analyze","translate_text"].contains(&name.as_str());
    let source=if name=="read_artifact"&&record["status"]=="succeeded" {app.tasks.artifact(record["arguments"]["id"].as_str().unwrap_or("").into()).await?}else{
        let parsed=serde_json::from_str::<Value>(&text).ok();
        let coverage=parsed.as_ref().map(|value|json!({"start_line":value["start_line"],"end_line":value["end_line"],"has_more":value["has_more"],"next_start_line":value["next_start_line"],"next_offset":value["next_offset"],"complete":value["complete"],"start_page":value["start_page"],"end_page":value["end_page"],"total_pages":value["total_pages"]}));
        app.tasks.put_artifact(run.into(),if derived {"derived_result"}else{"tool_result"}.into(),text.clone(),json!({"tool_id":record["id"],"tool_name":name,"status":record["status"],"path":parsed.as_ref().map(|value|&value["path"]),"url":parsed.as_ref().map(|value|&value["url"]),"coverage":coverage,"origin":if derived {"derived"}else{"tool_observation"}})).await?
    };
    record["result_artifact"]=source;
    if text.len()>16384 {
        let preview=if let Ok(value)=serde_json::from_str::<Value>(&text){let mut budget=2500;let mut value=compact_value(&value,&mut budget);if let Some(object)=value.as_object_mut(){object.insert("preview_only".into(),json!(true));}value.to_string()}else{crate::tools::truncate(&text,4000)};
        record["result"]=json!(preview);record["result_preview_only"]=json!(true);
    }
    let arguments=record["arguments"].to_string();
    if arguments.len()>8192 {
        if !["browser_action","write_stdin"].contains(&name.as_str()){
            record["arguments_artifact"]=app.tasks.put_artifact(run.into(),"tool_arguments".into(),arguments,json!({"tool_id":record["id"],"tool_name":name,"origin":"tool_arguments","searchable":false})).await?;
        }
        let mut budget=1500;record["arguments"]=compact_value(&record["arguments"],&mut budget);record["arguments_preview_only"]=json!(true);
    }
    Ok(record)
}
pub fn context_reference(context:String,artifact:&Value)->String {
    let reference=json!({"id":artifact["id"],"sha256":artifact["sha256"],"characters":artifact["characters"],"read_tool":"read_artifact"});
    if let Ok(mut value)=serde_json::from_str::<Value>(&context) {if let Some(object)=value.as_object_mut(){object.insert("artifact".into(),reference);return value.to_string();}}
    format!("{context}\nSTORED_RESULT: {reference}")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn unicode_pages_hashes_scopes_and_shared_body_deletion_are_consistent(){
        let dir=std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());std::fs::create_dir(&dir).unwrap();let store=Store::open(&dir).unwrap();
        store.begin("a".into(),"chat-a".into(),json!({"mode":"agent","content":"Analyze source"})).await.unwrap();store.begin("b".into(),"chat-b".into(),json!({"mode":"agent","content":"Another source"})).await.unwrap();
        let text="Документ 日本語 🦀\r\nMATERIAL_NEEDLE\n".repeat(3000);let expected=format!("{:x}",Sha256::digest(text.as_bytes()));
        let first=store.put_artifact("a".into(),"source".into(),text.clone(),json!({"url":"https://first.invalid/"})).await.unwrap();let second=store.put_artifact("b".into(),"source".into(),text.clone(),json!({"url":"https://second.invalid/"})).await.unwrap();
        assert_eq!(first["sha256"],expected);assert_eq!(second["sha256"],expected);assert_ne!(first["id"],second["id"]);
        let id=first["id"].as_str().unwrap().to_string();assert!(!store.artifact_owned(id.clone(),"b".into()).await.unwrap());
        let mut offset=0;let mut actual=String::new();loop{let page=store.artifact_page(id.clone(),offset).await.unwrap();actual.push_str(page["text"].as_str().unwrap());if page["has_more"]!=true {break;}offset=page["next_offset"].as_u64().unwrap() as usize;}assert_eq!(actual,text);
        let found=store.artifact_search("a".into(),"MATERIAL_NEEDLE".into(),0,None).await.unwrap();assert!(!found["results"].as_array().unwrap().is_empty());assert!(found["results"].as_array().unwrap().iter().all(|hit|hit["artifact_id"]==id));
        store.delete_conversation("chat-a".into()).await.unwrap();assert!(store.artifact(id).await.is_err());let second_id=second["id"].as_str().unwrap().to_string();assert!(store.artifact_page(second_id.clone(),0).await.is_ok());
        store.delete_conversation("chat-b".into()).await.unwrap();assert!(store.artifact(second_id).await.is_err());assert!(store.artifact_search("b".into(),"MATERIAL_NEEDLE".into(),0,None).await.unwrap()["results"].as_array().unwrap().is_empty());
        drop(store);std::fs::remove_dir_all(dir).unwrap();
    }
    #[tokio::test]
    async fn search_cursor_survives_duplicates_and_new_sources(){
        let dir=std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());std::fs::create_dir(&dir).unwrap();let store=Store::open(&dir).unwrap();store.begin("r".into(),"c".into(),json!({"mode":"agent","content":"Index"})).await.unwrap();
        let text="needle ".repeat(100000);for _ in 0..40{store.put_artifact("r".into(),"source".into(),text.clone(),json!({})).await.unwrap();}
        let first=store.artifact_search("r".into(),"needle".into(),0,None).await.unwrap();assert_eq!(first["results"].as_array().unwrap().len(),32);assert_eq!(first["has_more"],true);
        let new=store.put_artifact("r".into(),"source".into(),"new needle document".into(),json!({})).await.unwrap();let mut cursor=first["next_after"].as_i64().unwrap();let mut seen=first["results"].as_array().unwrap().iter().map(|hit|hit["chunk"].as_i64().unwrap()).collect::<std::collections::BTreeSet<_>>();
        loop{let page=store.artifact_search("r".into(),"needle".into(),cursor,Some(first["snapshot"].clone())).await.unwrap();for hit in page["results"].as_array().unwrap(){assert_ne!(hit["artifact_id"],new["id"]);assert!(seen.insert(hit["chunk"].as_i64().unwrap()));}if page["has_more"]!=true {break;}cursor=page["next_after"].as_i64().unwrap();}
        assert!(seen.len()>32);drop(store);std::fs::remove_dir_all(dir).unwrap();
    }
}
