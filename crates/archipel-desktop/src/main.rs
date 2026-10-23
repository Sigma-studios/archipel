//! archipel's desktop app: the same UI as the website, in a window, with archipel
//! running inside the app instead of on a server.
//!
//! The window loads `archipel://localhost/`, a URL scheme only this app answers:
//! every request made there goes to archipel's API in this process. No port is
//! opened for the UI, so there is no login. Servers run while the app does:
//! closing it stops them, once confirmed.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use archipel::desktop::{self, Desktop};
use tauri::{AppHandle, Manager, RunEvent, WebviewUrl, WebviewWindowBuilder, WindowEvent};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};

/// archipel, or why it could not start (shown in the window).
enum Inside {
    Running(Desktop),
    Failed(String),
}

type Shared = Arc<tokio::sync::OnceCell<Inside>>;

static QUITTING: AtomicBool = AtomicBool::new(false);

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "archipel=info,archipel_desktop=info".into()),
        )
        .init();
    let inside: Shared = Default::default();
    let app = tauri::Builder::default()
        // A second launch shows the window already open: one archipel per computer.
        .plugin(tauri_plugin_single_instance::init(|app, _, _| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.unminimize();
                let _ = window.show();
                let _ = window.set_focus();
            }
        }))
        .plugin(tauri_plugin_dialog::init())
        .register_asynchronous_uri_scheme_protocol("archipel", {
            let inside = inside.clone();
            move |_, request, responder| {
                let inside = inside.clone();
                tauri::async_runtime::spawn(async move {
                    let response = match inside.get() {
                        Some(Inside::Running(archipel)) => archipel.handle(request).await,
                        Some(Inside::Failed(why)) => failed_page(why),
                        None => failed_page("archipel is still starting"),
                    };
                    responder.respond(response);
                });
            }
        })
        .setup({
            let inside = inside.clone();
            move |app| {
                let started = tauri::async_runtime::block_on(async {
                    Desktop::start(desktop::data_dir()?).await
                });
                let _ = inside.set(match started {
                    Ok(archipel) => Inside::Running(archipel),
                    Err(e) => {
                        tracing::error!("archipel did not start: {e:#}");
                        Inside::Failed(format!("{e:#}"))
                    }
                });
                WebviewWindowBuilder::new(
                    app,
                    "main",
                    WebviewUrl::CustomProtocol("archipel://localhost/".parse()?),
                )
                .title("archipel")
                .inner_size(1100.0, 760.0)
                .min_inner_size(420.0, 500.0)
                .build()?;
                let handle = app.handle().clone();
                let inside = inside.clone();
                tauri::async_runtime::spawn(async move {
                    signalled().await;
                    quit(&handle, &inside);
                });
                Ok(())
            }
        })
        .on_window_event({
            let inside = inside.clone();
            move |window, event| {
                if let WindowEvent::CloseRequested { api, .. } = event {
                    // Servers stop first: the app ends once they have.
                    api.prevent_close();
                    ask_to_quit(window.app_handle(), &inside);
                }
            }
        })
        .build(tauri::generate_context!())
        .expect("the window");
    app.run(|_, event| {
        if let RunEvent::ExitRequested { api, code, .. } = event
            && code.is_none()
            && !QUITTING.load(Ordering::Relaxed)
        {
            api.prevent_exit();
        }
    });
}

/// Closing the window: when servers run, asks first, as closing stops them.
fn ask_to_quit(app: &AppHandle, inside: &Shared) {
    if QUITTING.load(Ordering::Relaxed) {
        return;
    }
    let running = match inside.get() {
        Some(Inside::Running(archipel)) => archipel.running(),
        _ => 0,
    };
    if running == 0 {
        quit(app, inside);
        return;
    }
    let (app, inside) = (app.clone(), inside.clone());
    let servers = if running == 1 {
        "your server".to_string()
    } else {
        format!("your {running} servers")
    };
    app.dialog()
        .message(format!(
            "Closing archipel stops {servers}: players will be disconnected. Worlds are \
             saved first."
        ))
        .title("Stop the servers and quit?")
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::OkCancelCustom(
            "Stop and quit".into(),
            "Keep running".into(),
        ))
        .show({
            let app = app.clone();
            move |stop| {
                if stop {
                    quit(&app, &inside);
                }
            }
        });
}

/// Stops every server cleanly (Minecraft saves its world), then ends the app.
fn quit(app: &AppHandle, inside: &Shared) {
    if QUITTING.swap(true, Ordering::Relaxed) {
        return;
    }
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.set_title("archipel: stopping servers…");
    }
    let (app, inside) = (app.clone(), inside.clone());
    tauri::async_runtime::spawn(async move {
        if let Some(Inside::Running(archipel)) = inside.get() {
            archipel.shutdown().await;
        }
        app.exit(0);
    });
}

/// The session ending (logging out, shutting down): SIGTERM, SIGHUP or SIGINT.
async fn signalled() {
    use tokio::signal::unix::{SignalKind, signal};
    let (Ok(mut term), Ok(mut hup), Ok(mut int)) = (
        signal(SignalKind::terminate()),
        signal(SignalKind::hangup()),
        signal(SignalKind::interrupt()),
    ) else {
        return std::future::pending().await;
    };
    tokio::select! {
        _ = term.recv() => {}
        _ = hup.recv() => {}
        _ = int.recv() => {}
    }
}

fn failed_page(why: &str) -> tauri::http::Response<Vec<u8>> {
    let escaped = why
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    let html = format!(
        "<!doctype html><meta charset=utf-8><title>archipel</title>\
         <body style=\"font-family:system-ui;margin:3em;max-width:40em\">\
         <h1>archipel could not start</h1><p>{escaped}</p>\
         <p>Close this window and open archipel again once it is fixed.</p>"
    );
    tauri::http::Response::builder()
        .status(500)
        .header("content-type", "text/html; charset=utf-8")
        .body(html.into_bytes())
        .expect("a response")
}
