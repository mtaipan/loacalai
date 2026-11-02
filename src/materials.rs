//! Durable streaming ingestion into immutable observations. No whole-file String.
use anyhow::{Context,Result};
use rusqlite::{params,OptionalExtension};
use serde_json::{json,Value};
use sha2::{Digest,Sha256};
use std::path::PathBuf;
use tokio_util::sync::CancellationToken;
pub fn install(conn:&rusqlite::Connection)->Result<()>{
    conn.execute_batch("CREATE TABLE IF NOT EXISTS material_ingests(sequence INTEGER PRIMARY KEY AUTOINCREMENT,id TEXT NOT NULL UNIQUE,run TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE,source_key TEXT NOT NULL,path TEXT NOT NULL,revision TEXT NOT NULL,encoding TEXT NOT NULL,body TEXT NOT NULL REFERENCES artifact_bodies(hash),next_byte INTEGER NOT NULL,characters INTEGER NOT NULL,bytes INTEGER NOT NULL,parts INTEGER NOT NULL,status TEXT NOT NULL,artifact_id TEXT,metadata TEXT NOT NULL,error TEXT);
    CREATE INDEX IF NOT EXISTS material_ingests_source ON material_ingests(source_key,revision,encoding);")?;Ok(())
}
impl crate::task_store::Store{
    pub async fn material_jobs(&self,run:String,before:Option<i64>)->Result<Value>{
        self.access(move|conn|{
            anyhow::ensure!(before.is_none_or(|value|value>0),"Курсор должен быть положительным");
            let mut query=conn.prepare("SELECT sequence,id,run,path,status,next_byte,bytes,error,encoding FROM material_ingests m WHERE (?2 IS NULL OR sequence<?2) AND status!='ready' AND (run=?1 OR EXISTS(SELECT 1 FROM task_runs a JOIN task_runs b ON a.task=b.task WHERE a.run=m.run AND b.run=?1)) ORDER BY sequence DESC LIMIT 33")?;
            let mut rows=query.query_map(params![run,before],|row|Ok(json!({"sequence":row.get::<_,i64>(0)?,"id":row.get::<_,String>(1)?,"run":row.get::<_,String>(2)?,"path":row.get::<_,String>(3)?,"status":row.get::<_,String>(4)?,"next_byte":row.get::<_,i64>(5)?,"decoded_bytes":row.get::<_,i64>(6)?,"error":row.get::<_,Option<String>>(7)?,"encoding":row.get::<_,String>(8)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
            let more=rows.len()>32;rows.truncate(32);let next=rows.last().map(|row|row["sequence"].clone());
            Ok(json!({"items":rows,"next_before":if more{next}else{None},"note":"Preparation progress, not analysis coverage. ingest_file resumes a matching source revision. A stopped run does not mean its source was fully prepared."}))
        }).await
    }
    pub async fn ingest_text(&self,run:String,source_key:String,path:PathBuf,encoding:String,metadata:Value,cancel:&CancellationToken)->Result<Value>{
        anyhow::ensure!(metadata.is_object(),"Метаданные материала должны быть объектом");
        let first_path=path.clone();let token=cancel.clone();let requested=encoding.clone();
        let first=tokio::task::spawn_blocking(move||crate::text_source::page(&first_path,&json!({"encoding":requested,"max_lines":500}),&token)).await??;
        let revision=first["revision"].as_str().context("Нет ревизии источника")?.to_string();let actual=first["encoding"].as_str().context("Нет кодировки источника")?.to_string();
        let mut metadata=metadata;metadata["source_bom_bytes"]=first["bom_bytes"].clone();metadata["source_bytes"]=first["total_bytes"].clone();metadata["decoded_utf8"]=json!(true);
        let r=run.clone();let key=source_key.clone();let rev=revision.clone();let enc=actual.clone();let source_path=path.to_string_lossy().to_string();
        let mut state=self.access(move|conn|{
            // Reuse only within the same persistent task. A matching content hash
            // may deduplicate storage later, but is never an access grant.
            let row:Option<Value>=conn.query_row("SELECT id,run,body,next_byte,characters,bytes,parts,status,artifact_id,metadata FROM material_ingests m WHERE source_key=?1 AND revision=?2 AND encoding=?3 AND (run=?4 OR EXISTS(SELECT 1 FROM task_runs a JOIN task_runs b ON a.task=b.task WHERE a.run=m.run AND b.run=?4)) ORDER BY rowid DESC LIMIT 1",params![key,rev,enc,r],|row|Ok(json!({"id":row.get::<_,String>(0)?,"run":row.get::<_,String>(1)?,"body":row.get::<_,String>(2)?,"offset":row.get::<_,i64>(3)?,"characters":row.get::<_,i64>(4)?,"bytes":row.get::<_,i64>(5)?,"parts":row.get::<_,i64>(6)?,"status":row.get::<_,String>(7)?,"artifact_id":row.get::<_,Option<String>>(8)?,"metadata":serde_json::from_str::<Value>(&row.get::<_,String>(9)?).map_err(|error|rusqlite::Error::FromSqlConversionFailure(9,rusqlite::types::Type::Text,Box::new(error)))?}))).optional()?;
            if let Some(row)=row{return Ok(row);}
            let id=uuid::Uuid::new_v4().to_string();let body=format!("ingest:{id}");let tx=conn.transaction()?;
            tx.execute("INSERT INTO artifact_bodies VALUES (?1,0,0)",[&body])?;
            tx.execute("INSERT INTO material_ingests(id,run,source_key,path,revision,encoding,body,next_byte,characters,bytes,parts,status,metadata) VALUES (?1,?2,?3,?4,?5,?6,?7,0,0,0,0,'ingesting',?8)",params![id,r,key,source_path,rev,enc,body,metadata.to_string()])?;tx.commit()?;
            Ok(json!({"id":id,"run":r,"body":body,"offset":0,"characters":0,"bytes":0,"parts":0,"status":"ingesting","metadata":metadata}))
        }).await?;
        if let Some(id)=state["artifact_id"].as_str(){return self.artifact(id.into()).await;}
        let id=state["id"].as_str().unwrap().to_string();
        let outcome:Result<Value>=async{
        let mut digest=Sha256::new();
        // SHA state is not serialized. On restart, hash the committed prefix in
        // bounded chunks, then resume from its stored source byte offset.
        let mut after=-1i64;
        loop{
            anyhow::ensure!(!cancel.is_cancelled(),"Подготовка материала остановлена");
            let body=state["body"].as_str().unwrap().to_string();let rows=self.access(move|conn|{
                let mut query=conn.prepare("SELECT part,text FROM artifact_chunks WHERE hash=?1 AND part>?2 ORDER BY part LIMIT 32")?;
                let rows=query.query_map(params![body,after],|row|Ok((row.get::<_,i64>(0)?,row.get::<_,String>(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;Ok(rows)
            }).await?;
            if rows.is_empty(){break;}for(part,text)in rows{after=part;digest.update(text.as_bytes());}
        }
            let mut next=if state["offset"]==0{Some(first)}else{None};
            loop{
                anyhow::ensure!(!cancel.is_cancelled(),"Подготовка материала остановлена");
                let page=if let Some(page)=next.take(){page}else{
                    let path=path.clone();let offset=state["offset"].as_u64().unwrap();let version=revision.clone();let enc=actual.clone();let token=cancel.clone();
                    tokio::task::spawn_blocking(move||crate::text_source::page(&path,&json!({"offset":offset,"revision":version,"encoding":enc,"max_lines":500}),&token)).await??
                };
                let text=page["text"].as_str().unwrap_or("").to_string();digest.update(text.as_bytes());
                let body=state["body"].as_str().unwrap().to_string();let cursor=page["end_byte"].as_i64().unwrap();let ingestion=id.clone();
                let characters=state["characters"].as_i64().unwrap();let bytes=state["bytes"].as_i64().unwrap();let part=state["parts"].as_i64().unwrap();
                state=self.access(move|conn|{
                    let tx=conn.transaction()?;let mut start=0usize;let mut chars=characters;let mut index=part;
                    while start<text.len(){let mut end=(start+16384).min(text.len());while !text.is_char_boundary(end){end-=1;}let block=&text[start..end];let count=block.chars().count() as i64;
                        tx.execute("INSERT INTO artifact_chunks(hash,part,char_start,characters,text) VALUES (?1,?2,?3,?4,?5)",params![body,index,chars,count,block])?;start=end;chars+=count;index+=1;
                    }
                    let size=bytes+i64::try_from(text.len())?;
                    tx.execute("UPDATE artifact_bodies SET bytes=?2,characters=?3 WHERE hash=?1",params![body,size,chars])?;
                    tx.execute("UPDATE material_ingests SET next_byte=?2,characters=?3,bytes=?4,parts=?5,status='ingesting',error=NULL WHERE id=?1",params![ingestion,cursor,chars,size,index])?;tx.commit()?;
                    Ok(json!({"body":body,"offset":cursor,"characters":chars,"bytes":size,"parts":index}))
                }).await?;
                if page["has_more"]!=true{break;}
            }
            let hash=format!("{:x}",digest.finalize());let ingestion=id.clone();let body=state["body"].as_str().unwrap().to_string();let size=state["bytes"].as_i64().unwrap();let characters=state["characters"].as_i64().unwrap();let enc=actual.clone();let version=revision.clone();
            let artifact=self.access(move|conn|{
                let tx=conn.transaction()?;let(source_run,raw):(String,String)=tx.query_row("SELECT run,metadata FROM material_ingests WHERE id=?1",[&ingestion],|row|Ok((row.get(0)?,row.get(1)?)))?;
                let mut metadata:Value=serde_json::from_str(&raw)?;metadata["encoding"]=json!(enc);metadata["source_revision"]=json!(version);metadata["coverage"]=json!({"complete":true,"characters":characters});if metadata.get("origin").is_none(){metadata["origin"]=json!("text_source");}
                let added=tx.execute("INSERT OR IGNORE INTO artifact_bodies VALUES (?1,?2,?3)",params![hash,size,characters])?;
                if added>0{tx.execute("UPDATE artifact_chunks SET hash=?2 WHERE hash=?1",params![body,hash])?;}
                let artifact=uuid::Uuid::new_v4().to_string();tx.execute("INSERT INTO artifacts(id,run,kind,metadata,hash,created) VALUES (?1,?2,'material_text',?3,?4,?5)",params![artifact,source_run,metadata.to_string(),hash,crate::api::now()])?;
                tx.execute("UPDATE material_ingests SET status='ready',artifact_id=?2,body=?3,error=NULL WHERE id=?1",params![ingestion,artifact,hash])?;
                tx.execute("DELETE FROM artifact_bodies WHERE hash=?1",[body])?;tx.commit()?;Ok(artifact)
            }).await?;
            self.artifact(artifact).await
        }.await;
        if let Err(error)=&outcome{
            let ingestion=id.clone();let message=format!("{error:#}");
            self.access(move|conn|{conn.execute("UPDATE material_ingests SET status='interrupted',error=?2 WHERE id=?1",params![ingestion,message])?;Ok(())}).await?;
        }
        outcome
    }
}
#[cfg(test)]
mod tests{
    use super::*;
    #[tokio::test]
    async fn streaming_ingest_resumes_committed_prefix_and_deduplicates_immutable_text(){
        let root=std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());std::fs::create_dir_all(&root).unwrap();let path=root.join("source.txt");
        let original="Привет 🦀 source\r\n".repeat(40000);std::fs::write(&path,original.as_bytes()).unwrap();
        let store=crate::task_store::Store::open(&root).unwrap();let project=root.to_string_lossy().to_string();store.begin_in_project("first".into(),"chat".into(),json!({"mode":"agent","content":"read source"}),project.clone()).await.unwrap();
        let worker=store.clone();let source=path.clone();let cancel=CancellationToken::new();let signal=cancel.clone();
        let running=tokio::spawn(async move{worker.ingest_text("first".into(),"upload-id".into(),source,"auto".into(),json!({"name":"source.txt"}),&signal).await});
        let mut offset=0;
        for _ in 0..1000{
            offset=store.access(|conn|Ok(conn.query_row("SELECT COALESCE(MAX(next_byte),0) FROM material_ingests",[],|row|row.get::<_,i64>(0))?)).await.unwrap();
            if offset>0{cancel.cancel();break;}tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
        assert!(offset>0);assert!(running.await.unwrap().is_err());
        let stored=store.access(|conn|Ok(conn.query_row("SELECT next_byte,status FROM material_ingests",[],|row|Ok((row.get::<_,i64>(0)?,row.get::<_,String>(1)?)))?)).await.unwrap();assert!(stored.0>0);assert_eq!(stored.1,"interrupted");
        let jobs=store.material_jobs("first".into(),None).await.unwrap();assert_eq!(jobs["items"][0]["status"],"interrupted");assert_eq!(jobs["items"][0]["next_byte"],stored.0);
        drop(store);let store=crate::task_store::Store::open(&root).unwrap();store.begin_in_project("next".into(),"chat".into(),json!({"mode":"agent","content":"continue"}),project).await.unwrap();
        let material=store.ingest_text("next".into(),"upload-id".into(),path.clone(),"auto".into(),json!({"name":"source.txt"}),&CancellationToken::new()).await.unwrap();
        assert_eq!(material["sha256"],format!("{:x}",Sha256::digest(original.as_bytes())));assert_eq!(material["characters"],original.chars().count());assert_eq!(material["metadata"]["coverage"]["complete"],true);
        let again=store.ingest_text("next".into(),"upload-id".into(),path.clone(),"auto".into(),json!({}),&CancellationToken::new()).await.unwrap();assert_eq!(again["id"],material["id"]);
        let mut output=String::new();let mut offset=0;loop{let page=store.artifact_page(material["id"].as_str().unwrap().into(),offset).await.unwrap();output.push_str(page["text"].as_str().unwrap());offset=page["next_offset"].as_u64().unwrap() as usize;if page["has_more"]!=true{break;}}assert_eq!(output,original);
        store.begin("other".into(),"other-chat".into(),json!({"mode":"agent","content":"another task"})).await.unwrap();assert!(store.material_jobs("other".into(),None).await.unwrap()["items"].as_array().unwrap().is_empty());
        let other=store.ingest_text("other".into(),"upload-id".into(),path,"auto".into(),json!({}),&CancellationToken::new()).await.unwrap();assert_ne!(other["id"],material["id"]);assert_eq!(other["sha256"],material["sha256"]);assert!(!store.artifact_owned(other["id"].as_str().unwrap().into(),"next".into()).await.unwrap());
        store.delete_conversation("chat".into()).await.unwrap();assert_eq!(store.artifact(other["id"].as_str().unwrap().into()).await.unwrap()["sha256"],other["sha256"]);
        store.delete_conversation("other-chat".into()).await.unwrap();let rows=store.access(|conn|Ok(conn.query_row("SELECT (SELECT count(*) FROM material_ingests)+(SELECT count(*) FROM artifact_bodies)",[],|row|row.get::<_,i64>(0))?)).await.unwrap();assert_eq!(rows,0);
        drop(store);std::fs::remove_dir_all(root).unwrap();
    }
}
