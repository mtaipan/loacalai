//! Verified file mutations with persistent before/after snapshots.
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    fs,
    path::{Path, PathBuf},
};

pub const MAX_BYTES: u64 = 16 * 1024 * 1024;

pub fn edited(text: &str, old: &str, new: &str) -> Result<Vec<u8>> {
    anyhow::ensure!(!old.is_empty(), "old_text не должен быть пустым");
    // Models commonly emit LF for Windows files. Adapt only line separators
    // of consistently CRLF files; content still has to match exactly once.
    let crlf = text.contains("\r\n") && !text.replace("\r\n", "").contains('\n');
    let old = if crlf {
        old.replace("\r\n", "\n").replace('\n', "\r\n")
    } else {
        old.to_string()
    };
    let new = if crlf {
        new.replace("\r\n", "\n").replace('\n', "\r\n")
    } else {
        new.to_string()
    };
    let first = text.find(&old);
    anyhow::ensure!(first.is_some() && first==text.rfind(&old),"old_text должен встречаться ровно один раз. Передайте точный фрагмент исходного текста без номеров строк, меток FILE_TEXT, внешних кавычек и буквальных JSON-экранирований. Прочитайте нужный участок заново.");
    Ok(text.replacen(&old, &new, 1).into_bytes())
}

pub fn snapshot(path: &Path) -> Result<Option<Vec<u8>>> {
    match fs::metadata(path) {
        Ok(meta) => {
            anyhow::ensure!(meta.is_file(), "Ожидается файл: {}", path.display());
            anyhow::ensure!(
                meta.len() <= MAX_BYTES,
                "Файл больше 16 МиБ: {}",
                path.display()
            );
            // Recheck the cap while reading: a file may grow after metadata().
            use std::io::Read;
            let mut data = Vec::new();
            fs::File::open(path)?
                .take(MAX_BYTES + 1)
                .read_to_end(&mut data)?;
            anyhow::ensure!(data.len() as u64 <= MAX_BYTES, "Файл вырос больше 16 МиБ");
            Ok(Some(data))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Replace a file without first deleting the old file (also on Windows).
pub fn replace(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let parent = path.parent().context("Нет родительской папки")?;
    fs::create_dir_all(parent)?;
    let tmp = parent.join(format!(".local-gemma-{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| -> Result<()> {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        file.write_all(bytes)?;
        #[cfg(unix)]
        match fs::metadata(path) {
            Ok(metadata) => file.set_permissions(metadata.permissions())?,
            Err(error) if error.kind()==std::io::ErrorKind::NotFound=>{},
            Err(error)=>return Err(error.into()),
        }
        file.sync_all()?;
        drop(file);
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStrExt;
            #[link(name = "kernel32")]
            extern "system" {
                fn MoveFileExW(from: *const u16, to: *const u16, flags: u32) -> i32;
            }
            let from: Vec<u16> = tmp.as_os_str().encode_wide().chain(Some(0)).collect();
            let to: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
            anyhow::ensure!(
                unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), 1 | 8) } != 0,
                "Не удалось заменить файл: {}",
                std::io::Error::last_os_error()
            );
        }
        #[cfg(not(windows))]
        {
            fs::rename(&tmp, path)?;
            fs::File::open(parent)?.sync_all()?;
        }
        Ok(())
    })();
    if tmp.exists() {
        let _ = fs::remove_file(tmp);
    }
    result
}

pub struct Mutation {
    pub path: PathBuf,
    pub before: Option<Vec<u8>>,
    pub after: Option<Vec<u8>>,
}
impl Mutation {
    pub fn new(path: PathBuf, after: Option<Vec<u8>>) -> Result<Self> {
        let before = snapshot(&path)?;
        Ok(Self {
            path,
            before,
            after,
        })
    }
}

#[derive(Serialize, Deserialize)]
struct Entry {
    path: PathBuf,
    before: bool,
    after: bool,
}
#[derive(Serialize, Deserialize)]
struct Change {
    id: String,
    created: String,
    operation: String,
    status: String,
    entries: Vec<Entry>,
}

fn save(dir: &Path, change: &Change) -> Result<()> {
    replace(
        &dir.join("change.json"),
        &serde_json::to_vec_pretty(change)?,
    )
}
fn set(path: &Path, bytes: &Option<Vec<u8>>) -> Result<()> {
    match bytes {
        Some(bytes) => replace(path, bytes),
        None => {
            fs::remove_file(path)?;
            Ok(())
        }
    }
}

/// Caller holds app.file_changes. Conflicts never overwrite an external edit.
pub fn apply(data: &Path, operation: &str, mutations: Vec<Mutation>) -> Result<Value> {
    anyhow::ensure!(!mutations.is_empty(), "Нет изменений");
    for item in &mutations {
        anyhow::ensure!(
            snapshot(&item.path)? == item.before,
            "Файл изменён после предпросмотра: {}. Прочитайте его заново.",
            item.path.display()
        );
        anyhow::ensure!(
            item.after
                .as_ref()
                .is_none_or(|v| v.len() as u64 <= MAX_BYTES),
            "Результат больше 16 МиБ"
        );
    }
    let id = uuid::Uuid::new_v4().to_string();
    let dir = data.join("changes").join(&id);
    fs::create_dir_all(&dir)?;
    let mut change = Change {
        id: id.clone(),
        created: crate::api::now(),
        operation: operation.into(),
        status: "prepared".into(),
        entries: vec![],
    };
    for (i, item) in mutations.iter().enumerate() {
        if let Some(bytes) = &item.before {
            replace(&dir.join(format!("{i}.before")), bytes)?;
        }
        if let Some(bytes) = &item.after {
            replace(&dir.join(format!("{i}.after")), bytes)?;
        }
        change.entries.push(Entry {
            path: item.path.clone(),
            before: item.before.is_some(),
            after: item.after.is_some(),
        });
    }
    save(&dir, &change)?;
    for item in &mutations {
        // A second check immediately before mutation catches edits during backup.
        if snapshot(&item.path)? != item.before {
            change.status = "partial".into();
            save(&dir, &change)?;
            anyhow::bail!(
                "Конфликт записи; резервная копия {}. Файл: {}",
                id,
                item.path.display()
            );
        }
        if let Err(error) = set(&item.path, &item.after).and_then(|_| {
            anyhow::ensure!(
                snapshot(&item.path)? == item.after,
                "Проверка результата записи не прошла"
            );
            Ok(())
        }) {
            change.status = "partial".into();
            let _ = save(&dir, &change);
            anyhow::bail!(
                "Операция не завершена: {error:#}. Резервные копии: {}",
                dir.display()
            );
        }
    }
    change.status = "applied".into();
    save(&dir, &change)?;
    Ok(
        json!({"change_id":id,"verified":true,"changed":mutations.iter().any(|m|m.before!=m.after),"operation":operation,"paths":change.entries.iter().map(|e|&e.path).collect::<Vec<_>>(),"backup":dir}),
    )
}

pub fn list(data: &Path) -> Result<Value> {
    let root = data.join("changes");
    if !root.exists() {
        return Ok(json!([]));
    }
    let mut changes = vec![];
    for dir in fs::read_dir(root)?.flatten() {
        if let Ok(bytes) = fs::read(dir.path().join("change.json")) {
            if let Ok(change) = serde_json::from_slice::<Change>(&bytes) {
                changes.push(change);
            }
        }
    }
    changes.sort_by(|a, b| b.created.cmp(&a.created));
    changes.truncate(200);
    Ok(serde_json::to_value(changes)?)
}

pub fn restore(data: &Path, id: &str, workspace: &Path, computer_access: bool) -> Result<Value> {
    uuid::Uuid::parse_str(id)?;
    let dir = data.join("changes").join(id);
    let mut change: Change = serde_json::from_slice(&fs::read(dir.join("change.json"))?)?;
    anyhow::ensure!(
        change.status == "applied" || change.status == "partial",
        "Изменение уже отменено или не было применено"
    );
    let mut restorations = vec![];
    for (i, entry) in change.entries.iter().enumerate() {
        let path = crate::filesystem::writable_path(
            workspace,
            &entry.path.to_string_lossy(),
            computer_access,
        )?;
        anyhow::ensure!(path == entry.path, "Путь изменился после записи");
        let before = if entry.before {
            Some(fs::read(dir.join(format!("{i}.before")))?)
        } else {
            None
        };
        let after = if entry.after {
            Some(fs::read(dir.join(format!("{i}.after")))?)
        } else {
            None
        };
        let current = snapshot(&path)?;
        if current == before {
            continue;
        }
        anyhow::ensure!(
            current == after,
            "Файл изменён после операции. Автоматическая отмена не перезапишет его: {}",
            path.display()
        );
        restorations.push(Mutation {
            path,
            before: current,
            after: before,
        });
    }
    // Undo is itself journaled, so it can also be undone.
    let result = if restorations.is_empty() {
        json!({"verified":true})
    } else {
        apply(data, "restore", restorations)?
    };
    change.status = "restored".into();
    save(&dir, &change)?;
    Ok(result)
}

pub fn preview(before: Option<&[u8]>, after: Option<&[u8]>) -> String {
    let old = before.and_then(|b| std::str::from_utf8(b).ok());
    let new = after.and_then(|b| std::str::from_utf8(b).ok());
    let header = format!(
        "Было: {} байт · Будет: {} байт\n",
        before.map_or(0, |v| v.len()),
        after.map_or(0, |v| v.len())
    );
    if (before.is_some() && old.is_none()) || (after.is_some() && new.is_none()) {
        return format!("{header}Двоичный файл; доступна резервная копия.");
    }
    let a: Vec<_> = old.unwrap_or("").lines().collect();
    let b: Vec<_> = new.unwrap_or("").lines().collect();
    let prefix = a.iter().zip(&b).take_while(|(a, b)| a == b).count();
    let suffix = a[prefix..]
        .iter()
        .rev()
        .zip(b[prefix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let mut diff = format!("{header}--- до\n+++ после\n@@ строка {} @@\n", prefix + 1);
    for line in &a[prefix.saturating_sub(2)..prefix] {
        diff.push_str(&format!(" {line}\n"));
    }
    for line in &a[prefix..a.len() - suffix] {
        diff.push_str(&format!("-{line}\n"));
    }
    for line in &b[prefix..b.len() - suffix] {
        diff.push_str(&format!("+{line}\n"));
    }
    for line in b[b.len() - suffix..].iter().take(2) {
        diff.push_str(&format!(" {line}\n"));
    }
    crate::tools::truncate(&diff, 20000)
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    #[test]
    fn replacement_preserves_executable_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir=std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        std::fs::create_dir(&dir).unwrap();
        let path=dir.join("script.sh");
        std::fs::write(&path,b"old").unwrap();
        std::fs::set_permissions(&path,std::fs::Permissions::from_mode(0o751)).unwrap();
        super::replace(&path,b"new").unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode()&0o777,0o751);
        assert_eq!(std::fs::read(&path).unwrap(),b"new");
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[cfg(windows)]
    #[test]
    fn failed_atomic_replace_preserves_old_data_and_removes_temp() {
        use std::os::windows::fs::OpenOptionsExt;
        let root=PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target").join(format!("atomic-test-{}",uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let path=root.join("settings.json");
        replace(&path,b"old complete data").unwrap();
        // A reader refusing delete-sharing simulates a replacement failure.
        let held=fs::OpenOptions::new().read(true).share_mode(1).open(&path).unwrap();
        assert!(replace(&path,b"new data").is_err());
        assert_eq!(fs::read(&path).unwrap(),b"old complete data");
        assert_eq!(fs::read_dir(&root).unwrap().count(),1);
        drop(held);
        replace(&path,b"new complete data").unwrap();
        assert_eq!(fs::read(&path).unwrap(),b"new complete data");
        fs::remove_dir_all(root).unwrap();
    }
    use super::*;
    #[test]
    fn edits_preserve_windows_newlines_without_fuzzy_matching() {
        let source = "def add(a, b):\r\n    return a - b\r\n";
        assert_eq!(
            edited(
                source,
                "def add(a, b):\n    return a - b",
                "def add(a, b):\n    return a + b"
            )
            .unwrap(),
            b"def add(a, b):\r\n    return a + b\r\n"
        );
        assert!(edited(source, "1: def add(a, b):\n2:     return a - b", "bad").is_err());
        assert!(edited("aaa", "aa", "x").is_err());
        assert!(edited("unchanged", "missing", "x").is_err());
    }
    #[test]
    fn move_and_delete_can_be_restored() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!("move-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let root = dunce::canonicalize(root).unwrap();
        let source = root.join("source.bin");
        let destination = root.join("renamed.bin");
        let bytes = vec![0, 1, 255, 0, 14];
        fs::write(&source, &bytes).unwrap();
        let moved = apply(
            &root,
            "move_file",
            vec![
                Mutation::new(destination.clone(), Some(bytes.clone())).unwrap(),
                Mutation::new(source.clone(), None).unwrap(),
            ],
        )
        .unwrap();
        assert!(!source.exists());
        assert_eq!(fs::read(&destination).unwrap(), bytes);
        restore(&root, moved["change_id"].as_str().unwrap(), &root, true).unwrap();
        assert!(!destination.exists());
        assert_eq!(fs::read(&source).unwrap(), bytes);
        let deleted = apply(
            &root,
            "delete_file",
            vec![Mutation::new(source.clone(), None).unwrap()],
        )
        .unwrap();
        assert!(!source.exists());
        restore(&root, deleted["change_id"].as_str().unwrap(), &root, true).unwrap();
        assert_eq!(fs::read(&source).unwrap(), bytes);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn verified_write_restore_and_conflict() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!("changes-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let path = dunce::canonicalize(&root).unwrap().join("file.txt");
        fs::write(&path, b"old").unwrap();
        let first = apply(
            &root,
            "write_file",
            vec![Mutation::new(path.clone(), Some(b"new".to_vec())).unwrap()],
        )
        .unwrap();
        assert_eq!(first["verified"], true);
        fs::write(&path, b"external").unwrap();
        assert!(restore(&root, first["change_id"].as_str().unwrap(), &root, true).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"external");
        fs::write(&path, b"new").unwrap();
        restore(&root, first["change_id"].as_str().unwrap(), &root, true).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"old");
        let prepared = Mutation::new(path.clone(), Some(b"stale".to_vec())).unwrap();
        fs::write(&path, b"latest").unwrap();
        assert!(apply(&root, "write_file", vec![prepared]).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"latest");
        fs::remove_dir_all(root).unwrap();
    }
}
