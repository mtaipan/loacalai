//! User origin authority is durable and distinct from discovered source provenance.
use anyhow::Result;
use rusqlite::{params,Connection};
use crate::task_store::Store;
pub fn install(conn:&Connection)->Result<()> {
    conn.execute_batch("CREATE TABLE IF NOT EXISTS web_user_origins(run TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE,origin TEXT NOT NULL,PRIMARY KEY(run,origin));
    CREATE INDEX IF NOT EXISTS web_user_origins_lookup ON web_user_origins(origin,run);
    CREATE TABLE IF NOT EXISTS web_user_origin_receipts(run TEXT PRIMARY KEY REFERENCES runs(id) ON DELETE CASCADE);
")?;Ok(())
}
pub fn record(conn:&Connection,run:&str,content:&str)->Result<()> {
    for url in crate::task_scope::urls(content) {
        if let Ok(url)=crate::web_tools::checked_url(url.as_str()) {
            conn.execute("INSERT OR IGNORE INTO web_user_origins VALUES (?1,?2)",params![run,url.origin().ascii_serialization()])?;
        }
    }
    conn.execute("INSERT OR IGNORE INTO web_user_origin_receipts VALUES (?1)",[run])?;Ok(())
}
impl Store {
    pub async fn backfill_web_authority(&self,run:String)->Result<bool> {
        self.access(move|conn|{
            let tx=conn.transaction()?;
            let more={
                let mut statement=tx.prepare("WITH allowed(run) AS (SELECT ?1 UNION SELECT owner.run FROM task_runs owner JOIN task_runs current ON owner.task=current.task WHERE current.run=?1) SELECT r.id,json_extract(r.input,'$.content') FROM runs r JOIN allowed ON allowed.run=r.id WHERE NOT EXISTS(SELECT 1 FROM web_user_origin_receipts done WHERE done.run=r.id) ORDER BY r.created,r.id LIMIT 9")?;
                let mut rows=statement.query([run])?;let mut count=0;let mut more=false;
                while let Some(row)=rows.next()? {
                    if count==8{more=true;break;}
                    record(&tx,&row.get::<_,String>(0)?,&row.get::<_,Option<String>>(1)?.unwrap_or_default())?;count+=1;
                }more
            };tx.commit()?;Ok(more)
        }).await
    }
    pub async fn known_web_origin(&self,run:String,origin:String,explicit:bool)->Result<bool> {
        self.access(move|conn|{
            let prefix=format!("{origin}/");let upper=format!("{origin}0");
            Ok(conn.query_row("WITH allowed(run) AS (SELECT ?1 UNION SELECT owner.run FROM task_runs owner JOIN task_runs current ON owner.task=current.task WHERE current.run=?1) SELECT EXISTS(SELECT 1 FROM web_user_origins u JOIN allowed ON allowed.run=u.run WHERE u.origin=?2) OR (?3=0 AND EXISTS(SELECT 1 FROM web_sources s JOIN allowed ON allowed.run=s.run WHERE s.canonical_url>=?4 AND s.canonical_url<?5))",params![run,origin,explicit,prefix,upper],|row|row.get(0))?)
        }).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;use serde_json::json;
    #[tokio::test]
    async fn explicit_and_discovered_origins_survive_restart_without_cross_task_authority() {
        let root=std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());std::fs::create_dir(&root).unwrap();
        let store=Store::open(&root).unwrap();
        store.begin_in_project("a".into(),"chat".into(),json!({"mode":"agent","content":"Open http://localhost:8123/ and https://Example.org:443/source"}),"project".into()).await.unwrap();
        store.begin_in_project("b".into(),"chat".into(),json!({"mode":"agent","content":"continue"}),"project".into()).await.unwrap();
        store.begin_in_project("foreign".into(),"other".into(),json!({"mode":"agent","content":"other"}),"project".into()).await.unwrap();
        let value=json!({"url":"https://observed.example/","redirect_to":"https://redirect.example/next","results":(0..300).map(|i|json!({"url":format!("https://source{i}.example/path")})).collect::<Vec<_>>(),"links":[{"url":"http://localhost:9999/"}],"status":302});
        let source=store.put_artifact("a".into(),"tool_result".into(),value.to_string(),json!({})).await.unwrap();store.index_web_source(source["id"].as_str().unwrap().into(),value).await.unwrap();
        for i in 0..300{assert!(store.known_web_origin("b".into(),format!("https://source{i}.example"),false).await.unwrap());}
        for origin in ["http://localhost:8123","https://example.org"]{assert!(store.known_web_origin("b".into(),origin.into(),true).await.unwrap());}
        for origin in ["https://observed.example","https://redirect.example","http://localhost:9999"] {
            assert!(store.known_web_origin("b".into(),origin.into(),false).await.unwrap());assert!(!store.known_web_origin("b".into(),origin.into(),true).await.unwrap());
            assert!(!store.known_web_origin("foreign".into(),origin.into(),false).await.unwrap());
        }
        for origin in ["https://example.org.evil","http://localhost:8124","http://127.0.0.1:8123","https://example.org:444"]{assert!(!store.known_web_origin("b".into(),origin.into(),false).await.unwrap());}
        drop(store);let store=Store::open(&root).unwrap();assert!(store.known_web_origin("b".into(),"http://localhost:8123".into(),true).await.unwrap());
        store.begin_in_project("new".into(),"chat".into(),json!({"mode":"agent","content":"another","new_task":true}),"project".into()).await.unwrap();assert!(!store.known_web_origin("new".into(),"http://localhost:8123".into(),true).await.unwrap());
        store.delete_conversation("chat".into()).await.unwrap();assert!(!store.known_web_origin("b".into(),"http://localhost:8123".into(),true).await.unwrap());drop(store);std::fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn old_requests_backfill_in_packets_without_a_total_run_limit() {
        let root=std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());std::fs::create_dir(&root).unwrap();let store=Store::open(&root).unwrap();
        for i in 0..21{store.begin_in_project(format!("r{i}"),"chat".into(),json!({"mode":"agent","content":format!("open http://localhost:{}/",8100+i)}),"project".into()).await.unwrap();}
        store.access(|conn|{conn.execute("DELETE FROM web_user_origins",[])?;conn.execute("DELETE FROM web_user_origin_receipts",[])?;Ok(())}).await.unwrap();drop(store);
        let store=Store::open(&root).unwrap();let mut packets=1;while store.backfill_web_authority("r20".into()).await.unwrap(){packets+=1;}assert_eq!(packets,3);
        for i in 0..21{assert!(store.known_web_origin("r20".into(),format!("http://localhost:{}",8100+i),true).await.unwrap());}
        drop(store);std::fs::remove_dir_all(root).unwrap();
    }
}
