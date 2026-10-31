//! Windows selection capture never writes a sentinel to the user's clipboard.
//! Snapshot raw memory formats, then restore only the exact observed revision.
use anyhow::{ensure, Context, Result};
use clipboard_win::{formats, raw, Clipboard};

#[link(name = "user32")]
extern "system" {
    fn GetClipboardSequenceNumber() -> u32;
    fn GetForegroundWindow() -> isize;
    fn IsWindow(hwnd: isize) -> i32;
    fn SetForegroundWindow(hwnd: isize) -> i32;
}
pub fn foreground() -> isize {
    unsafe { GetForegroundWindow() }
}
pub fn restore_focus(hwnd: isize) {
    unsafe {
        if hwnd != 0 && IsWindow(hwnd) != 0 {
            SetForegroundWindow(hwnd);
        }
    }
}
pub fn revision() -> u32 {
    unsafe { GetClipboardSequenceNumber() }
}
pub struct Snapshot {
    formats: Vec<(u32, Vec<u8>)>,
    pub revision: u32,
}
impl Snapshot {
    pub fn capture() -> Result<Self> {
        let _lock = Clipboard::new_attempts(20).context("Windows не дала доступ к буферу. Повторите сочетание или скопируйте текст вручную и нажмите «Перевести буфер»")?;
        let ids: Vec<_> = raw::EnumFormats::new().collect();
        let mut data = Vec::new();
        let mut total = 0usize;
        for id in &ids {
            // Windows synthesizes CF_BITMAP from DIB. Other GDI/owner-display
            // and private handle formats cannot be safely copied as bytes.
            if *id == 2 && (ids.contains(&8) || ids.contains(&17)) {
                continue;
            }
            ensure!(matches!(*id, 1 | 4..=8 | 10..=13 | 15..=17 | 0xc000..=0xffff),
                "В буфере специальный формат {id}; используйте ручное копирование и «Перевести буфер». Буфер не изменён.");
            let bytes: Vec<u8> = clipboard_win::get(formats::RawData(*id)).with_context(|| {
                format!("Не удалось сохранить формат буфера {id}; буфер не изменён")
            })?;
            total = total
                .checked_add(bytes.len())
                .context("Буфер слишком большой")?;
            ensure!(
                total <= 64 * 1024 * 1024,
                "Буфер больше 64 МБ; используйте «Перевести буфер»"
            );
            data.push((*id, bytes));
        }
        Ok(Self {
            formats: data,
            revision: revision(),
        })
    }
    pub fn restore(&self, expected: u32) -> Result<bool> {
        let _lock = Clipboard::new_attempts(20).context("Буфер обмена занят при восстановлении")?;
        // Lock and revision check are in the same critical section: another
        // application cannot copy between the check and EmptyClipboard.
        if revision() != expected {
            return Ok(false);
        }
        raw::empty().context("Не удалось восстановить буфер")?;
        for (id, bytes) in &self.formats {
            raw::set_without_clear(*id, bytes)
                .with_context(|| format!("Не восстановлен формат буфера {id}"))?;
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "uses the real Windows clipboard; run explicitly with no other clipboard tests"]
    fn real_clipboard_roundtrip_and_concurrent_copy() {
        let original = Snapshot::capture().expect("Original clipboard cannot be safely backed up");
        struct Restore(Snapshot);
        impl Drop for Restore {
            fn drop(&mut self) {
                let _ = self.0.restore(revision());
            }
        }
        let _restore = Restore(original);
        let custom = clipboard_win::register_format("LocalGemma.Roundtrip.Test")
            .unwrap()
            .get();
        let html = clipboard_win::register_format("HTML Format").unwrap().get();
        {
            let _lock = Clipboard::new_attempts(20).unwrap();
            raw::empty().unwrap();
            raw::set_string_with("same text", clipboard_win::options::NoClear).unwrap();
            raw::set_without_clear(custom, &[0, 255, 3, 0, 19]).unwrap();
            raw::set_without_clear(html, b"Version:1.0\r\n<html><b>text</b></html>\0").unwrap();
        }
        let saved = Snapshot::capture().unwrap();
        clipboard_win::set_clipboard_string("same text").unwrap();
        assert_ne!(saved.revision, revision());
        assert!(saved.restore(revision()).unwrap());
        let after = Snapshot::capture().unwrap();
        for (id, bytes) in &saved.formats {
            assert_eq!(
                after.formats.iter().find(|(i, _)| i == id).unwrap().1,
                *bytes
            );
        }
        clipboard_win::set_clipboard_string("new selection").unwrap();
        let selected_revision = revision();
        clipboard_win::set_clipboard_string("concurrent user copy").unwrap();
        assert!(!saved.restore(selected_revision).unwrap());
        assert_eq!(
            clipboard_win::get_clipboard_string().unwrap(),
            "concurrent user copy"
        );
    }
}
