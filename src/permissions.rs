//! Explicit, canonical scopes. A command's cwd is not an OS sandbox.
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tokio::sync::oneshot;
tokio::task_local! {pub static TASK: String;}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Scope {
    pub kind: String,
    pub project: Option<PathBuf>,
    pub paths: Vec<PathBuf>,
    pub command: Option<String>,
    pub task: Option<String>,
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
        }
    }
    pub fn command(directory: &Path, command: &str) -> Self {
        Self {
            kind: "command".into(),
            project: dunce::canonicalize(directory).ok(),
            paths: vec![],
            command: Some(command.into()),
            task: TASK.try_with(Clone::clone).ok(),
        }
    }
    pub fn reusable(&self) -> bool {
        self.project.is_some()
            && (self.kind == "command"
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
pub struct Grant {
    pub id: String,
    pub scope: Scope,
    pub created: String,
}
#[derive(Default)]
pub struct Grants {
    pub entries: Vec<Grant>,
}
impl Grants {
    pub fn load(data: &Path) -> Result<Self> {
        let path = data.join("permissions.json");
        let entries = match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => vec![],
            Err(e) => return Err(e.into()),
        };
        Ok(Self { entries })
    }
    fn save(&self, data: &Path) -> Result<()> {
        let persisted: Vec<_> = self
            .entries
            .iter()
            .filter(|g| g.scope.task.is_none())
            .collect();
        crate::changes::replace(
            &data.join("permissions.json"),
            &serde_json::to_vec_pretty(&persisted)?,
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
                && g.scope.project == scope.project
                && g.scope.command == scope.command
                && (g.scope.task.is_none() || g.scope.task == scope.task)
        })
    }
    pub fn add(&mut self, data: &Path, mut scope: Scope, duration: &str) -> Result<()> {
        anyhow::ensure!(
            scope.reusable(),
            "Для этого действия доступно только однократное разрешение"
        );
        match duration {
            "task" => anyhow::ensure!(scope.task.is_some(), "Нет активной задачи"),
            "project" => scope.task = None,
            _ => anyhow::bail!("Неизвестный срок разрешения"),
        }
        let grant = Grant {
            id: uuid::Uuid::new_v4().to_string(),
            scope,
            created: crate::api::now(),
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    #[tokio::test]
    async fn policy_and_grant_changes_resolve_only_matching_live_waiters() {
        let root = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        let app = Arc::new(crate::api::App::new(root.clone()).unwrap());
        let project = root.join("data/workspace");
        std::fs::create_dir_all(&project).unwrap();
        let mut files = Scope::files(&project, vec![project.join("a.txt")], false);
        files.task = Some("run".into());
        let command = Scope::command(&project, "echo test");
        let (sender, mut file_waiter) = oneshot::channel();
        app.approvals.lock().await.insert("file".into(), Pending {sender, scope:files.clone()});
        let (sender, mut command_waiter) = oneshot::channel();
        app.approvals.lock().await.insert("command".into(), Pending {sender, scope:command.clone()});
        app.settings.lock().await.permission_mode="project".into();
        resolve_pending(&app).await;
        assert!(file_waiter.try_recv().unwrap());
        assert!(matches!(command_waiter.try_recv(), Err(oneshot::error::TryRecvError::Empty)));
        app.grants.lock().await.add(&app.data, command.clone(), "project").unwrap();
        resolve_pending(&app).await;
        assert!(command_waiter.try_recv().unwrap());
        let (sender, mut destructive_waiter) = oneshot::channel();
        app.approvals.lock().await.insert("delete".into(), Pending {sender, scope:Scope::once()});
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
        let command = Scope::command(&root, "echo hello");
        grants.add(&root, command.clone(), "project").unwrap();
        assert!(grants.allows(&command, "manual"));
        assert!(!grants.allows(&Scope::command(&root, "echo hello; delete"), "manual"));
        assert!(!grants.allows(&Scope::once(), "project"));
        assert!(grants.allows(&Scope::once(), "full"));
        std::fs::remove_dir_all(root).unwrap();
    }
}
