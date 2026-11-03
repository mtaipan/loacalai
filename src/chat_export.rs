//! Immutable export manifests release SQL snapshots before a slow client reads.
use anyhow::{Context,Result};
use rusqlite::params;
use serde_json::{json,Value};
impl crate::task_store::Store{
    pub async fn chat_export_begin(&self,id:String)->Result<(Value,Lease)>{let store=self.clone();self.access(move|db|{
        let tx=db.transaction()?;let header=crate::chat_store::header(&tx,&id)?;let export=uuid::Uuid::new_v4().to_string();let lease=Lease{store,id:export.clone()};
        tx.execute("INSERT INTO chat_exports VALUES(?1,?2,?3)",params![export,id,header.to_string()])?;
        tx.execute("INSERT INTO chat_export_items SELECT ?1,m.display_seq,m.key,m.content,m.raw,m.metadata FROM chat_messages m JOIN chat_attempts a ON a.id=m.attempt AND a.active=1 WHERE m.conversation=?2",params![export,id])?;
        tx.commit()?;Ok((json!({"id":export,"header":header}),lease))
    }).await}
    pub async fn chat_export_page(&self,export:String,after:i64)->Result<Vec<Value>>{self.access(move|db|{
        let alive:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM chat_exports e WHERE e.id=?1 AND NOT EXISTS(SELECT 1 FROM deleted_conversations d WHERE d.id=e.conversation))",[&export],|r|r.get(0))?;anyhow::ensure!(alive,"Экспорт закрыт или чат удалён");
        let mut q=db.prepare("SELECT sequence,key,content,raw,metadata FROM chat_export_items WHERE export=?1 AND sequence>?2 ORDER BY sequence LIMIT 16")?;
        let rows=q.query_map(params![export,after],|r|Ok(json!({"sequence":r.get::<_,i64>(0)?,"key":r.get::<_,String>(1)?,"content":r.get::<_,String>(2)?,"raw":r.get::<_,String>(3)?,"metadata":serde_json::from_str::<Value>(&r.get::<_,String>(4)?).unwrap_or(json!({}))})))?.collect::<rusqlite::Result<Vec<_>>>()?;Ok(rows)
    }).await}
    pub async fn chat_export_chunk(&self,export:String,body:String,after:i64)->Result<Option<(i64,String)>>{self.access(move|db|{
        let allowed:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM chat_exports e WHERE e.id=?1 AND NOT EXISTS(SELECT 1 FROM deleted_conversations d WHERE d.id=e.conversation) AND (EXISTS(SELECT 1 FROM chat_export_items i WHERE i.export=e.id AND i.content=?2) OR EXISTS(SELECT 1 FROM chat_export_items i WHERE i.export=e.id AND i.raw=?2)))",params![export,body],|r|r.get(0))?;anyhow::ensure!(allowed,"Источник экспорта недоступен");
        use rusqlite::OptionalExtension;
        Ok(db.query_row("SELECT part,text FROM chat_chunks WHERE body=?1 AND part>?2 ORDER BY part LIMIT 1",params![body,after],|r|Ok((r.get(0)?,r.get(1)?))).optional()?)
    }).await}
    pub async fn chat_export_body(&self,export:String,body:String)->Result<(i64,String)>{self.access(move|db|{
        Ok(db.query_row("SELECT b.bytes,b.hash FROM chat_bodies b WHERE b.id=?2 AND b.complete=1 AND EXISTS(SELECT 1 FROM chat_exports e WHERE e.id=?1 AND NOT EXISTS(SELECT 1 FROM deleted_conversations d WHERE d.id=e.conversation) AND (EXISTS(SELECT 1 FROM chat_export_items i WHERE i.export=e.id AND i.content=b.id) OR EXISTS(SELECT 1 FROM chat_export_items i WHERE i.export=e.id AND i.raw=b.id)))",params![export,body],|r|Ok((r.get(0)?,r.get(1)?)))?)
    }).await}
    pub async fn chat_export_end(&self,export:String)->Result<()>{self.access(move|db|{db.execute("DELETE FROM chat_exports WHERE id=?1",[export])?;Ok(())}).await}
}
pub(crate) struct Lease{store:crate::task_store::Store,id:String}
impl Drop for Lease{fn drop(&mut self){let store=self.store.clone();let id=self.id.clone();if let Ok(runtime)=tokio::runtime::Handle::try_current(){runtime.spawn(async move{let _=store.chat_export_end(id).await;});}}}
pub async fn response(app:crate::api::Shared,id:String,format:String)->Result<axum::response::Response>{
    anyhow::ensure!(["view","json","markdown"].contains(&format.as_str()),"Формат экспорта не поддерживается");
    let (manifest,lease)=app.tasks.chat_export_begin(id).await?;let export=manifest["id"].as_str().context("Нет ID экспорта")?.to_string();let header=manifest["header"].clone();let markdown=format=="markdown";let backup=format=="json";
    let stream=async_stream::try_stream!{
        let _lease=lease;
        if markdown{yield format!("# {}\n\n",header["title"].as_str().unwrap_or("Чат")).into_bytes();}
        else{let mut h=json!({"id":header["id"],"title":header["title"],"updated":header["updated"],"mode":header["mode"]});h["format"]=json!(if backup{"conversation_messages_v1"}else{"conversation_view_v1"});let prefix=serde_json::to_string(&h)?;yield format!("{},\"messages\":[",prefix.trim_end_matches('}')).into_bytes();}
        let mut after=-1i64;let mut first=true;
        loop{let rows=app.tasks.chat_export_page(export.clone(),after).await?;if rows.is_empty(){break;}
            for item in rows{after=item["sequence"].as_i64().unwrap();let mut metadata=item["metadata"].clone();if let Some(tools)=metadata["tools"].as_array_mut(){for tool in tools{crate::tools::public_record(tool);}}
                if markdown{if !first{yield b"\n\n---\n\n".to_vec();}yield format!("## {}\n\n",if metadata["role"]=="user"{"Вы"}else{"Ответ"}).into_bytes();}
                else{if !first{yield b",".to_vec();}if !backup{let meta=serde_json::to_string(&metadata)?;yield format!("{},\"content\":\"",meta.trim_end_matches('}')).into_bytes();}}
                first=false;let body=item[if backup{"raw"}else{"content"}].as_str().unwrap().to_string();let mut part=-1;let(expected_bytes,expected_hash)=app.tasks.chat_export_body(export.clone(),body.clone()).await?;let mut bytes=0u64;use sha2::{Digest,Sha256};let mut hash=Sha256::new();
                while let Some((next,text))=app.tasks.chat_export_chunk(export.clone(),body.clone(),part).await?{if next!=part+1{Err(anyhow::anyhow!("Экспорт прерван: пропущен пакет текста"))?;}part=next;bytes+=text.len() as u64;hash.update(text.as_bytes());if markdown||backup{yield text.into_bytes();}else{let encoded=serde_json::to_string(&text)?;yield encoded.as_bytes()[1..encoded.len()-1].to_vec();}}
                if i64::try_from(bytes)?!=expected_bytes||format!("{:x}",hash.finalize())!=expected_hash{Err(anyhow::anyhow!("Экспорт прерван: размер или SHA256 текста не совпадает"))?;}
                if !markdown&&!backup{yield b"\"}".to_vec();}
                if markdown{if let Some(attachments)=metadata["attachments"].as_array(){if !attachments.is_empty(){yield format!("\n\nВложения: {}",attachments.iter().filter_map(|v|v["name"].as_str()).collect::<Vec<_>>().join(", ")).into_bytes();}}if let Some(tools)=metadata["tools"].as_array(){for tool in tools{yield format!("\n\n<details>\n<summary>{}</summary>\n\n```text\n{}\n```\n</details>",tool["name"].as_str().unwrap_or("Действие"),tool["result"].as_str().unwrap_or("")).into_bytes();}}}
            }
        }
        if !markdown{yield b"]}".to_vec();}
    };
    let mut headers=axum::http::HeaderMap::new();headers.insert("content-type",if markdown{"text/markdown; charset=utf-8"}else{"application/json; charset=utf-8"}.parse()?);
    if format!="view"{headers.insert("content-disposition",format!("attachment; filename=\"conversation.{}\"",if markdown{"md"}else{"json"}).parse()?);}
    use axum::response::IntoResponse;
    let stream:futures_util::stream::BoxStream<'static,Result<Vec<u8>,std::io::Error>>=Box::pin(futures_util::TryStreamExt::map_err(stream,|error:anyhow::Error|std::io::Error::other(error.to_string())));
    Ok((headers,axum::body::Body::from_stream(stream)).into_response())
}

#[cfg(test)]mod tests{
    use super::*;
    #[tokio::test]
    async fn corrupt_stored_text_never_exports_as_a_complete_success(){
        let root=std::env::temp_dir().join(format!("export-corrupt-{}",uuid::Uuid::new_v4()));let app=std::sync::Arc::new(crate::api::App::new(root.clone()).unwrap());let id=uuid::Uuid::new_v4().to_string();app.tasks.chat_create(id.clone(),"integrity".into(),"chat".into()).await.unwrap();
        app.tasks.begin_in_project("integrity-run".into(),id.clone(),json!({"mode":"chat","content":"я🦀".repeat(50000)}),"project".into()).await.unwrap();
        let good=response(app.clone(),id.clone(),"json".into()).await.unwrap();let bytes=axum::body::to_bytes(good.into_body(),2_000_000).await.unwrap();let value:Value=serde_json::from_slice(&bytes).unwrap();assert_eq!(value["messages"][0]["content"],"я🦀".repeat(50000));
        app.tasks.access(|db|{let body:String=db.query_row("SELECT raw FROM chat_messages LIMIT 1",[],|r|r.get(0))?;db.execute("DELETE FROM chat_chunks WHERE body=?1 AND part=1",[body])?;Ok(())}).await.unwrap();
        let broken=response(app.clone(),id.clone(),"json".into()).await.unwrap();assert!(axum::body::to_bytes(broken.into_body(),2_000_000).await.is_err());
        app.tasks.access(|db|{let body:String=db.query_row("SELECT content FROM chat_messages LIMIT 1",[],|r|r.get(0))?;db.execute("UPDATE chat_chunks SET text=replace(text,'я','а') WHERE body=?1 AND part=0",[&body])?;assert!(crate::chat_store::whole(db,&body).is_err());db.execute("DELETE FROM chat_chunks WHERE body=?1 AND part=1",[&body])?;assert!(crate::chat_store::slice(db,&body,5000,16000).is_err());Ok(())}).await.unwrap();
        app.tasks.delete_conversation(id).await.unwrap();drop(app);
    }
    #[tokio::test]
    async fn unpolled_response_and_cancelled_export_creation_release_their_sql_manifests(){
        let root=std::env::temp_dir().join(format!("export-lease-{}",uuid::Uuid::new_v4()));let app=std::sync::Arc::new(crate::api::App::new(root.clone()).unwrap());let id=uuid::Uuid::new_v4().to_string();app.tasks.chat_create(id.clone(),"export".into(),"chat".into()).await.unwrap();
        let response=response(app.clone(),id.clone(),"markdown".into()).await.unwrap();
        assert_eq!(app.tasks.access(|db|Ok(db.query_row("SELECT COUNT(*) FROM chat_exports",[],|r|r.get::<_,i64>(0))?)).await.unwrap(),1);drop(response);
        for _ in 0..100{if app.tasks.access(|db|Ok(db.query_row("SELECT COUNT(*) FROM chat_exports",[],|r|r.get::<_,i64>(0))?)).await.unwrap()==0{break;}tokio::task::yield_now().await;}
        assert_eq!(app.tasks.access(|db|Ok(db.query_row("SELECT COUNT(*) FROM chat_exports",[],|r|r.get::<_,i64>(0))?)).await.unwrap(),0);
        let worker=app.clone();let chat=id.clone();let ready=std::sync::Arc::new(tokio::sync::Notify::new());let signal=ready.clone();
        let job=tokio::spawn(async move{let (_,lease)=worker.tasks.chat_export_begin(chat).await.unwrap();signal.notify_one();std::future::pending::<()>().await;drop(lease);});ready.notified().await;job.abort();let _=job.await;
        for _ in 0..100{if app.tasks.access(|db|Ok(db.query_row("SELECT COUNT(*) FROM chat_exports",[],|r|r.get::<_,i64>(0))?)).await.unwrap()==0{break;}tokio::task::yield_now().await;}
        assert_eq!(app.tasks.access(|db|Ok(db.query_row("SELECT COUNT(*) FROM chat_exports",[],|r|r.get::<_,i64>(0))?)).await.unwrap(),0);app.tasks.delete_conversation(id).await.unwrap();drop(app);std::fs::remove_dir_all(root).unwrap();
    }
}
