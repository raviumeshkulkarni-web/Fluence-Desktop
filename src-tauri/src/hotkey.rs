// Fluence Windows — Hotkey module (v2: dual independent hotkeys)
// Registers two global shortcuts: one for transcription, one for agent mode.
// Each supports push_to_toggle OR hold_to_record independently.

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use tauri::{AppHandle, Emitter};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

// Independent state for each mode
static TRANSCRIPTION_RECORDING: AtomicBool = AtomicBool::new(false);
static AGENT_RECORDING: AtomicBool = AtomicBool::new(false);
// 0 = none, 1 = transcription, 2 = agent. The audio recorder has one
// physical owner even though the two shortcuts are independently configured.
static ACTIVE_RECORDING_OWNER: AtomicU8 = AtomicU8::new(0);
static HOTKEYS_REGISTERED: AtomicBool = AtomicBool::new(false);

pub(crate) fn clear_active_recording_owner() {
    let owner = ACTIVE_RECORDING_OWNER.swap(0, Ordering::SeqCst);
    match owner {
        1 => TRANSCRIPTION_RECORDING.store(false, Ordering::SeqCst),
        2 => AGENT_RECORDING.store(false, Ordering::SeqCst),
        _ => {
            TRANSCRIPTION_RECORDING.store(false, Ordering::SeqCst);
            AGENT_RECORDING.store(false, Ordering::SeqCst);
        }
    }
}

/// Clears the hotkey arbiter only after the recorder stop path has completed.
pub(crate) struct RecordingStopGuard;

impl Drop for RecordingStopGuard {
    fn drop(&mut self) {
        clear_active_recording_owner();
    }
}

pub fn register_hotkeys(
    app: &AppHandle,
    transcription_shortcut_str: &str,
    transcription_mode: &str,
    agent_shortcut_str: &str,
    agent_mode: &str,
) -> Result<(), String> {
    // Do not tear down an active recording's ownership while reconfiguring.
    if crate::audio::is_recording() {
        return Err("Cannot update hotkeys while recording is active".to_string());
    }

    // Parse both shortcuts up-front so invalid settings do not disturb the
    // currently registered shortcuts.
    let t_shortcut = <tauri_plugin_global_shortcut::Shortcut as std::str::FromStr>::from_str(
        transcription_shortcut_str,
    )
    .map_err(|e| {
        format!(
            "Invalid transcription hotkey '{}': {}",
            transcription_shortcut_str, e
        )
    })?;

    let a_shortcut =
        <tauri_plugin_global_shortcut::Shortcut as std::str::FromStr>::from_str(agent_shortcut_str)
            .map_err(|e| format!("Invalid agent hotkey '{}': {}", agent_shortcut_str, e))?;

    if t_shortcut == a_shortcut {
        return Err("Transcription and agent hotkeys must be different".to_string());
    }

    // Unregister all existing hotkeys first
    if HOTKEYS_REGISTERED.load(Ordering::SeqCst) {
        let _ = app.global_shortcut().unregister_all();
        HOTKEYS_REGISTERED.store(false, Ordering::SeqCst);
        TRANSCRIPTION_RECORDING.store(false, Ordering::SeqCst);
        AGENT_RECORDING.store(false, Ordering::SeqCst);
        ACTIVE_RECORDING_OWNER.store(0, Ordering::SeqCst);
    }

    // Register transcription hotkey
    {
        let mode = transcription_mode.to_string();
        let app_clone = app.clone();
        app.global_shortcut()
            .on_shortcut(t_shortcut, move |_app, _shortcut, event| {
                handle_hotkey_event(
                    &app_clone,
                    event.state(),
                    &mode,
                    &TRANSCRIPTION_RECORDING,
                    1,
                    "hotkey-start-recording",
                    "hotkey-stop-recording",
                );
            })
            .map_err(|e| e.to_string())?;
    }

    // Register agent hotkey
    {
        let mode = agent_mode.to_string();
        let app_clone = app.clone();
        if let Err(e) =
            app.global_shortcut()
                .on_shortcut(a_shortcut, move |_app, _shortcut, event| {
                    handle_hotkey_event(
                        &app_clone,
                        event.state(),
                        &mode,
                        &AGENT_RECORDING,
                        2,
                        "hotkey-start-agent-recording",
                        "hotkey-stop-agent-recording",
                    );
                })
        {
            let _ = app.global_shortcut().unregister_all();
            return Err(e.to_string());
        }
    }

    HOTKEYS_REGISTERED.store(true, Ordering::SeqCst);
    log::info!(
        "Hotkeys registered: transcription='{}' ({}), agent='{}' ({})",
        transcription_shortcut_str,
        transcription_mode,
        agent_shortcut_str,
        agent_mode
    );
    Ok(())
}

fn handle_hotkey_event(
    app: &AppHandle,
    state: ShortcutState,
    mode: &str,
    is_recording: &AtomicBool,
    owner: u8,
    start_event: &str,
    stop_event: &str,
) {
    match state {
        ShortcutState::Pressed => {
            if mode == "hold_to_record" {
                if !is_recording.load(Ordering::SeqCst) {
                    if ACTIVE_RECORDING_OWNER
                        .compare_exchange(0, owner, Ordering::SeqCst, Ordering::SeqCst)
                        .is_err()
                    {
                        log::debug!(
                            "Ignoring {} start because another recording mode is active",
                            start_event
                        );
                        return;
                    }
                    is_recording.store(true, Ordering::SeqCst);
                    if app.emit(start_event, ()).is_err() {
                        is_recording.store(false, Ordering::SeqCst);
                        let _ = ACTIVE_RECORDING_OWNER.compare_exchange(
                            owner,
                            0,
                            Ordering::SeqCst,
                            Ordering::SeqCst,
                        );
                    }
                }
            } else {
                // push_to_toggle
                if is_recording.load(Ordering::SeqCst) {
                    if is_recording
                        .compare_exchange(true, false, Ordering::SeqCst, Ordering::SeqCst)
                        .is_ok()
                    {
                        let _ = app.emit(stop_event, ());
                    }
                } else {
                    if ACTIVE_RECORDING_OWNER
                        .compare_exchange(0, owner, Ordering::SeqCst, Ordering::SeqCst)
                        .is_err()
                    {
                        log::debug!(
                            "Ignoring {} start because another recording mode is active",
                            start_event
                        );
                        return;
                    }
                    if is_recording
                        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                        .is_err()
                    {
                        let _ = ACTIVE_RECORDING_OWNER.compare_exchange(
                            owner,
                            0,
                            Ordering::SeqCst,
                            Ordering::SeqCst,
                        );
                        return;
                    }
                    if app.emit(start_event, ()).is_err() {
                        is_recording.store(false, Ordering::SeqCst);
                        let _ = ACTIVE_RECORDING_OWNER.compare_exchange(
                            owner,
                            0,
                            Ordering::SeqCst,
                            Ordering::SeqCst,
                        );
                    }
                }
            }
        }
        ShortcutState::Released => {
            if mode == "hold_to_record"
                && is_recording
                    .compare_exchange(true, false, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok()
            {
                let _ = app.emit(stop_event, ());
            }
        }
    }
}

// Tauri commands

#[tauri::command]
pub fn update_hotkeys(
    app: AppHandle,
    transcription_shortcut: String,
    transcription_mode: String,
    agent_shortcut: String,
    agent_mode: String,
) -> Result<(), String> {
    register_hotkeys(
        &app,
        &transcription_shortcut,
        &transcription_mode,
        &agent_shortcut,
        &agent_mode,
    )
}

#[tauri::command]
pub fn get_hotkey_state() -> serde_json::Value {
    serde_json::json!({
        "transcription_recording": TRANSCRIPTION_RECORDING.load(Ordering::SeqCst),
        "agent_recording": AGENT_RECORDING.load(Ordering::SeqCst),
        "active_owner": ACTIVE_RECORDING_OWNER.load(Ordering::SeqCst),
    })
}
