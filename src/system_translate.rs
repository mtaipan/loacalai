//! Selection is captured only after an explicit global shortcut. No keyboard logging.
pub fn source_language(text: &str, requested: &str) -> String {
    if requested != "auto" && !requested.is_empty() {
        return requested.into();
    }
    if let Some(info) = whatlang::detect(text) {
        if info.is_reliable() {
            let code = match info.lang().code() {
                "eng" => "en",
                "rus" => "ru",
                "deu" => "de",
                "fra" => "fr",
                "spa" => "es",
                "ita" => "it",
                "por" => "pt",
                "ukr" => "uk",
                "pol" => "pl",
                "ces" => "cs",
                "slk" => "sk",
                "bul" => "bg",
                "bel" => "be",
                "srp" => "sr",
                "hrv" => "hr",
                "slv" => "sl",
                "nld" => "nl",
                "dan" => "da",
                "swe" => "sv",
                "nob" => "no",
                "fin" => "fi",
                "est" => "et",
                "lav" => "lv",
                "lit" => "lt",
                "ell" => "el",
                "tur" => "tr",
                "ara" => "ar",
                "heb" => "he",
                "hin" => "hi",
                "ben" => "bn",
                "tam" => "ta",
                "vie" => "vi",
                "tha" => "th",
                "ind" => "id",
                "jpn" => "ja",
                "kor" => "ko",
                "cmn" => "zh",
                "ron" => "ro",
                "hun" => "hu",
                "kat" => "ka",
                "hye" => "hy",
                _ => "en",
            };
            return code.into();
        }
    }
    if text.chars().any(|c| ('\u{3040}'..='\u{30ff}').contains(&c)) {
        "ja"
    } else if text.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)) {
        "zh"
    } else if text.chars().any(|c| ('\u{0400}'..='\u{04ff}').contains(&c)) {
        "ru"
    } else {
        "en"
    }
    .into()
}

#[cfg(feature = "desktop")]
trait HotkeyRegistry {
    fn register_key(&mut self, key: global_hotkey::hotkey::HotKey) -> anyhow::Result<()>;
    fn unregister_key(&mut self, key: global_hotkey::hotkey::HotKey) -> anyhow::Result<()>;
}
#[cfg(feature = "desktop")]
impl HotkeyRegistry for global_hotkey::GlobalHotKeyManager {
    fn register_key(&mut self, key: global_hotkey::hotkey::HotKey) -> anyhow::Result<()> {
        self.register(key).map_err(Into::into)
    }
    fn unregister_key(&mut self, key: global_hotkey::hotkey::HotKey) -> anyhow::Result<()> {
        self.unregister(key).map_err(Into::into)
    }
}
#[cfg(feature = "desktop")]
fn replace_hotkeys(
    registry: &mut impl HotkeyRegistry,
    slots: &mut [Option<global_hotkey::hotkey::HotKey>; 2],
    specs: [&str; 2],
) -> anyhow::Result<()> {
    use global_hotkey::hotkey::HotKey;
    let keys: [HotKey; 2] = [
        specs[0].parse().map_err(|e| anyhow::anyhow!("{e}"))?,
        specs[1].parse().map_err(|e| anyhow::anyhow!("{e}"))?,
    ];
    anyhow::ensure!(keys[0].id() != keys[1].id(), "Сочетания должны различаться");
    if *slots == keys.map(Some) {
        return Ok(());
    }
    let old = *slots;
    let result = (|| -> anyhow::Result<()> {
        // Release both before registering either: this permits swapping shortcuts.
        for slot in slots.iter_mut() {
            if let Some(key) = *slot {
                registry.unregister_key(key)?;
                *slot = None;
            }
        }
        for (i, key) in keys.into_iter().enumerate() {
            registry.register_key(key)?;
            slots[i] = Some(key);
        }
        Ok(())
    })();
    if let Err(error) = result {
        let mut rollback = Vec::new();
        for slot in slots.iter_mut() {
            if let Some(key) = *slot {
                if let Err(e) = registry.unregister_key(key) {
                    rollback.push(e.to_string());
                } else {
                    *slot = None;
                }
            }
        }
        for (slot, previous) in slots.iter_mut().zip(old) {
            if slot.is_none() {
                if let Some(key) = previous {
                    match registry.register_key(key) {
                        Ok(()) => *slot = Some(key),
                        Err(e) => rollback.push(e.to_string()),
                    }
                }
            }
        }
        anyhow::bail!(
            "{error}{}",
            if rollback.is_empty() {
                String::new()
            } else {
                format!("; восстановление сочетаний: {}", rollback.join("; "))
            }
        );
    }
    Ok(())
}

#[cfg(feature = "desktop")]
pub struct SelectionAgent {
    manager: global_hotkey::GlobalHotKeyManager,
    hotkey: Option<global_hotkey::hotkey::HotKey>,
    last_spec: String,
    area_hotkey: Option<global_hotkey::hotkey::HotKey>,
    last_area_spec: String,
    checked: std::time::Instant,
    busy: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ready: std::sync::mpsc::Receiver<()>,
    sender: std::sync::mpsc::Sender<()>,
    root: std::path::PathBuf,
}
#[cfg(feature = "desktop")]
impl SelectionAgent {
    pub fn new(root: std::path::PathBuf) -> anyhow::Result<Self> {
        let (sender, ready) = std::sync::mpsc::channel();
        Ok(Self {
            manager: global_hotkey::GlobalHotKeyManager::new()?,
            hotkey: None,
            area_hotkey: None,
            last_area_spec: String::new(),
            last_spec: String::new(),
            checked: std::time::Instant::now() - std::time::Duration::from_secs(5),
            busy: Default::default(),
            ready,
            sender,
            root,
        })
    }
    pub fn poll(
        &mut self,
        runtime: &tokio::runtime::Runtime,
        client: &reqwest::Client,
        url: &str,
        mut before_capture: impl FnMut(bool),
    ) -> bool {
        use global_hotkey::{hotkey::HotKey, GlobalHotKeyEvent, HotKeyState};
        use std::sync::atomic::Ordering;
        if self.checked.elapsed() > std::time::Duration::from_secs(2) {
            self.checked = std::time::Instant::now();
            let settings = std::fs::read(self.root.join("data/settings.json"))
                .ok()
                .and_then(|bytes| serde_json::from_slice::<crate::api::Settings>(&bytes).ok());
            let spec = settings
                .as_ref()
                .map(|s| s.quick_hotkey.clone())
                .unwrap_or_else(|| {
                    if cfg!(target_os = "macos") {
                        "Super+Shift+T"
                    } else {
                        "Control+Shift+T"
                    }
                    .into()
                });
            let area_spec = settings
                .as_ref()
                .map(|s| s.quick_area_hotkey.clone())
                .unwrap_or_else(crate::api::default_area_hotkey);
            if spec != self.last_spec || area_spec != self.last_area_spec {
                let mut slots = [self.hotkey, self.area_hotkey];
                let result = replace_hotkeys(&mut self.manager, &mut slots, [&spec, &area_spec]);
                self.hotkey = slots[0];
                self.area_hotkey = slots[1];
                self.last_spec = spec.clone();
                self.last_area_spec = area_spec.clone();
                let error = result.err().map(|e| e.to_string());
                let ready = |i: usize, requested: &str| {
                    requested.parse::<HotKey>().ok() == slots[i] && slots[i].is_some()
                };
                let status = serde_json::json!({
                    "hotkey":spec,"ready":ready(0, &spec),"error":error,
                    "area_hotkey":area_spec,"area_ready":ready(1, &area_spec),"area_error":error,
                    "active_hotkey":slots[0].map(|h| h.to_string()),
                    "active_area_hotkey":slots[1].map(|h| h.to_string())
                });
                let _ = std::fs::write(
                    self.root.join("data/hotkey-status.json"),
                    serde_json::to_vec(&status).unwrap(),
                );
            }
        }
        while let Ok(event) = GlobalHotKeyEvent::receiver().try_recv() {
            let area = self.area_hotkey.is_some_and(|h| h.id() == event.id);
            if event.state != HotKeyState::Released
                || (!area && !self.hotkey.is_some_and(|h| h.id() == event.id))
            {
                continue;
            }
            if self.busy.swap(true, Ordering::SeqCst) {
                continue;
            }
            before_capture(area);
            let client = client.clone();
            let url = url.to_string();
            let ready = self.sender.clone();
            let busy = self.busy.clone();
            runtime.spawn(async move {
                let payload = if area {
                    serde_json::json!({})
                } else {
                    match tokio::task::spawn_blocking(capture_selection).await {
                        Ok(Ok(text)) => serde_json::json!({"text":text}),
                        Ok(Err(error)) => serde_json::json!({"error":error.to_string()}),
                        Err(error) => serde_json::json!({"error":error.to_string()}),
                    }
                };
                let result = async {
                    let bootstrap = client
                        .get(format!("{url}/api/bootstrap"))
                        .send()
                        .await?
                        .json::<serde_json::Value>()
                        .await?;
                    let response = client
                        .post(format!(
                            "{url}{}",
                            if area {
                                "/api/quick/area"
                            } else {
                                "/api/quick"
                            }
                        ))
                        .header("X-Studio-Token", bootstrap["token"].as_str().unwrap_or(""))
                        .json(&payload)
                        .send()
                        .await?
                        .error_for_status()?
                        .json::<serde_json::Value>()
                        .await?;
                    Ok::<bool, reqwest::Error>(response["cancelled"] != true)
                }
                .await;
                let show = match result {
                    Ok(show) => show,
                    Err(error) => {
                        eprintln!("System translation: {error}");
                        true
                    }
                };
                busy.store(false, Ordering::SeqCst);
                if show {
                    let _ = ready.send(());
                }
            });
        }
        self.ready.try_recv().is_ok()
    }
}

#[cfg(feature = "desktop")]
fn capture_selection() -> anyhow::Result<String> {
    #[cfg(not(windows))]
    use clipboard_rs::common::ContentFormat;
    use clipboard_rs::{Clipboard, ClipboardContext};
    use enigo::{Direction, Enigo, Key, Keyboard, Settings};
    use std::{thread::sleep, time::Duration};
    #[cfg(windows)]
    let source_window = crate::selection_clipboard::foreground();
    // Do not move focus until the copy command has completed in the source application.
    #[cfg(windows)]
    {
        #[link(name = "user32")]
        extern "system" {
            fn GetAsyncKeyState(key: i32) -> i16;
        }
        for _ in 0..100 {
            if [16, 17, 18, 91, 92]
                .iter()
                .all(|key| unsafe { GetAsyncKeyState(*key) } >= 0)
            {
                break;
            }
            sleep(Duration::from_millis(15));
        }
        anyhow::ensure!(
            [16, 17, 18, 91, 92]
                .iter()
                .all(|key| unsafe { GetAsyncKeyState(*key) } >= 0),
            "Отпустите клавиши сочетания и повторите перевод"
        );
    }
    #[cfg(not(windows))]
    sleep(Duration::from_millis(300));
    let clipboard = ClipboardContext::new().map_err(|e| anyhow::anyhow!(e.to_string()))?;
    #[cfg(windows)]
    let backup = crate::selection_clipboard::Snapshot::capture()?;
    #[cfg(not(windows))]
    let backup = clipboard
        .get(&[
            ContentFormat::Text,
            ContentFormat::Rtf,
            ContentFormat::Html,
            ContentFormat::Image,
            ContentFormat::Files,
        ])
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    #[cfg(not(windows))]
    let marker = format!("local-gemma-selection-{}", uuid::Uuid::new_v4());
    #[cfg(not(windows))]
    clipboard
        .set_text(marker.clone())
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    #[cfg(windows)]
    let mut captured_revision = None;
    let captured = (|| -> anyhow::Result<String> {
        let mut keyboard = Enigo::new(&Settings::default()).map_err(|e| {
            anyhow::anyhow!(
                "Нет доступа к выделению: {e}. На macOS разрешите приложению Универсальный доступ."
            )
        })?;
        let modifier = if cfg!(target_os = "macos") {
            Key::Meta
        } else {
            Key::Control
        };
        let copy_key = if cfg!(target_os = "macos") {
            Key::Other(8)
        } else {
            Key::Other(0x43)
        };
        #[cfg(windows)]
        anyhow::ensure!(
            source_window != 0 && crate::selection_clipboard::foreground() == source_window,
            "Фокус окна изменился. Выделите текст в нужном окне и повторите перевод."
        );
        keyboard.key(modifier, Direction::Press)?;
        let copy = keyboard.key(copy_key, Direction::Click);
        let release = keyboard.key(modifier, Direction::Release);
        copy?;
        release?;
        for _ in 0..40 {
            sleep(Duration::from_millis(40));
            #[cfg(windows)]
            let revision = crate::selection_clipboard::revision();
            #[cfg(windows)]
            if revision == backup.revision {
                continue;
            }
            if let Ok(text) = clipboard.get_text() {
                #[cfg(windows)]
                if !text.trim().is_empty() && crate::selection_clipboard::revision() == revision {
                    captured_revision = Some(revision);
                    return Ok(text);
                }
                #[cfg(not(windows))]
                if text != marker && !text.trim().is_empty() {
                    return Ok(text);
                }
            }
        }
        anyhow::bail!("Не удалось скопировать выделение. Выделите обычный текст и повторите. В защищённых или повышенных окнах может потребоваться ручное Ctrl/⌘+C и кнопка «Перевести буфер».")
    })();
    // Avoid overwriting a new clipboard value copied by the user concurrently.
    #[cfg(windows)]
    if let Some(revision) = captured_revision {
        // A restore error is visible; never silently claim the clipboard was preserved.
        backup.restore(revision)?;
    }
    #[cfg(not(windows))]
    {
        let current = clipboard.get_text().unwrap_or_default();
        if current == marker || captured.as_ref().is_ok_and(|text| *text == current) {
            let _ = if backup.is_empty() {
                clipboard.clear()
            } else {
                clipboard.set(backup)
            };
        }
    }
    captured
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "desktop")]
    #[test]
    fn hotkeys_swap_and_rollback_on_conflict() {
        use global_hotkey::hotkey::HotKey;
        #[derive(Default)]
        struct Registry {
            active: std::collections::HashSet<u32>,
            blocked: Option<u32>,
        }
        impl HotkeyRegistry for Registry {
            fn register_key(&mut self, key: HotKey) -> anyhow::Result<()> {
                anyhow::ensure!(
                    self.blocked != Some(key.id()) && self.active.insert(key.id()),
                    "occupied"
                );
                Ok(())
            }
            fn unregister_key(&mut self, key: HotKey) -> anyhow::Result<()> {
                anyhow::ensure!(self.active.remove(&key.id()), "not registered");
                Ok(())
            }
        }
        let mut registry = Registry::default();
        let mut slots = [None, None];
        replace_hotkeys(
            &mut registry,
            &mut slots,
            ["Control+Shift+T", "Control+Shift+O"],
        )
        .unwrap();
        let original = slots;
        replace_hotkeys(
            &mut registry,
            &mut slots,
            ["Control+Shift+O", "Control+Shift+T"],
        )
        .unwrap();
        assert_eq!(slots, [original[1], original[0]]);
        let swapped = slots;
        registry.blocked = Some("Control+Shift+P".parse::<HotKey>().unwrap().id());
        assert!(replace_hotkeys(
            &mut registry,
            &mut slots,
            ["Control+Shift+I", "Control+Shift+P"]
        )
        .is_err());
        assert_eq!(slots, swapped);
        assert_eq!(registry.active.len(), 2);
        assert!(replace_hotkeys(
            &mut registry,
            &mut slots,
            ["Control+Shift+T", "Control+Shift+T"]
        )
        .is_err());
        assert_eq!(slots, swapped);
        assert!(
            replace_hotkeys(&mut registry, &mut slots, ["INVALID", "Control+Shift+T"]).is_err()
        );
        assert_eq!(slots, swapped);
    }
    #[test]
    fn detects_text_languages() {
        assert_eq!(
            source_language("Good morning. The meeting starts at nine.", "auto"),
            "en"
        );
        assert_eq!(
            source_language("Доброе утро, встреча начинается в девять утра.", "auto"),
            "ru"
        );
        assert_eq!(source_language("Hello", "de"), "de");
    }
}
