//! Source provenance is independent of the visible tool window and model history.
use anyhow::{Context,Result};
use rusqlite::{params,Connection};
use serde_json::{json,Value};
use sha2::{Digest,Sha256};
use crate::task_store::Store;
pub fn install(conn:&Connection)->Result<()>{
    conn.execute_batch("CREATE TABLE IF NOT EXISTS web_sources(
        sequence INTEGER PRIMARY KEY AUTOINCREMENT,run TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
        source_id TEXT NOT NULL,canonical_url TEXT NOT NULL,url TEXT NOT NULL,title TEXT NOT NULL,
        state TEXT NOT NULL,published TEXT,retrieved TEXT NOT NULL,artifact TEXT NOT NULL REFERENCES artifacts(id) ON DELETE CASCADE,
        coverage TEXT NOT NULL,UNIQUE(artifact,source_id));
        CREATE INDEX IF NOT EXISTS web_sources_run ON web_sources(run,sequence);
        CREATE INDEX IF NOT EXISTS web_sources_id ON web_sources(source_id,sequence);
        CREATE INDEX IF NOT EXISTS task_runs_task ON task_runs(task,run);
        CREATE INDEX IF NOT EXISTS artifacts_scope_sequence ON artifacts(run,sequence);
        CREATE TABLE IF NOT EXISTS web_source_receipts(artifact TEXT PRIMARY KEY REFERENCES artifacts(id) ON DELETE CASCADE);
        CREATE TABLE IF NOT EXISTS web_source_index_errors(artifact TEXT PRIMARY KEY REFERENCES artifacts(id) ON DELETE CASCADE,error TEXT NOT NULL,created TEXT NOT NULL);")?;Ok(())
}
pub fn canonical(raw:&str)->Result<String>{
    let mut url=crate::web_tools::checked_url(raw)?;url.set_fragment(None);
    let query=url.query_pairs().filter(|(key,_)|{let key=key.to_ascii_lowercase();!key.starts_with("utm_")&&!["fbclid","gclid","msclkid"].contains(&key.as_str())}).map(|(key,value)|(key.into_owned(),value.into_owned())).collect::<Vec<_>>();
    if query.is_empty(){url.set_query(None);}else{url.query_pairs_mut().clear().extend_pairs(query);}
    Ok(url.to_string())
}
pub fn source_id(raw:&str)->Result<String>{Ok(format!("S-{:x}",Sha256::digest(canonical(raw)?.as_bytes())))}
fn decorate(item:&mut Value,state:&str){
    if let Some(url)=item["url"].as_str(){if let Ok(canonical)=canonical(url){item["source_id"]=json!(format!("S-{:x}",Sha256::digest(canonical.as_bytes())));item["canonical_url"]=json!(canonical);item["state"]=json!(state);item["retrieved"]=json!(crate::api::now());}}
}
pub fn normalize(value:&mut Value){
    for key in ["results","page_results","links"]{if let Some(rows)=value[key].as_array_mut(){for row in rows{decorate(row,if key=="links"{"linked"}else{"found"});}}}
    let state=match value["status"].as_u64(){Some(400..)=>"unavailable",Some(300..=399)=>"redirect",_=>if value["complete"]==true{"received"}else{"partial"}};decorate(value,state);
}
fn rows(value:&Value)->Vec<Value>{
    let mut result=vec![];let mut seen=std::collections::HashSet::new();
    // A root read receipt wins over a link pointing back to the current page.
    let mut append=|item:&Value,state:&str|{
        let Some(url)=item["url"].as_str()else{return;};let Ok(canonical)=canonical(url)else{return;};let id=source_id(url).unwrap();if !seen.insert(id.clone()){return;}
        result.push(json!({"source_id":id,"canonical_url":canonical,"url":url,"title":item["title"].as_str().unwrap_or(""),"state":state,"published":item["published"],"retrieved":item["retrieved"].as_str().unwrap_or(""),"coverage":{"offset":item["offset"],"next_offset":item["next_offset"],"complete":item["complete"],"status":item["status"],"model_full_reading_verified":false}}));
    };
    let state=match value["status"].as_u64(){Some(400..)=>"unavailable",Some(300..=399)=>"redirect",_=>if value["complete"]==true{"received"}else{"partial"}};append(value,state);
    for key in ["results","links"]{for row in value[key].as_array().into_iter().flatten(){append(row,if key=="results"{"found"}else{"linked"});}}
    result
}
impl Store{
    pub async fn web_source_history(&self,run:String,source:String,before:Option<i64>,snapshot:Option<i64>)->Result<Value>{
        anyhow::ensure!(source.len()==66&&source.starts_with("S-")&&source.as_bytes()[2..].iter().all(u8::is_ascii_hexdigit),"Некорректный идентификатор источника");
        anyhow::ensure!(before.is_none_or(|n|n>0)&&snapshot.is_none_or(|n|n>=0),"Некорректный курсор обращений");
        self.access(move|conn|{
            let snapshot=match snapshot{Some(n)=>n,None=>conn.query_row("SELECT COALESCE(MAX(sequence),0) FROM web_sources",[],|row|row.get::<_,i64>(0))?};
            let mut query=conn.prepare("WITH allowed(run) AS (SELECT ?1 UNION SELECT owner.run FROM task_runs owner JOIN task_runs current ON owner.task=current.task WHERE current.run=?1) SELECT s.source_id,s.canonical_url,s.url,s.title,s.state,s.published,s.retrieved,s.artifact,s.coverage,s.sequence FROM web_sources s JOIN allowed ON allowed.run=s.run WHERE s.source_id=?2 AND s.sequence<=?3 AND (?4 IS NULL OR s.sequence<?4) ORDER BY s.sequence DESC LIMIT 33")?;
            let mut items=query.query_map(params![run,source,snapshot,before],|row|Ok(json!({"source_id":row.get::<_,String>(0)?,"canonical_url":row.get::<_,String>(1)?,"url":row.get::<_,String>(2)?,"title":row.get::<_,String>(3)?,"state":row.get::<_,String>(4)?,"published":row.get::<_,Option<String>>(5)?,"retrieved":row.get::<_,String>(6)?,"artifact":row.get::<_,String>(7)?,"coverage":serde_json::from_str::<Value>(&row.get::<_,String>(8)?).unwrap_or(Value::Null),"sequence":row.get::<_,i64>(9)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
            let more=items.len()>32;items.truncate(32);let next=if more{items.last().map(|item|item["sequence"].clone()).unwrap_or(Value::Null)}else{Value::Null};
            Ok(json!({"source_id":source,"items":items,"next_before":next,"has_more":more,"snapshot":snapshot,"view":"receipts","note":"Every indexed receipt for this URL in the task. Open its artifact to inspect actual historical content, status and coverage. A URL identity is not content identity or semantic verification."}))
        }).await
    }
    pub async fn source_index_error(&self,artifact:String,error:String)->Result<()>{self.access(move|conn|{conn.execute("INSERT OR REPLACE INTO web_source_index_errors VALUES (?1,?2,?3)",params![artifact,error,crate::api::now()])?;Ok(())}).await}
    pub async fn source_index_errors(&self,run:String,retry:bool)->Result<Value>{
        self.access(move|conn|{
            if retry{conn.execute("DELETE FROM web_source_index_errors WHERE artifact IN (SELECT a.id FROM artifacts a WHERE a.run=?1 OR EXISTS(SELECT 1 FROM task_runs owner JOIN task_runs current ON owner.task=current.task WHERE owner.run=a.run AND current.run=?1))",[&run])?;}
            let mut query=conn.prepare("WITH allowed(run) AS (SELECT ?1 UNION SELECT owner.run FROM task_runs owner JOIN task_runs current ON owner.task=current.task WHERE current.run=?1) SELECT e.artifact,e.error,COUNT(*) OVER() FROM web_source_index_errors e JOIN artifacts a ON a.id=e.artifact JOIN allowed ON allowed.run=a.run ORDER BY a.sequence DESC LIMIT 8")?;
            let rows=query.query_map([run],|row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,i64>(2)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;let total=rows.first().map_or(0,|row|row.2);Ok(json!({"count":total,"items":rows.into_iter().map(|row|json!({"artifact":row.0,"error":row.1})).collect::<Vec<_>>(),"originals_preserved":true}))
        }).await
    }
    pub async fn has_web_sources(&self,run:String)->Result<bool>{
        self.access(move|conn|Ok(conn.query_row("WITH allowed(run) AS (SELECT ?1 UNION SELECT owner.run FROM task_runs owner JOIN task_runs current ON owner.task=current.task WHERE current.run=?1) SELECT EXISTS(SELECT 1 FROM allowed JOIN web_sources s ON s.run=allowed.run)",[run],|row|row.get::<_,bool>(0))?)).await
    }
    pub async fn index_web_source(&self,artifact:String,value:Value)->Result<()>{
        let rows=rows(&value);self.access(move|conn|{
            let tx=conn.transaction()?;let(run,created):(String,String)=tx.query_row("SELECT run,created FROM artifacts WHERE id=?1",[&artifact],|row|Ok((row.get(0)?,row.get(1)?)))?;
            if tx.query_row("SELECT EXISTS(SELECT 1 FROM web_source_receipts WHERE artifact=?1)",[&artifact],|row|row.get::<_,bool>(0))?{return Ok(());}
            for row in rows{let retrieved=row["retrieved"].as_str().filter(|value|!value.is_empty()).unwrap_or(&created);tx.execute("INSERT OR IGNORE INTO web_sources(run,source_id,canonical_url,url,title,state,published,retrieved,artifact,coverage) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",params![run,row["source_id"].as_str(),row["canonical_url"].as_str(),row["url"].as_str(),row["title"].as_str(),row["state"].as_str(),row["published"].as_str(),retrieved,artifact,row["coverage"].to_string()])?;}
            tx.execute("INSERT INTO web_source_receipts VALUES (?1)",[artifact])?;tx.commit()?;Ok(())
        }).await
    }
    pub async fn web_source_page(&self,run:String,before:Option<i64>,snapshot:Option<i64>)->Result<Value>{
        anyhow::ensure!(before.is_none_or(|n|n>0)&&snapshot.is_none_or(|n|n>=0),"Некорректный курсор источников");
        self.access(move|conn|{
            let snapshot=match snapshot{Some(n)=>n,None=>conn.query_row("SELECT COALESCE(MAX(sequence),0) FROM web_sources",[],|row|row.get::<_,i64>(0))?};
            let mut query=conn.prepare("WITH allowed(run) AS (SELECT ?1 UNION SELECT owner.run FROM task_runs owner JOIN task_runs current ON owner.task=current.task WHERE current.run=?1), accessible AS (SELECT s.* FROM web_sources s JOIN allowed ON allowed.run=s.run WHERE s.sequence<=?2), ranked AS (SELECT *,MAX(sequence) OVER(PARTITION BY source_id) AS last_seen,FIRST_VALUE(state) OVER(PARTITION BY source_id ORDER BY sequence DESC) AS latest_state,ROW_NUMBER() OVER(PARTITION BY source_id ORDER BY CASE WHEN state IN ('received','partial') THEN 1 ELSE 0 END DESC,sequence DESC) AS representative FROM accessible) SELECT source_id,canonical_url,url,title,state,published,retrieved,artifact,coverage,last_seen,latest_state FROM ranked WHERE representative=1 AND (?3 IS NULL OR last_seen<?3) ORDER BY last_seen DESC LIMIT 33")?;
            let mut items=query.query_map(params![run,snapshot,before],|row|Ok(json!({"source_id":row.get::<_,String>(0)?,"canonical_url":row.get::<_,String>(1)?,"url":row.get::<_,String>(2)?,"title":row.get::<_,String>(3)?,"state":row.get::<_,String>(4)?,"published":row.get::<_,Option<String>>(5)?,"retrieved":row.get::<_,String>(6)?,"artifact":row.get::<_,String>(7)?,"coverage":serde_json::from_str::<Value>(&row.get::<_,String>(8)?).unwrap_or(Value::Null),"sequence":row.get::<_,i64>(9)?,"latest_state":row.get::<_,String>(10)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
            let more=items.len()>32;items.truncate(32);let before=if more{items.last().map(|item|item["sequence"].clone()).unwrap_or(Value::Null)}else{Value::Null};
            Ok(json!({"items":items,"next_before":before,"has_more":more,"snapshot":snapshot,"note":"Received means saved response text, not semantic support or full reading by the model. A later discovery does not erase an earlier saved receipt. latest_state may describe a later unavailable page."}))
        }).await
    }
    pub async fn web_source_matches(&self,run:String,ids:Vec<String>)->Result<Value>{
        self.access(move|conn|{
            let mut query=conn.prepare("WITH allowed(run) AS (SELECT ?1 UNION SELECT owner.run FROM task_runs owner JOIN task_runs current ON owner.task=current.task WHERE current.run=?1), requested AS (SELECT value AS id FROM json_each(?2)), accessible AS (SELECT s.* FROM web_sources s JOIN requested wanted ON wanted.id=s.source_id JOIN allowed ON allowed.run=s.run), ranked AS (SELECT *,ROW_NUMBER() OVER(PARTITION BY source_id ORDER BY CASE WHEN state IN ('received','partial') THEN 1 ELSE 0 END DESC,sequence DESC) AS representative FROM accessible) SELECT source_id,url,title,state,artifact FROM ranked WHERE representative=1")?;
            let items=query.query_map(params![run,serde_json::to_string(&ids)?],|row|Ok(json!({"source_id":row.get::<_,String>(0)?,"url":row.get::<_,String>(1)?,"title":row.get::<_,String>(2)?,"state":row.get::<_,String>(3)?,"artifact":row.get::<_,String>(4)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;Ok(json!(items))
        }).await
    }
}
pub async fn page(app:&crate::api::Shared,run:&str,before:Option<i64>,snapshot:Option<i64>,source:Option<String>,retry_errors:bool,cancel:&tokio_util::sync::CancellationToken)->Result<Value>{
    if retry_errors{app.tasks.source_index_errors(run.into(),true).await?;}
    let indexing=backfill(app,run,cancel).await?;
    let mut page=match source{Some(source)=>app.tasks.web_source_history(run.into(),source,before,snapshot).await?,None=>app.tasks.web_source_page(run.into(),before,snapshot).await?};page["indexing"]=indexing;Ok(page)
}
pub async fn backfill(app:&crate::api::Shared,run:&str,cancel:&tokio_util::sync::CancellationToken)->Result<Value>{
    let source_run=run.to_string();let pending=app.tasks.access(move|conn|{
        let mut query=conn.prepare("WITH allowed(run) AS (SELECT ?1 UNION SELECT owner.run FROM task_runs owner JOIN task_runs current ON owner.task=current.task WHERE current.run=?1) SELECT a.id FROM artifacts a JOIN allowed ON allowed.run=a.run WHERE (a.kind='search_results' OR (a.kind='tool_result' AND json_extract(a.metadata,'$.status')='succeeded' AND json_extract(a.metadata,'$.tool_name') IN ('web_search','fetch_url','browser_open','browser_read','browser_action'))) AND NOT EXISTS(SELECT 1 FROM web_source_receipts done WHERE done.artifact=a.id) AND NOT EXISTS(SELECT 1 FROM web_source_index_errors broken WHERE broken.artifact=a.id) ORDER BY a.sequence LIMIT 9")?;
        let rows=query.query_map([source_run],|row|row.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;Ok(rows)
    }).await?;
    let more=pending.len()>8;let mut count=0;
    for id in pending.into_iter().take(8){let value:Result<Value>=async{let mut text=String::new();let mut offset=0;loop{anyhow::ensure!(!cancel.is_cancelled(),"Остановлено пользователем");let page=app.tasks.artifact_page(id.clone(),offset).await?;text.push_str(page["text"].as_str().context("Нет сохранённого текста источника")?);if page["has_more"]!=true{break;}offset=usize::try_from(page["next_offset"].as_u64().context("Нет курсора источника")?)?;}Ok(serde_json::from_str(&text).context("Сохранённый результат веб-инструмента повреждён")?)}.await;
        match value{Ok(value)=>{app.tasks.index_web_source(id,value).await?;count+=1;},Err(error)=>{anyhow::ensure!(!cancel.is_cancelled(),"Остановлено пользователем");app.tasks.source_index_error(id,format!("{error:#}")).await?;}}
    }
    let errors=app.tasks.source_index_errors(run.into(),false).await?;
    Ok(json!({"processed":count,"has_more":more,"errors":errors,"coverage":"saved_web_artifacts","legacy_json_tool_history_migrated":false}))
}
#[cfg(test)]mod tests{
    use super::*;
    #[test]fn canonical_identity_preserves_semantic_queries_and_overrides_untrusted_ids(){
        assert_eq!(canonical("HTTPS://EXAMPLE.org/a?meaning=1&utm_source=x#fragment").unwrap(),"https://example.org/a?meaning=1");assert_ne!(source_id("https://example.org/a?meaning=1").unwrap(),source_id("https://example.org/a?meaning=2").unwrap());
        let mut value=json!({"results":[{"url":"https://example.org/a?meaning=1","source_id":"forged","state":"received"},{"url":"javascript:bad","source_id":"forged"}]});normalize(&mut value);let entries=rows(&value);assert_eq!(entries.len(),1);assert_ne!(entries[0]["source_id"],"forged");assert_eq!(entries[0]["state"],"found");
    }
    #[tokio::test]async fn receipt_history_preserves_revisions_scope_and_snapshot_cursors(){
        let root=std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());std::fs::create_dir_all(&root).unwrap();let store=Store::open(&root).unwrap();
        for(run,chat)in [("a","chat"),("b","chat"),("foreign","other")]{store.begin_in_project(run.into(),chat.into(),json!({"mode":"agent","content":"Research receipts"}),"project".into()).await.unwrap();}
        let id=source_id("https://example.org/changed").unwrap();let mut references=vec![];
        for version in 0..43{let mut value=json!({"url":"https://example.org/changed","title":"Changing source","text":format!("Revision {version}"),"status":if version==42{404}else{200},"complete":true});normalize(&mut value);let source=store.put_artifact("a".into(),"tool_result".into(),value.to_string(),json!({})).await.unwrap();references.push(source["id"].as_str().unwrap().to_string());store.index_web_source(source["id"].as_str().unwrap().into(),value).await.unwrap();}
        let first=store.web_source_history("b".into(),id.clone(),None,None).await.unwrap();assert_eq!(first["items"].as_array().unwrap().len(),32);assert_eq!(first["items"][0]["state"],"unavailable");let snapshot=first["snapshot"].as_i64();let before=first["next_before"].as_i64();
        let value=json!({"url":"https://example.org/changed","complete":false,"text":"Later revision"});let source=store.put_artifact("b".into(),"tool_result".into(),value.to_string(),json!({})).await.unwrap();store.index_web_source(source["id"].as_str().unwrap().into(),value).await.unwrap();
        let last=store.web_source_history("b".into(),id.clone(),before,snapshot).await.unwrap();assert_eq!(last["items"].as_array().unwrap().len(),11);let restored=first["items"].as_array().unwrap().iter().chain(last["items"].as_array().unwrap()).map(|row|row["artifact"].as_str().unwrap().to_string()).collect::<Vec<_>>();assert_eq!(restored,references.into_iter().rev().collect::<Vec<_>>());assert_eq!(last["has_more"],false);
        assert!(store.web_source_history("foreign".into(),id,None,None).await.unwrap()["items"].as_array().unwrap().is_empty());assert!(store.web_source_history("b".into(),"not-an-id".into(),None,None).await.is_err());drop(store);std::fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]async fn source_pages_deduplicate_without_erasing_receipts_and_remain_scoped_after_restart_delete(){
        let root=std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());std::fs::create_dir_all(&root).unwrap();let store=Store::open(&root).unwrap();
        for(run,chat)in [("a","chat"),("b","chat"),("foreign","other")]{store.begin_in_project(run.into(),chat.into(),json!({"mode":"agent","content":"Research sources"}),"project".into()).await.unwrap();}
        let mut value=json!({"results":(0..80).map(|n|json!({"url":format!("https://example.org/{n}"),"title":format!("Source {n}")})).collect::<Vec<_>>()});normalize(&mut value);
        let source=store.put_artifact("a".into(),"search_results".into(),value.to_string(),json!({})).await.unwrap();let id=source["id"].as_str().unwrap().to_string();store.index_web_source(id.clone(),value.clone()).await.unwrap();store.index_web_source(id,value.clone()).await.unwrap();
        let first=store.web_source_page("b".into(),None,None).await.unwrap();assert_eq!(first["items"].as_array().unwrap().len(),32);assert_eq!(first["has_more"],true);let snapshot=first["snapshot"].as_i64();let mut before=first["next_before"].as_i64();let mut gathered=first["items"].as_array().unwrap().clone();
        let mut read=json!({"url":"https://example.org/0","title":"Saved partial receipt","text":"actual excerpt","status":200,"complete":false,"offset":0,"next_offset":8000});normalize(&mut read);let source=store.put_artifact("b".into(),"tool_result".into(),read.to_string(),json!({})).await.unwrap();store.index_web_source(source["id"].as_str().unwrap().into(),read).await.unwrap();
        while let Some(cursor)=before{let page=store.web_source_page("b".into(),Some(cursor),snapshot).await.unwrap();gathered.extend(page["items"].as_array().unwrap().clone());before=page["next_before"].as_i64();}
        assert_eq!(gathered.len(),80);assert_eq!(gathered.iter().map(|row|row["source_id"].as_str().unwrap()).collect::<std::collections::HashSet<_>>().len(),80);
        let source=store.put_artifact("b".into(),"search_results".into(),value.to_string(),json!({})).await.unwrap();store.index_web_source(source["id"].as_str().unwrap().into(),value.clone()).await.unwrap();
        let id=source_id("https://example.org/0").unwrap();let selected=store.web_source_matches("b".into(),vec![id.clone()]).await.unwrap();assert_eq!(selected[0]["state"],"partial");assert!(store.web_source_matches("foreign".into(),vec![id]).await.unwrap().as_array().unwrap().is_empty());
        assert!(store.web_source_page("foreign".into(),None,None).await.unwrap()["items"].as_array().unwrap().is_empty());
        let source=store.put_artifact("foreign".into(),"search_results".into(),value.to_string(),json!({})).await.unwrap();store.index_web_source(source["id"].as_str().unwrap().into(),value).await.unwrap();
        drop(store);let store=Store::open(&root).unwrap();let mut page=store.web_source_page("b".into(),None,None).await.unwrap();let mut found=false;loop{found|=page["items"].as_array().unwrap().iter().any(|row|row["state"]=="partial"&&row["latest_state"]=="found");let Some(before)=page["next_before"].as_i64()else{break;};page=store.web_source_page("b".into(),Some(before),page["snapshot"].as_i64()).await.unwrap();}assert!(found);store.delete_conversation("chat".into()).await.unwrap();assert_eq!(store.web_source_page("foreign".into(),None,None).await.unwrap()["items"].as_array().unwrap().len(),32);store.delete_conversation("other".into()).await.unwrap();
        let count=store.access(|conn|Ok(conn.query_row("SELECT count(*) FROM web_sources",[],|row|row.get::<_,i64>(0))?)).await.unwrap();assert_eq!(count,0);drop(store);std::fs::remove_dir_all(root).unwrap();
    }
}
