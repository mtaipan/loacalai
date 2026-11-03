//! Preserve pre-journal user requests before a continuation creates a new Task.
//! Historical assistant claims are not execution receipts and are never replayed.
use crate::{api::Conversation,task_store::Store};
use anyhow::Result;
use rusqlite::{params,OptionalExtension};
use serde_json::json;
use std::collections::HashMap;
use sha2::{Digest,Sha256};
struct KnownRequest{id:String,request:Option<String>,start:Option<chrono::DateTime<chrono::FixedOffset>>,end:Option<chrono::DateTime<chrono::FixedOffset>>}
fn signature(content:&str,attachments:&serde_json::Value)->String{format!("{:x}",Sha256::digest(json!([content,attachments]).to_string().as_bytes()))}
pub fn install(conn:&rusqlite::Connection)->Result<()>{
    conn.execute_batch("CREATE TABLE IF NOT EXISTS legacy_task_imports(task TEXT PRIMARY KEY REFERENCES work_tasks(id) ON DELETE CASCADE,conversation TEXT NOT NULL,request_count INTEGER NOT NULL,original_workspace TEXT NOT NULL,created TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS legacy_index_imports(conversation TEXT PRIMARY KEY REFERENCES chat_conversations(id) ON DELETE CASCADE,revision INTEGER NOT NULL,project TEXT NOT NULL,workspace TEXT NOT NULL,cursor INTEGER NOT NULL DEFAULT 9223372036854775807);
CREATE TABLE IF NOT EXISTS legacy_index_candidates(conversation TEXT NOT NULL REFERENCES legacy_index_imports(conversation) ON DELETE CASCADE,sequence INTEGER NOT NULL,run TEXT NOT NULL,message TEXT NOT NULL,content TEXT NOT NULL,attachments TEXT NOT NULL,details TEXT NOT NULL,created TEXT NOT NULL,substantive INTEGER NOT NULL,PRIMARY KEY(conversation,sequence));
CREATE TABLE IF NOT EXISTS legacy_index_matches(conversation TEXT NOT NULL REFERENCES legacy_index_imports(conversation) ON DELETE CASCADE,run TEXT NOT NULL,PRIMARY KEY(conversation,run));")?;Ok(())
}
fn substantive(content:&str)->bool{
    let text=content.trim().trim_end_matches(['!','.','?']).to_lowercase();
    !text.is_empty()&&!["привет","здравствуйте","спасибо","ок","окей","хорошо","понял","hello","hi","thanks","thank you","ok","okay"].contains(&text.as_str())
}
impl Store{
    pub async fn needs_legacy_import(&self,conversation:String)->Result<bool>{
        self.access(move|conn|Ok(!conn.query_row("SELECT EXISTS(SELECT 1 FROM work_tasks WHERE conversation=?1)",[conversation],|row|row.get::<_,bool>(0))?)).await
    }
    /// Read one indexed user message at a time. Staging is private until publication.
    pub async fn import_indexed_legacy_requests(&self,conversation:String,project:String)->Result<bool>{
        if !self.needs_legacy_import(conversation.clone()).await?{return Ok(false);}
        let id=conversation.clone();let scope=project.clone();
        self.access(move|db|{
            let tx=db.transaction()?;
            let revision:i64=tx.query_row("SELECT revision FROM chat_conversations WHERE id=?1 AND state='ready'",[&id],|r|r.get(0))?;
            let old:Option<(i64,String)>=tx.query_row("SELECT revision,project FROM legacy_index_imports WHERE conversation=?1",[&id],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
            if old.is_some_and(|(v,p)|v!=revision||p!=scope){tx.execute("DELETE FROM legacy_index_imports WHERE conversation=?1",[&id])?;}
            let workspace=tx.query_row("SELECT workspace FROM chat_working WHERE conversation=?1",[&id],|r|r.get::<_,String>(0)).optional()?.unwrap_or_default();
            tx.execute("INSERT OR IGNORE INTO legacy_index_imports(conversation,revision,project,workspace) VALUES(?1,?2,?3,?4)",params![id,revision,scope,workspace])?;
            tx.commit()?;Ok(())
        }).await?;
        loop {
            let id=conversation.clone();
            let more=self.access(move|db|{
                let (cursor,revision):(i64,i64)=db.query_row("SELECT cursor,revision FROM legacy_index_imports WHERE conversation=?1",[&id],|r|Ok((r.get(0)?,r.get(1)?)))?;
                anyhow::ensure!(!db.query_row("SELECT EXISTS(SELECT 1 FROM deleted_conversations WHERE id=?1)",[&id],|r|r.get::<_,bool>(0))?,"Чат удалён");
                let current:i64=db.query_row("SELECT revision FROM chat_conversations WHERE id=?1",[&id],|r|r.get(0))?;
                anyhow::ensure!(revision==current,"Чат изменился во время импорта прежних требований; повторите запрос");
                let next:Option<(i64,String,String,String,String)>=db.query_row("SELECT m.display_seq,m.original_id,m.created,m.content,m.metadata FROM chat_messages m JOIN chat_attempts a ON a.id=m.attempt AND a.active=1 WHERE m.conversation=?1 AND m.role='user' AND m.display_seq<?2 ORDER BY m.display_seq DESC LIMIT 1",params![id,cursor],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional()?;
                let Some((sequence,message,created,body,metadata))=next else{return Ok(false);};
                let content=crate::chat_store::whole(db,&body)?;
                let metadata:serde_json::Value=serde_json::from_str(&metadata)?;
                let details=metadata.get("attachments").cloned().unwrap_or_else(||json!([]));
                let attachments=json!(details.as_array().into_iter().flatten().filter_map(|item|item["id"].as_str()).collect::<Vec<_>>());
                let created_at=chrono::DateTime::parse_from_rfc3339(&created).ok();
                let matched={
                    let mut query=db.prepare("SELECT r.id,json_extract(r.input,'$.request_id'),r.created,r.updated FROM runs r WHERE r.conversation=?1 AND json_extract(r.input,'$.mode') IS NOT 'legacy' AND json_extract(r.input,'$.content')=?2 AND CASE WHEN json_type(r.input,'$.attachments') IS NULL THEN '[]' ELSE json_extract(r.input,'$.attachments') END=?3 AND NOT EXISTS(SELECT 1 FROM legacy_index_matches k WHERE k.conversation=?1 AND k.run=r.id) ORDER BY r.created DESC,r.id DESC")?;
                    let mut rows=query.query(params![id,content,attachments.to_string()])?;let mut matched=None;
                    while let Some(row)=rows.next()?{
                        let run:String=row.get(0)?;let request:Option<String>=row.get(1)?;let start:String=row.get(2)?;let end:String=row.get(3)?;
                        let span=chrono::DateTime::parse_from_rfc3339(&start).ok().zip(chrono::DateTime::parse_from_rfc3339(&end).ok());
                        if run==message||request.as_deref()==Some(message.as_str())||created_at.is_some_and(|at|span.is_some_and(|(start,end)|at>=start&&at<=end)){matched=Some(run);break;}
                    }matched
                };
                let tx=db.transaction()?;
                if let Some(run)=matched{tx.execute("INSERT INTO legacy_index_matches VALUES(?1,?2)",params![id,run])?;}
                else {tx.execute("INSERT INTO legacy_index_candidates VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",params![id,sequence,uuid::Uuid::new_v4().to_string(),message,content,attachments.to_string(),details.to_string(),created,substantive(&content)||details.as_array().is_some_and(|v|!v.is_empty())])?;}
                tx.execute("UPDATE legacy_index_imports SET cursor=?1 WHERE conversation=?2",params![sequence,id])?;
                tx.commit()?;Ok(true)
            }).await?;
            if !more{break;}
        }
        self.access(move|db|{
            let tx=db.transaction()?;
            anyhow::ensure!(!tx.query_row("SELECT EXISTS(SELECT 1 FROM deleted_conversations WHERE id=?1)",[&conversation],|r|r.get::<_,bool>(0))?,"Чат удалён");
            let stale:bool=tx.query_row("SELECT i.revision<>c.revision FROM legacy_index_imports i JOIN chat_conversations c ON c.id=i.conversation WHERE i.conversation=?1",[&conversation],|r|r.get(0))?;
            anyhow::ensure!(!stale,"Чат изменился во время импорта прежних требований; повторите запрос");
            let established:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM work_tasks WHERE conversation=?1)",[&conversation],|r|r.get(0))?;
            let has_goal:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM legacy_index_candidates WHERE conversation=?1 AND substantive=1)",[&conversation],|r|r.get(0))?;
            if established||!has_goal {tx.execute("DELETE FROM legacy_index_imports WHERE conversation=?1",[&conversation])?;tx.commit()?;return Ok(false);}
            let task=uuid::Uuid::new_v4().to_string();let now=crate::api::now();
            tx.execute("INSERT INTO work_tasks SELECT ?1,?2,?3,CASE WHEN trim(content)='' THEN 'Вложения из прежнего чата' ELSE content END,'open',?4,?4 FROM legacy_index_candidates WHERE conversation=?2 AND substantive=1 ORDER BY sequence LIMIT 1",params![task,conversation,project,now])?;
            tx.execute("INSERT INTO runs SELECT run,conversation,json_object('mode','legacy','content',content,'attachments',json(attachments),'legacy_message_id',message,'legacy_attachment_details',json(details),'original_created',created,'origin','legacy_json','effects_verified',json('false')),'legacy_imported',?2,?2 FROM legacy_index_candidates WHERE conversation=?1 ORDER BY sequence",params![conversation,now])?;
            tx.execute("INSERT INTO task_runs SELECT run,?2 FROM legacy_index_candidates WHERE conversation=?1 ORDER BY sequence",params![conversation,task])?;
            tx.execute("INSERT INTO task_requests(task,run,content,attachments) SELECT ?2,run,content,attachments FROM legacy_index_candidates WHERE conversation=?1 ORDER BY sequence",params![conversation,task])?;
            tx.execute("INSERT INTO legacy_task_imports SELECT ?2,?1,(SELECT COUNT(*) FROM legacy_index_candidates WHERE conversation=?1),workspace,?3 FROM legacy_index_imports WHERE conversation=?1",params![conversation,task,now])?;
            tx.execute("DELETE FROM legacy_index_imports WHERE conversation=?1",[&conversation])?;
            tx.commit()?;Ok(true)
        }).await
    }
    pub async fn import_legacy_requests(&self,chat:&Conversation,project:String)->Result<bool>{
        let conversation=chat.id.clone();let workspace=chat.history_workspace.clone();
        if !self.needs_legacy_import(conversation.clone()).await?{return Ok(false);}
        let check=conversation.clone();let mut known=self.access(move|conn|{
            let mut requests=HashMap::<String,Vec<KnownRequest>>::new();let mut query=conn.prepare("SELECT id,input,created,updated FROM runs WHERE conversation=?1 ORDER BY created DESC,id DESC")?;
            for row in query.query_map([check],|row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,String>(2)?,row.get::<_,String>(3)?)))?{
                let(id,input,start,end)=row?;let input:serde_json::Value=serde_json::from_str(&input)?;if input["mode"]=="legacy"{continue;}
                let key=signature(input["content"].as_str().unwrap_or(""),input.get("attachments").unwrap_or(&json!([])));
                requests.entry(key).or_default().push(KnownRequest{id,request:input["request_id"].as_str().map(str::to_owned),start:chrono::DateTime::parse_from_rfc3339(&start).ok(),end:chrono::DateTime::parse_from_rfc3339(&end).ok()});
            }Ok(requests)
        }).await?;
        let mut rows=vec![];
        for message in chat.messages.iter().rev().filter(|message|message.role=="user"){
            let attachments=json!(message.attachments.iter().map(|item|item.id.clone()).collect::<Vec<_>>());let key=signature(&message.content,&attachments);let created=chrono::DateTime::parse_from_rfc3339(&message.created).ok();
            if let Some(matches)=known.get_mut(&key){if let Some(index)=matches.iter().position(|item|item.id==message.id||item.request.as_deref()==Some(&message.id)||created.is_some_and(|at|item.start.zip(item.end).is_some_and(|(start,end)|at>=start&&at<=end))){matches.remove(index);continue;}}
            rows.push(json!({"id":message.id,"content":message.content,"attachments":attachments,"attachment_details":message.attachments,"created":message.created}));
        }
        rows.reverse();
        let Some(first)=rows.iter().find(|row|substantive(row["content"].as_str().unwrap_or(""))||row["attachments"].as_array().is_some_and(|items|!items.is_empty())) else{return Ok(false);};
        let goal=if first["content"].as_str().unwrap_or("").trim().is_empty(){"Вложения из прежнего чата".to_string()}else{first["content"].as_str().unwrap().to_string()};
        self.access(move|conn|{
            let tx=conn.transaction()?;
            anyhow::ensure!(!tx.query_row("SELECT EXISTS(SELECT 1 FROM deleted_conversations WHERE id=?1)",[&conversation],|row|row.get::<_,bool>(0))?,"Чат удалён");
            // Do not reclassify or merge an already established Task boundary.
            if tx.query_row("SELECT EXISTS(SELECT 1 FROM work_tasks WHERE conversation=?1)",[&conversation],|row|row.get::<_,bool>(0))?{return Ok(false);}
            let task=uuid::Uuid::new_v4().to_string();let now=crate::api::now();
            tx.execute("INSERT INTO work_tasks VALUES (?1,?2,?3,?4,'open',?5,?5)",params![task,conversation,project,goal,now])?;
            for row in &rows{
                let run=uuid::Uuid::new_v4().to_string();
                let input=json!({"mode":"legacy","content":row["content"],"attachments":row["attachments"],"legacy_message_id":row["id"],"legacy_attachment_details":row["attachment_details"],"original_created":row["created"],"origin":"legacy_json","effects_verified":false});
                tx.execute("INSERT INTO runs VALUES (?1,?2,?3,'legacy_imported',?4,?4)",params![run,conversation,input.to_string(),now])?;
                tx.execute("INSERT INTO task_runs VALUES (?1,?2)",params![run,task])?;
                tx.execute("INSERT INTO task_requests(task,run,content,attachments) VALUES (?1,?2,?3,?4)",params![task,run,row["content"].as_str().unwrap_or(""),row["attachments"].to_string()])?;
            }
            tx.execute("INSERT INTO legacy_task_imports VALUES (?1,?2,?3,?4,?5)",params![task,conversation,i64::try_from(rows.len())?,workspace,now])?;
            tx.commit()?;Ok(true)
        }).await
    }
}
#[cfg(test)]
mod tests{
    use super::*;
    #[tokio::test]
    async fn indexed_import_keeps_requirements_order_matching_and_provenance(){
        let root=std::env::temp_dir().join(format!("indexed-legacy-{}",uuid::Uuid::new_v4()));std::fs::create_dir_all(root.join("chats")).unwrap();let store=Store::open(&root).unwrap();let id=uuid::Uuid::new_v4().to_string();
        let known=uuid::Uuid::new_v4().to_string();store.begin(known.clone(),id.clone(),json!({"mode":"chat","content":"ты кто","attachments":[]})).await.unwrap();
        let goal="Build the original Unicode 🦀 project and test it.\r\n".repeat(2000);
        let messages=json!([
            {"id":"greeting","role":"user","content":"Привет","created":"old"},
            {"id":"goal","role":"user","content":goal,"created":"old"},
            {"id":"claim","role":"assistant","content":"Everything passed".repeat(100000),"created":"old"},
            {"id":known,"role":"user","content":"ты кто","created":"old"},
            {"id":"repeat-but-not-journaled","role":"user","content":"ты кто","created":"old"},
            {"id":"more","role":"user","content":"Preserve the second requirement","created":"old"}
        ]);
        let source=json!({"id":id,"title":"Legacy","mode":"agent","updated":"old","messages":messages,"history":[],"history_workspace":"original-workspace"});
        std::fs::write(root.join("chats").join(format!("{id}.json")),serde_json::to_vec(&source).unwrap()).unwrap();
        crate::chat_store::ensure_import(&root,&id,&tokio_util::sync::CancellationToken::new()).unwrap();
        assert!(store.import_indexed_legacy_requests(id.clone(),"accepted-workspace".into()).await.unwrap());
        assert!(!store.import_indexed_legacy_requests(id.clone(),"accepted-workspace".into()).await.unwrap());
        let chat=id.clone();let expected=goal.clone();
        store.access(move|db|{
            let(task,stored):(String,String)=db.query_row("SELECT id,goal FROM work_tasks WHERE conversation=?1",[&chat],|r|Ok((r.get(0)?,r.get(1)?)))?;assert_eq!(stored,expected);
            let mut q=db.prepare("SELECT json_extract(r.input,'$.legacy_message_id'),json_extract(r.input,'$.effects_verified') FROM task_requests t JOIN runs r ON r.id=t.run WHERE t.task=?1 ORDER BY t.sequence")?;
            let rows=q.query_map([&task],|r|Ok((r.get::<_,String>(0)?,r.get::<_,bool>(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
            assert_eq!(rows.iter().map(|r|r.0.as_str()).collect::<Vec<_>>(),vec!["greeting","goal","repeat-but-not-journaled","more"]);assert!(rows.iter().all(|r|!r.1));
            let workspace:String=db.query_row("SELECT original_workspace FROM legacy_task_imports WHERE task=?1",[task],|r|r.get(0))?;assert_eq!(workspace,"original-workspace");
            assert_eq!(db.query_row("SELECT COUNT(*) FROM legacy_index_imports",[],|r|r.get::<_,i64>(0))?,0);Ok(())
        }).await.unwrap();
        store.delete_conversation(id).await.unwrap();drop(store);std::fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn indexed_import_handles_attachment_goal_and_never_recreates_deleted_chat(){
        let root=std::env::temp_dir().join(format!("indexed-attachments-{}",uuid::Uuid::new_v4()));std::fs::create_dir_all(root.join("chats")).unwrap();let store=Store::open(&root).unwrap();let id=uuid::Uuid::new_v4().to_string();
        let source=json!({"id":id,"title":"Attachments","mode":"agent","updated":"old","messages":[{"id":"input","role":"user","content":"","attachments":[{"id":"file","name":"original.txt","path":"untrusted-original-path","kind":"text","size":12}],"created":"old"}],"history":[]});
        std::fs::write(root.join("chats").join(format!("{id}.json")),serde_json::to_vec(&source).unwrap()).unwrap();crate::chat_store::ensure_import(&root,&id,&tokio_util::sync::CancellationToken::new()).unwrap();
        assert!(store.import_indexed_legacy_requests(id.clone(),"project".into()).await.unwrap());
        let task=store.access(|db|Ok(db.query_row("SELECT id,goal FROM work_tasks",[],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?)))?)).await.unwrap();assert_eq!(task.1,"Вложения из прежнего чата");
        let record=store.access(|db|Ok(db.query_row("SELECT input FROM runs WHERE status='legacy_imported'",[],|r|r.get::<_,String>(0))?)).await.unwrap();let input:serde_json::Value=serde_json::from_str(&record).unwrap();assert_eq!(input["attachments"],json!(["file"]));assert_eq!(input["legacy_attachment_details"][0]["name"],"original.txt");
        store.delete_conversation(id.clone()).await.unwrap();assert!(store.import_indexed_legacy_requests(id,"project".into()).await.is_err());drop(store);std::fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn journaled_conversation_without_a_work_task_is_not_reimported_as_legacy(){
        let root=std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());std::fs::create_dir_all(&root).unwrap();let store=Store::open(&root).unwrap();
        store.begin("known".into(),"chat".into(),json!({"mode":"chat","content":"ты кто","attachments":[]})).await.unwrap();let created=crate::api::now();store.append("known".into(),json!({"type":"done","status":"completed"})).await.unwrap();
        let chat=Conversation{id:"chat".into(),title:"Chat".into(),mode:"chat".into(),updated:created.clone(),messages:vec![crate::api::Message{material:None,diagnostics:None,duration_ms:None,model:None,id:"different-message-id".into(),role:"user".into(),content:"ты кто".into(),attachments:vec![],tools:vec![],created}],history:vec![],history_model:String::new(),history_workspace:String::new(),history_task:String::new()};
        assert!(!store.import_legacy_requests(&chat,"project".into()).await.unwrap());store.begin_in_project("actual".into(),"chat".into(),json!({"mode":"agent","content":"Build the actual new project"}),"project".into()).await.unwrap();
        assert_eq!(store.task_for_run("actual".into()).await.unwrap().unwrap()["goal_preview"],"Build the actual new project");drop(store);std::fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn old_user_requirements_survive_continuation_without_importing_assistant_success_as_proof(){
        let root=std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());std::fs::create_dir_all(&root).unwrap();let store=Store::open(&root).unwrap();
        let goal="Build the original project. Preserve exact Unicode 🦀 requirements.\r\n".repeat(700);
        let message=|role:&str,content:String|crate::api::Message{material:None,diagnostics:None,duration_ms:None,model:None,id:uuid::Uuid::new_v4().to_string(),role:role.into(),content,attachments:vec![],tools:vec![],created:crate::api::now()};
        let chat=Conversation{id:"chat".into(),title:"Original".into(),mode:"agent".into(),updated:crate::api::now(),messages:vec![message("user","Привет".into()),message("user",goal.clone()),message("assistant","Everything passed; project complete".into()),message("user","Also preserve the second requirement".into())],history:vec![],history_model:String::new(),history_workspace:"old-workspace".into(),history_task:String::new()};
        assert!(store.import_legacy_requests(&chat,"current-workspace".into()).await.unwrap());assert!(!store.import_legacy_requests(&chat,"current-workspace".into()).await.unwrap());
        store.begin_in_project("continue".into(),"chat".into(),json!({"mode":"agent","content":"продолжай работу"}),"current-workspace".into()).await.unwrap();
        let task=store.task_for_run("continue".into()).await.unwrap().unwrap();let id=task["id"].as_str().unwrap().to_string();assert_eq!(task["goal_characters"],goal.chars().count());assert_eq!(task["requests"].as_array().unwrap().len(),4);assert_eq!(task["legacy_context"]["effects_verified"],false);assert_eq!(task["completion_verified"],false);
        let sequence=task["requests"][1]["sequence"].as_i64().unwrap();let mut offset=0;let mut text=String::new();loop{let page=store.task_request_slice(id.clone(),sequence,offset,1000).await.unwrap();text.push_str(page["text"].as_str().unwrap());offset=page["next_offset"].as_u64().unwrap() as usize;if page["has_more"]!=true{break;}}assert_eq!(text,goal);
        store.begin_in_project("new".into(),"chat".into(),json!({"mode":"agent","content":"New explicit task","new_task":true}),"different-workspace".into()).await.unwrap();assert_ne!(store.task_for_run("new".into()).await.unwrap().unwrap()["id"],id);
        drop(store);let store=Store::open(&root).unwrap();assert_eq!(store.task(id,0).await.unwrap()["legacy_context"]["request_count"],3);store.delete_conversation("chat".into()).await.unwrap();drop(store);std::fs::remove_dir_all(root).unwrap();
    }
}
