//! Nugget desktop shell: a Tauri 2 window plus tray icon that talks to `nuggetd` over local IPC.

// No console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod daemon;
mod open;
mod tray;

#[cfg(target_os = "macos")]
use tauri::RunEvent;
use tauri::{Manager, WindowEvent};
use tracing_subscriber::EnvFilter;

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_env("NUGGET_LOG").unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let app = tauri::Builder::default()
        // Only the Rust API is used (`commands::pick_folders`); the webview is granted none of the
        // plugin's own commands.
        .plugin(tauri_plugin_dialog::init())
        .manage(daemon::Daemon::new())
        .invoke_handler(tauri::generate_handler![
            commands::connection_info,
            commands::start_daemon,
            commands::status,
            commands::providers_list,
            commands::providers_probe,
            commands::chat,
            commands::chat_cancel,
            commands::search,
            commands::models_list,
            commands::models_install,
            commands::egress_recent,
            commands::sources_list,
            commands::sources_add,
            commands::sources_update,
            commands::sources_remove,
            commands::sources_sync,
            commands::sources_auth_begin,
            commands::sources_secret_set,
            commands::ingest_status,
            commands::retrieve_context,
            commands::retrieve_ask,
            commands::runtime_status,
            commands::knowledge_entities,
            commands::knowledge_entity,
            commands::knowledge_graph,
            commands::knowledge_merge,
            commands::knowledge_claim_status,
            commands::knowledge_status,
            commands::mcp_clients_list,
            commands::mcp_clients_create,
            commands::mcp_clients_revoke,
            commands::open_url,
            commands::pick_folders,
        ])
        .setup(|app| {
            if let Err(e) = tray::build(app) {
                // E.g. no status-notifier host on some Linux desktops. The window still works,
                // and closing it quits the app.
                tracing::warn!(error = %e, "tray icon unavailable");
            }
            // Connect early so the tray reflects the daemon state before the window asks.
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let daemon = handle.state::<daemon::Daemon>();
                let _ = daemon.connection_info(&handle).await;
            });
            Ok(())
        })
        .on_window_event(|window, event| {
            // With a tray, closing the window hides it; Quit lives in the tray menu.
            if let WindowEvent::CloseRequested { api, .. } = event
                && window.label() == "main"
                && tray::has_tray(window.app_handle())
            {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .build(tauri::generate_context!())
        .expect("failed to build the Nugget app");

    app.run(|_handle, _event| {
        // macOS: clicking the Dock icon brings the hidden window back.
        #[cfg(target_os = "macos")]
        if let RunEvent::Reopen { .. } = _event {
            tray::show_main(_handle, None);
        }
    });
}
