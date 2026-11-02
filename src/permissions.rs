//! Explicit, canonical scopes. A command's cwd is not an OS sandbox.
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tokio::sync::oneshot;
tokio::task_local! {pub static TASK: String;}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scope {
    pub kind: String,
    pub project: Option<PathBuf>,
    pub paths: Vec<PathBuf>,
    pub command: Option<String>,
    pub task: Option<String>,
    #[serde(default)]
    pub work_task:Option<String>,
    #[serde(default)]
    pub directory:Option<PathBuf>,
    #[serde(default)]
    pub legacy_cwd:bool,
}
impl Scope {
    pub fn once() -> Self {
        Self::default()
    }
    pub fn files(project: &Path, paths: Vec<PathBuf>, destructive: bool) -> Self {
        Self {
            kind: if destructive {
                "delete_files"
            } else {
                "write_files"
            }
            .into(),
            project: dunce::canonicalize(project).ok(),
            paths,
            command: None,
            task: TASK.try_with(Clone::clone).ok(),
            work_task:None,directory:None,legacy_cwd:false,
        }
    }
    pub fn command_in(project:&Path,directory:&Path,command:&str)->Self{
        Self {
            kind: "command".into(),
            project: dunce::canonicalize(project).ok(),
            paths: vec![],
            command: Some(command.into()),
            task: TASK.try_with(Clone::clone).ok(),
            work_task:None,directory:dunce::canonicalize(directory).ok(),legacy_cwd:false,
        }
    }
    pub fn reusable(&self) -> bool {
        self.project.is_some()
            && ((self.kind == "command"&&self.directory.as_ref().is_some_and(|directory|directory.starts_with(self.project.as_ref().unwrap())))
                || (self.kind == "write_files"
                    && !self.paths.is_empty()
                    && self
                        .paths
                        .iter()
                        .all(|p| p.starts_with(self.project.as_ref().unwrap()))))
    }
}
pub struct Pending {
    pub sender: oneshot::Sender<bool>,
    pub scope: Scope,
    /// Memory-only preview, never part of durable events or grants.
    pub private_details: Option<String>,
}
/// Lock order for permission decisions: pending -> settings -> grants.
/// Registration uses the same order, so a policy change cannot miss a new waiter.
pub async fn resolve_pending(app: &crate::api::Shared) {
    let mut pending = app.approvals.lock().await;
    let mode = app.settings.lock().await.permission_mode.clone();
    let grants = app.grants.lock().await;
    let ids: Vec<_> = pending.iter()
        .filter(|(_, item)| item.sender.is_closed() || grants.allows(&item.scope, &mode))
        .map(|(id, _)| id.clone()).collect();
    for id in ids {
        if let Some(item) = pending.remove(&id) { let _ = item.sender.send(true); }
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Grant {
    pub id: String,
    pub scope: Scope,
    pub created: String,
    #[serde(default)]
    pub duration:String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PermissionFile{version:u32,grants:Vec<Grant>}
#[derive(Default)]
pub struct Grants {
    pub entries: Vec<Grant>,
}
impl Grants {
    pub fn load(data: &Path) -> Result<Self> {
        let path = data.join("permissions.json");
        let mut entries:Vec<Grant> = match std::fs::read(path) {
            Ok(bytes) => {
                let value:serde_json::Value=serde_json::from_slice(&bytes)?;
                if value.is_array(){
                    let mut grants:Vec<Grant>=serde_json::from_value(value)?;
                    for grant in &mut grants{
                        if grant.duration.is_empty(){anyhow::ensure!(grant.scope.work_task.is_none(),"Новая область задачи требует версии файла разрешений");grant.duration=if grant.scope.task.is_some(){"run"}else{"project"}.into();}
                        if grant.scope.kind=="command"&&grant.scope.directory.is_none(){grant.scope.directory=grant.scope.project.clone();grant.scope.legacy_cwd=true;}
                    }grants
                }else{
                    let file:PermissionFile=serde_json::from_value(value)?;
                    anyhow::ensure!(file.version==2,"Неизвестная версия сохранённых разрешений; файл сохранён");file.grants
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => vec![],
            Err(e) => return Err(e.into()),
        };
        for grant in &entries{
            anyhow::ensure!(["command","write_files"].contains(&grant.scope.kind.as_str())&&grant.scope.project.is_some(),"Некорректная область сохранённого разрешения");
            if grant.scope.kind=="command"{anyhow::ensure!(grant.scope.command.is_some()&&grant.scope.directory.is_some()&&(!grant.scope.legacy_cwd||grant.scope.directory==grant.scope.project),"Некорректная папка сохранённой команды");}
            else{anyhow::ensure!(!grant.scope.legacy_cwd&&grant.scope.directory.is_none()&&grant.scope.command.is_none(),"Некорректная область разрешения файлов");}
            let valid=match grant.duration.as_str(){"run"=>grant.scope.task.is_some(),"work_task"=>grant.scope.task.is_none()&&grant.scope.work_task.as_ref().is_some_and(|id|!id.is_empty()),"project"=>grant.scope.task.is_none()&&grant.scope.work_task.is_none(),_=>false};
            anyhow::ensure!(valid,"Срок разрешения не соответствует его области; файл сохранён");
        }
        // Run grants never survive a process restart, including legacy files.
        entries.retain(|grant|grant.scope.task.is_none());Ok(Self { entries })
    }
    fn save(&self, data: &Path) -> Result<()> {
        let persisted: Vec<_> = self
            .entries
            .iter()
            .filter(|g| g.scope.task.is_none())
            .collect();
        crate::changes::replace(
            &data.join("permissions.json"),
            // Older applications must fail to parse rather than ignore work_task
            // and silently turn a logical-task grant into a project-wide grant.
            &serde_json::to_vec_pretty(&serde_json::json!({"version":2,"grants":persisted}))?,
        )
    }
    pub fn allows(&self, scope: &Scope, mode: &str) -> bool {
        if mode == "full" {
            return true;
        }
        if !scope.reusable() {
            return false;
        }
        if mode == "project" && scope.kind == "write_files" {
            return true;
        }
        self.entries.iter().any(|g| {
            g.scope.kind == scope.kind
                && (if scope.kind=="command"{
                    if g.scope.legacy_cwd{g.scope.directory==scope.directory}
                    else{g.scope.project==scope.project&&g.scope.directory==scope.directory}
                }else{g.scope.project==scope.project})
                && g.scope.command == scope.command
                && (g.scope.task.is_none() || g.scope.task == scope.task)
                && (g.scope.work_task.is_none()||g.scope.work_task==scope.work_task)
        })
    }
    pub fn add(&mut self, data: &Path, mut scope: Scope, duration: &str) -> Result<()> {
        anyhow::ensure!(
            scope.reusable(),
            "Для этого действия доступно только однократное разрешение"
        );
        match duration {
            "task" => anyhow::ensure!(scope.task.is_some(), "Нет активной задачи"),
            "work_task"=>{anyhow::ensure!(scope.work_task.as_ref().is_some_and(|id|!id.is_empty()),"Нет логической задачи для сохранения разрешения");scope.task=None;},
            "project" => {scope.task = None;scope.work_task=None;},
            _ => anyhow::bail!("Неизвестный срок разрешения"),
        }
        let grant = Grant {
            id: uuid::Uuid::new_v4().to_string(),
            scope,
            created: crate::api::now(),
            duration:if duration=="task"{"run"}else{duration}.into(),
        };
        self.entries.push(grant);
        if let Err(error) = self.save(data) {
            self.entries.pop();
            return Err(error);
        }
        Ok(())
    }
    pub fn revoke(&mut self, data: &Path, id: &str) -> Result<()> {
        let previous = self.entries.clone();
        self.entries.retain(|g| g.id != id);
        if let Err(error) = self.save(data) {
            self.entries = previous;
            return Err(error);
        }
        Ok(())
    }
    pub fn finish(&mut self, task: &str) {
        self.entries
            .retain(|g| g.scope.task.as_deref() != Some(task));
    }
    pub fn forget_work_tasks(&mut self,data:&Path,tasks:&[String])->Result<()>{
        let previous=self.entries.clone();self.entries.retain(|grant|!grant.scope.work_task.as_ref().is_some_and(|id|tasks.contains(id)));
        if self.entries.len()!=previous.len(){if let Err(error)=self.save(data){self.entries=previous;return Err(error);}}Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    #[test]
    fn malformed_persisted_scopes_never_fall_back_to_broader_permissions(){
        let root=std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());std::fs::create_dir_all(root.join("sub")).unwrap();let mut scope=Scope::command_in(&root,&root.join("sub"),"echo exact");scope.work_task=Some("goal-a".into());let mut grants=Grants::default();grants.add(&root,scope,"work_task").unwrap();
        let path=root.join("permissions.json");let original:serde_json::Value=serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        for field in ["work_task","directory"]{let mut broken=original.clone();broken["grants"][0]["scope"].as_object_mut().unwrap().remove(field);let bytes=broken.to_string().into_bytes();std::fs::write(&path,&bytes).unwrap();assert!(Grants::load(&root).is_err(),"{field}");assert_eq!(std::fs::read(&path).unwrap(),bytes);}
        let mut broken=original.clone();broken["grants"][0].as_object_mut().unwrap().remove("duration");std::fs::write(&path,broken.to_string()).unwrap();assert!(Grants::load(&root).is_err());
        let mut broken=original;broken["grants"][0]["scope"]["unknown_scope"]=serde_json::json!(true);std::fs::write(&path,broken.to_string()).unwrap();assert!(Grants::load(&root).is_err());std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn logical_task_grants_survive_runs_restart_but_not_other_tasks_or_outside_directories(){
        let root=std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());let project=root.join("project");let child=project.join("src");let outside=root.join("outside");std::fs::create_dir_all(&child).unwrap();std::fs::create_dir_all(&outside).unwrap();
        let mut scope=Scope::command_in(&project,&child,"echo exact");scope.task=Some("run-1".into());scope.work_task=Some("goal-a".into());let mut grants=Grants::default();grants.add(&root,scope.clone(),"work_task").unwrap();grants.finish("run-1");let mut reloaded=Grants::load(&root).unwrap();scope.task=Some("run-2".into());assert!(reloaded.allows(&scope,"manual"));
        let bytes=std::fs::read(root.join("permissions.json")).unwrap();assert!(serde_json::from_slice::<Vec<Grant>>(&bytes).is_err());assert_eq!(serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()["version"],2);
        let mut other=scope.clone();other.work_task=Some("goal-b".into());assert!(!reloaded.allows(&other,"manual"));other=scope.clone();other.directory=Some(dunce::canonicalize(&outside).unwrap());assert!(!other.reusable());assert!(!reloaded.allows(&other,"manual"));assert!(reloaded.add(&root,other,"project").is_err());other=scope.clone();other.command=Some("echo different".into());assert!(!reloaded.allows(&other,"manual"));
        reloaded.forget_work_tasks(&root,&["goal-a".into()]).unwrap();assert!(!Grants::load(&root).unwrap().allows(&scope,"manual"));std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn legacy_command_grants_keep_exact_cwd_without_authorizing_outside_the_current_project(){
        let root=std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());let project=root.join("project");let child=project.join("src");let outside=root.join("outside");std::fs::create_dir_all(&child).unwrap();std::fs::create_dir_all(&outside).unwrap();
        let legacy=serde_json::json!([{"id":"legacy","scope":{"kind":"command","project":dunce::canonicalize(&child).unwrap(),"paths":[],"command":"echo exact","task":null},"created":crate::api::now()}]);std::fs::write(root.join("permissions.json"),legacy.to_string()).unwrap();let mut grants=Grants::load(&root).unwrap();
        assert!(grants.allows(&Scope::command_in(&project,&child,"echo exact"),"manual"));assert!(!grants.allows(&Scope::command_in(&project,&project,"echo exact"),"manual"));
        grants.entries[0].scope.project=Some(dunce::canonicalize(&outside).unwrap());grants.entries[0].scope.directory=grants.entries[0].scope.project.clone();assert!(!grants.allows(&Scope::command_in(&project,&outside,"echo exact"),"manual"));std::fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn policy_and_grant_changes_resolve_only_matching_live_waiters() {
        let root = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        let app = Arc::new(crate::api::App::new(root.clone()).unwrap());
        let project = root.join("data/workspace");
        std::fs::create_dir_all(&project).unwrap();
        let mut files = Scope::files(&project, vec![project.join("a.txt")], false);
        files.task = Some("run".into());
        let command = Scope::command_in(&project,&project, "echo test");
        let (sender, mut file_waiter) = oneshot::channel();
        app.approvals.lock().await.insert("file".into(), Pending {sender, scope:files.clone(), private_details:None});
        let (sender, mut command_waiter) = oneshot::channel();
        app.approvals.lock().await.insert("command".into(), Pending {sender, scope:command.clone(), private_details:None});
        app.settings.lock().await.permission_mode="project".into();
        resolve_pending(&app).await;
        assert!(file_waiter.try_recv().unwrap());
        assert!(matches!(command_waiter.try_recv(), Err(oneshot::error::TryRecvError::Empty)));
        app.grants.lock().await.add(&app.data, command.clone(), "project").unwrap();
        resolve_pending(&app).await;
        assert!(command_waiter.try_recv().unwrap());
        let (sender, mut destructive_waiter) = oneshot::channel();
        app.approvals.lock().await.insert("delete".into(), Pending {sender, scope:Scope::once(), private_details:None});
        resolve_pending(&app).await;
        assert!(matches!(destructive_waiter.try_recv(), Err(oneshot::error::TryRecvError::Empty)));
        app.settings.lock().await.permission_mode="full".into();
        resolve_pending(&app).await;
        assert!(destructive_waiter.try_recv().unwrap());
        assert!(app.approvals.lock().await.is_empty());
        drop(app);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn grants_match_project_task_and_exact_command_without_granting_outside_files() {
        let root = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        std::fs::create_dir(&root).unwrap();
        let mut scope = Scope::files(&root, vec![root.join("a.txt")], false);
        scope.task = Some("task1".into());
        let mut grants = Grants::default();
        assert!(!grants.allows(&scope, "manual"));
        grants.add(&root, scope.clone(), "task").unwrap();
        assert!(grants.allows(&scope, "manual"));
        let mut other = scope.clone();
        other.task = Some("task2".into());
        assert!(!grants.allows(&other, "manual"));
        other.paths = vec![root.parent().unwrap().join("outside.txt")];
        assert!(!grants.allows(&other, "project"));
        assert!(!Grants::load(&root).unwrap().allows(&scope, "manual"));
        grants.finish("task1");
        assert!(!grants.allows(&scope, "manual"));
        grants.add(&root, scope.clone(), "project").unwrap();
        let mut reloaded = Grants::load(&root).unwrap();
        assert!(reloaded.allows(&scope, "manual"));
        reloaded
            .revoke(&root, &reloaded.entries[0].id.clone())
            .unwrap();
        assert!(!reloaded.allows(&scope, "manual"));
        let command = Scope::command_in(&root,&root, "echo hello");
        grants.add(&root, command.clone(), "project").unwrap();
        assert!(grants.allows(&command, "manual"));
        assert!(!grants.allows(&Scope::command_in(&root,&root, "echo hello; delete"), "manual"));
        assert!(!grants.allows(&Scope::once(), "project"));
        assert!(grants.allows(&Scope::once(), "full"));
        std::fs::remove_dir_all(root).unwrap();
    }
}
