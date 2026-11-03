//! Validated document units survive failures. Scope and generation inputs are immutable.
use anyhow::Result;
use rusqlite::{params,OptionalExtension};
use serde_json::{json,Value};
use sha2::{Digest,Sha256};
pub fn hash(value:&Value)->Result<String>{Ok(format!("{:x}",Sha256::digest(serde_json::to_vec(value)?)))}
pub fn install(conn:&rusqlite::Connection)->Result<()>{
    conn.execute_batch("CREATE TABLE IF NOT EXISTS document_jobs(sequence INTEGER PRIMARY KEY AUTOINCREMENT,id TEXT UNIQUE NOT NULL,owner TEXT NOT NULL,definition TEXT NOT NULL,total INTEGER NOT NULL,created TEXT NOT NULL);CREATE INDEX IF NOT EXISTS document_jobs_owner ON document_jobs(owner,sequence);CREATE TABLE IF NOT EXISTS document_parts(job TEXT NOT NULL REFERENCES document_jobs(id) ON DELETE CASCADE,unit TEXT NOT NULL,source_hash TEXT NOT NULL,result TEXT NOT NULL,PRIMARY KEY(job,unit));")?;Ok(())
}
impl crate::task_store::Store{
    pub async fn document_begin(&self,owner:String,definition:Value,total:usize)->Result<Value>{
        let id=hash(&json!({"owner":owner,"definition":definition}))?;
        self.access(move|conn|{
            if let Some(task)=owner.strip_prefix("task:"){anyhow::ensure!(conn.query_row("SELECT EXISTS(SELECT 1 FROM work_tasks WHERE id=?1)",[task],|r|r.get::<_,bool>(0))?,"Задача удалена");}
            if let Some(chat)=owner.strip_prefix("conversation:"){anyhow::ensure!(!conn.query_row("SELECT EXISTS(SELECT 1 FROM deleted_conversations WHERE id=?1)",[chat],|r|r.get::<_,bool>(0))?,"Чат удалён");}
            if let Some(run)=owner.strip_prefix("run:"){anyhow::ensure!(conn.query_row("SELECT EXISTS(SELECT 1 FROM runs WHERE id=?1)",[run],|r|r.get::<_,bool>(0))?,"Запуск удалён");}
            conn.execute("INSERT OR IGNORE INTO document_jobs(id,owner,definition,total,created) VALUES(?1,?2,?3,?4,?5)",params![id,owner,definition.to_string(),i64::try_from(total)?,crate::api::now()])?;
            let(saved_owner,saved_definition,saved_total):(String,String,i64)=conn.query_row("SELECT owner,definition,total FROM document_jobs WHERE id=?1",[&id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
            anyhow::ensure!(saved_owner==owner&&saved_definition==definition.to_string()&&saved_total==i64::try_from(total)?,"Параметры сохранённого перевода не совпадают");
            let ready:i64=conn.query_row("SELECT COUNT(*) FROM document_parts WHERE job=?1",[&id],|r|r.get(0))?;
            Ok(json!({"id":id,"saved_units":ready,"total_units":total}))
        }).await
    }
    pub async fn document_part(&self,job:String,unit:Value,result:Option<Value>)->Result<Option<Value>>{
        let id=unit["id"].as_str().ok_or_else(||anyhow::anyhow!("Нет ID фрагмента"))?.to_string();let digest=hash(&unit)?;
        self.access(move|conn|{
            // First validated writer wins if two callers translate the same unit concurrently.
            if let Some(result)=result{conn.execute("INSERT OR IGNORE INTO document_parts(job,unit,source_hash,result) VALUES(?1,?2,?3,?4)",params![job,id,digest,result.to_string()])?;}
            let saved:Option<(String,String)>=conn.query_row("SELECT source_hash,result FROM document_parts WHERE job=?1 AND unit=?2",params![job,id],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
            saved.map(|(source,result)|{anyhow::ensure!(source==digest,"Исходный фрагмент сохранённого перевода изменился");Ok(serde_json::from_str(&result)?)}).transpose()
        }).await
    }
    pub async fn document_job_page(&self,owner:String,before:Option<i64>,limit:usize)->Result<Value>{
        self.access(move|conn|{
            anyhow::ensure!((1..=32).contains(&limit)&&before.is_none_or(|v|v>0),"Некорректная страница переводов");
            let mut query=conn.prepare("SELECT sequence,id,definition,total,(SELECT COUNT(*) FROM document_parts p WHERE p.job=j.id) FROM document_jobs j WHERE owner=?1 AND (?2 IS NULL OR sequence<?2) ORDER BY sequence DESC LIMIT ?3")?;
            let mut items=query.query_map(params![owner,before,i64::try_from(limit+1)?],|r|Ok(json!({"sequence":r.get::<_,i64>(0)?,"id":r.get::<_,String>(1)?,"definition":serde_json::from_str::<Value>(&r.get::<_,String>(2)?).map_err(|e|rusqlite::Error::FromSqlConversionFailure(2,rusqlite::types::Type::Text,Box::new(e)))?,"total_units":r.get::<_,i64>(3)?,"saved_units":r.get::<_,i64>(4)?,"output_verified":false})))?.collect::<rusqlite::Result<Vec<_>>>()?;
            let more=items.len()>limit;items.truncate(limit);let next=if more{items.last().map(|v|v["sequence"].clone()).unwrap_or(Value::Null)}else{Value::Null};
            Ok(json!({"items":items,"has_more":more,"next_before_document":next,"note":"Validated units, not proof of an exported file. Repeat translation with the same source, model identity and options to resume. Changed sources/models create a separate checkpoint."}))
        }).await
    }
}
#[cfg(test)]mod tests{
    use super::*;
    #[tokio::test]
    async fn committed_units_resume_after_restart_and_scope_changes_never_reuse_or_resurrect(){
        let root=std::env::temp_dir().join(format!("document-checkpoint-{}",uuid::Uuid::new_v4()));std::fs::create_dir_all(&root).unwrap();let store=crate::task_store::Store::open(&root).unwrap();
        store.begin_in_project("run".into(),"chat".into(),json!({"mode":"agent","content":"translate file"}),"project".into()).await.unwrap();
        let task=store.permission_task("run".into()).await.unwrap().unwrap().0;let owner=format!("task:{task}");let definition=json!({"path":"source.docx","source_sha256":"original","source":"en","target":"ru","model_identity":"digest-one"});
        let job=store.document_begin(owner.clone(),definition.clone(),2).await.unwrap();let id=job["id"].as_str().unwrap().to_string();let unit=json!({"id":"p1","text":"original"});
        let first=json!({"text":"перевод","unchanged":false});assert_eq!(store.document_part(id.clone(),unit.clone(),Some(first.clone())).await.unwrap(),Some(first.clone()));
        assert_eq!(store.document_part(id.clone(),unit.clone(),Some(json!({"text":"concurrent later result"}))).await.unwrap(),Some(first.clone()));drop(store);
        let store=crate::task_store::Store::open(&root).unwrap();assert_eq!(store.document_begin(owner.clone(),definition.clone(),2).await.unwrap()["saved_units"],1);assert_eq!(store.document_part(id.clone(),unit.clone(),None).await.unwrap(),Some(first));
        assert!(store.document_part(id.clone(),json!({"id":"p1","text":"changed"}),None).await.is_err());
        let changed=store.document_begin(owner.clone(),json!({"path":"source.docx","source_sha256":"new"}),2).await.unwrap();assert_ne!(changed["id"],id);assert_eq!(changed["saved_units"],0);
        let ui=store.document_begin("document-ui".into(),definition.clone(),2).await.unwrap();assert_ne!(ui["id"],id);
        let page=store.document_job_page(owner.clone(),None,1).await.unwrap();assert_eq!(page["has_more"],true);let next=store.document_job_page(owner.clone(),page["next_before_document"].as_i64(),1).await.unwrap();assert_eq!(next["items"][0]["id"],id);
        store.delete_conversation("chat".into()).await.unwrap();assert!(store.document_begin(owner.clone(),definition,2).await.is_err());assert!(store.document_part(id,unit,Some(json!({"text":"late"}))).await.is_err());assert!(store.document_job_page(owner,None,32).await.unwrap()["items"].as_array().unwrap().is_empty());assert_eq!(store.document_job_page("document-ui".into(),None,32).await.unwrap()["items"].as_array().unwrap().len(),1);drop(store);std::fs::remove_dir_all(root).unwrap();
    }
}
