//! Explicit file resolution; durable discovery lives in discovery.rs.
use anyhow::{Context, Result};
use std::path::{Path,PathBuf};
use tokio_util::sync::CancellationToken;

pub(crate) fn wildcard(pattern: &str, text: &str) -> bool {
    let pattern: Vec<_> = pattern.chars().collect();
    let text: Vec<_> = text.chars().collect();
    let (mut p, mut t, mut star, mut retry) = (0, 0, None, 0);
    while t < text.len() {
        if p < pattern.len() && (pattern[p] == '?' || pattern[p] == text[t]) {
            p += 1;
            t += 1;
        } else if p < pattern.len() && pattern[p] == '*' {
            star = Some(p);
            p += 1;
            retry = t;
        } else if let Some(s) = star {
            retry += 1;
            t = retry;
            p = s + 1;
        } else {
            return false;
        }
    }
    while p < pattern.len() && pattern[p] == '*' {
        p += 1;
    }
    p == pattern.len()
}
pub fn writable_path(root: &Path, value: &str, computer_access: bool) -> Result<PathBuf> {
    let expanded = crate::system_paths::expand(value)?;
    if !computer_access {
        return crate::tools::checked_path(root, &expanded.to_string_lossy());
    }
    let value = expanded.as_path();
    anyhow::ensure!(
        !value
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir)),
        "Путь с .. недопустим"
    );
    let candidate = if value.is_absolute() {
        value.to_path_buf()
    } else {
        root.join(value)
    };
    let mut existing = candidate.as_path();
    let mut missing = vec![];
    while !existing.exists() {
        missing.push(
            existing
                .file_name()
                .context("Неверный путь")?
                .to_os_string(),
        );
        existing = existing.parent().context("Нет родительского каталога")?;
    }
    let mut resolved = dunce::canonicalize(existing)?;
    for name in missing.into_iter().rev() {
        resolved.push(name);
    }
    Ok(resolved)
}

pub async fn readable(
    app: &crate::api::Shared,
    value: &str,
    _cancel: &CancellationToken,
) -> Result<PathBuf> {
    let settings = app.settings.lock().await.clone();
    let root = PathBuf::from(settings.workspace);
    let expanded = crate::system_paths::expand(value)?;
    if !settings.computer_access {
        return crate::tools::checked_path(&root, &expanded.to_string_lossy());
    }
    let path = expanded.as_path();
    let candidate = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    };
    if candidate.exists() {
        return dunce::canonicalize(candidate).context("Файл недоступен");
    }

    anyhow::bail!(
        "Путь не существует: {}. Вызовите find_files по имени файла.",
        candidate.display()
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wildcard_names() {
        assert!(wildcard("*.txt", "makar.txt"));
        assert!(wildcard("m?kar.*", "makar.txt"));
        assert!(!wildcard("*.md", "makar.txt"));
    }
    #[tokio::test]
    async fn reads_absolute_file_outside_workspace_when_enabled() {
        let root = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        std::fs::create_dir_all(root.join("outside")).unwrap();
        let file = root.join("outside/test.txt");
        std::fs::write(&file, "outside fixture").unwrap();
        let app = std::sync::Arc::new(crate::api::App::new(root.clone()).unwrap());
        let cancel = CancellationToken::new();
        assert!(readable(&app, &file.to_string_lossy(), &cancel)
            .await
            .is_ok());
        app.settings.lock().await.computer_access = false;
        assert!(readable(&app, &file.to_string_lossy(), &cancel)
            .await
            .is_err());
        drop(app); // SQLite owns open files until the application is dropped.
        std::fs::remove_dir_all(root).unwrap();
    }
}
