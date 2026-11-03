//! Indexed conversation storage. Immutable originals are never the live write path.
use anyhow::{Context,Result};
use rusqlite::{params,Connection,OptionalExtension};
use serde_json::{json,Value};
use sha2::{Digest,Sha256};
use std::{collections::HashSet,fs::File,io::{Read,Seek,SeekFrom,Write},path::{Path,PathBuf}};
use tokio_util::sync::CancellationToken;
use crate::json_stream::{Reader,Span};

pub fn install(db:&Connection)->Result<()>{db.execute_batch("CREATE TABLE IF NOT EXISTS chat_conversations(id TEXT PRIMARY KEY,title TEXT NOT NULL,mode TEXT NOT NULL,updated TEXT NOT NULL,revision INTEGER NOT NULL DEFAULT 0,epoch INTEGER NOT NULL DEFAULT 0,next_position INTEGER NOT NULL DEFAULT 0,active_count INTEGER NOT NULL DEFAULT 0,state TEXT NOT NULL,original TEXT,source_hash TEXT);
CREATE INDEX IF NOT EXISTS chat_sidebar ON chat_conversations(state,updated,id);
CREATE TABLE IF NOT EXISTS chat_bodies(id TEXT PRIMARY KEY,owner TEXT NOT NULL,bytes INTEGER NOT NULL DEFAULT 0,characters INTEGER NOT NULL DEFAULT 0,cost INTEGER NOT NULL DEFAULT 0,hash TEXT,complete INTEGER NOT NULL DEFAULT 0);
CREATE TABLE IF NOT EXISTS chat_chunks(body TEXT NOT NULL REFERENCES chat_bodies(id) ON DELETE CASCADE,part INTEGER NOT NULL,char_start INTEGER NOT NULL,characters INTEGER NOT NULL,text TEXT NOT NULL,PRIMARY KEY(body,part));
CREATE INDEX IF NOT EXISTS chat_chunk_seek ON chat_chunks(body,char_start);
CREATE TABLE IF NOT EXISTS chat_attempts(id TEXT PRIMARY KEY,conversation TEXT NOT NULL REFERENCES chat_conversations(id) ON DELETE CASCADE,position INTEGER NOT NULL,active INTEGER NOT NULL DEFAULT 1,state TEXT NOT NULL);
CREATE UNIQUE INDEX IF NOT EXISTS chat_attempt_active ON chat_attempts(conversation,position) WHERE active=1;
CREATE TABLE IF NOT EXISTS chat_messages(key TEXT PRIMARY KEY,conversation TEXT NOT NULL REFERENCES chat_conversations(id) ON DELETE CASCADE,position INTEGER NOT NULL,ordinal INTEGER NOT NULL,display_seq INTEGER NOT NULL,attempt TEXT NOT NULL REFERENCES chat_attempts(id) ON DELETE CASCADE,original_id TEXT NOT NULL,role TEXT NOT NULL,created TEXT NOT NULL,content TEXT NOT NULL REFERENCES chat_bodies(id),material TEXT REFERENCES chat_bodies(id),raw TEXT NOT NULL REFERENCES chat_bodies(id),metadata TEXT NOT NULL,UNIQUE(attempt,ordinal));
CREATE INDEX IF NOT EXISTS chat_message_order ON chat_messages(conversation,display_seq);
CREATE TABLE IF NOT EXISTS chat_history(generation TEXT NOT NULL,conversation TEXT NOT NULL REFERENCES chat_conversations(id) ON DELETE CASCADE,item INTEGER NOT NULL,round INTEGER NOT NULL,cost INTEGER NOT NULL,body TEXT NOT NULL REFERENCES chat_bodies(id),PRIMARY KEY(generation,item));
CREATE INDEX IF NOT EXISTS chat_history_round ON chat_history(generation,round,item);
CREATE TABLE IF NOT EXISTS chat_working(conversation TEXT PRIMARY KEY REFERENCES chat_conversations(id) ON DELETE CASCADE,generation TEXT NOT NULL,model TEXT NOT NULL,workspace TEXT NOT NULL,task TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS chat_imports(conversation TEXT PRIMARY KEY REFERENCES chat_conversations(id) ON DELETE CASCADE,messages INTEGER NOT NULL DEFAULT 0,history INTEGER NOT NULL DEFAULT 0,round INTEGER NOT NULL DEFAULT 0);
CREATE TABLE IF NOT EXISTS chat_exports(id TEXT PRIMARY KEY,conversation TEXT NOT NULL REFERENCES chat_conversations(id) ON DELETE CASCADE,header TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS chat_export_items(export TEXT NOT NULL REFERENCES chat_exports(id) ON DELETE CASCADE,sequence INTEGER NOT NULL,key TEXT NOT NULL,content TEXT NOT NULL,raw TEXT NOT NULL,metadata TEXT NOT NULL,PRIMARY KEY(export,sequence));
CREATE INDEX IF NOT EXISTS chat_export_content ON chat_export_items(export,content);
CREATE INDEX IF NOT EXISTS chat_export_raw ON chat_export_items(export,raw);")?;Ok(())}
fn open(data:&Path)->Result<Connection>{let db=Connection::open(data.join("tasks.sqlite3"))?;db.busy_timeout(std::time::Duration::from_secs(5))?;db.pragma_update(None,"foreign_keys",true)?;Ok(db)}
fn present(db:&Connection,id:&str)->Result<bool>{Ok(db.query_row("SELECT EXISTS(SELECT 1 FROM chat_conversations WHERE id=?1 AND state='ready')",[id],|r|r.get(0))?)}
fn alive(db:&Connection,id:&str)->Result<()>{if db.query_row("SELECT EXISTS(SELECT 1 FROM deleted_conversations WHERE id=?1)",[id],|r|r.get::<_,bool>(0))?{return Err(crate::task_store::MissingResource("Чат удалён").into());}Ok(())}

// A body sink writes scalar-aligned packets; committed staging bodies are not visible messages.
thread_local!{static BODY_OWNER:std::cell::RefCell<String>=const{std::cell::RefCell::new(String::new())};}
struct BodyScope(String);
impl BodyScope{fn new(id:&str)->Self{Self(BODY_OWNER.with(|owner|owner.replace(id.to_owned())))}}
impl Drop for BodyScope{fn drop(&mut self){BODY_OWNER.with(|owner|{owner.replace(self.0.clone());});}}
struct Body<'a>{db:&'a Connection,id:String,part:i64,bytes:i64,characters:i64,cost:usize,hash:Sha256,own_transaction:bool,pending:usize}
impl<'a> Body<'a>{
    fn new(db:&'a Connection)->Result<Self>{let own_transaction=db.is_autocommit();if own_transaction{db.execute_batch("BEGIN IMMEDIATE")?;}let id=uuid::Uuid::new_v4().to_string();db.execute("INSERT INTO chat_bodies(id,owner) VALUES(?1,?2)",params![id,BODY_OWNER.with(|v|v.borrow().clone())])?;Ok(Self{db,id,part:0,bytes:0,characters:0,cost:0,hash:Sha256::new(),own_transaction,pending:0})}
    fn push(&mut self,text:&str)->Result<()>{if text.is_empty(){return Ok(());}let count=i64::try_from(text.chars().count())?;self.db.execute("INSERT INTO chat_chunks VALUES(?1,?2,?3,?4,?5)",params![self.id,self.part,self.characters,count,text])?;self.part+=1;self.characters+=count;self.bytes+=i64::try_from(text.len())?;self.cost+=crate::context::text_tokens(text);self.hash.update(text.as_bytes());self.pending+=1;if self.own_transaction&&self.pending>=64{self.db.execute_batch("COMMIT; BEGIN IMMEDIATE")?;self.pending=0;}Ok(())}
    fn finish(mut self)->Result<String>{self.db.execute("UPDATE chat_bodies SET bytes=?1,characters=?2,cost=?3,hash=?4,complete=1 WHERE id=?5",params![self.bytes,self.characters,i64::try_from(self.cost)?,format!("{:x}",self.hash.clone().finalize()),self.id])?;if self.own_transaction{self.db.execute_batch("COMMIT")?;self.own_transaction=false;}Ok(self.id.clone())}
}
impl Drop for Body<'_>{fn drop(&mut self){if self.own_transaction{let _=self.db.execute_batch("ROLLBACK");}}}
pub fn text_body(db:&Connection,text:&str)->Result<String>{let mut body=Body::new(db)?;let mut start=0;while start<text.len(){let mut end=(start+16384).min(text.len());while !text.is_char_boundary(end){end-=1;}body.push(&text[start..end])?;start=end;}body.finish()}
fn json_body<T:serde::Serialize>(db:&Connection,value:&T)->Result<String>{
    struct Sink<'a>{body:Body<'a>,pending:Vec<u8>}
    impl Write for Sink<'_>{
        fn write(&mut self,bytes:&[u8])->std::io::Result<usize>{let total=bytes.len();for part in bytes.chunks(16384){self.pending.extend_from_slice(part);while self.pending.len()>=16384{let mut end=16384;while std::str::from_utf8(&self.pending[..end]).is_err(){end-=1;if end<16380{return Err(std::io::Error::other("Invalid serializer UTF-8"));}}self.body.push(std::str::from_utf8(&self.pending[..end]).map_err(std::io::Error::other)?).map_err(std::io::Error::other)?;self.pending.drain(..end);}}Ok(total)}
        fn flush(&mut self)->std::io::Result<()>{if !self.pending.is_empty(){self.body.push(std::str::from_utf8(&self.pending).map_err(std::io::Error::other)?).map_err(std::io::Error::other)?;self.pending.clear();}Ok(())}
    }
    let mut writer=Sink{body:Body::new(db)?,pending:Vec::with_capacity(16388)};serde_json::to_writer(&mut writer,value)?;writer.flush()?;anyhow::ensure!(writer.pending.is_empty(),"Незавершённый UTF-8 JSON");writer.body.finish()
}
fn raw_body(db:&Connection,path:&Path,span:Span)->Result<String>{let mut file=File::open(path)?;file.seek(SeekFrom::Start(span.start))?;let mut remaining=span.end-span.start;let mut body=Body::new(db)?;let mut bytes=vec![0;16384];let mut tail=Vec::new();while remaining>0{let n=file.read(&mut bytes[..remaining.min(16384) as usize])?;anyhow::ensure!(n>0,"Архив JSON оборван");remaining-=n as u64;tail.extend_from_slice(&bytes[..n]);let valid=match std::str::from_utf8(&tail){Ok(_)=>tail.len(),Err(error)=>{anyhow::ensure!(error.error_len().is_none(),"Некорректный UTF-8 архив");error.valid_up_to()}};body.push(std::str::from_utf8(&tail[..valid])?)?;tail.drain(..valid);}anyhow::ensure!(tail.is_empty(),"Незавершённый UTF-8 архив");body.finish()}
fn decoded_body(db:&Connection,path:&Path,span:Span,cancel:&CancellationToken)->Result<String>{let mut body=Body::new(db)?;let mut reader=Reader::open(path,cancel)?;reader.seek(span.start)?;reader.string(|part|body.push(part))?;anyhow::ensure!(reader.position==span.end,"Неверная граница JSON строки");body.finish()}
pub fn slice(db:&Connection,body:&str,offset:u64,count:u64)->Result<Value>{
    anyhow::ensure!((1..=16000).contains(&count),"Размер текста от1 до16000 символов");let (total,bytes,hash):(i64,i64,String)=db.query_row("SELECT characters,bytes,hash FROM chat_bodies WHERE id=?1 AND complete=1",[body],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;let total=u64::try_from(total)?;let bytes=u64::try_from(bytes)?;anyhow::ensure!(offset<=total,"Смещение вне текста");
    let first:i64=db.query_row("SELECT char_start FROM chat_chunks WHERE body=?1 AND char_start<=?2 ORDER BY char_start DESC LIMIT 1",params![body,i64::try_from(offset)?],|r|r.get(0)).optional()?.unwrap_or(0);
    let mut q=db.prepare("SELECT char_start,text FROM chat_chunks WHERE body=?1 AND char_start>=?2 AND char_start<?3 ORDER BY char_start")?;let mut out=String::new();let mut expected_start=first as u64;for row in q.query_map(params![body,first,i64::try_from(offset.saturating_add(count))?],|r|Ok((r.get::<_,i64>(0)? as u64,r.get::<_,String>(1)?)))?{let(start,text)=row?;anyhow::ensure!(start==expected_start,"Сохранённый текст повреждён: разрыв диапазона");expected_start+=text.chars().count() as u64;out.extend(text.chars().skip(offset.saturating_sub(start) as usize).take(count.saturating_sub(out.chars().count() as u64) as usize));}
    let read=out.chars().count() as u64;anyhow::ensure!(read==count.min(total-offset),"Сохранённый текст повреждён: отсутствует часть диапазона");
    let next=offset+read;Ok(json!({"text":out,"offset":offset,"next_offset":next,"has_more":next<total,"characters":total,"bytes":bytes,"revision":hash,"body":body}))
}
pub(crate) fn whole(db:&Connection,body:&str)->Result<String>{
    let (bytes,hash):(i64,String)=db.query_row("SELECT bytes,hash FROM chat_bodies WHERE id=?1 AND complete=1",[body],|r|Ok((r.get(0)?,r.get(1)?)))?;
    let mut q=db.prepare("SELECT part,text FROM chat_chunks WHERE body=?1 ORDER BY part")?;let mut result=String::new();let mut expected=0i64;
    for row in q.query_map([body],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?)))?{let(part,text)=row?;anyhow::ensure!(part==expected,"Сохранённый текст повреждён: пропущен пакет");expected+=1;result.push_str(&text);}
    anyhow::ensure!(i64::try_from(result.len())?==bytes&&format!("{:x}",Sha256::digest(result.as_bytes()))==hash,"Сохранённый текст повреждён: размер или SHA256 не совпадает");Ok(result)
}

pub fn header(db:&Connection,id:&str)->Result<Value>{alive(db,id)?;Ok(db.query_row("SELECT title,updated,mode,active_count,revision,epoch,next_position FROM chat_conversations WHERE id=?1 AND state='ready'",[id],|r|Ok(json!({"id":id,"title":r.get::<_,String>(0)?,"updated":r.get::<_,String>(1)?,"mode":r.get::<_,String>(2)?,"count":r.get::<_,i64>(3)?,"revision":r.get::<_,i64>(4)?,"epoch":r.get::<_,i64>(5)?,"next_position":r.get::<_,i64>(6)?})))?)}
pub fn create(db:&Connection,id:&str,title:&str,mode:&str)->Result<()>{alive(db,id)?;db.execute("INSERT INTO chat_conversations(id,title,mode,updated,state) VALUES(?1,?2,?3,?4,'ready')",params![id,title,mode,crate::api::now()])?;Ok(())}

/// Called inside the transaction that accepts the Run and associates its Task.
pub fn accept(db:&Connection,run:&str,id:&str,input:&Value)->Result<()>{
    if !present(db,id)?{return Ok(());} // Legacy Store unit fixtures may not have a conversation.
    alive(db,id)?;let h=header(db,id)?;let retry=input["retry"]==true;
    let position=if retry{db.query_row("SELECT position FROM chat_attempts WHERE conversation=?1 AND active=1 ORDER BY position DESC LIMIT 1",[id],|r|r.get::<_,i64>(0)).optional()?.context("Нет сообщения для повторной генерации")?}else{h["next_position"].as_i64().unwrap()};
    let removed=if retry{let count:i64=db.query_row("SELECT COUNT(*) FROM chat_messages m JOIN chat_attempts a ON a.id=m.attempt WHERE a.conversation=?1 AND a.position=?2 AND a.active=1",params![id,position],|r|r.get(0))?;db.execute("UPDATE chat_attempts SET active=0 WHERE conversation=?1 AND position=?2",params![id,position])?;count}else{0};
    db.execute("INSERT INTO chat_attempts VALUES(?1,?2,?3,1,'accepted')",params![run,id,position])?;
    let now=crate::api::now();let user=crate::api::Message{material:None,diagnostics:None,duration_ms:None,model:None,id:input["request_id"].as_str().unwrap_or(run).into(),role:"user".into(),content:input["content"].as_str().unwrap_or("").into(),attachments:vec![],tools:vec![],created:now.clone()};
    save_message(db,id,run,position,0,&user)?;
    let title=if h["count"]==0{user.content.chars().take(55).collect::<String>()}else{h["title"].as_str().unwrap().into()};
    db.execute("UPDATE chat_conversations SET active_count=active_count-?1+1,mode=?2,title=?3,updated=?4,revision=revision+1,epoch=epoch+?5,next_position=MAX(next_position,?6) WHERE id=?7",params![removed,input["mode"].as_str().unwrap_or("chat"),title,now,i64::from(retry),position+2,id])?;Ok(())
}
fn save_message(db:&Connection,id:&str,run:&str,position:i64,ordinal:i64,message:&crate::api::Message)->Result<()>{
    let _body_scope=BodyScope::new(id);
    let content=text_body(db,&message.content)?;let material=message.material.as_deref().map(|text|text_body(db,text)).transpose()?;
    // Serde writes one message, never the archive. Current turn content already exists in engine.
    let raw=json_body(db,message)?;let mut metadata=json!({"id":message.id,"role":message.role,"created":message.created,"model":message.model,"duration_ms":message.duration_ms,"diagnostics":message.diagnostics,"attachments":message.attachments,"tools":message.tools});
    for field in ["diagnostics","tools","attachments"]{if metadata[field].to_string().len()>65536{metadata[format!("{field}_partial")]=json!(true);metadata[field]=if field=="diagnostics"{Value::Null}else{json!([])};}}
    let key=uuid::Uuid::new_v4().to_string();db.execute("INSERT INTO chat_messages VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13) ON CONFLICT(attempt,ordinal) DO UPDATE SET original_id=excluded.original_id,role=excluded.role,created=excluded.created,content=excluded.content,material=excluded.material,raw=excluded.raw,metadata=excluded.metadata",params![key,id,position,ordinal,position+ordinal,run,message.id,message.role,message.created,content,material,raw,metadata.to_string()])?;Ok(())
}
pub fn message_view(db:&Connection,key:&str,characters:u64)->Result<Value>{
    let (id,metadata,body):(String,String,String)=db.query_row("SELECT conversation,metadata,content FROM chat_messages WHERE key=?1",[key],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;alive(db,&id)?;
    let mut view:Value=serde_json::from_str(&metadata)?;let page=slice(db,&body,0,characters)?;view["content"]=page["text"].clone();view["content_partial"]=page["has_more"].clone();view["content_source"]=json!({"message":key,"characters":page["characters"],"bytes":page["bytes"],"revision":page["revision"]});
    if !view["attachments"].is_array(){view["attachments"]=json!([]);}if !view["tools"].is_array(){view["tools"]=json!([]);}for tool in view["tools"].as_array_mut().unwrap(){crate::tools::public_record(tool);}Ok(view)
}
pub fn messages_page(db:&Connection,id:&str,before:Option<u64>,limit:u64,revision:Option<&str>)->Result<Value>{
    anyhow::ensure!((1..=32).contains(&limit),"Размер страницы сообщений от1 до32");let h=header(db,id)?;let current=format!("sql:{}",h["epoch"]);if revision.is_some_and(|v|v!=current){return Err(crate::chat_pages::Changed.into());}
    let end=before.unwrap_or(h["next_position"].as_u64().unwrap());anyhow::ensure!(end<=h["next_position"].as_u64().unwrap(),"Неверная страница сообщений");
    let mut q=db.prepare("SELECT m.key,m.display_seq FROM chat_messages m JOIN chat_attempts a ON a.id=m.attempt AND a.active=1 WHERE m.conversation=?1 AND m.display_seq<?2 ORDER BY m.display_seq DESC LIMIT ?3")?;
    let mut rows=q.query_map(params![id,i64::try_from(end)?,i64::try_from(limit)?],|r|Ok((r.get::<_,String>(0)?,r.get::<_,i64>(1)? as u64)))?.collect::<rusqlite::Result<Vec<_>>>()?;rows.reverse();let start=rows.first().map_or(0,|(_,seq)|*seq);let mut messages=Vec::new();for(key,_)in rows{messages.push(message_view(db,&key,8000)?);}
    let rank:i64=db.query_row("SELECT COUNT(*) FROM chat_messages m JOIN chat_attempts a ON a.id=m.attempt AND a.active=1 WHERE m.conversation=?1 AND m.display_seq<?2",params![id,i64::try_from(start)?],|r|r.get(0))?;
    let newer:i64=db.query_row("SELECT COUNT(*) FROM chat_messages m JOIN chat_attempts a ON a.id=m.attempt AND a.active=1 WHERE m.conversation=?1 AND m.display_seq>=?2",params![id,i64::try_from(end)?],|r|r.get(0))?;
    let newer_before:Option<i64>=if newer==0{None}else{Some(db.query_row("SELECT m.display_seq FROM chat_messages m JOIN chat_attempts a ON a.id=m.attempt AND a.active=1 WHERE m.conversation=?1 AND m.display_seq>=?2 ORDER BY m.display_seq LIMIT 1 OFFSET ?3",params![id,i64::try_from(end)?,i64::try_from(limit)?],|r|r.get(0)).optional()?.unwrap_or(h["next_position"].as_i64().unwrap()))};
    Ok(json!({"id":id,"title":h["title"],"updated":h["updated"],"mode":h["mode"],"messages":messages,"message_page":{"start":rank,"end":rank+messages.len() as i64,"cursor_end":end,"total":h["count"],"before":if rank>0{Some(start)}else{None},"newer_before":newer_before,"at_latest":newer==0,"revision":current,"limit":limit}}))
}
impl crate::task_store::Store{
    pub async fn ensure_chat(&self,data:PathBuf,id:String,cancel:CancellationToken)->Result<()>{tokio::task::spawn_blocking(move||ensure_import(&data,&id,&cancel)).await?}
    pub async fn chat_header(&self,id:String)->Result<Value>{self.access(move|db|header(db,&id)).await}
    pub async fn chat_page(&self,id:String,before:Option<u64>,limit:u64,revision:Option<String>)->Result<Value>{self.access(move|db|messages_page(db,&id,before,limit,revision.as_deref())).await}
    pub async fn chat_create(&self,id:String,title:String,mode:String)->Result<()>{self.access(move|db|create(db,&id,&title,&mode)).await}
    pub async fn chat_body_page(&self,id:String,key:String,offset:u64,count:u64,revision:Option<String>)->Result<Value>{self.access(move|db|{alive(db,&id)?;let body:Option<String>=db.query_row("SELECT content FROM chat_messages WHERE key=?1 AND conversation=?2",params![key,id],|r|r.get(0)).optional()?;let page=slice(db,&body.ok_or(crate::task_store::MissingResource("Сообщение не найдено в этом чате"))?,offset,count)?;if revision.is_some_and(|v|page["revision"]!=v){return Err(crate::chat_pages::Changed.into());}Ok(page)}).await}
    pub async fn chat_runtime(&self,id:String,run:String,budget:usize)->Result<crate::api::Conversation>{self.access(move|db|runtime(db,&id,&run,budget)).await}
    pub async fn chat_checkpoint(&self,run:String,chat:crate::api::Conversation)->Result<crate::api::Conversation>{self.access(move|db|{
        let tx=db.transaction()?;alive(&tx,&chat.id)?;let (position,active):(i64,bool)=tx.query_row("SELECT position,active FROM chat_attempts WHERE id=?1 AND conversation=?2",params![run,chat.id],|r|Ok((r.get(0)?,r.get(1)?)))?;anyhow::ensure!(active,"Попытка ответа заменена");
        let last=chat.messages.last().context("Нет текущего сообщения")?;let ordinal=i64::from(last.role=="assistant");let existed:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM chat_messages WHERE attempt=?1 AND ordinal=?2)",params![run,ordinal],|r|r.get(0))?;
        save_message(&tx,&chat.id,&run,position,ordinal,last)?;
        if ordinal==1{
            save_history(&tx,&chat.id,&run,&chat.history,&chat.history_model,&chat.history_workspace,&chat.history_task)?;
            tx.execute("UPDATE chat_attempts SET state='answered' WHERE id=?1",[&run])?;
        }
        tx.execute("UPDATE chat_conversations SET title=?1,mode=?2,updated=?3,revision=revision+1,active_count=active_count+?4 WHERE id=?5",params![chat.title,chat.mode,chat.updated,i64::from(!existed),chat.id])?;tx.commit()?;Ok(chat)
    }).await}
    pub async fn chat_error(&self,id:String,run:String,text:String,duration:u64)->Result<Option<Value>>{self.access(move|db|{
        let tx=db.transaction()?;alive(&tx,&id)?;let position:Option<i64>=tx.query_row("SELECT position FROM chat_attempts WHERE id=?1 AND conversation=?2 AND active=1",params![run,id],|r|r.get(0)).optional()?;let Some(position)=position else{return Ok(None);};
        let exists:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM chat_messages WHERE attempt=?1 AND ordinal=1)",[&run],|r|r.get(0))?;if exists{return Ok(None);}
        let message=crate::api::Message{material:None,diagnostics:None,duration_ms:Some(duration),model:None,id:uuid::Uuid::new_v4().to_string(),role:"assistant".into(),content:format!("⚠ {text}"),attachments:vec![],tools:vec![],created:crate::api::now()};save_message(&tx,&id,&run,position,1,&message)?;
        tx.execute("UPDATE chat_conversations SET active_count=active_count+1,revision=revision+1,updated=?1 WHERE id=?2",params![crate::api::now(),id])?;tx.execute("UPDATE chat_attempts SET state='failed' WHERE id=?1",[&run])?;let h=header(&tx,&id)?;tx.commit()?;Ok(Some(h))
    }).await}
    pub async fn chat_list(&self)->Result<Vec<Value>>{self.access(|db|{let mut q=db.prepare("SELECT id,title,updated,mode,active_count FROM chat_conversations WHERE state='ready' AND NOT EXISTS(SELECT 1 FROM deleted_conversations d WHERE d.id=chat_conversations.id) ORDER BY updated DESC,id")?;let rows=q.query_map([],|r|Ok(json!({"id":r.get::<_,String>(0)?,"title":r.get::<_,String>(1)?,"updated":r.get::<_,String>(2)?,"mode":r.get::<_,String>(3)?,"count":r.get::<_,i64>(4)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;Ok(rows)}).await}
    pub async fn chat_rename(&self,id:String,title:String)->Result<()>{self.access(move|db|{alive(db,&id)?;db.execute("UPDATE chat_conversations SET title=?1,updated=?2 WHERE id=?3 AND state='ready'",params![title,crate::api::now(),id])?;Ok(())}).await}
}

fn save_history(db:&Connection,id:&str,generation:&str,history:&[Value],model:&str,workspace:&str,task:&str)->Result<()>{
    let _body_scope=BodyScope::new(id);
    db.execute("DELETE FROM chat_history WHERE generation=?1",[generation])?;let mut round=0i64;
    for(index,item)in history.iter().enumerate(){if item["role"]=="user"{round+=1;}let mut item=item.clone();crate::tools::public_history(std::slice::from_mut(&mut item));let cost=crate::context::history_tokens(std::slice::from_ref(&item));let body=json_body(db,&item)?;db.execute("INSERT INTO chat_history VALUES(?1,?2,?3,?4,?5,?6)",params![generation,id,i64::try_from(index)?,round,i64::try_from(cost)?,body])?;}
    db.execute("INSERT OR REPLACE INTO chat_working VALUES(?1,?2,?3,?4,?5)",params![id,generation,model,workspace,task])?;Ok(())
}
fn runtime(db:&Connection,id:&str,run:&str,budget:usize)->Result<crate::api::Conversation>{
    let h=header(db,id)?;let mut chat=crate::api::Conversation{id:id.into(),title:h["title"].as_str().unwrap().into(),mode:h["mode"].as_str().unwrap().into(),updated:h["updated"].as_str().unwrap().into(),messages:vec![],history:vec![],history_model:String::new(),history_workspace:String::new(),history_task:String::new()};
    // Choose whole visible task rounds by stored cost before decoding any body.
    let mut query=db.prepare("SELECT id FROM chat_attempts WHERE conversation=?1 AND active=1 AND id<>?2 ORDER BY position DESC")?;
    let mut cost=0usize;let mut attempts=Vec::new();let mut rows=query.query(params![id,run])?;while let Some(row)=rows.next()?{let attempt=row.get::<_,String>(0)?;let amount:i64=db.query_row("SELECT COALESCE(SUM(MAX(c.cost,COALESCE(b.cost,c.cost))),0) FROM chat_messages m JOIN chat_bodies c ON c.id=m.content LEFT JOIN chat_bodies b ON b.id=m.material WHERE m.attempt=?1",[&attempt],|r|r.get(0))?;let amount=usize::try_from(amount)?.saturating_add(48);if cost.saturating_add(amount)>budget{break;}cost+=amount;attempts.push(attempt);}
    attempts.reverse();for attempt in attempts{let mut q=db.prepare("SELECT metadata,content,material FROM chat_messages WHERE attempt=?1 ORDER BY ordinal")?;for row in q.query_map([attempt],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,Option<String>>(2)?)))?{let(metadata,body,material)=row?;let mut value:Value=serde_json::from_str(&metadata)?;value["content"]=json!(whole(db,&body)?);if let Some(body)=material{value["material"]=json!(whole(db,&body)?);}chat.messages.push(serde_json::from_value(value)?);}}
    if let Some((generation,model,workspace,task))=db.query_row("SELECT generation,model,workspace,task FROM chat_working WHERE conversation=?1",[id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?))).optional()?{
        let retry:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM runs WHERE id=?1 AND json_extract(input,'$.retry')=1)",[run],|r|r.get(0))?;
        if !retry{chat.history_model=model;chat.history_workspace=workspace;chat.history_task=task;
            let mut rounds=db.prepare("SELECT round,SUM(cost) FROM chat_history WHERE generation=?1 GROUP BY round ORDER BY round DESC")?;let mut rows=rounds.query([&generation])?;let mut selected=Vec::new();let mut cost=0usize;while let Some(row)=rows.next()?{let amount=usize::try_from(row.get::<_,i64>(1)?)?;if cost.saturating_add(amount)>budget{break;}cost+=amount;selected.push(row.get::<_,i64>(0)?);}selected.reverse();
            for round in selected{let mut items=db.prepare("SELECT body FROM chat_history WHERE generation=?1 AND round=?2 ORDER BY item")?;for body in items.query_map(params![generation,round],|r|r.get::<_,String>(0))?{chat.history.push(serde_json::from_str(&whole(db,&body?)?)?);}}crate::tools::public_history(&mut chat.history);
        }
    }crate::tools::public_chat(&mut chat);Ok(chat)
}

fn original(data:&Path,id:&str,cancel:&CancellationToken)->Result<(PathBuf,String)>{
    let source=data.join("chats").join(format!("{id}.json"));let before=std::fs::metadata(&source)?;let directory=data.join("chat-originals").join(id);std::fs::create_dir_all(&directory)?;
    let staging=directory.join(format!(".{}.tmp",uuid::Uuid::new_v4()));let mut input=File::open(&source)?;let mut output=std::fs::OpenOptions::new().write(true).create_new(true).open(&staging)?;let mut hash=Sha256::new();let mut bytes=[0;65536];
    let copied=(||->Result<()>{loop{anyhow::ensure!(!cancel.is_cancelled(),"Копирование исходного чата остановлено");let n=input.read(&mut bytes)?;if n==0{break;}output.write_all(&bytes[..n])?;hash.update(&bytes[..n]);}output.sync_all()?;let after=std::fs::metadata(&source)?;anyhow::ensure!(before.len()==after.len()&&before.modified()?==after.modified()?,"Исходный чат изменился при копировании");Ok(())})();drop(output);if let Err(error)=copied{let _=std::fs::remove_file(staging);return Err(error);}
    let hash=format!("{:x}",hash.finalize());let archive=directory.join(format!("{hash}.json"));if archive.exists(){std::fs::remove_file(&staging)?;}else{std::fs::rename(&staging,&archive)?;}Ok((archive,hash))
}

fn import_message(db:&Connection,path:&Path,id:&str,index:i64,span:Span,cancel:&CancellationToken)->Result<()>{
    let mut fields=Reader::open(path,cancel)?;fields.seek(span.start)?;let mut content=None;let mut material=None;let mut metadata=serde_json::Map::new();let mut seen=HashSet::new();
    fields.fields(|key,value|{
        if ["id","role","created","content","material","diagnostics","duration_ms","model","attachments","tools"].contains(&key){anyhow::ensure!(seen.insert(key.to_string()),"Повтор поля сообщения: {key}");}
        match key{
            "content"=>content=Some(decoded_body(db,path,value,cancel)?),
            "material"=>{if crate::json_stream::small(path,value,4).ok()==Some(Value::Null){material=None;}else{material=Some(decoded_body(db,path,value,cancel)?);}},
            "id"|"role"|"created"|"model"|"duration_ms"=>{metadata.insert(key.into(),crate::json_stream::small(path,value,65536)?);},
            "attachments"|"tools"|"diagnostics"=>{if value.end-value.start<=65536{metadata.insert(key.into(),crate::json_stream::small(path,value,65536)?);}else{metadata.insert(format!("{key}_partial"),json!(true));}},_=>{}
        }Ok(())
    })?;
    let content=content.context("Нет content сообщения")?;let original=metadata["id"].as_str().context("Нет ID сообщения")?.to_string();let role=metadata["role"].as_str().context("Нет роли сообщения")?.to_string();let created=metadata["created"].as_str().context("Нет даты сообщения")?.to_string();
    let raw=raw_body(db,path,span)?;let position=if role=="user"{index}else{db.query_row("SELECT COALESCE(MAX(position),?1) FROM chat_messages WHERE conversation=?2",params![index,id],|r|r.get::<_,i64>(0))?};let attempt=format!("legacy:{id}:{position}");let ordinal=db.query_row("SELECT COUNT(*) FROM chat_messages WHERE attempt=?1",[&attempt],|r|r.get::<_,i64>(0))?;let key=uuid::Uuid::new_v4().to_string();
    db.execute_batch("BEGIN IMMEDIATE")?;
    db.execute("INSERT OR IGNORE INTO chat_attempts VALUES(?1,?2,?3,1,'legacy')",params![attempt,id,position])?;
    db.execute("INSERT OR IGNORE INTO chat_messages VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",params![key,id,position,ordinal,index,attempt,original,role,created,content,material,raw,Value::Object(metadata).to_string()])?;
    db.execute("UPDATE chat_imports SET messages=?1 WHERE conversation=?2",params![index+1,id])?;db.execute_batch("COMMIT")?;Ok(())
}

/// Import staging rows remain hidden until the complete immutable source validates.
pub fn ensure_import(data:&Path,id:&str,cancel:&CancellationToken)->Result<()>{
    let _body_scope=BodyScope::new(id);uuid::Uuid::parse_str(id)?;let db=open(data)?;alive(&db,id)?;if present(&db,id)?{return Ok(());}
    let directory=data.join("chat-originals").join(id);std::fs::create_dir_all(&directory)?;
    let lease=std::fs::OpenOptions::new().create(true).truncate(false).read(true).write(true).open(directory.join("import.lock"))?;
    lease.try_lock().map_err(|_|anyhow::anyhow!("Этот чат уже импортируется; повторите чтение после завершения"))?;
    let existing:Option<(String,String)>=db.query_row("SELECT original,source_hash FROM chat_conversations WHERE id=?1 AND state='importing'",[id],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    let(path,hash)=match existing{Some((p,h))=>(PathBuf::from(p),h),None=>original(data,id,cancel)?};
    let mut archive=File::open(&path)?;let mut digest=Sha256::new();let mut buffer=[0;65536];loop{anyhow::ensure!(!cancel.is_cancelled(),"Импорт JSON остановлен");let n=archive.read(&mut buffer)?;if n==0{break;}digest.update(&buffer[..n]);}anyhow::ensure!(format!("{:x}",digest.finalize())==hash,"Архив исходного чата изменился; публикация остановлена");
    db.execute("INSERT OR IGNORE INTO chat_conversations(id,title,mode,updated,state,original,source_hash) VALUES(?1,'','agent','','importing',?2,?3)",params![id,path.to_string_lossy(),hash])?;db.execute("INSERT OR IGNORE INTO chat_imports(conversation) VALUES(?1)",[id])?;
    let (done_messages,done_history,mut round):(i64,i64,i64)=db.query_row("SELECT messages,history,round FROM chat_imports WHERE conversation=?1",[id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
    let mut root=Reader::open(&path,cancel)?;let mut title=String::new();let mut mode="agent".to_string();let mut updated=String::new();let mut model=String::new();let mut workspace=String::new();let mut task=String::new();let mut seen=HashSet::new();let mut messages=0i64;let mut histories=0i64;
    root.fields(|key,span|{
        if ["id","title","mode","updated","messages","history","history_model","history_workspace","history_task"].contains(&key){anyhow::ensure!(seen.insert(key.to_string()),"Повтор поля чата: {key}");}
        match key{
            "messages"=>{let mut reader=Reader::open(&path,cancel)?;reader.seek(span.start)?;reader.array(|message|{if messages>=done_messages{let result=import_message(&db,&path,id,messages,message,cancel);if let Err(error)=result{let _=db.execute_batch("ROLLBACK");return Err(error);}}messages+=1;Ok(())})?;},
            "history"=>{let mut reader=Reader::open(&path,cancel)?;reader.seek(span.start)?;reader.array(|item|{if histories>=done_history{let mut role=String::new();let mut fields=Reader::open(&path,cancel)?;fields.seek(item.start)?;fields.fields(|key,value|{if key=="role"{role=crate::json_stream::small(&path,value,4096)?.as_str().context("История без роли")?.into();}Ok(())})?;if role=="user"{round+=1;}let body=raw_body(&db,&path,item)?;db.execute_batch("BEGIN IMMEDIATE")?;let result=(||->Result<()>{let cost:i64=db.query_row("SELECT cost FROM chat_bodies WHERE id=?1",[&body],|r|r.get(0))?;db.execute("INSERT OR IGNORE INTO chat_history VALUES(?1,?2,?3,?4,?5,?6)",params![format!("legacy:{id}"),id,histories,round,cost+24,body])?;db.execute("UPDATE chat_imports SET history=?1,round=?2 WHERE conversation=?3",params![histories+1,round,id])?;Ok(())})();if let Err(error)=result{let _=db.execute_batch("ROLLBACK");return Err(error);}db.execute_batch("COMMIT")?;}histories+=1;Ok(())})?;},
            "id"=>anyhow::ensure!(crate::json_stream::small(&path,span,4096)?.as_str()==Some(id),"ID чата не соответствует имени файла"),
            "title"|"mode"|"updated"|"history_model"|"history_workspace"|"history_task"=>{let value=crate::json_stream::small(&path,span,65536)?;let text=value.as_str().context("Метаданные чата должны быть строкой")?.to_string();match key{"title"=>title=text,"mode"=>mode=text,"updated"=>updated=text,"history_model"=>model=text,"history_workspace"=>workspace=text,_=>task=text}},_=>{}
        }Ok(())
    })?;root.end()?;anyhow::ensure!(seen.contains("id")&&seen.contains("messages"),"Нет ID или messages чата");
    db.execute_batch("BEGIN IMMEDIATE")?;let publish=(||->Result<()>{alive(&db,id)?;db.execute("UPDATE chat_conversations SET title=?1,mode=?2,updated=?3,next_position=?4,active_count=?4,state='ready' WHERE id=?5",params![title,mode,updated,messages,id])?;db.execute("INSERT OR REPLACE INTO chat_working VALUES(?1,?2,?3,?4,?5)",params![id,format!("legacy:{id}"),model,workspace,task])?;Ok(())})();if let Err(error)=publish{let _=db.execute_batch("ROLLBACK");return Err(error);}db.execute_batch("COMMIT")?;Ok(())
}

#[cfg(test)]mod tests{
    use super::*;
    fn fixture()->(PathBuf,crate::task_store::Store,String){let root=std::env::temp_dir().join(format!("chat-store-{}",uuid::Uuid::new_v4()));std::fs::create_dir_all(root.join("chats")).unwrap();let store=crate::task_store::Store::open(&root).unwrap();(root,store,uuid::Uuid::new_v4().to_string())}
    #[tokio::test]
    async fn message_pages_use_visible_ranks_and_real_cursors_across_unanswered_round_gaps(){
        let(root,store,id)=fixture();store.chat_create(id.clone(),"gaps".into(),"chat".into()).await.unwrap();
        for i in 0..10{store.begin_in_project(format!("gap-{i}"),id.clone(),json!({"mode":"chat","content":format!("request {i}"),"attachments":[]}),"project".into()).await.unwrap();}
        let latest=store.chat_page(id.clone(),None,3,None).await.unwrap();let page=&latest["message_page"];assert_eq!(page["start"],7);assert_eq!(page["end"],10);assert_eq!(page["cursor_end"],20);assert_eq!(page["before"],14);assert_eq!(page["at_latest"],true);
        let previous=store.chat_page(id.clone(),page["before"].as_u64(),3,Some(page["revision"].as_str().unwrap().into())).await.unwrap();let p=&previous["message_page"];assert_eq!(p["start"],4);assert_eq!(p["end"],7);assert_eq!(p["before"],8);assert_eq!(p["newer_before"],20);assert_eq!(p["at_latest"],false);
        let back=store.chat_page(id.clone(),p["newer_before"].as_u64(),3,None).await.unwrap();assert_eq!(back["messages"],latest["messages"]);
        let first=store.chat_page(id.clone(),Some(2),3,None).await.unwrap();assert_eq!(first["message_page"]["start"],0);assert_eq!(first["message_page"]["end"],1);assert_eq!(first["message_page"]["before"],Value::Null);
        store.begin_in_project("retry-gap".into(),id.clone(),json!({"mode":"chat","content":"new attempt","retry":true}),"project".into()).await.unwrap();
        let error=store.chat_page(id.clone(),None,3,Some(page["revision"].as_str().unwrap().into())).await.unwrap_err();assert!(error.is::<crate::chat_pages::Changed>());
        assert_eq!(store.chat_header(id.clone()).await.unwrap()["count"],10);store.delete_conversation(id).await.unwrap();drop(store);std::fs::remove_dir_all(root).unwrap();
    }
    #[test]fn importer_preserves_original_large_strings_and_ranges(){let(root,store,id)=fixture();let text=format!("{}\0\r\n🦀", "русский漢字".repeat(50000));let original=json!({"id":id,"title":"source","mode":"chat","updated":"now","unknown":{"retained":true},"messages":[{"id":"same","role":"user","created":"now","content":text,"material":"private extraction"},{"id":"same","role":"assistant","created":"now","content":"answer"}],"history":[{"role":"user","content":text},{"role":"assistant","content":"answer"}],"history_model":"test"});let bytes=serde_json::to_vec_pretty(&original).unwrap();let source=root.join("chats").join(format!("{id}.json"));std::fs::write(&source,&bytes).unwrap();ensure_import(&root,&id,&CancellationToken::new()).unwrap();let db=open(&root).unwrap();let h=header(&db,&id).unwrap();assert_eq!(h["count"],2);let archive:String=db.query_row("SELECT original FROM chat_conversations WHERE id=?1",[&id],|r|r.get(0)).unwrap();assert_eq!(std::fs::read(archive).unwrap(),bytes);assert_eq!(std::fs::read(source).unwrap(),bytes);let page=messages_page(&db,&id,None,2,None).unwrap();assert_eq!(page["messages"][0]["content_partial"],true);assert!(page["messages"][0].get("material").is_none());let key=page["messages"][0]["content_source"]["message"].as_str().unwrap();let body:String=db.query_row("SELECT content FROM chat_messages WHERE key=?1",[key],|r|r.get(0)).unwrap();let last=slice(&db,&body,text.chars().count() as u64-5,5).unwrap();assert_eq!(last["text"],text.chars().rev().take(5).collect::<Vec<_>>().into_iter().rev().collect::<String>());let selected=runtime(&db,&id,"none",100).unwrap();assert!(selected.messages.is_empty()&&selected.history.is_empty());drop(db);drop(store);std::fs::remove_dir_all(root).unwrap();}
    #[test]fn invalid_import_never_publishes_partial_chat(){let(root,store,id)=fixture();let source=root.join("chats").join(format!("{id}.json"));std::fs::write(&source,format!("{{\"id\":\"{id}\",\"messages\":[{{\"id\":\"a\",\"role\":\"user\",\"created\":\"now\",\"content\":\"ok\"}}]}} trailing")).unwrap();assert!(ensure_import(&root,&id,&CancellationToken::new()).is_err());let db=open(&root).unwrap();assert!(!present(&db,&id).unwrap());assert!(source.exists());drop(db);drop(store);std::fs::remove_dir_all(root).unwrap();}
    #[tokio::test]async fn accepted_turn_retry_and_error_are_scoped_and_idempotent(){let(root,store,id)=fixture();store.chat_create(id.clone(),"title".into(),"agent".into()).await.unwrap();let first=json!({"mode":"agent","content":"first","attachments":[]});store.begin_in_project("run-one".into(),id.clone(),first.clone(),"project".into()).await.unwrap();let h=store.chat_header(id.clone()).await.unwrap();assert_eq!(h["count"],1);assert!(!store.begin_in_project("run-one".into(),id.clone(),first,"project".into()).await.unwrap());assert!(store.chat_error(id.clone(),"run-one".into(),"failed".into(),1).await.unwrap().is_some());assert!(store.chat_error(id.clone(),"run-one".into(),"duplicate".into(),2).await.unwrap().is_none());store.begin_in_project("run-two".into(),id.clone(),json!({"mode":"agent","content":"first","attachments":[],"retry":true}),"project".into()).await.unwrap();assert_eq!(store.chat_header(id.clone()).await.unwrap()["count"],1);let db=open(&root).unwrap();assert_eq!(db.query_row("SELECT COUNT(*) FROM chat_attempts WHERE conversation=?1",[&id],|r|r.get::<_,i64>(0)).unwrap(),2);drop(db);drop(store);std::fs::remove_dir_all(root).unwrap();}
}
