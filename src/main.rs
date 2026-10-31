#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]
mod agent;
mod api;
mod changes;
mod compute;
mod documents;
mod engine;
mod executions;
mod filesystem;
mod model_adapter;
mod model_storage;
mod models;
mod projects;
#[cfg(all(windows, feature = "desktop"))]
mod selection_clipboard;
mod system_paths;
mod system_translate;
mod task_scope;
mod thinking;
mod tools;
mod web_tools;

use anyhow::Result;
use std::{path::PathBuf, sync::Arc};
fn lifecycle(message: &str) {
    if let Some(path) = std::env::var_os("LOCAL_GEMMA_LIFECYCLE_LOG") {
        use std::io::Write;
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = file.write_all(format!("{} {message}\n", chrono::Utc::now()).as_bytes());
        }
    }
}

fn root_dir() -> PathBuf {
    if let Some(root) = std::env::var_os("LOCAL_GEMMA_HOME") {
        return root.into();
    }
    let exe = std::env::current_exe().unwrap();
    #[cfg(target_os = "macos")]
    for ancestor in exe.ancestors() {
        if ancestor.extension().is_some_and(|s| s == "app") {
            return ancestor.parent().unwrap().into();
        }
    }
    let parent = exe.parent().unwrap();
    if parent
        .file_name()
        .is_some_and(|n| n == "release" || n == "debug")
    {
        return PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    }
    parent.into()
}

fn main() -> Result<()> {
    let root = root_dir();
    // Resolve shortcuts/junctions so Ollama receives the physical storage path.
    let root = dunce::canonicalize(&root).unwrap_or(root);
    std::fs::create_dir_all(root.join("data"))?;
    for name in ["temp", "cache", "home", "webview"] {
        std::fs::create_dir_all(root.join("data").join(name))?;
    }
    for name in ["TEMP", "TMP", "TMPDIR"] {
        std::env::set_var(name, root.join("data/temp"));
    }
    std::env::set_var("HF_HOME", root.join("data/cache/huggingface"));
    std::env::set_var("PIP_CACHE_DIR", root.join("data/cache/pip"));
    std::env::set_var("OLLAMA_HOME", root.join("data/home"));
    let runtime = tokio::runtime::Runtime::new()?;
    let state = Arc::new(api::App::new(root)?);
    let port: u16 = std::env::var("LOCAL_GEMMA_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3210);
    let addr = format!("127.0.0.1:{port}");
    let listener = runtime.block_on(tokio::net::TcpListener::bind(&addr));
    let url = format!("http://{addr}");
    let listener = match listener {
        Ok(listener) => Some(listener),
        Err(error) => {
            let existing = runtime.block_on(async {
                state
                    .client
                    .get(format!("{url}/api/bootstrap"))
                    .send()
                    .await?
                    .json::<serde_json::Value>()
                    .await
            });
            if existing.is_ok_and(|value| value["app"] == "Local Gemma Studio") {
                None
            } else {
                return Err(error.into());
            }
        }
    };
    let owns_server = listener.is_some();
    if let Some(listener) = listener {
        let server_state = state.clone();
        let server_shutdown = state.shutdown.clone();
        runtime.spawn(async move {
            if let Err(error) = axum::serve(listener, api::router(server_state))
                .with_graceful_shutdown(server_shutdown.cancelled_owned())
                .await
            {
                eprintln!("Server: {error}");
            }
        });
        let initialize = state.clone();
        runtime.spawn(async move {
            if let Err(error) = engine::ensure_ollama(&initialize).await {
                eprintln!("Ollama: {error:#}");
            }
        });
    }
    let shutdown = state.shutdown.clone();
    println!("Local Gemma Studio: {url}");
    if std::env::args().any(|arg| arg == "--server") {
        if owns_server {
            runtime.block_on(async {
                tokio::select! {_=tokio::signal::ctrl_c()=>{},_=shutdown.cancelled()=>{}}
            });
        }
        return Ok(());
    }
    if std::env::args().any(|arg| arg == "--browser") {
        webbrowser::open(&url)?;
        runtime.block_on(async {
            tokio::select! {_=tokio::signal::ctrl_c()=>{},_=shutdown.cancelled()=>{}}
        });
        return Ok(());
    }
    #[cfg(feature = "desktop")]
    {
        // WebView2/OS window initialization and teardown may block the UI thread.
        // Explicit shutdown cancels tool workers first; bound only the remaining
        // desktop-process lifetime. The separate Ollama process stays untouched.
        let desktop_shutdown = shutdown.clone();
        let shutdown_runtime = runtime.handle().clone();
        std::thread::spawn(move || {
            shutdown_runtime.block_on(desktop_shutdown.cancelled());
            lifecycle("desktop shutdown deadline started");
            std::thread::sleep(std::time::Duration::from_secs(8));
            lifecycle("desktop shutdown deadline exceeded; exiting process");
            std::process::exit(0);
        });
        use tao::platform::run_return::EventLoopExtRunReturn;
        use tao::{
            event::{Event, WindowEvent},
            event_loop::ControlFlow,
            window::WindowBuilder,
        };
        #[derive(Debug)]
        enum NativeEvent {
            CaptureArea,
            ShowQuick,
            HideQuick,
        }
        let mut event_loop =
            tao::event_loop::EventLoopBuilder::<NativeEvent>::with_user_event().build();
        let quick_proxy = event_loop.create_proxy();
        let shutdown_proxy = event_loop.create_proxy();
        let wake_shutdown = shutdown.clone();
        runtime.spawn(async move {
            wake_shutdown.cancelled().await;
            let _ = shutdown_proxy.send_event(NativeEvent::HideQuick);
        });
        lifecycle("native event loop created");
        let window = WindowBuilder::new()
            .with_title("Local Gemma Studio")
            .with_inner_size(tao::dpi::LogicalSize::new(1360., 900.))
            .with_min_inner_size(tao::dpi::LogicalSize::new(820., 600.))
            .build(&event_loop)?;
        let base_url = url.clone();
        let quick_window = WindowBuilder::new()
            .with_title("Быстрый перевод — Local Gemma")
            .with_inner_size(tao::dpi::LogicalSize::new(740., 560.))
            .with_min_inner_size(tao::dpi::LogicalSize::new(440., 340.))
            .with_always_on_top(true)
            .with_visible(false)
            .build(&event_loop)?;
        let mut quick_context = wry::WebContext::new(Some(state.data.join("webview/quick")));
        let mut main_context = wry::WebContext::new(Some(state.data.join("webview/main")));
        let quick_view = wry::WebViewBuilder::with_web_context(&mut quick_context)
            .with_url(format!("{url}/quick.html"))
            .with_ipc_handler(move |request| {
                let event = match request.body().as_str() {
                    "capture-area" => NativeEvent::CaptureArea,
                    "show-quick" => NativeEvent::ShowQuick,
                    "hide-quick" => NativeEvent::HideQuick,
                    _ => return,
                };
                let _ = quick_proxy.send_event(event);
            })
            .build(&quick_window)?;
        lifecycle("quick webview created");
        let mut selection_agent = match system_translate::SelectionAgent::new(state.root.clone()) {
            Ok(agent) => Some(agent),
            Err(error) => {
                eprintln!("Global shortcuts unavailable: {error}");
                let status = serde_json::json!({"ready":false,"area_ready":false,"error":error.to_string(),"area_error":error.to_string()});
                let _ = std::fs::write(
                    state.data.join("hotkey-status.json"),
                    serde_json::to_vec(&status).unwrap(),
                );
                None
            }
        };
        #[cfg(windows)]
        let mut quick_origin = 0isize;
        let selection_client = state.client.clone();
        let selection_url = url.clone();
        let webview = wry::WebViewBuilder::with_web_context(&mut main_context)
            .with_url(&url)
            .with_navigation_handler(move |target| {
                let local = target.starts_with(&format!("{base_url}/"))
                    || target == base_url
                    || target == "about:blank";
                if !local && (target.starts_with("https://") || target.starts_with("http://")) {
                    let _ = webbrowser::open(&target);
                }
                local
            })
            .with_devtools(cfg!(debug_assertions))
            .build(&window);
        lifecycle("main webview created");
        match webview {
            Ok(view) => {
                let code = event_loop.run_return(|event, _, flow| {
                    let _keep_alive = (&view, &runtime, &quick_view, &main_context, &quick_context);
                    if selection_agent.as_mut().is_some_and(|agent| {
                        agent.poll(&runtime, &selection_client, &selection_url, |area| {
                            #[cfg(windows)]
                            {
                                use tao::platform::windows::WindowExtWindows;
                                let source = selection_clipboard::foreground();
                                if source != quick_window.hwnd() {
                                    quick_origin = source;
                                }
                            }
                            if area {
                                quick_window.set_visible(false);
                            }
                        })
                    }) {
                        quick_window.set_visible(true);
                        quick_window.set_focus();
                        let _ = quick_view.evaluate_script("window.refreshQuick?.()");
                    }
                    *flow = if shutdown.is_cancelled() {
                        lifecycle("native event loop received shutdown");
                        ControlFlow::Exit
                    } else {
                        ControlFlow::WaitUntil(
                            std::time::Instant::now() + std::time::Duration::from_millis(250),
                        )
                    };
                    match &event {
                        Event::UserEvent(NativeEvent::CaptureArea) => {
                            quick_window.set_visible(false);
                            let _ = quick_view.evaluate_script("window.startAreaCapture?.()");
                        }
                        Event::UserEvent(NativeEvent::ShowQuick) => {
                            quick_window.set_visible(true);
                            quick_window.set_focus();
                        }
                        Event::UserEvent(NativeEvent::HideQuick) => {
                            quick_window.set_visible(false);
                            #[cfg(windows)]
                            selection_clipboard::restore_focus(quick_origin);
                        }
                        _ => {}
                    }
                    if let Event::WindowEvent {
                        event: WindowEvent::CloseRequested,
                        window_id,
                        ..
                    } = event
                    {
                        if window_id == quick_window.id() {
                            quick_window.set_visible(false);
                            #[cfg(windows)]
                            selection_clipboard::restore_focus(quick_origin);
                        } else {
                            shutdown.cancel();
                            *flow = ControlFlow::Exit;
                        }
                    }
                });
                lifecycle("native event loop returned");
                drop(view);
                lifecycle("main webview dropped");
                code
            }
            Err(error) => {
                eprintln!("Desktop webview unavailable ({error}); opening browser.");
                webbrowser::open(&url)?;
                runtime.block_on(async {
                    let _ = tokio::signal::ctrl_c().await;
                });
                0
            }
        };
        lifecycle("dropping desktop resources");
    }
    #[cfg(not(feature = "desktop"))]
    {
        webbrowser::open(&url)?;
        runtime.block_on(async {
            let _ = tokio::signal::ctrl_c().await;
        });
    }
    shutdown.cancel();
    lifecycle("shutting down runtime");
    runtime.shutdown_timeout(std::time::Duration::from_secs(3));
    lifecycle("runtime stopped");
    Ok(())
}
