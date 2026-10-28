//! Nugget desktop shell: a Tauri 2 window plus tray icon that talks to `nuggetd` over local IPC.

// No console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod autostart;
mod commands;
mod daemon;
mod doubletap;
mod history;
mod login;
mod open;
mod settings;
mod tray;
mod updater;
mod window_state;

use tauri::{Manager, RunEvent, WindowEvent};
use tracing_subscriber::EnvFilter;

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_env("NUGGET_LOG").unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let app = tauri::Builder::default()
        // First, so a second launch exits before anything else starts; it focuses this window
        // instead (an autostart launch while Nugget already runs changes nothing).
        .plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            if !argv.iter().any(|a| a == autostart::HIDDEN_ARG) {
                tray::show_main(app, None);
            }
        }))
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
            history::chat_history_load,
            history::chat_history_save,
            history::chat_history_delete,
            history::chat_history_clear,
            updater::updates_status,
            updater::updates_set_settings,
            updater::updates_check,
            updater::updates_install,
            updater::login_set,
            autostart::autostart_get,
            autostart::autostart_set,
            doubletap::commands::doubletap_settings_get,
            doubletap::commands::doubletap_settings_set,
            doubletap::commands::doubletap_status,
            doubletap::commands::doubletap_request_permissions,
            doubletap::commands::doubletap_default_excluded,
            doubletap::commands::doubletap_fire,
            doubletap::commands::voice_status,
            doubletap::commands::overlay_state,
            doubletap::commands::overlay_accept,
            doubletap::commands::overlay_cancel,
        ])
        .setup(|app| {
            app.manage(updater::Updates::new(app.handle()));
            tray::init(app);
            // The window starts hidden (tauri.conf.json) so it can be placed before it shows,
            // and stays hidden for an autostart launch, unless there is no tray to reopen it.
            let window_state = window_state::init(app);
            if let Some(main) = app.get_webview_window("main") {
                window_state.restore(&main);
                if !(autostart::launched_hidden() && tray::has_tray(app.handle())) {
                    let _ = main.show();
                    let _ = main.set_focus();
                }
            }
            app.manage(window_state);
            autostart::reconcile(app.handle());
            // Connect early so the tray reflects the daemon state before the window asks.
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let daemon = handle.state::<daemon::Daemon>();
                let _ = daemon.connection_info(&handle).await;
            });
            updater::spawn_background_checks(app.handle());
            doubletap::setup(app);
            Ok(())
        })
        .on_window_event(|window, event| {
            doubletap::on_window_event(window, event);
            // Events can arrive before `setup` has managed the state.
            let Some(state) = window.try_state::<window_state::WindowState>() else {
                return;
            };
            if window.label() != "main" {
                return;
            }
            match event {
                WindowEvent::Moved(_) | WindowEvent::Resized(_) => state.capture(window),
                WindowEvent::CloseRequested { api, .. } => {
                    state.capture(window);
                    state.save();
                    // With a tray, closing the window hides it; Quit lives in the tray menu.
                    if tray::has_tray(window.app_handle()) {
                        api.prevent_close();
                        let _ = window.hide();
                    }
                }
                _ => {}
            }
        })
        .build(tauri::generate_context!())
        .expect("failed to build the Nugget app");

    app.run(|handle, event| match event {
        // An update the user opted to auto-install is applied as Nugget quits.
        RunEvent::Exit => {
            if let Some(state) = handle.try_state::<window_state::WindowState>() {
                state.save();
            }
            handle.state::<updater::Updates>().install_staged();
        }
        // macOS: clicking the Dock icon brings the hidden window back.
        #[cfg(target_os = "macos")]
        RunEvent::Reopen { .. } => tray::show_main(handle, None),
        _ => {}
    });
}
