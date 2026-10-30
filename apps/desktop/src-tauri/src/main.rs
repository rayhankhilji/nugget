//! Nugget desktop shell: a Tauri 2 window plus tray icon that talks to `nuggetd` over local IPC.

// No console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod agent;
mod autostart;
mod commands;
mod daemon;
mod doubletap;
mod history;
mod login;
mod notify;
mod open;
mod settings;
mod tray;
mod updater;
mod window_state;

use tauri::{Manager, RunEvent, WindowEvent};
use tracing_subscriber::EnvFilter;

/// Passed by a compositor key binding: start a Double Tap draft in the running Nugget.
const DOUBLE_TAP_ARG: &str = "--double-tap";

/// Whether a second launch's arguments (program name first) ask for a Double Tap draft.
fn wants_double_tap(argv: &[String]) -> bool {
    argv.iter().skip(1).any(|a| a == DOUBLE_TAP_ARG)
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_env("NUGGET_LOG").unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let app = tauri::Builder::default()
        // First, so a second launch exits before anything else starts; it focuses this window
        // instead (an autostart launch while Nugget already runs changes nothing).
        // `--double-tap` starts a draft instead, for desktops without a GlobalShortcuts portal
        // (sway: `bindsym <keys> exec nugget-desktop --double-tap`); Nugget must already be
        // running, since a first launch with it just opens normally.
        .plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            if wants_double_tap(&argv) {
                tauri::async_runtime::spawn(doubletap::flow::fire(app.clone()));
            } else if !argv.iter().any(|a| a == autostart::HIDDEN_ARG) {
                tray::show_main(app, None);
            }
        }))
        // Only the Rust API is used (`commands::pick_folders`); the webview is granted none of the
        // plugin's own commands.
        .plugin(tauri_plugin_dialog::init())
        // Driven from `updater.rs` only; the webview has no `updater:` permission.
        .plugin(tauri_plugin_updater::Builder::new().build())
        // Driven from `commands::notify` only; the webview has no `notification:` permission.
        .plugin(tauri_plugin_notification::init())
        .manage(daemon::Daemon::new())
        .manage(notify::PendingToday::default())
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
            commands::ai_key_test,
            commands::ai_key_save,
            commands::models_fit,
            commands::knowledge_entities,
            commands::knowledge_entity,
            commands::knowledge_graph,
            commands::knowledge_merge,
            commands::knowledge_claim_status,
            commands::knowledge_split,
            commands::knowledge_status,
            commands::mcp_clients_list,
            commands::mcp_clients_create,
            commands::mcp_clients_revoke,
            commands::open_url,
            commands::pick_folders,
            commands::cards_list,
            commands::cards_feedback,
            commands::cards_preview,
            commands::cards_execute,
            commands::brief_get,
            commands::proactive_metrics,
            commands::proactive_refresh,
            commands::proactive_learning,
            commands::proactive_reset_learning,
            commands::reminders_list,
            commands::notify,
            commands::open_mailto,
            commands::plugins_list,
            commands::plugins_inspect,
            commands::plugins_install,
            commands::plugins_approve,
            commands::plugins_remove,
            commands::plugins_enable,
            commands::pick_plugin_file,
            history::chat_history_load,
            history::chat_history_save,
            history::chat_history_delete,
            history::chat_history_clear,
            updater::updates_status,
            updater::updates_set_settings,
            updater::updates_check,
            updater::updates_install,
            updater::login_set,
            agent::commands::agent_run,
            agent::commands::agent_cancel,
            agent::commands::agent_confirm,
            agent::commands::agent_answer,
            agent::commands::agent_list,
            agent::commands::agent_get,
            agent::commands::policy_get,
            agent::commands::policy_set,
            agent::commands::policy_approvals,
            agent::commands::policy_kill,
            agent::commands::policy_resume,
            agent::commands::policy_log_export,
            agent::commands::browser_status,
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
            agent::setup(app);
            // Connect early so the tray reflects the daemon state before the window asks, starting
            // the bundled nuggetd if none is answering (the Offline screen offers a retry).
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let daemon = handle.state::<daemon::Daemon>();
                if let Err(e) = daemon.start(&handle).await {
                    tracing::warn!(error = %e, "couldn't start nuggetd");
                }
                let _ = daemon.connection_info(&handle).await;
            });
            updater::spawn_background_checks(app.handle());
            doubletap::setup(app);
            Ok(())
        })
        .on_window_event(|window, event| {
            doubletap::on_window_event(window, event);
            notify::on_window_event(window, event);
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
                    } else {
                        // Hidden helper windows (the Double Tap overlay) would keep the app alive.
                        window.app_handle().exit(0);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn double_tap_argument() {
        let argv = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(wants_double_tap(&argv(&["nugget-desktop", "--double-tap"])));
        assert!(!wants_double_tap(&argv(&["nugget-desktop", "--minimized"])));
        assert!(!wants_double_tap(&argv(&["--double-tap"])));
        assert!(!wants_double_tap(&[]));
    }
}
