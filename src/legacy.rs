//! Preserve pre-journal user requests before a continuation creates a new Task.
//! Historical assistant claims are not execution receipts and are never replayed.
use crate::{api::Conversation,task_store::Store};
use anyhow::Result;
use rusqlite::params;
use serde_json::json;
use std::collections::HashMap;
use sha2::{Digest,Sha256};
struct KnownRequest{id:String,request:Option<String>,start:Option<chrono::DateTime<chrono::FixedOffset>>,end:Option<chrono::DateTime<chrono::FixedOffset>>}
fn signature(content:&str,attachments:&serde_json::Value)->String{format!("{:x}",Sha256::digest(json!([content,attachments]).to_string().as_bytes()))}
pub fn install(conn:&rusqlite::Connection)->Result<()>{
    conn.execute_batch("CREATE TABLE IF NOT EXISTS legacy_task_imports(task TEXT PRIMARY KEY REFERENCES work_tasks(id) ON DELETE CASCADE,conversation TEXT NOT NULL,request_count INTEGER NOT NULL,original_workspace TEXT NOT NULL,created TEXT NOT NULL);")?;Ok(())
}
fn substantive(content:&str)->bool{
    let text=content.trim().trim_end_matches(['!','.','?']).to_lowercase();
    !text.is_empty()&&!["привет","здравствуйте","спасибо","ок","окей","хорошо","понял","hello","hi","thanks","thank you","ok","okay"].contains(&text.as_str())
}
impl Store{
    pub async fn import_legacy_requests(&self,chat:&Conversation,project:String)->Result<bool>{
        let conversation=chat.id.clone();let workspace=chat.history_workspace.clone();
        let check=conversation.clone();if self.access(move|conn|Ok(conn.query_row("SELECT EXISTS(SELECT 1 FROM work_tasks WHERE conversation=?1)",[check],|row|row.get::<_,bool>(0))?)).await?{return Ok(false);}
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
