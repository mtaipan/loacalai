//! Durable runs and ordered events. The UI is a subscriber, never the owner of a run.
use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::{
    path::Path,
    sync::{Arc, Mutex},
};

#[derive(Clone)]
pub struct Store(Arc<Mutex<Connection>>);
impl Store {
    pub fn open(data: &Path) -> Result<Self> {
        let conn = Connection::open(data.join("tasks.sqlite3"))?;
        let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        anyhow::ensure!(
            version <= 1,
            "База задач создана более новой версией приложения; исходный файл сохранён"
        );
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "FULL")?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS runs (
            id TEXT PRIMARY KEY, conversation TEXT NOT NULL, input TEXT NOT NULL,
            status TEXT NOT NULL, created TEXT NOT NULL, updated TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS events (
            sequence INTEGER PRIMARY KEY AUTOINCREMENT, run TEXT NOT NULL,
            payload TEXT NOT NULL, FOREIGN KEY(run) REFERENCES runs(id));
            CREATE INDEX IF NOT EXISTS events_run ON events(run,sequence);
            PRAGMA user_version=1;",
        )?;
        // A crash leaves effects with unknown outcome. Never replay mutations automatically.
        conn.execute(
            "UPDATE runs SET status='interrupted', updated=?1 WHERE status IN ('running','queued')",
            [crate::api::now()],
        )?;
        Ok(Self(Arc::new(Mutex::new(conn))))
    }
    async fn access<T: Send + 'static>(
        &self,
        f: impl FnOnce(&mut Connection) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let conn = self.0.clone();
        tokio::task::spawn_blocking(move || {
            let mut guard = conn
                .lock()
                .map_err(|_| anyhow::anyhow!("Task store lock poisoned"))?;
            f(&mut guard)
        })
        .await?
    }
    pub async fn begin(&self, id: String, conversation: String, input: Value) -> Result<bool> {
        self.access(move |conn| {
            let tx = conn.transaction()?;
            let old: Option<String> = tx
                .query_row("SELECT input FROM runs WHERE id=?1", [&id], |r| r.get(0))
                .optional()?;
            if let Some(old) = old {
                anyhow::ensure!(
                    serde_json::from_str::<Value>(&old)? == input,
                    "Идентификатор запроса уже использован для другого сообщения"
                );
                return Ok(false);
            }
            tx.execute(
                "INSERT INTO runs VALUES (?1,?2,?3,'running',?4,?4)",
                params![id, conversation, input.to_string(), crate::api::now()],
            )?;
            tx.commit()?;
            Ok(true)
        })
        .await
    }
    pub async fn append(&self, id: String, event: Value) -> Result<()> {
        self.access(move |conn| {
            let tx = conn.transaction()?;
            tx.execute("INSERT INTO events(run,payload) VALUES (?1,?2)", params![id,event.to_string()])?;
            let status = if event["type"]=="done" {Some(event["status"].as_str().unwrap_or("completed"))} else if event["type"]=="error" {Some("failed")} else {None};
            if let Some(status) = status {
                tx.execute("UPDATE runs SET status=CASE WHEN status='failed' THEN status ELSE ?1 END, updated=?2 WHERE id=?3", params![status,crate::api::now(),id])?;
            }
            tx.commit()?;
            Ok(())
        }).await
    }
    pub async fn events(&self, id: String, after: i64) -> Result<Vec<Value>> {
        self.access(move |conn| {
            let mut statement=conn.prepare("SELECT sequence,payload FROM events WHERE run=?1 AND sequence>?2 ORDER BY sequence LIMIT 256")?;
            let rows=statement.query_map(params![id,after], |row| Ok((row.get::<_,i64>(0)?,row.get::<_,String>(1)?)))?;
            rows.map(|row| {let (sequence,payload)=row?;let mut value:Value=serde_json::from_str(&payload)?;value["sequence"]=json!(sequence);Ok(value)}).collect()
        }).await
    }
    pub async fn get(&self, id: String) -> Result<Value> {
        self.access(move |conn| {
            let (conversation,status,input): (String,String,String) = conn.query_row("SELECT conversation,status,input FROM runs WHERE id=?1", [&id], |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?.context("Задача не найдена")?;
            Ok(json!({"id":id,"conversation":conversation,"status":status,"input":serde_json::from_str::<Value>(&input)?}))
        }).await
    }
    pub async fn list(&self) -> Result<Value> {
        self.access(|conn| {
            let mut statement=conn.prepare("SELECT id,conversation,status,created,updated FROM runs ORDER BY created DESC LIMIT 100")?;
            let rows=statement.query_map([], |r|Ok(json!({"id":r.get::<_,String>(0)?,"conversation":r.get::<_,String>(1)?,"status":r.get::<_,String>(2)?,"created":r.get::<_,String>(3)?,"updated":r.get::<_,String>(4)?})))?;
            Ok(Value::Array(rows.collect::<rusqlite::Result<Vec<_>>>()?))
        }).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn replay_is_ordered_idempotent_and_crash_requires_review() {
        let dir = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        std::fs::create_dir(&dir).unwrap();
        let store = Store::open(&dir).unwrap();
        assert!(store
            .begin("r".into(), "c".into(), json!({"text":"task"}))
            .await
            .unwrap());
        assert!(!store
            .begin("r".into(), "c".into(), json!({"text":"task"}))
            .await
            .unwrap());
        assert!(store
            .begin("r".into(), "c".into(), json!({"text":"different"}))
            .await
            .is_err());
        store
            .append("r".into(), json!({"type":"token","text":"partial"}))
            .await
            .unwrap();
        let events = store.events("r".into(), 0).await.unwrap();
        assert_eq!(events[0]["text"], "partial");
        assert!(store
            .events("r".into(), events[0]["sequence"].as_i64().unwrap())
            .await
            .unwrap()
            .is_empty());
        drop(store);
        let store = Store::open(&dir).unwrap();
        assert_eq!(
            store.get("r".into()).await.unwrap()["status"],
            "interrupted"
        );
        assert_eq!(store.events("r".into(), 0).await.unwrap().len(), 1);
        drop(store);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
