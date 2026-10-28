//! Nugget desktop shell: a Tauri 2 window plus tray icon that talks to `nuggetd` over local IPC.

// No console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod daemon;
mod login;
mod open;
mod settings;
mod tray;
mod updater;

use tauri::{Manager, RunEvent, WindowEvent};
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
        // Driven from `updater.rs` only; the webview has no `updater:` permission.
        .plugin(tauri_plugin_updater::Builder::new().build())
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
            updater::updates_status,
            updater::updates_set_settings,
            updater::updates_check,
            updater::updates_install,
            updater::login_set,
        ])
        .setup(|app| {
            app.manage(updater::Updates::new(app.handle()));
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
            updater::spawn_background_checks(app.handle());
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

    app.run(|handle, event| match event {
        // An update the user opted to auto-install is applied as Nugget quits.
        RunEvent::Exit => handle.state::<updater::Updates>().install_staged(),
        // macOS: clicking the Dock icon brings the hidden window back.
        #[cfg(target_os = "macos")]
        RunEvent::Reopen { .. } => tray::show_main(handle, None),
        _ => {}
    });
}
