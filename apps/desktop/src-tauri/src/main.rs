//! Heed desktop shell: a Tauri 2 window plus tray icon that talks to `nuggetd` over local IPC.

// No console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod agent;
mod autostart;
mod cloud;
mod commands;
mod daemon;
mod deeplink;
mod doubletap;
mod history;
mod lifecycle;
mod login;
mod logs;
mod notify;
mod open;
mod platform;
mod quickask;
mod settings;
mod tray;
mod updater;
mod window_state;

use tauri::menu::{Menu, MenuItem, MenuItemKind};
use tauri::{AppHandle, Manager, RunEvent, Runtime, WindowEvent};

/// Passed by a compositor key binding: start a Double Tap draft in the running Heed.
const DOUBLE_TAP_ARG: &str = "--double-tap";

/// Whether a second launch's arguments (program name first) ask for a Double Tap draft.
fn wants_double_tap(argv: &[String]) -> bool {
    argv.iter().skip(1).any(|a| a == DOUBLE_TAP_ARG)
}

/// Whether a launch's arguments ask for the quick-ask box (a compositor key binding where no
/// global shortcut can be registered, e.g. Wayland).
fn wants_quick_ask(argv: &[String]) -> bool {
    argv.iter().skip(1).any(|a| a == quickask::QUICK_ASK_ARG)
}

/// The app menu's own Quit item (macOS), see [`app_menu`].
const APP_QUIT_ID: &str = "app-quit";

/// macOS: the default app menu's Quit (and ⌘Q) sends `terminate:`, which ends the app without
/// `RunEvent::ExitRequested`, so the quit question could never be asked. It is replaced by an
/// item that quits like the tray's Quit Heed (`app.exit`). Not used elsewhere: Windows and
/// Linux windows have no menu bar.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn app_menu<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<Menu<R>> {
    let menu = Menu::default(app)?;
    if let Some(MenuItemKind::Submenu(first)) = menu.items()?.into_iter().next() {
        // The app submenu ends with the predefined Quit.
        let n = first.items()?.len();
        if n > 0 {
            first.remove_at(n - 1)?;
        }
        let name = app.package_info().name.clone();
        let quit = MenuItem::with_id(
            app,
            APP_QUIT_ID,
            format!("Quit {name}"),
            true,
            Some("CmdOrCtrl+Q"),
        )?;
        first.append(&quit)?;
    }
    Ok(menu)
}

fn main() {
    // Kept until the process ends, so the last lines reach the log file.
    let _log_guard = logs::init();

    let builder = tauri::Builder::default();
    #[cfg(target_os = "macos")]
    let builder = builder.menu(app_menu).on_menu_event(|app, e| {
        if e.id().as_ref() == APP_QUIT_ID {
            app.exit(0);
        }
    });
    let app = builder
        // First, so a second launch exits before anything else starts; it focuses this window
        // instead (an autostart launch while Nugget already runs changes nothing).
        // `--double-tap` starts a draft instead, for desktops without a GlobalShortcuts portal
        // (sway: `bindsym <keys> exec nugget-desktop --double-tap`); Nugget must already be
        // running, since a first launch with it just opens normally.
        // A second launch may also carry a `nugget://` link or a `.nuggetplugin` file (Windows,
        // Linux), or `--quick-ask`.
        .plugin(tauri_plugin_single_instance::init(|app, argv, cwd| {
            let links = deeplink::from_args_in(&argv, &cwd);
            if wants_double_tap(&argv) {
                tauri::async_runtime::spawn(doubletap::flow::fire(app.clone()));
            } else if wants_quick_ask(&argv) {
                quickask::toggle(app);
            } else if !links.is_empty() {
                deeplink::dispatch(app, links);
            } else if !argv.iter().any(|a| a == autostart::HIDDEN_ARG) {
                tray::show_main(app, None);
            }
        }))
        // macOS: links and opened files arrive as Apple events (`deeplink::setup`). The webview
        // has no `deep-link:` permission.
        .plugin(tauri_plugin_deep_link::init())
        // Only the Rust API is used (`commands::pick_folders`); the webview is granted none of the
        // plugin's own commands.
        .plugin(tauri_plugin_dialog::init())
        // Driven from `updater.rs` only; the webview has no `updater:` permission.
        .plugin(tauri_plugin_updater::Builder::new().build())
        // Driven from `commands::notify` only; the webview has no `notification:` permission.
        .plugin(tauri_plugin_notification::init())
        .manage(daemon::Daemon::new())
        .manage(lifecycle::Lifecycle::default())
        .manage(notify::PendingOpen::default())
        // Nugget Cloud (or why it is off in this build); tokens stay in Rust (`cloud.rs`).
        .manage(cloud::Cloud::from_env())
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
            commands::models_cancel,
            commands::egress_recent,
            commands::egress_page,
            commands::search_page,
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
            commands::runtime_retry,
            commands::ai_key_test,
            commands::ai_key_save,
            commands::ai_key_update,
            commands::ai_key_remove,
            commands::ai_key_saved,
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
            commands::plugins_catalog,
            commands::pick_plugin_file,
            commands::backup_status,
            commands::backup_configure,
            commands::backup_set_passphrase,
            commands::backup_run,
            commands::backup_list,
            commands::backup_restore,
            commands::export_run,
            commands::search_faceted,
            commands::search_timeline,
            commands::search_preview,
            commands::search_upgrade_status,
            commands::search_upgrade_start,
            commands::search_upgrade_dismiss,
            history::chat_history_load,
            history::chat_history_save,
            history::chat_history_delete,
            history::chat_history_clear,
            updater::updates_status,
            updater::updates_set_settings,
            updater::updates_check,
            updater::updates_install,
            updater::login_set,
            commands::agent_run,
            commands::agent_cancel,
            commands::agent_confirm,
            commands::agent_answer,
            commands::agent_list,
            commands::agent_get,
            commands::policy_get,
            commands::policy_set,
            commands::policy_approvals,
            commands::policy_kill,
            commands::policy_resume,
            commands::policy_log_export,
            commands::browser_status,
            autostart::autostart_get,
            autostart::autostart_set,
            doubletap::commands::doubletap_settings_get,
            doubletap::commands::doubletap_settings_set,
            doubletap::commands::doubletap_status,
            doubletap::commands::doubletap_request_permissions,
            doubletap::commands::doubletap_open_privacy_settings,
            doubletap::commands::doubletap_default_excluded,
            doubletap::commands::doubletap_fire,
            doubletap::commands::voice_status,
            doubletap::commands::overlay_state,
            doubletap::commands::overlay_accept,
            doubletap::commands::overlay_cancel,
            doubletap::commands::overlay_retry,
            doubletap::commands::overlay_refine,
            doubletap::commands::overlay_resize,
            doubletap::commands::overlay_insert_previous,
            doubletap::commands::doubletap_history,
            doubletap::commands::doubletap_history_clear,
            doubletap::commands::doubletap_history_remove,
            doubletap::commands::doubletap_practice,
            quickask::quickask_settings_get,
            quickask::quickask_settings_set,
            quickask::quickask_hide,
            quickask::quickask_open_main,
            platform::platform_settings_get,
            platform::platform_settings_set,
            platform::shell_labels_set,
            lifecycle::quit_answer,
            lifecycle::quit_ack,
            lifecycle::quit_cancel,
            lifecycle::daemon_version,
            lifecycle::daemon_version_recheck,
            lifecycle::shell_info,
            lifecycle::indexing_pause_toggle,
            deeplink::deeplink_take,
            cloud::cloud_session,
            cloud::cloud_sign_in,
            cloud::cloud_sign_out,
            cloud::cloud_connections_list,
            cloud::cloud_connect,
            cloud::cloud_disconnect,
            cloud::cloud_sync,
            cloud::cloud_ask,
            quickask::quickask_prefill_take,
            quickask::quickask_handover_take,
        ])
        .setup(|app| {
            app.manage(updater::Updates::new(app.handle()));
            // Before the tray, which reads its labels and paused sources.
            platform::setup(app);
            tray::init(app);
            // The window starts hidden (tauri.conf.json) so it can be placed before it shows,
            // and stays hidden for an autostart launch, unless there is no tray to reopen it.
            let window_state = window_state::init(app);
            if let Some(main) = app.get_webview_window("main") {
                window_state.restore(&main);
                let argv = deeplink::launch_args();
                let hidden = autostart::launched_hidden() || wants_quick_ask(&argv);
                if !(hidden && tray::has_tray(app.handle())) {
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
            quickask::setup(app);
            if wants_quick_ask(&deeplink::launch_args()) {
                quickask::toggle(app.handle());
            }
            deeplink::setup(app);
            notify::follow_agent_events(app.handle());
            Ok(())
        })
        .on_window_event(|window, event| {
            doubletap::on_window_event(window, event);
            quickask::on_window_event(window, event);
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
                    api.prevent_close();
                    if tray::has_tray(window.app_handle()) {
                        let _ = window.hide();
                    } else {
                        // quit the way Quit Nugget does (`lifecycle`), which may first ask
                        // in this window what to do with the background service; the window stays
                        // open for that. Hidden helper windows (the Double Tap overlay) would keep
                        // the app alive, so this exits rather than closing the window.
                        window.app_handle().exit(0);
                    }
                }
                _ => {}
            }
        })
        .build(tauri::generate_context!())
        .expect("failed to build the Heed app");

    app.run(|handle, event| match event {
        // Every way to quit (tray, app menu, closing the window without a tray) arrives here
        // first: the first quit that would stop the background service asks what to do.
        RunEvent::ExitRequested { code, api, .. } => {
            lifecycle::on_exit_requested(handle, code, &api);
        }
        // An update the user opted to auto-install is applied as Nugget quits, and the
        // background service the app started is stopped unless the user chose to keep it.
        RunEvent::Exit => {
            if let Some(state) = handle.try_state::<window_state::WindowState>() {
                state.save();
            }
            lifecycle::on_exit(handle);
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
        assert!(wants_quick_ask(&argv(&["nugget-desktop", "--quick-ask"])));
        assert!(!wants_quick_ask(&argv(&["--quick-ask"])));
    }
}
