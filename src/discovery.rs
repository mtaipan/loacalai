//! Durable, task-scoped discovery packets. Queue and retry receipts live on disk.
use anyhow::{Context,Result};
use rusqlite::{params,Connection,OptionalExtension};
use serde_json::{json,Value};
use std::path::{Path,PathBuf};
use tokio_util::sync::CancellationToken;

pub async fn tool(app:&crate::api::Shared,kind:&str,args:Value,cancel:&CancellationToken)->Result<Value>{
    let settings=app.settings.lock().await.clone();
    let project=dunce::canonicalize(&settings.workspace)?;
    let supplied=if kind=="list_files"||kind=="inspect_project"{args["path"].as_str()}else{args["directory"].as_str()};
    let root=crate::filesystem::writable_path(&project,supplied.filter(|s|!s.is_empty()).unwrap_or("."),settings.computer_access)?;
    let root=dunce::canonicalize(root)?;anyhow::ensure!(root.is_dir(),"Ожидается каталог");
    let owner=if let Ok(run)=crate::permissions::TASK.try_with(Clone::clone){
        app.tasks.permission_task(run.clone()).await?.map(|(id,_)|format!("task:{id}")).unwrap_or(format!("run:{run}"))
    }else{format!("project:{}",project.to_string_lossy())};
    let data=app.data.clone();let kind=kind.to_owned();let cancel=cancel.clone();
    tokio::task::spawn_blocking(move||page(&data,&owner,&project,&root,&kind,&args,&cancel)).await?
}

fn connection(data:&Path)->Result<Connection>{
    std::fs::create_dir_all(data)?;let db=Connection::open(data.join("discovery.sqlite"))?;
    db.busy_timeout(std::time::Duration::from_millis(100))?;
    let version:i64=db.query_row("PRAGMA user_version",[],|r|r.get(0))?;
    anyhow::ensure!((0..=1).contains(&version),"Неизвестная версия индекса файлов; файл сохранён");
    if version==1{return Ok(db);}
    db.execute_batch("PRAGMA journal_mode=WAL;CREATE TABLE IF NOT EXISTS scans(id TEXT PRIMARY KEY,owner TEXT NOT NULL,project TEXT NOT NULL,root TEXT NOT NULL,kind TEXT NOT NULL,options TEXT NOT NULL,next INTEGER NOT NULL DEFAULT 0,checked INTEGER NOT NULL DEFAULT 0,skipped INTEGER NOT NULL DEFAULT 0);CREATE TABLE IF NOT EXISTS queue(seq INTEGER PRIMARY KEY AUTOINCREMENT,scan TEXT NOT NULL,path TEXT NOT NULL,state TEXT NOT NULL);CREATE INDEX IF NOT EXISTS queue_scan ON queue(scan,seq);CREATE TABLE IF NOT EXISTS packets(scan TEXT NOT NULL,page INTEGER NOT NULL,body TEXT NOT NULL,PRIMARY KEY(scan,page));PRAGMA user_version=1;")?;Ok(db)
}
struct WriteTransaction{db:Connection,committed:bool}
impl std::ops::Deref for WriteTransaction{type Target=Connection;fn deref(&self)->&Connection{&self.db}}
impl Drop for WriteTransaction{fn drop(&mut self){if !self.committed{let _=self.db.execute_batch("ROLLBACK");}}}
impl WriteTransaction{
    fn begin(db:Connection,cancel:&CancellationToken)->Result<Self>{
        loop{match db.execute_batch("BEGIN IMMEDIATE"){
            Ok(())=>return Ok(Self{db,committed:false}),
            Err(rusqlite::Error::SqliteFailure(code,_)) if code.code==rusqlite::ErrorCode::DatabaseBusy||code.code==rusqlite::ErrorCode::DatabaseLocked=>{
                anyhow::ensure!(!cancel.is_cancelled(),"Ожидание индекса остановлено; прежний cursor сохранён");
                std::thread::sleep(std::time::Duration::from_millis(10));
            },Err(error)=>return Err(error.into())
        }}
    }
    fn commit(mut self)->Result<()>{self.db.execute_batch("COMMIT")?;self.committed=true;Ok(())}
}
/// A deleted conversation must also release its discovery receipts and queue.
pub async fn forget(data:PathBuf,tasks:Vec<String>,runs:Vec<String>)->Result<()>{
    tokio::task::spawn_blocking(move||->Result<()>{
        if !data.join("discovery.sqlite").exists(){return Ok(());}
        let tx=WriteTransaction::begin(connection(&data)?,&CancellationToken::new())?;
        for owner in tasks.into_iter().map(|s|format!("task:{s}")).chain(runs.into_iter().map(|s|format!("run:{s}"))){
            tx.execute("DELETE FROM packets WHERE scan IN (SELECT id FROM scans WHERE owner=?1)",[&owner])?;
            tx.execute("DELETE FROM queue WHERE scan IN (SELECT id FROM scans WHERE owner=?1)",[&owner])?;
            tx.execute("DELETE FROM scans WHERE owner=?1",[&owner])?;
        }
        tx.commit()?;Ok(())
    }).await?
}
fn options(kind:&str,args:&Value)->Result<Value>{
    let query=if kind=="find_files"{args["name"].as_str().context("Укажите имя файла")?}else if kind=="search_files"{args["query"].as_str().context("Укажите текст поиска")?}else{""};
    anyhow::ensure!(kind!="find_files"&&kind!="search_files"||!query.trim().is_empty(),"Пустой запрос");
    let exclude=args["exclude_directories"].as_array().cloned().unwrap_or_default();
    anyhow::ensure!(exclude.iter().all(|v|v.as_str().is_some_and(|s|!s.is_empty()&&!s.contains(['/', '\\'])&&s!="..")),"Исключения должны быть именами каталогов");
    Ok(json!({"query":query,"pattern":args["pattern"].as_str().unwrap_or("*"),"encoding":args["encoding"].as_str().unwrap_or("auto"),"exclude":exclude}))
}
fn safe(root:&Path,path:&Path)->Result<PathBuf>{
    let metadata=std::fs::symlink_metadata(path)?;anyhow::ensure!(!metadata.file_type().is_symlink(),"Символьная ссылка пропущена");
    #[cfg(windows)]{use std::os::windows::fs::MetadataExt;anyhow::ensure!(metadata.file_attributes()&0x400==0,"Reparse point пропущен");}
    let canonical=dunce::canonicalize(path)?;anyhow::ensure!(canonical.starts_with(root),"Путь выходит за выбранный каталог");Ok(canonical)
}

pub fn page(data:&Path,owner:&str,project:&Path,root:&Path,kind:&str,args:&Value,cancel:&CancellationToken)->Result<Value>{
    anyhow::ensure!(["find_files","search_files","list_files","inspect_project"].contains(&kind),"Неизвестный вид обзора");
    let limit=args["limit"].as_u64().unwrap_or(64);anyhow::ensure!((1..=100).contains(&limit),"Размер пакета от 1 до 100");
    let tx=WriteTransaction::begin(connection(data)?,cancel)?;
    let (id,index)=if let Some(cursor)=args["cursor"].as_str(){let(a,b)=cursor.split_once(':').context("Неверный cursor")?;(uuid::Uuid::parse_str(a)?.to_string(),b.parse::<i64>()?)}else{(uuid::Uuid::new_v4().to_string(),0)};
    anyhow::ensure!(index>=0,"Неверная страница");
    let existing:Option<(String,String,String,String,String,i64,i64,i64)>=tx.query_row("SELECT owner,project,root,kind,options,next,checked,skipped FROM scans WHERE id=?1",[&id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?))).optional()?;
    let (opts,mut checked,mut skipped)=if let Some((saved_owner,saved_project,saved_root,saved_kind,opts,next,checked,skipped))=existing{
        anyhow::ensure!(saved_owner==owner&&Path::new(&saved_project)==project&&Path::new(&saved_root)==root&&saved_kind==kind,"Cursor недоступен этой задаче или области");
        if let Some(body)=tx.query_row("SELECT body FROM packets WHERE scan=?1 AND page=?2",params![id,index],|r|r.get::<_,String>(0)).optional()?{return Ok(serde_json::from_str(&body)?);}
        anyhow::ensure!(index==next,"Неизвестная страница cursor");
        // Parameters are immutable in a scan. A cursor needs only the same root/kind.
        (serde_json::from_str::<Value>(&opts)?,checked,skipped)
    }else{
        anyhow::ensure!(args["cursor"].is_null(),"Cursor не найден");let opts=options(kind,args)?;
        tx.execute("INSERT INTO scans(id,owner,project,root,kind,options) VALUES(?1,?2,?3,?4,?5,?6)",params![id,owner,project.to_string_lossy(),root.to_string_lossy(),kind,opts.to_string()])?;
        tx.execute("INSERT INTO queue(scan,path,state) VALUES(?1,?2,'{}')",params![id,root.to_string_lossy()])?;(opts,0,0)
    };
    let mut matches=vec![];let mut errors=vec![];let mut processed=0;let mut text_bytes=0usize;let mut interrupted=false;
    while matches.len()<limit as usize&&processed<1000&&text_bytes<1024*1024{
        if cancel.is_cancelled(){interrupted=true;break;}
        let item:Option<(i64,String,String)>=tx.query_row("SELECT seq,path,state FROM queue WHERE scan=?1 ORDER BY seq LIMIT 1",[&id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
        let Some((seq,path,state))=item else{break;};let path=PathBuf::from(path);let mut state:Value=serde_json::from_str(&state)?;
        let previous_matches=matches.len();let previous_skipped=skipped;let previous_errors=errors.len();
        let result=(||->Result<bool>{
            let resolved=safe(root,&path)?;
            if resolved.is_dir(){
                if resolved!=root {
                    let relative=resolved.strip_prefix(root)?.to_string_lossy();
                    if kind=="inspect_project"||kind=="list_files" { matches.push(json!({"path":relative,"directory":true})); }
                    let excluded=opts["exclude"].as_array().is_some_and(|items|items.iter().any(|v|v.as_str()==resolved.file_name().and_then(|n|n.to_str())));
                    if excluded { skipped+=1;return Ok(true); }
                    if kind=="list_files" { return Ok(true); }
                }
                // A directory is spooled in a savepoint: cancellation never commits a partial listing.
                tx.execute_batch("SAVEPOINT directory_packet")?;
                let enumeration=(||->Result<()>{for entry in std::fs::read_dir(&resolved)?{
                    anyhow::ensure!(!cancel.is_cancelled(),"Обход остановлен");
                    match entry{Ok(entry)=>{let child=entry.path();
                        let Some(child_text)=child.to_str() else { skipped+=1;if errors.len()<16{errors.push(json!({"path_display":child.to_string_lossy(),"error":"Имя не представимо в UTF-8; путь не подменён"}));}continue;};tx.execute("INSERT INTO queue(scan,path,state) VALUES(?1,?2,'{}')",params![id,child_text])?;},Err(error)=>{skipped+=1;if errors.len()<16{errors.push(json!({"path":resolved,"error":error.to_string()}));}}}
                }Ok(())})();
                if let Err(error)=enumeration{tx.execute_batch("ROLLBACK TO directory_packet; RELEASE directory_packet")?;return Err(error);}
                tx.execute_batch("RELEASE directory_packet")?;
                return Ok(true);
            }
            if !resolved.is_file(){anyhow::bail!("Необычный тип файла пропущен");}
            let name=resolved.file_name().unwrap_or_default().to_string_lossy().to_lowercase();
            if kind=="find_files"{let query=opts["query"].as_str().unwrap().to_lowercase();if crate::filesystem::wildcard(&query,&name){matches.push(json!(resolved));}return Ok(true);}
            if kind=="list_files"||kind=="inspect_project"{matches.push(json!({"path":resolved.strip_prefix(root)?.to_string_lossy(),"directory":false}));return Ok(true);}
            if !crate::filesystem::wildcard(&opts["pattern"].as_str().unwrap().to_lowercase(),&name){return Ok(true);}
            let mut read=json!({"max_lines":500,"encoding":opts["encoding"]});
            if state["offset"].is_number(){read["offset"]=state["offset"].clone();read["revision"]=state["revision"].clone();read["encoding"]=state["encoding"].clone();}
            let packet=crate::text_source::page(&resolved,&read,cancel)?;let text=packet["text"].as_str().unwrap();text_bytes+=text.len();
            let tail=state["tail"].as_str().unwrap_or("");let lowered=text.to_lowercase();let joined=format!("{tail}{lowered}");let query=opts["query"].as_str().unwrap().to_lowercase();
            // One receipt per source page with all occurrences counted. Range reads recover full text.
            let first=joined.match_indices(&query).find(|(start,_)|*start+query.len()>tail.len()).map(|(start,_)|start);
            let occurrences=joined.match_indices(&query).filter(|(start,_)|*start+query.len()>tail.len()).count();
            if occurrences>0{matches.push(json!({"path":resolved,"start_byte":packet["start_byte"],"end_byte":packet["end_byte"],"revision":packet["revision"],"encoding":packet["encoding"],"occurrences":occurrences,"crosses_previous_page":first.is_some_and(|p|p<tail.len()),"text":first.map(|start|crate::tools::truncate(&joined[start..],300)),"line":first.map(|start|state["lines"].as_u64().unwrap_or(0)+1-tail.bytes().filter(|c|*c==b'\n').count() as u64+joined[..start].bytes().filter(|c|*c==b'\n').count() as u64),"range_note":"Совпадения относятся к этому диапазону и его границе с предыдущим; text — приведённый к нижнему регистру фрагмент первого совпадения"}));}
            if packet["has_more"]==true{let keep=query.chars().count().saturating_sub(1);let tail:String=joined.chars().rev().take(keep).collect::<Vec<_>>().into_iter().rev().collect();state=json!({"offset":packet["next_offset"],"revision":packet["revision"],"encoding":packet["encoding"],"tail":tail,"lines":state["lines"].as_u64().unwrap_or(0)+text.bytes().filter(|c|*c==b'\n').count() as u64});tx.execute("UPDATE queue SET state=?1 WHERE seq=?2",params![state.to_string(),seq])?;Ok(false)}else{Ok(true)}
        })();
        match result{
            Ok(done)=>{if done{tx.execute("DELETE FROM queue WHERE seq=?1",[seq])?;checked+=1;}},
            Err(error)=>{if cancel.is_cancelled(){matches.truncate(previous_matches);skipped=previous_skipped;errors.truncate(previous_errors);interrupted=true;break;}skipped+=1;if errors.len()<16{errors.push(json!({"path":path,"error":error.to_string()}));}tx.execute("DELETE FROM queue WHERE seq=?1",[seq])?;}
        }
        processed+=1;
    }
    let remains:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM queue WHERE scan=?1)",[&id],|r|r.get(0))?;
    let mut body=json!({"matches":matches,"files":if kind=="list_files"||kind=="inspect_project"{Some(&matches)}else{None},"root":root,"query":opts["query"],"complete":!remains&&skipped==0,"traversal_finished":!remains,"cancelled":interrupted,"checked_entries":checked,"inaccessible_or_excluded":skipped,"errors":errors,"has_more":remains,"cursor":format!("{id}:{index}"),"previous_cursor":if index>0{Some(format!("{id}:{}",index-1))}else{None},"next_cursor":if remains{Some(format!("{id}:{}",index+1))}else{None},"snapshot_kind":"per_directory_spool_and_file_size_mtime","note":"Каждый каталог фиксируется при перечислении; это не атомарный снимок проекта. Изменённые файлы и ссылки сообщаются как пропуски. Продолжайте next_cursor с той же областью; размер пакета не ограничивает весь поиск."});
    if kind=="inspect_project" {
        let overview=crate::projects::overview(root,matches.clone(),!remains,skipped as usize)?;
        for (key,value) in overview.as_object().unwrap(){if !["files","traversal_finished","note"].contains(&key.as_str()){body[key]=value.clone();}}
    }
    tx.execute("INSERT INTO packets(scan,page,body) VALUES(?1,?2,?3)",params![id,index,body.to_string()])?;
    tx.execute("UPDATE scans SET next=?1,checked=?2,skipped=?3 WHERE id=?4",params![index+1,checked,skipped,id])?;tx.commit()?;Ok(body)
}

#[cfg(test)]
mod tests{
    use super::*;
    fn fixture()->(PathBuf,PathBuf,PathBuf){let data=std::env::temp_dir().join(format!("discovery-{}",uuid::Uuid::new_v4()));let root=data.join("project");std::fs::create_dir_all(&root).unwrap();let root=dunce::canonicalize(root).unwrap();(data.clone(),root.clone(),root)}
    fn collect(data:&Path,root:&Path,kind:&str,mut args:Value)->Vec<Value>{let mut all=vec![];loop{let body=page(data,"task:one",root,root,kind,&args,&CancellationToken::new()).unwrap();all.extend(body["matches"].as_array().unwrap().clone());if body["has_more"]!=true{assert_eq!(body["complete"],true,"{body}");break;}let cursor=body["next_cursor"].clone();let retry=page(data,"task:one",root,root,kind,&args,&CancellationToken::new()).unwrap();if args["cursor"].is_string(){assert_eq!(retry,body);}args["cursor"]=cursor;}all}
    #[test]
    fn more_than_old_caps_deep_tree_replay_and_scope(){
        let(data,root,_)=fixture();let deep=root.join("a/b/c/d/e/f/g/h");std::fs::create_dir_all(&deep).unwrap();for n in 0..401{std::fs::write(deep.join(format!("файл-{n}.txt")),"needle").unwrap();}
        let results=collect(&data,&root,"find_files",json!({"name":"*.txt","limit":7}));assert_eq!(results.len(),401);
        let listed=collect(&data,&root,"list_files",json!({"limit":1}));assert_eq!(listed.len(),1);assert_eq!(listed[0]["directory"],true);
        let inspected=collect(&data,&root,"inspect_project",json!({"limit":5}));assert_eq!(inspected.iter().filter(|v|v["directory"]==false).count(),401);
        let body=page(&data,"task:one",&root,&root,"find_files",&json!({"name":"*.txt","limit":1}),&CancellationToken::new()).unwrap();let args=json!({"cursor":body["next_cursor"]});
        assert!(page(&data,"task:foreign",&root,&root,"find_files",&args,&CancellationToken::new()).is_err());
        assert!(page(&data,"task:one",&root,&deep,"find_files",&args,&CancellationToken::new()).is_err());
        std::fs::remove_dir_all(data).unwrap();
    }
    #[test]
    fn huge_unicode_lines_encoded_files_and_cancel_resume(){
        let(data,root,_)=fixture();let original=format!("first\n{}НУЖНЫЙ ТЕКСТ\nlast", "я".repeat(1_100_000));std::fs::write(root.join("large.txt"),&original).unwrap();
        let mut utf16=vec![0xff,0xfe];for unit in "один\r\nНУЖНЫЙ ТЕКСТ 🦀\r\n".encode_utf16(){utf16.extend(unit.to_le_bytes());}std::fs::write(root.join("utf16.txt"),utf16).unwrap();
        // Force a match across 20k-byte source packet boundary.
        std::fs::write(root.join("boundary.txt"),format!("{}НУЖНЫЙ ТЕКСТ", "x".repeat(19995))).unwrap();
        let matches=collect(&data,&root,"search_files",json!({"query":"нужный текст","limit":1}));assert_eq!(matches.len(),3);assert!(matches.iter().any(|v|v["crosses_previous_page"]==true));assert!(matches.iter().any(|v|v["encoding"]=="utf-16le"&&v["line"]==2));
        let cancel=CancellationToken::new();cancel.cancel();let first=page(&data,"task:one",&root,&root,"find_files",&json!({"name":"*"}),&cancel).unwrap();assert_eq!(first["cancelled"],true);assert!(first["next_cursor"].is_string());
        let resumed=page(&data,"task:one",&root,&root,"find_files",&json!({"cursor":first["next_cursor"]}),&CancellationToken::new()).unwrap();assert_eq!(resumed["matches"].as_array().unwrap().len(),3);
        std::fs::remove_dir_all(data).unwrap();
    }
    #[test]
    fn contention_can_be_cancelled_without_losing_receipt(){
        let(data,root,_)=fixture();std::fs::write(root.join("one.txt"),"fixture").unwrap();
        let first=page(&data,"task:one",&root,&root,"find_files",&json!({"name":"*","limit":1}),&CancellationToken::new()).unwrap();
        let lock=connection(&data).unwrap();lock.execute_batch("BEGIN IMMEDIATE").unwrap();
        let cancel=CancellationToken::new();let token=cancel.clone();let d=data.clone();let r=root.clone();
        let cursor=first["cursor"].clone();let worker=std::thread::spawn(move||page(&d,"task:one",&r,&r,"find_files",&json!({"cursor":cursor}),&token));
        std::thread::sleep(std::time::Duration::from_millis(30));let started=std::time::Instant::now();cancel.cancel();
        assert!(worker.join().unwrap().is_err());assert!(started.elapsed()<std::time::Duration::from_secs(2));
        lock.execute_batch("ROLLBACK").unwrap();drop(lock);
        let replay=page(&data,"task:one",&root,&root,"find_files",&json!({"cursor":first["cursor"]}),&CancellationToken::new()).unwrap();assert_eq!(first,replay);
        std::fs::remove_dir_all(data).unwrap();
    }
    #[test]
    fn mutation_is_reported_and_exclusions_are_explicit(){
        let(data,root,_)=fixture();let file=root.join("long.txt");std::fs::write(&file,"n".repeat(2_200_000)).unwrap();
        let first=page(&data,"task:one",&root,&root,"search_files",&json!({"query":"absent"}),&CancellationToken::new()).unwrap();assert_eq!(first["has_more"],true);std::fs::write(&file,"changed").unwrap();
        let next=page(&data,"task:one",&root,&root,"search_files",&json!({"cursor":first["next_cursor"]}),&CancellationToken::new()).unwrap();assert_eq!(next["complete"],false);assert!(next["errors"].as_array().unwrap().iter().any(|v|v["error"].as_str().unwrap().contains("изменился")));
        std::fs::create_dir(root.join("vendor")).unwrap();std::fs::write(root.join("vendor/hidden.txt"),"content").unwrap();let excluded=page(&data,"task:one",&root,&root,"inspect_project",&json!({"exclude_directories":["vendor"]}),&CancellationToken::new()).unwrap();assert_eq!(excluded["complete"],false);assert_eq!(excluded["traversal_finished"],true);
        std::fs::remove_dir_all(data).unwrap();
    }
}
