//! Commands are approved by tools.rs; records describe processes, not correctness.
use crate::{api::Shared, tools};
use anyhow::Result;
use serde_json::{json, Value};
use std::path::Path;
use tokio_util::sync::CancellationToken;

pub async fn run(
    app: &Shared,
    script: &str,
    cwd: &Path,
    seconds: u64,
    cancel: &CancellationToken,
) -> Result<String> {
    anyhow::ensure!(!cancel.is_cancelled(), "Остановлено пользователем");
    let id = uuid::Uuid::new_v4().to_string();
    let started = std::time::Instant::now();
    #[cfg(windows)]
    let mut command = {
        let mut c = tokio::process::Command::new("powershell.exe");
        c.args(["-NoProfile","-NonInteractive","-Command",&format!("[Console]::OutputEncoding=[Text.UTF8Encoding]::new($false); $OutputEncoding=[Console]::OutputEncoding; $ProgressPreference='SilentlyContinue'; $ErrorActionPreference='Stop'; $global:LASTEXITCODE=0; & {{\n{script}\n}}; exit $LASTEXITCODE")]);
        c
    };
    #[cfg(not(windows))]
    let mut command = {
        let mut c = tokio::process::Command::new("/bin/sh");
        c.args(["-c", script]);
        c
    };
    command.current_dir(cwd);
    let result = tools::capture(command, cancel, seconds).await;
    let mut report = json!({"execution_id":id,"command":script,"directory":cwd,"created":crate::api::now(),"duration_ms":started.elapsed().as_millis().min(u64::MAX as u128) as u64,"timeout_seconds":seconds});
    match &result {
        Ok((code, out, err)) => {
            report["status"] = json!(if *code == 0 { "succeeded" } else { "failed" });
            report["exit_code"] = json!(code);
            report["stdout"] = json!(out);
            report["stderr"] = json!(err);
            report["output_may_be_capped"] = json!(out.len() >= 250000 || err.len() >= 250000);
        }
        Err(error) => {
            report["status"] = json!(if cancel.is_cancelled() {
                "cancelled"
            } else if let Some(reason) = error.downcast_ref::<crate::agent::ProcessInterrupted>() {
                reason.0
            } else {
                "failed"
            });
            report["error"] = json!(format!("{error:#}"));
            report["stdout"] = Value::Null;
            report["stderr"] = Value::Null;
            report["exit_code"] = Value::Null;
            report["note"]=json!("Процесс не завершился обычным образом. Частичный вывод не сохранён; команда могла выполнить часть действий.");
        }
    }
    let folder = app.data.join("executions");
    std::fs::create_dir_all(&folder)?;
    crate::changes::replace(
        &folder.join(format!("{id}.json")),
        &serde_json::to_vec_pretty(&report)?,
    )?;
    for key in ["stdout", "stderr"] {
        if let Some(text) = report[key].as_str() {
            report[key] = json!(tools::truncate(
                text,
                if key == "stdout" { 10000 } else { 5000 }
            ));
        }
    }
    report["note_process_only"]=json!("exit_code=0 подтверждает завершение этой команды, а не корректность всей задачи. Полный сохранённый вывод доступен по execution_id (до 250 КБ на поток).");
    let text = report.to_string();
    match result {
        Ok((0, _, _)) => Ok(text),
        Ok((code, _, _)) => Err(crate::agent::CommandFailed { code, output: text }.into()),
        Err(_) => Err(crate::agent::ExecutionStopped { report: text }.into()),
    }
}
