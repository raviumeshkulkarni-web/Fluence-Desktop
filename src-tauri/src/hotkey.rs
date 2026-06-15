// Fluence Windows — Hotkey module (v2: dual independent hotkeys)
// Registers two global shortcuts: one for transcription, one for agent mode.
// Each supports push_to_toggle OR hold_to_record independently.

use std::sync::atomic::{AtomicBool, Ordering};
use tauri::{AppHandle, Emitter};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

// Independent state for each mode
static TRANSCRIPTION_RECORDING: AtomicBool = AtomicBool::new(false);
static AGENT_RECORDING: AtomicBool = AtomicBool::new(false);
static HOTKEYS_REGISTERED: AtomicBool = AtomicBool::new(false);

pub fn register_hotkeys(
    app: &AppHandle,
    transcription_shortcut_str: &str,
    transcription_mode: &str,
    agent_shortcut_str: &str,
    agent_mode: &str,
) -> Result<(), String> {
    // Unregister all existing hotkeys first
    if HOTKEYS_REGISTERED.load(Ordering::SeqCst) {
        let _ = app.global_shortcut().unregister_all();
        HOTKEYS_REGISTERED.store(false, Ordering::SeqCst);
        TRANSCRIPTION_RECORDING.store(false, Ordering::SeqCst);
        AGENT_RECORDING.store(false, Ordering::SeqCst);
    }

    // Parse both shortcuts up-front so we fail fast before registering either
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
        app.global_shortcut()
            .on_shortcut(a_shortcut, move |_app, _shortcut, event| {
                handle_hotkey_event(
                    &app_clone,
                    event.state(),
                    &mode,
                    &AGENT_RECORDING,
                    "hotkey-start-agent-recording",
                    "hotkey-stop-agent-recording",
                );
            })
            .map_err(|e| e.to_string())?;
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
    start_event: &str,
    stop_event: &str,
) {
    match state {
        ShortcutState::Pressed => {
            if mode == "hold_to_record" {
                if !is_recording.load(Ordering::SeqCst) {
                    is_recording.store(true, Ordering::SeqCst);
                    let _ = app.emit(start_event, ());
                }
            } else {
                // push_to_toggle
                let currently = is_recording.load(Ordering::SeqCst);
                is_recording.store(!currently, Ordering::SeqCst);
                if !currently {
                    let _ = app.emit(start_event, ());
                } else {
                    let _ = app.emit(stop_event, ());
                }
            }
        }
        ShortcutState::Released => {
            if mode == "hold_to_record" && is_recording.load(Ordering::SeqCst) {
                is_recording.store(false, Ordering::SeqCst);
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
    })
}
