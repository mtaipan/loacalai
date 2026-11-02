//! Read-only discovery across mounted local disks. Never elevates OS permissions.
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::{
    collections::{HashSet, VecDeque},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;

pub fn roots() -> Vec<PathBuf> {
    let mut roots = vec![];
    if let Some(home) = crate::system_paths::home() {
        for folder in ["desktop", "documents", "downloads"] {
            let Some(path) = crate::system_paths::folder(folder) else {
                continue;
            };
            if path.is_dir() {
                roots.push(path);
            }
        }
        roots.push(home);
    }
    #[cfg(windows)]
    {
        #[link(name = "kernel32")]
        extern "system" {
            fn GetLogicalDrives() -> u32;
            fn GetDriveTypeW(path: *const u16) -> u32;
        }
        let drives = unsafe { GetLogicalDrives() };
        for index in 0..26 {
            if drives & (1 << index) != 0 {
                let path = format!("{}:\\", char::from(b'A' + index));
                let wide: Vec<u16> = path.encode_utf16().chain(Some(0)).collect();
                let kind = unsafe { GetDriveTypeW(wide.as_ptr()) };
                if kind == 2 || kind == 3 {
                    roots.push(PathBuf::from(path));
                }
            }
        }
    }
    #[cfg(not(windows))]
    roots.push(PathBuf::from("/"));
    roots
}
#[cfg(test)]
pub fn find_in_roots(
    roots: Vec<PathBuf>,
    query: &str,
    cancel: &CancellationToken,
    seconds: u64,
) -> Result<Value> {
    find_mode(roots, query, cancel, seconds, false)
}
fn find_mode(
    roots: Vec<PathBuf>,
    query: &str,
    cancel: &CancellationToken,
    seconds: u64,
    first: bool,
) -> Result<Value> {
    anyhow::ensure!(!query.trim().is_empty(), "Укажите имя файла");
    let query = query.to_lowercase();
    let has_wildcards = query.contains('*') || query.contains('?');
    let started = Instant::now();
    let mut directories: VecDeque<_> = roots.iter().cloned().collect();
    let mut seen = HashSet::new();
    let mut matches = vec![];
    let mut checked = 0usize;
    let mut denied = 0usize;
    let mut complete = true;
    let mut found_first = false;
    while let Some(directory) = directories.pop_front() {
        if cancel.is_cancelled() {
            anyhow::bail!("Поиск остановлен");
        }
        if started.elapsed() > Duration::from_secs(seconds.clamp(1, 300))
            || checked >= 2_500_000
            || matches.len() >= 100
        {
            complete = false;
            break;
        }
        let Ok(canonical) = dunce::canonicalize(&directory) else {
            denied += 1;
            continue;
        };
        if !seen.insert(canonical) {
            continue;
        }
        let entries = match std::fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(_) => {
                denied += 1;
                continue;
            }
        };
        for entry in entries {
            if cancel.is_cancelled() {
                anyhow::bail!("Поиск остановлен");
            }
            if started.elapsed() > Duration::from_secs(seconds.clamp(1, 300))
                || checked >= 2_500_000
                || matches.len() >= 100
            {
                complete = false;
                break;
            }
            let Ok(entry) = entry else {
                denied += 1;
                continue;
            };
            checked += 1;
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_symlink() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_lowercase();
            if kind.is_file()
                && (if has_wildcards {
                    wildcard(&query, &name)
                } else {
                    name == query
                })
            {
                if let Ok(path) = dunce::canonicalize(entry.path()) {
                    matches.push(path.to_string_lossy().into_owned());
                    if first && !has_wildcards {
                        found_first = true;
                        complete = false;
                        break;
                    }
                }
            }
            if kind.is_dir() {
                #[cfg(not(windows))]
                if ["/proc", "/dev", "/sys", "/System/Volumes"]
                    .iter()
                    .any(|path| entry.path() == Path::new(path))
                {
                    continue;
                }
                directories.push_back(entry.path());
            }
        }
        if !complete {
            break;
        }
    }
    Ok(
        json!({"matches":matches,"complete":complete,"checked_entries":checked,"inaccessible_directories":denied,"elapsed_seconds":started.elapsed().as_secs(),"roots":roots,"note":if found_first{"Найден файл с точным именем; поиск остановлен после первого совпадения. Можно читать возвращённый абсолютный путь. Для нескольких совпадений используйте маску имени."}else if complete{"Проверены доступные каталоги; защищённые ОС пути могут быть недоступны"}else{"Поиск прерван лимитом времени, количества файлов или результатов. Это не означает, что файла нет. Уточните каталог или увеличьте seconds до 300."}}),
    )
}
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
pub async fn find(
    query: String,
    start: Option<PathBuf>,
    cancel: CancellationToken,
    seconds: u64,
) -> Result<Value> {
    let roots = start.map(|path| vec![path]).unwrap_or_else(roots);
    tokio::task::spawn_blocking(move || find_mode(roots, &query, &cancel, seconds, false)).await?
}

/// Resolve mutations without guessing a filename or silently creating it elsewhere.
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

#[cfg(test)]
pub fn read_lines(path:&Path,start:u64,count:u64)->Result<Value>{
    crate::text_source::page(path,&json!({"start_line":start,"max_lines":count}),&CancellationToken::new())
}

pub async fn search_text(
    query: String,
    directories: Vec<PathBuf>,
    pattern: String,
    seconds: u64,
    cancel: CancellationToken,
) -> Result<Value> {
    tokio::task::spawn_blocking(move || {
        use std::io::{BufRead, BufReader, Read};
        anyhow::ensure!(!query.trim().is_empty(), "Укажите текст поиска");
        let started = Instant::now();
        let mut pending: VecDeque<_> = directories.iter().cloned().collect();
        let mut seen = HashSet::new();
        let mut hits = vec![];
        let mut inaccessible = 0usize;
        let mut scanned = 0usize;
        let mut excluded = 0usize;
        let mut entries_count = 0usize;
        let mut complete = true;
        let query = query.to_lowercase();
        'scan: while let Some(dir) = pending.pop_front() {
            if cancel.is_cancelled() { anyhow::bail!("Поиск остановлен"); }
            if started.elapsed().as_secs() >= seconds.clamp(1,300) || entries_count >= 2_500_000 { complete=false; break; }
            let Ok(canonical) = dunce::canonicalize(&dir) else { inaccessible+=1; continue; };
            if !seen.insert(canonical) { continue; }
            let Ok(entries) = std::fs::read_dir(&dir) else { inaccessible+=1; continue; };
            for entry in entries {
                if cancel.is_cancelled() { anyhow::bail!("Поиск остановлен"); }
                if started.elapsed().as_secs() >= seconds.clamp(1,300) || entries_count >= 2_500_000 || hits.len() >= 100 { complete=false; break 'scan; }
                entries_count += 1;
                let Ok(entry) = entry else { inaccessible+=1; continue; };
                let Ok(kind) = entry.file_type() else { inaccessible+=1; continue; };
                if kind.is_symlink() { excluded+=1; continue; }
                if kind.is_dir() {
                    #[cfg(not(windows))]
                    if ["/proc","/dev","/sys","/System/Volumes"].iter().any(|p|entry.path()==Path::new(p)) { excluded+=1; continue; }
                    pending.push_back(entry.path()); continue;
                }
                if !kind.is_file() || !wildcard(&pattern.to_lowercase(), &entry.file_name().to_string_lossy().to_lowercase()) { continue; }
                let Ok(meta) = entry.metadata() else { inaccessible+=1; continue; };
                if meta.len() > 2 * 1024 * 1024 { excluded+=1; continue; }
                let Ok(file) = std::fs::File::open(entry.path()) else { inaccessible+=1; continue; };
                let mut reader = BufReader::new(file.take(2 * 1024 * 1024 + 1));
                let mut number = 0usize;
                let mut line = Vec::new();
                scanned+=1;
                loop {
                    if cancel.is_cancelled() { anyhow::bail!("Поиск остановлен"); }
                    if started.elapsed().as_secs() >= seconds.clamp(1,300) { complete=false; break 'scan; }
                    line.clear();
                    match reader.by_ref().take(128 * 1024).read_until(b'\n', &mut line) {
                        Ok(0) => break,
                        Ok(n) if n >= 128 * 1024 => { excluded+=1; break; }
                        Ok(_) => {}, Err(_) => { inaccessible+=1; break; }
                    }
                    number+=1;
                    let Ok(text) = std::str::from_utf8(&line) else { excluded+=1; break; };
                    if text.contains('\0') { excluded+=1; break; }
                    if text.to_lowercase().contains(&query) {
                        hits.push(json!({"path":entry.path(),"line":number,"text":crate::tools::truncate(text.trim_end(),300)}));
                        if hits.len() >= 100 { complete=false; break 'scan; }
                    }
                }
            }
        }
        Ok(json!({"matches":hits,"complete":complete && inaccessible==0 && excluded==0,"traversal_finished":complete,"roots":directories,"scanned_files":scanned,"inaccessible":inaccessible,"excluded_files_or_links":excluded,"elapsed_seconds":started.elapsed().as_secs(),"note":"Поиск буквального текста без учёта регистра, только UTF-8 до 2 МиБ, строки до 128 КиБ; символьные ссылки пропускаются. complete=false не означает, что текста нет."}))
    }).await?
}
pub async fn readable(
    app: &crate::api::Shared,
    value: &str,
    cancel: &CancellationToken,
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
    if path.components().count() == 1 {
        let result = find(value.into(), None, cancel.clone(), 120).await?;
        let matches = result["matches"].as_array().unwrap();
        if matches.len() == 1 {
            return Ok(PathBuf::from(matches[0].as_str().unwrap()));
        }
        if matches.len() > 1 {
            anyhow::bail!("Найдено несколько файлов. Укажите точный путь: {}", result);
        }
        anyhow::bail!("Файл пока не найден. Результат поиска: {}", result);
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
    async fn pages_and_deep_content_search_are_explicit() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!("search-test-{}", uuid::Uuid::new_v4()));
        let deep = root.join("a/b/c/d/e/f/g");
        std::fs::create_dir_all(&deep).unwrap();
        let file = deep.join("sample.md");
        std::fs::write(&file, "один\nneedle two\nтри\nчетыре\n").unwrap();
        let page = read_lines(&file, 2, 2).unwrap();
        assert_eq!(page["has_more"], true);
        assert_eq!(page["next_start_line"], 4);
        assert!(page["text"].as_str().unwrap().starts_with("needle two"));
        let last = read_lines(&file, 4, 2).unwrap();
        assert_eq!(last["has_more"], false);
        let result = search_text(
            "NEEDLE".into(),
            vec![root.clone()],
            "*.md".into(),
            5,
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(result["matches"].as_array().unwrap().len(), 1);
        assert_eq!(result["matches"][0]["line"], 2);
        let cancel = CancellationToken::new();
        cancel.cancel();
        assert!(
            search_text("needle".into(), vec![root.clone()], "*".into(), 5, cancel)
                .await
                .is_err()
        );
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn finds_beyond_workspace() {
        let root = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        std::fs::create_dir_all(root.join("outside/deep")).unwrap();
        std::fs::write(root.join("outside/deep/makar.txt"), "fixture").unwrap();
        let result = find_in_roots(
            vec![root.clone()],
            "MAKAR.TXT",
            &CancellationToken::new(),
            5,
        )
        .unwrap();
        assert_eq!(result["matches"].as_array().unwrap().len(), 1);
        assert_eq!(result["complete"], true);
        std::fs::remove_dir_all(root).unwrap();
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
