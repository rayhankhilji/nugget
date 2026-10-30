//! Tauri commands for the Double Tap settings page and the overlay window.

use nugget_core::ipc::{VoiceStatus, method};
use nugget_hotkey::Platform;
use tauri::{AppHandle, Emitter, Manager, State, WebviewWindow};

use super::overlay::{Event, OverlayView};
use super::permissions::{self, DoubleTapStatus, PermissionState};
use super::settings::{self, DoubleTapSettings, TriggerSpec};
use super::{DoubleTap, OVERLAY_LABEL, SETTINGS_EVENT, flow, lock, readahead};
use crate::daemon::{CmdError, Daemon};

fn only_overlay(window: &WebviewWindow) -> Result<(), CmdError> {
    if window.label() == OVERLAY_LABEL {
        Ok(())
    } else {
        Err(CmdError::Other(
            "only the Double Tap overlay can do this".into(),
        ))
    }
}

fn not_overlay(window: &WebviewWindow) -> Result<(), CmdError> {
    if window.label() == OVERLAY_LABEL {
        Err(CmdError::Other("not available in the overlay".into()))
    } else {
        Ok(())
    }
}

#[tauri::command]
pub fn doubletap_settings_get(state: State<'_, DoubleTap>) -> DoubleTapSettings {
    lock(&state.settings).clone()
}

/// Validate, save, restart the listener with the new trigger and the read-ahead loop, and tell
/// every window (the settings page and onboarding both write here; this file is the one source
/// of truth).
#[tauri::command]
pub async fn doubletap_settings_set(
    app: AppHandle,
    window: WebviewWindow,
    settings: DoubleTapSettings,
) -> Result<DoubleTapSettings, CmdError> {
    not_overlay(&window)?;
    let settings = settings.validated().map_err(CmdError::Other)?;
    let state = app.state::<DoubleTap>();
    if let Some(path) = &state.settings_path {
        settings::save(path, &settings)
            .map_err(|e| CmdError::Other(format!("cannot save the Double Tap settings: {e}")))?;
    }
    *lock(&state.settings) = settings.clone();
    flow::restart_listener_async(&app).await;
    readahead::restart(&app);
    if let Err(e) = app.emit(SETTINGS_EVENT, &settings) {
        tracing::debug!(error = %e, "cannot announce the Double Tap settings");
    }
    Ok(settings)
}

async fn status(app: &AppHandle) -> DoubleTapStatus {
    let state = app.state::<DoubleTap>();
    let a11y = state.a11y.clone();
    let accessibility: PermissionState =
        tauri::async_runtime::spawn_blocking(move || a11y.api().permission())
            .await
            .map(Into::into)
            .unwrap_or(PermissionState::Unknown);
    let trigger = lock(&state.settings).trigger.clone();
    let platform = Platform::current();
    let session = nugget_hotkey::session();
    let trigger_error = lock(&state.trigger_error).clone();
    DoubleTapStatus {
        platform: permissions::platform_name(platform),
        session: permissions::session_name(session),
        listening: lock(&state.listener).is_some(),
        trigger_error: trigger_error.clone(),
        accessibility,
        input_monitoring: permissions::input_monitoring(platform),
        hint: permissions::hint_for(
            platform,
            session,
            accessibility,
            &trigger,
            trigger_error.as_deref(),
        ),
        modifiers: permissions::modifiers(platform),
        recommended_trigger: TriggerSpec::recommended(),
        shortcut: lock(&state.listener)
            .as_ref()
            .map(|l| permissions::shortcut_status(l.state(), &trigger)),
    }
}

#[tauri::command]
pub async fn doubletap_status(app: AppHandle) -> Result<DoubleTapStatus, CmdError> {
    Ok(status(&app).await)
}

/// macOS: show the Accessibility prompt. Linux and Windows have nothing to prompt; the status
/// carries the hint.
#[tauri::command]
pub async fn doubletap_request_permissions(
    app: AppHandle,
    window: WebviewWindow,
) -> Result<DoubleTapStatus, CmdError> {
    not_overlay(&window)?;
    let a11y = app.state::<DoubleTap>().a11y.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let api = a11y.api();
        if api.permission() == nugget_a11y::Permission::Denied {
            api.request_permission()
        } else {
            Ok(())
        }
    })
    .await
    .map_err(|e| CmdError::Other(e.to_string()))?
    .map_err(|e| CmdError::Other(e.to_string()))?;
    Ok(status(&app).await)
}

#[tauri::command]
pub fn doubletap_default_excluded() -> Vec<String> {
    settings::default_excluded()
}

/// Start a draft as if the trigger fired.
#[tauri::command]
pub async fn doubletap_fire(app: AppHandle, window: WebviewWindow) -> Result<(), CmdError> {
    not_overlay(&window)?;
    tauri::async_runtime::spawn(flow::fire(app));
    Ok(())
}

#[tauri::command]
pub async fn voice_status(
    app: AppHandle,
    daemon: State<'_, Daemon>,
) -> Result<VoiceStatus, CmdError> {
    daemon
        .call(&app, method::VOICE_STATUS, &serde_json::json!({}))
        .await
}

#[tauri::command]
pub fn overlay_state(state: State<'_, DoubleTap>) -> OverlayView {
    lock(&state.machine).view().clone()
}

/// Enter: `text` is null to accept the draft as shown, else the edited text.
#[tauri::command]
pub fn overlay_accept(
    app: AppHandle,
    window: WebviewWindow,
    text: Option<String>,
) -> Result<(), CmdError> {
    only_overlay(&window)?;
    flow::apply(&app, Event::Accept(text));
    Ok(())
}

/// Esc.
#[tauri::command]
pub fn overlay_cancel(app: AppHandle, window: WebviewWindow) -> Result<(), CmdError> {
    only_overlay(&window)?;
    flow::apply(&app, Event::Cancel);
    Ok(())
}
