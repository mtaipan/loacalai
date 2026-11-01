//! One-shot commands share the managed process/log lifecycle.
use crate::api::Shared;
use anyhow::Result;
use std::path::Path;
use tokio_util::sync::CancellationToken;

pub async fn run(app:&Shared,script:&str,cwd:&Path,seconds:u64,cancel:&CancellationToken)->Result<String>{
    let started=crate::processes::start(app,script,cwd,seconds,cancel,false).await?;
    let id=started["execution_id"].as_str().unwrap();
    let report=crate::processes::wait(app,id).await?;
    let text=report.to_string();
    if report["logs_synced"]!=true || !report["persistence_error"].is_null(){return Err(crate::agent::ExecutionStopped{report:text}.into());}
    match report["status"].as_str(){
        Some("succeeded")=>Ok(text),
        Some("failed") if report["exit_code"].as_i64().is_some()=>Err(crate::agent::CommandFailed{code:report["exit_code"].as_i64().unwrap() as i32,output:text}.into()),
        _=>Err(crate::agent::ExecutionStopped{report:text}.into())
    }
}
