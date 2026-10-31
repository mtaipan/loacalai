use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

pub fn home() -> Option<PathBuf> {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from)
}
#[cfg(windows)]
fn known_folder(id: u128) -> Option<PathBuf> {
    #[repr(C)]
    struct Guid {
        a: u32,
        b: u16,
        c: u16,
        d: [u8; 8],
    }
    #[link(name = "shell32")]
    extern "system" {
        fn SHGetKnownFolderPath(
            id: *const Guid,
            flags: u32,
            token: isize,
            out: *mut *mut u16,
        ) -> i32;
    }
    #[link(name = "ole32")]
    extern "system" {
        fn CoTaskMemFree(ptr: *const std::ffi::c_void);
    }
    let guid = Guid {
        a: (id >> 96) as u32,
        b: (id >> 80) as u16,
        c: (id >> 64) as u16,
        d: (id as u64).to_be_bytes(),
    };
    let mut out = std::ptr::null_mut();
    // Current redirected path, never KF_FLAG_DEFAULT_PATH or a guessed drive.
    let status = unsafe { SHGetKnownFolderPath(&guid, 0, 0, &mut out) };
    if out.is_null() {
        return None;
    }
    let value = if status >= 0 {
        let mut len = 0;
        unsafe {
            while *out.add(len) != 0 && len < 32768 {
                len += 1;
            }
        }
        Some(PathBuf::from(String::from_utf16_lossy(unsafe {
            std::slice::from_raw_parts(out, len)
        })))
    } else {
        None
    };
    unsafe {
        CoTaskMemFree(out.cast());
    }
    value
}
pub fn folder(name: &str) -> Option<PathBuf> {
    #[cfg(windows)]
    return known_folder(match name {
        "desktop" => 0xB4BFCC3A_DB2C_424C_B029_7FE99A87C641,
        "documents" => 0xFDD39AD0_238F_46AF_ADB4_6C85480369C7,
        "downloads" => 0x374DE290_123F_4565_9164_39C4925E467B,
        _ => return None,
    });
    #[cfg(not(windows))]
    home().map(|h| {
        h.join(match name {
            "desktop" => "Desktop",
            "documents" => "Documents",
            "downloads" => "Downloads",
            _ => name,
        })
    })
}
pub fn describe() -> Value {
    json!({"home":home(),"desktop":folder("desktop"),"documents":folder("documents"),"downloads":folder("downloads"),"source":if cfg!(windows){"Windows Known Folders (current redirected paths)"}else{"User home directories"},"note":"Use the exact OS paths returned here. Never infer Desktop from the application workspace or a drive letter. null means unresolved: ask instead of guessing."})
}
pub fn expand(value: &str) -> Result<PathBuf> {
    let normalized = value.replace('\\', "/");
    let (first, rest) = normalized.split_once('/').unwrap_or((&normalized, ""));
    let alias = match first.to_lowercase().as_str() {
        "@desktop" | "desktop" | "рабочий стол" => Some("desktop"),
        "@documents" | "documents" | "документы" => Some("documents"),
        "@downloads" | "downloads" | "загрузки" => Some("downloads"),
        _ => None,
    };
    if let Some(name) = alias {
        let base = folder(name).with_context(|| {
            format!("Не удалось определить системную папку {name}. Укажите точный путь.")
        })?;
        anyhow::ensure!(
            !Path::new(rest).is_absolute()
                && !Path::new(rest).components().any(|p| matches!(
                    p,
                    std::path::Component::ParentDir | std::path::Component::Prefix(_)
                )),
            "Недопустимый путь после имени системной папки"
        );
        return Ok(base.join(rest));
    }
    if first == "~" {
        return Ok(home().context("Домашняя папка не определена")?.join(rest));
    }
    Ok(PathBuf::from(value))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn desktop_alias_is_os_folder_not_workspace() {
        let desktop = folder("desktop").expect("OS desktop available");
        assert_eq!(
            expand("@desktop/example.txt").unwrap(),
            desktop.join("example.txt")
        );
        assert_eq!(
            expand("Рабочий стол\\example.txt").unwrap(),
            desktop.join("example.txt")
        );
        assert!(expand("@desktop/../escape.txt").is_err());
        assert_eq!(
            expand("./Desktop/example.txt").unwrap(),
            PathBuf::from("./Desktop/example.txt")
        );
    }
}
