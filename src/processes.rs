//! Owned process sessions with disk-backed logs. Log size is not a RAM quota.
use crate::api::Shared;
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::{path::{Path, PathBuf}, process::Stdio, sync::Arc};
use tokio::{io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt}, sync::{watch, Mutex}};
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
pub struct Active {
    pub cancel: CancellationToken,
    stdin: Arc<Mutex<Option<tokio::process::ChildStdin>>>,
    done: watch::Receiver<Option<Value>>,
    task: Option<String>,
    conversation: Option<String>,
    run: Option<String>,
}
fn paths(app:&crate::api::App,id:&str)->Result<(PathBuf,PathBuf,PathBuf)> {
    uuid::Uuid::parse_str(id)?;
    let root=app.data.join("executions");
    Ok((root.join(format!("{id}.json")),root.join(format!("{id}.stdout.log")),root.join(format!("{id}.stderr.log"))))
}
async fn persist(app:&Shared,id:&str,report:&Value)->Result<()> {
    let (path,_,_)=paths(app,id)?;let bytes=serde_json::to_vec_pretty(report)?;
    tokio::task::spawn_blocking(move||crate::changes::replace(&path,&bytes)).await??;
    Ok(())
}
pub async fn start(app:&Shared,script:&str,cwd:&Path,seconds:u64,cancel:&CancellationToken,interactive:bool)->Result<Value> {
    anyhow::ensure!(!cancel.is_cancelled(),"Остановлено пользователем");
    let deadline=crate::lifecycle::deadline(seconds)?;
    let id=uuid::Uuid::new_v4().to_string();
    let (metadata,stdout,stderr)=paths(app,&id)?;
    let owner_run=crate::permissions::TASK.try_with(Clone::clone).ok();
    let ownership=if let Some(run)=&owner_run {app.tasks.task_for_run(run.clone()).await?}else{None};
    let owner_task=ownership.as_ref().and_then(|task|task["id"].as_str().map(str::to_owned));
    let conversation=ownership.as_ref().and_then(|task|task["conversation"].as_str().map(str::to_owned));
    let report=json!({"execution_id":id,"command":script,"directory":cwd,"created":crate::api::now(),"status":"starting","exit_code":null,"timeout_seconds":seconds,"log_version":2,"stdin_available":interactive,"owner_run":owner_run,"owner_task":owner_task,"owner_conversation":conversation,"note_process_only":"Состояние процесса не подтверждает корректность всей задачи."});
    let bytes=serde_json::to_vec_pretty(&report)?;
    let files=tokio::task::spawn_blocking(move||->Result<_>{
        std::fs::create_dir_all(metadata.parent().unwrap())?;
        crate::changes::replace(&metadata,&bytes)?;
        Ok((std::fs::OpenOptions::new().write(true).create_new(true).open(stdout)?,std::fs::OpenOptions::new().write(true).create_new(true).open(stderr)?))
    }).await??;
    anyhow::ensure!(!cancel.is_cancelled(),"Остановлено пользователем");
    #[cfg(windows)]
    let mut command={let mut command=tokio::process::Command::new("powershell.exe");command.args(["-NoProfile","-NonInteractive","-Command",&format!("[Console]::OutputEncoding=[Text.UTF8Encoding]::new($false); $OutputEncoding=[Console]::OutputEncoding; $ProgressPreference='SilentlyContinue'; $ErrorActionPreference='Stop'; $global:LASTEXITCODE=0; & {{\n{script}\n}}; exit $LASTEXITCODE")]);command};
    #[cfg(not(windows))]
    let mut command={let mut command=tokio::process::Command::new("/bin/sh");command.args(["-c",script]);command};
    command.current_dir(cwd).stdout(Stdio::from(files.0)).stderr(Stdio::from(files.1)).stdin(if interactive {Stdio::piped()}else{Stdio::null()}).kill_on_drop(true);
    let mut group=crate::process_group::Group::prepare(&mut command)?;
    let mut child=match command.spawn(){Ok(child)=>child,Err(error)=>{let mut failed=report;failed["status"]=json!("failed");failed["error"]=json!(error.to_string());persist(app,&id,&failed).await?;return Err(crate::agent::ExecutionStopped{report:failed.to_string()}.into());}};
    if let Err(error)=group.attach(&child){group.terminate();let _=child.kill().await;return Err(error);}
    let stdin=Arc::new(Mutex::new(child.stdin.take()));
    let token=cancel.child_token();let (complete,done)=watch::channel(None);
    let mut report=report;report["status"]=json!("running");report["pid"]=json!(child.id());
    if let Err(error)=persist(app,&id,&report).await {group.terminate();let _=child.kill().await;return Err(error);}
    app.processes.lock().await.insert(id.clone(),Active{cancel:token.clone(),stdin:stdin.clone(),done,task:owner_task,conversation,run:owner_run});
    let worker=app.clone();let process=id.clone();
    tokio::spawn(async move {
        let started=std::time::Instant::now();
        let outcome=tokio::select! {
            status=child.wait()=>match status {Ok(status)=>(if status.success(){"succeeded"}else{"failed"},status.code(),None),Err(error)=>("failed",None,Some(error.to_string()))},
            _=token.cancelled()=>("cancelled",None,None),
            _=crate::lifecycle::wait(deadline)=>("timed_out",None,None)
        };
        // The shell's exit never detaches descendants. Persistent commands keep
        // the shell alive; detached servers must use a managed session explicitly.
        group.terminate();let _=child.wait().await;drop(group);*stdin.lock().await=None;
        report["status"]=json!(outcome.0);report["exit_code"]=json!(outcome.1);report["error"]=json!(outcome.2);report["finished"]=json!(crate::api::now());report["duration_ms"]=json!(started.elapsed().as_millis().min(u64::MAX as u128) as u64);report["stdin_available"]=json!(false);
        if outcome.0=="cancelled"||outcome.0=="timed_out" {report["note"]=json!("Процесс остановлен; частичный вывод сохранён. Уже выполненные действия не отменены.");}
        let synchronized=async{for stream in ["stdout","stderr"]{tokio::fs::OpenOptions::new().write(true).open(log_path(&worker,&process,stream)?).await?.sync_all().await?;}Ok::<_,anyhow::Error>(())}.await;
        report["logs_synced"]=json!(synchronized.is_ok());
        if let Err(error)=synchronized {report["log_sync_error"]=json!(error.to_string());}
        let saved=persist(&worker,&process,&report).await;
        if let Err(error)=&saved {report["persistence_error"]=json!(error.to_string());}
        let _=complete.send(Some(report));
        if saved.is_ok(){worker.processes.lock().await.remove(&process);}
    });
    view(app,&id).await
}
pub async fn metadata(app:&Shared,id:&str)->Result<Value> {
    let active=app.processes.lock().await.get(id).cloned();
    if let Some(active)=&active {if let Some(report)=active.done.borrow().as_ref(){return Ok(report.clone());}}
    let (path,_,_)=paths(app,id)?;
    let mut report:Value=serde_json::from_slice(&tokio::fs::read(path).await?)?;
    if active.is_none()&&report["log_version"]==2&&matches!(report["status"].as_str(),Some("starting"|"running")){report["status"]=json!("interrupted");report["note"]=json!("У приложения нет владельца этого процесса. Старый PID не подтверждает его состояние.");}
    Ok(report)
}
pub async fn log(app:&Shared,id:&str,stream:&str,offset:u64)->Result<Value> {
    anyhow::ensure!(["stdout","stderr"].contains(&stream),"Выберите stdout или stderr");
    let (_,out,err)=paths(app,id)?;
    let mut file=tokio::fs::File::open(if stream=="stdout" {out}else{err}).await?;
    let total=file.metadata().await?.len();file.seek(std::io::SeekFrom::Start(offset)).await?;
    let mut bytes=vec![0u8;16384];let count=file.read(&mut bytes).await?;bytes.truncate(count);
    let next=offset.checked_add(count as u64).context("Переполнение смещения журнала")?;
    Ok(json!({"execution_id":id,"stream":stream,"offset":offset,"next_offset":next,"bytes":total.max(next),"has_more":next<total,"text":String::from_utf8_lossy(&bytes),"encoding":"utf-8","decoding_lossy":std::str::from_utf8(&bytes).is_err(),"raw_download":format!("/api/executions/{id}/logs/{stream}")}))
}
pub async fn view(app:&Shared,id:&str)->Result<Value> {
    let mut report=metadata(app,id).await?;
    if report["log_version"]==2 {
        for stream in ["stdout","stderr"] {let page=log(app,id,stream,0).await?;report[stream]=page["text"].clone();report[format!("{stream}_page")]=page;}
        report["output_may_be_capped"]=json!(false);
        report["output_preview_partial"]=json!(report["stdout_page"]["has_more"]==true||report["stderr_page"]["has_more"]==true);
    }
    Ok(report)
}
pub async fn wait(app:&Shared,id:&str)->Result<Value> {
    let active=app.processes.lock().await.get(id).cloned();
    if let Some(active)=active {let mut done=active.done;while done.borrow().is_none(){done.changed().await.context("Процесс завершился без подтверждения состояния")?;}}
    view(app,id).await
}
pub async fn poll(app:&Shared,id:&str,millis:u64,cancel:&CancellationToken)->Result<Value> {
    let active=app.processes.lock().await.get(id).cloned();
    if let Some(active)=active {if millis>0&&active.done.borrow().is_none(){let mut done=active.done;let at=tokio::time::Instant::now().checked_add(std::time::Duration::from_millis(millis)).context("Срок ожидания не представим системными часами")?;tokio::select!{_=done.changed()=>{},_=tokio::time::sleep_until(at)=>{},_=cancel.cancelled()=>anyhow::bail!("Остановлено пользователем")}}}
    view(app,id).await
}
pub async fn stop(app:&Shared,id:&str)->Result<()> {
    uuid::Uuid::parse_str(id)?;
    if let Some(active)=app.processes.lock().await.get(id){active.cancel.cancel();}
    Ok(())
}
pub async fn input(app:&Shared,id:&str,text:&str,cancel:&CancellationToken)->Result<()> {
    let active=app.processes.lock().await.get(id).cloned().context("Процесс больше не работает")?;
    let mut stdin=active.stdin.lock().await;
    let stdin=stdin.as_mut().context("Процесс не принимает stdin")?;
    tokio::select!{result=stdin.write_all(text.as_bytes())=>result?,_=cancel.cancelled()=>anyhow::bail!("Остановлено пользователем"),_=active.cancel.cancelled()=>anyhow::bail!("Процесс останавливается")};
    Ok(())
}
pub async fn ensure_owner(app:&Shared,id:&str)->Result<()> {
    let run=crate::permissions::TASK.try_with(Clone::clone).context("Нет активного запуска")?;
    let report=metadata(app,id).await?;
    if report["owner_run"]==run {return Ok(());}
    let task=app.tasks.task_for_run(run).await?;
    anyhow::ensure!(task.as_ref().is_some_and(|task|!report["owner_task"].is_null()&&report["owner_task"]==task["id"]),"Процесс принадлежит другой задаче");
    Ok(())
}
pub async fn for_task(app:&Shared,task:&str)->Result<Vec<Value>> {
    let ids:Vec<_>=app.processes.lock().await.iter().filter(|(_,active)|active.task.as_deref()==Some(task)).map(|(id,_)|id.clone()).collect();
    let mut result=vec![];
    for id in ids {let report=metadata(app,&id).await?;result.push(json!({"execution_id":id,"status":report["status"],"directory":report["directory"],"command_preview":crate::tools::truncate(report["command"].as_str().unwrap_or(""),300)}));}
    Ok(result)
}
pub async fn has_live_conversation(app:&Shared,id:&str)->bool {
    app.processes.lock().await.values().any(|active|active.conversation.as_deref()==Some(id)&&active.done.borrow().is_none())
}
pub async fn cancel_run(app:&Shared,run:&str) {
    let owned:Vec<_>=app.processes.lock().await.iter().filter(|(_,active)|active.run.as_deref()==Some(run)).map(|(id,active)|(id.clone(),active.cancel.clone())).collect();
    for (_,token) in &owned {token.cancel();}
    for (id,_) in owned {if let Err(error)=wait(app,&id).await {eprintln!("Process cleanup {id}: {error:#}");}}
}
pub fn recover(data:&Path)->Result<Vec<String>> {
    let folder=data.join("executions");let mut errors=vec![];if !folder.exists(){return Ok(errors);}
    for entry in std::fs::read_dir(folder)? {let path=entry?.path();if path.extension().is_some_and(|e|e=="json") {
        let restored=(||->Result<()>{let mut report:Value=serde_json::from_slice(&std::fs::read(&path)?)?;anyhow::ensure!(report.is_object()&&report["execution_id"].as_str().is_some_and(|id|uuid::Uuid::parse_str(id).is_ok()),"Неверный формат журнала процесса");if report["log_version"]==2&&matches!(report["status"].as_str(),Some("starting"|"running")){report["status"]=json!("interrupted");report["note"]=json!("Приложение перезапущено. Состояние старого процесса и его эффектов неизвестно; PID не используется для автоматического завершения или повтора команды.");crate::changes::replace(&path,&serde_json::to_vec_pretty(&report)?)?;}Ok(())})();
        if let Err(error)=restored {errors.push(format!("{}: {error:#}",path.display()));}
    }}
    Ok(errors)
}
pub fn log_path(app:&crate::api::App,id:&str,stream:&str)->Result<PathBuf>{let(_,out,err)=paths(app,id)?;match stream {"stdout"=>Ok(out),"stderr"=>Ok(err),_=>anyhow::bail!("Неизвестный поток журнала")}}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture()->Shared {let root=std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());Arc::new(crate::api::App::new(root).unwrap())}
    #[tokio::test]
    async fn restart_preserves_corrupt_records_and_never_trusts_old_pids() {
        let app=fixture();let root=app.root.clone();let folder=app.data.join("executions");std::fs::create_dir(&folder).unwrap();
        let id=uuid::Uuid::new_v4().to_string();let (path,_,_)=paths(&app,&id).unwrap();
        std::fs::write(&path,json!({"execution_id":id,"log_version":2,"status":"running","pid":1}).to_string()).unwrap();
        let broken=folder.join(format!("{}.json",uuid::Uuid::new_v4()));std::fs::write(&broken,b"{broken").unwrap();
        drop(app);let app=Arc::new(crate::api::App::new(root.clone()).unwrap());
        assert_eq!(app.process_recovery_errors.len(),1);assert_eq!(std::fs::read(&broken).unwrap(),b"{broken");
        assert_eq!(metadata(&app,&id).await.unwrap()["status"],"interrupted");
        drop(app);std::fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn large_logs_are_complete_on_disk_and_paged_in_memory() {
        let app=fixture();let root=app.root.clone();let cwd=app.data.join("workspace");
        let script=if cfg!(windows){"[Console]::Write(('x' * 1100000)); [Console]::Error.Write('err')"}else{"head -c 1100000 /dev/zero | tr '\\000' x; printf err >&2"};
        let process=start(&app,script,&cwd,0,&CancellationToken::new(),false).await.unwrap();
        let id=process["execution_id"].as_str().unwrap();let report=wait(&app,id).await.unwrap();
        assert_eq!(report["status"],"succeeded");assert_eq!(report["stdout_page"]["bytes"],1100000);assert_eq!(report["stderr"],"err");
        assert!(report["stdout"].as_str().unwrap().len()<=16384);assert_eq!(report["stdout_page"]["has_more"],true);
        let tail=log(&app,id,"stdout",1099990).await.unwrap();assert_eq!(tail["text"],"xxxxxxxxxx");assert_eq!(tail["has_more"],false);
        assert!(app.processes.lock().await.is_empty());drop(app);std::fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn stdin_and_cancel_preserve_partial_logs_and_cleanup() {
        let app=fixture();let root=app.root.clone();let cwd=app.data.join("workspace");let token=CancellationToken::new();
        let echo=if cfg!(windows){"$line=[Console]::ReadLine(); [Console]::WriteLine('ECHO:'+$line)"}else{"read -r line; printf 'ECHO:%s' \"$line\""};
        let process=start(&app,echo,&cwd,0,&token,true).await.unwrap();let id=process["execution_id"].as_str().unwrap();
        input(&app,id,"hello\n",&token).await.unwrap();let report=wait(&app,id).await.unwrap();assert!(report["stdout"].as_str().unwrap().contains("ECHO:hello"));
        let script=if cfg!(windows){"[Console]::WriteLine('before-stop'); Start-Sleep -Seconds 30; Set-Content -LiteralPath survived.txt -Value bad"}else{"printf before-stop; sleep 30; printf bad > survived.txt"};
        let process=start(&app,script,&cwd,0,&token,false).await.unwrap();let id=process["execution_id"].as_str().unwrap();
        let mut observed=false;for _ in 0..400 {if log(&app,id,"stdout",0).await.unwrap()["text"].as_str().unwrap().contains("before-stop"){observed=true;break;}tokio::time::sleep(std::time::Duration::from_millis(25)).await;}
        assert!(observed);token.cancel();let report=wait(&app,id).await.unwrap();assert_eq!(report["status"],"cancelled");assert!(report["stdout"].as_str().unwrap().contains("before-stop"));assert!(!cwd.join("survived.txt").exists());
        assert!(app.processes.lock().await.is_empty());drop(app);std::fs::remove_dir_all(root).unwrap();
    }
}
