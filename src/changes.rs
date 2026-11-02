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

pub fn edited_encoded(bytes:&[u8],encoding:&str,old:&str,new:&str)->Result<Vec<u8>>{
    let decoded=crate::text_source::decode(bytes,encoding)?;
    let edited=edited(&decoded.text,old,new)?;
    decoded.encode(std::str::from_utf8(&edited)?)
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

/// A fully written replacement that has not yet changed the destination.
/// Dropping it after failed validation/reconfiguration releases only its own temporary file.
pub struct PreparedReplacement { path:PathBuf, temporary:PathBuf }
impl Drop for PreparedReplacement {
    fn drop(&mut self){let _=fs::remove_file(&self.temporary);}
}
impl PreparedReplacement {
    pub fn new(path:&Path,bytes:&[u8])->Result<Self>{
        use std::io::Write;
        Self::write(path,|file|{file.write_all(bytes)?;Ok(())})
    }
    pub fn write<F>(path:&Path,write:F)->Result<Self> where F:FnOnce(&mut fs::File)->Result<()>{
        let parent=path.parent().context("Нет родительской папки")?;fs::create_dir_all(parent)?;
        let staged=Self{path:path.to_owned(),temporary:parent.join(format!(".local-gemma-{}.tmp",uuid::Uuid::new_v4()))};
        let mut file=fs::OpenOptions::new().write(true).create_new(true).open(&staged.temporary)?;
        write(&mut file)?;
        #[cfg(unix)]
        match fs::metadata(path){Ok(metadata)=>file.set_permissions(metadata.permissions())?,Err(error) if error.kind()==std::io::ErrorKind::NotFound=>{},Err(error)=>return Err(error.into())}
        file.sync_all()?;drop(file);Ok(staged)
    }
    pub fn commit(self)->Result<()> {
        #[cfg(windows)]{
            use std::os::windows::ffi::OsStrExt;
            #[link(name="kernel32")]
            extern "system"{fn MoveFileExW(from:*const u16,to:*const u16,flags:u32)->i32;}
            let from:Vec<u16>=self.temporary.as_os_str().encode_wide().chain(Some(0)).collect();
            let to:Vec<u16>=self.path.as_os_str().encode_wide().chain(Some(0)).collect();
            anyhow::ensure!(unsafe{MoveFileExW(from.as_ptr(),to.as_ptr(),1|8)}!=0,"Не удалось заменить файл: {}",std::io::Error::last_os_error());
        }
        #[cfg(not(windows))]{
            fs::rename(&self.temporary,&self.path)?;
            fs::File::open(self.path.parent().context("Нет родительской папки")?)?.sync_all()?;
        }
        Ok(())
    }
}
/// Replace a file without first deleting the old file (also on Windows).
pub fn replace(path:&Path,bytes:&[u8])->Result<()> {PreparedReplacement::new(path,bytes)?.commit()}

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

#[cfg(test)]
pub fn preview(before:Option<&[u8]>,after:Option<&[u8]>)->String{preview_encoded(before,after,"auto")}
pub fn preview_encoded(before:Option<&[u8]>,after:Option<&[u8]>,encoding:&str)->String{
    let old=before.map(|bytes|crate::text_source::decode(bytes,encoding));
    let new=after.map(|bytes|crate::text_source::decode(bytes,encoding));
    let header=format!("Было: {} байт · Будет: {} байт\n",before.map_or(0,|v|v.len()),after.map_or(0,|v|v.len()));
    if old.as_ref().is_some_and(Result::is_err)||new.as_ref().is_some_and(Result::is_err){return format!("{header}Текстовый diff недоступен: двоичные данные или неизвестная кодировка.");}
    let old=old.and_then(Result::ok);let new=new.and_then(Result::ok);
    let a=old.as_ref().map_or("",|value|value.text.as_str());let b=new.as_ref().map_or("",|value|value.text.as_str());
    let prefix=a.lines().zip(b.lines()).take_while(|(a,b)|a==b).count();
    let al=a.lines().count();let bl=b.lines().count();
    let suffix=a.lines().rev().zip(b.lines().rev()).take((al-prefix).min(bl-prefix)).take_while(|(a,b)|a==b).count();
    let mut diff=format!("{header}Кодировка: {} → {}\n--- до\n+++ после\n@@ строка {} @@\n",old.as_ref().map_or("—",|v|v.encoding()),new.as_ref().map_or("—",|v|v.encoding()),prefix+1);
    let line_endings=|text:&str|{let crlf=text.matches("\r\n").count();format!("CRLF {}, LF {}, CR {}; завершающий LF: {}",crlf,text.bytes().filter(|c|*c==b'\n').count()-crlf,text.bytes().filter(|c|*c==b'\r').count()-crlf,if text.ends_with('\n'){"да"}else{"нет"})};
    diff.push_str(&format!("Переводы строк: {} → {}\n",line_endings(a),line_endings(b)));
    let mut remaining=20000usize.saturating_sub(diff.chars().count());
    let mut append=|sign:char,line:&str|->bool{
        if remaining<2{return false;}diff.push(sign);remaining-=1;
        for character in line.chars(){if remaining<=1{return false;}diff.push(character);remaining-=1;}
        diff.push('\n');remaining-=1;true
    };
    let mut complete=true;
    for (sign,line) in a.lines().skip(prefix.saturating_sub(2)).take(prefix.min(2)).map(|l|(' ',l))
        .chain(a.lines().skip(prefix).take(al-prefix-suffix).map(|l|('-',l)))
        .chain(b.lines().skip(prefix).take(bl-prefix-suffix).map(|l|('+',l)))
        .chain(b.lines().skip(bl-suffix).take(2).map(|l|(' ',l))){
        if !append(sign,line){complete=false;break;}
    }
    if !complete{diff.push_str("\n[Предпросмотр сокращён; операция включает весь указанный файл.]\n");}diff
}

#[cfg(test)]
mod tests {
    #[test]
    fn streaming_serialization_failure_leaves_original_and_removes_its_temporary(){
        use std::io::Write;
        let root=std::env::temp_dir().join(format!("stream-save-{}",uuid::Uuid::new_v4()));fs::create_dir_all(&root).unwrap();let path=root.join("chat.json");fs::write(&path,b"original").unwrap();
        let failure=PreparedReplacement::write(&path,|file|{file.write_all(b"partially serialized")?;anyhow::bail!("fixture serializer failed")});
        assert!(failure.is_err());assert_eq!(fs::read(&path).unwrap(),b"original");assert_eq!(fs::read_dir(&root).unwrap().count(),1);
        PreparedReplacement::write(&path,|file|{let mut writer=std::io::BufWriter::new(file);serde_json::to_writer(&mut writer,&json!({"content":"text 🦀\r\n\"quoted\""}))?;writer.flush()?;Ok(())}).unwrap().commit().unwrap();
        assert_eq!(serde_json::from_slice::<Value>(&fs::read(&path).unwrap()).unwrap()["content"],"text 🦀\r\n\"quoted\"");fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn encoded_edits_preserve_bom_endianness_crlf_and_legacy_bytes(){
        let source="строка\r\nстарое значение 🦀\r\n";
        let replacement="новое значение 🦀";
        for (encoding,bom) in [("utf-8",vec![0xef,0xbb,0xbf]),("utf-16le",vec![0xff,0xfe]),("utf-16be",vec![0xfe,0xff]),("utf-32le",vec![0xff,0xfe,0,0]),("utf-32be",vec![0,0,0xfe,0xff])]{
            let codec=crate::text_source::decode(&bom,encoding).unwrap();let before=codec.encode(source).unwrap();
            let after=edited_encoded(&before,"auto","старое значение 🦀",replacement).unwrap();
            assert!(after.starts_with(&bom));let decoded=crate::text_source::decode(&after,"auto").unwrap();assert_eq!(decoded.encoding(),encoding);assert_eq!(decoded.text,source.replace("старое значение 🦀",replacement));
            let preview=preview(Some(&before),Some(&after));assert!(preview.contains("-старое значение 🦀"));assert!(preview.contains("+новое значение 🦀"));assert!(preview.contains("CRLF 2, LF 0"));
        }
        for encoding in ["windows-1251","ibm866"]{
            let codec=crate::text_source::decode(&[],encoding).unwrap();let before=codec.encode("старое\r\n").unwrap();
            assert!(edited_encoded(&before,"auto","старое","новое").is_err());
            let after=edited_encoded(&before,encoding,"старое","новое").unwrap();assert_eq!(crate::text_source::decode(&after,encoding).unwrap().text,"новое\r\n");
            assert!(preview_encoded(Some(&before),Some(&after),encoding).contains("+новое"));
            assert!(edited_encoded(&before,encoding,"старое","🦀").is_err());
        }
        assert!(edited_encoded(&[0xff,0xfe,0x00],"auto","x","y").is_err());
        assert!(edited_encoded(&[0xff,0xfe,0x00,0xd8],"auto","x","y").is_err());
        assert!(crate::text_source::decode(&[0xff,0xfe],"utf-8").is_err());
    }
    #[test]
    fn huge_line_preview_is_bounded_and_admits_omission(){
        let before="я".repeat(2_000_000);let after="x";
        let diff=preview(Some(before.as_bytes()),Some(after.as_bytes()));assert!(diff.chars().count()<20200);assert!(diff.contains("Предпросмотр сокращён"));
    }
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
