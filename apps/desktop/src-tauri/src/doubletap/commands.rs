//! Tauri commands for the Double Tap settings page and the overlay window.

use nugget_core::ipc::{VoiceStatus, method};
use nugget_hotkey::Platform;
use tauri::{AppHandle, Emitter, Manager, State, WebviewWindow};

use super::history::Entry;
use super::overlay::{Event, OverlayView, Refinement};
use super::permissions::{self, DoubleTapStatus, PermissionState};
use super::practice::PracticeField;
use super::settings::{self, DoubleTapSettings, TriggerSpec};
use super::{DoubleTap, MAIN_LABEL, OVERLAY_LABEL, SETTINGS_EVENT, flow, lock, readahead};
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
    super::ghost_flow::restart(&app);
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
        ghost: super::ghost_flow::status(app),
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

/// macOS Privacy & Security panes Double Tap may open. The URLs are fixed here and
/// never built from input, so the general URL allowlist (which refuses
/// `x-apple.systempreferences:`) stays as strict as it is.
pub(crate) fn privacy_pane_url(pane: &str) -> Option<&'static str> {
    match pane {
        "accessibility" => {
            Some("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility")
        }
        "input_monitoring" => {
            Some("x-apple.systempreferences:com.apple.preference.security?Privacy_ListenEvent")
        }
        _ => None,
    }
}

/// macOS: open System Settings at the Accessibility or Input Monitoring list, so a user who
/// denied the prompt (which macOS shows only once) can turn Nugget on. Returns `false` where
/// there is no such pane (Linux, Windows).
#[tauri::command]
pub async fn doubletap_open_privacy_settings(
    window: WebviewWindow,
    pane: String,
) -> Result<bool, CmdError> {
    not_overlay(&window)?;
    let Some(url) = privacy_pane_url(&pane) else {
        return Err(CmdError::Other(format!("unknown settings pane `{pane}`")));
    };
    #[cfg(target_os = "macos")]
    {
        let status = tauri::async_runtime::spawn_blocking(move || {
            std::process::Command::new("/usr/bin/open")
                .arg(url)
                .status()
        })
        .await
        .map_err(|e| CmdError::Other(e.to_string()))?
        .map_err(|e| CmdError::Other(format!("couldn't open System Settings: {e}")))?;
        if !status.success() {
            return Err(CmdError::Other(format!(
                "couldn't open System Settings ({status})"
            )));
        }
        Ok(true)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = url;
        Ok(false)
    }
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

/// Retry: a new draft from the same context.
#[tauri::command]
pub fn overlay_retry(app: AppHandle, window: WebviewWindow) -> Result<(), CmdError> {
    only_overlay(&window)?;
    flow::apply(&app, Event::Retry);
    Ok(())
}

/// A refinement of `base` (the user's edit), or of the text shown when `base` is null.
#[tauri::command]
pub fn overlay_refine(
    app: AppHandle,
    window: WebviewWindow,
    kind: Refinement,
    base: Option<String>,
) -> Result<(), CmdError> {
    only_overlay(&window)?;
    // The overlay's text is untrusted input to a new request: keep it to a draft's size.
    let base = base.map(|b| b.chars().take(super::history::MAX_CHARS).collect());
    flow::apply(&app, Event::Refine { kind, base });
    Ok(())
}

/// The overlay measured its content (logical pixels); returns the height applied.
#[tauri::command]
pub fn overlay_resize(app: AppHandle, window: WebviewWindow, height: f64) -> Result<f64, CmdError> {
    only_overlay(&window)?;
    Ok(flow::resize_overlay(&app, height))
}

/// Insert a draft from the history instead of the one shown.
#[tauri::command]
pub fn overlay_insert_previous(
    app: AppHandle,
    window: WebviewWindow,
    state: State<'_, DoubleTap>,
    id: u64,
) -> Result<(), CmdError> {
    only_overlay(&window)?;
    let text = lock(&state.history)
        .get(id)
        .map(|e| e.text.clone())
        .ok_or_else(|| CmdError::Other("that draft is no longer in the history".into()))?;
    flow::apply(&app, Event::InsertPrevious(text));
    Ok(())
}

/// The draft history, newest first.
#[tauri::command]
pub fn doubletap_history(state: State<'_, DoubleTap>) -> Vec<Entry> {
    lock(&state.history).list()
}

#[tauri::command]
pub fn doubletap_history_clear(
    app: AppHandle,
    window: WebviewWindow,
    state: State<'_, DoubleTap>,
) -> Result<(), CmdError> {
    not_overlay(&window)?;
    if lock(&state.history).clear() {
        flow::emit_history(&app, &[]);
    }
    Ok(())
}

#[tauri::command]
pub fn doubletap_history_remove(
    app: AppHandle,
    window: WebviewWindow,
    state: State<'_, DoubleTap>,
    id: u64,
) -> Result<(), CmdError> {
    not_overlay(&window)?;
    let list = {
        let mut h = lock(&state.history);
        if !h.remove(id) {
            return Ok(());
        }
        h.list()
    };
    flow::emit_history(&app, &list);
    Ok(())
}

/// The practice field on the Double Tap page gained focus or changed (`null` when it lost it).
#[tauri::command]
pub fn doubletap_practice(
    window: WebviewWindow,
    state: State<'_, DoubleTap>,
    field: Option<PracticeField>,
) -> Result<(), CmdError> {
    if window.label() != MAIN_LABEL {
        return Err(CmdError::Other(
            "only the main window has the practice field".into(),
        ));
    }
    let field = field
        .map(PracticeField::validated)
        .transpose()
        .map_err(CmdError::Other)?;
    *lock(&state.practice) = field;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::privacy_pane_url;

    #[test]
    fn privacy_panes_are_fixed_urls() {
        assert_eq!(
            privacy_pane_url("accessibility"),
            Some("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility")
        );
        assert_eq!(
            privacy_pane_url("input_monitoring"),
            Some("x-apple.systempreferences:com.apple.preference.security?Privacy_ListenEvent")
        );
        for other in ["", "camera", "../x", "accessibility?x", "Accessibility"] {
            assert_eq!(privacy_pane_url(other), None, "{other}");
        }
    }
}
