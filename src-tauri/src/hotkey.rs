// Fluence Windows - Hotkey module (v2: dual independent hotkeys)
// Registers two global shortcuts: one for transcription, one for agent mode.
// Each supports push_to_toggle OR hold_to_record independently.

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use tauri::{AppHandle, Emitter};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

// Independent state for each mode
static TRANSCRIPTION_RECORDING: AtomicBool = AtomicBool::new(false);
static AGENT_RECORDING: AtomicBool = AtomicBool::new(false);
// 0 = none, 1 = transcription, 2 = agent. The audio recorder has one
// physical owner even though the two shortcuts are independently configured.
static ACTIVE_RECORDING_OWNER: AtomicU8 = AtomicU8::new(0);
// Monotonically increasing native recording session ID
static RECORDING_SESSION_ID: AtomicU64 = AtomicU64::new(0);
// Timestamp (ms since UNIX epoch) when stop was requested, to differentiate active finalization from orphaned owners
static LAST_STOP_REQUEST_MS: AtomicU64 = AtomicU64::new(0);
static HOTKEYS_REGISTERED: AtomicBool = AtomicBool::new(false);

pub(crate) fn clear_active_recording_owner() {
    LAST_STOP_REQUEST_MS.store(0, Ordering::SeqCst);
    let owner = ACTIVE_RECORDING_OWNER.swap(0, Ordering::SeqCst);
    log::info!(
        "clear_active_recording_owner: prev_owner={}, session_id={}",
        owner,
        RECORDING_SESSION_ID.load(Ordering::SeqCst)
    );
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
        log::debug!("RecordingStopGuard dropped; releasing recording owner");
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
                    let active_owner = ACTIVE_RECORDING_OWNER.load(Ordering::SeqCst);
                    if active_owner == owner {
                        let stop_ms = LAST_STOP_REQUEST_MS.load(Ordering::SeqCst);
                        let now_ms = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_millis() as u64;
                        let elapsed = now_ms.saturating_sub(stop_ms);
                        if elapsed < 2000 {
                            // Legitimate finalization is still running. Do not trigger native cleanup!
                            log::debug!(
                                "Ignoring {} start because previous recording is still finalizing (elapsed={}ms)",
                                start_event,
                                elapsed
                            );
                            let _ = app.emit(
                                "hotkey-busy",
                                serde_json::json!({
                                    "requested": start_event,
                                    "active_owner": active_owner,
                                    "finalizing": true,
                                }),
                            );
                            return;
                        }

                        // More than 2000ms elapsed: previous hold release was never drained by frontend.
                        let session_id = RECORDING_SESSION_ID.load(Ordering::SeqCst);
                        log::warn!(
                            "Detected orphaned recording owner {} after {}ms (session_id={}) in hold_to_record; re-emitting {} and cleaning up",
                            owner,
                            elapsed,
                            session_id,
                            stop_event
                        );
                        let _ = app.emit(stop_event, serde_json::json!({ "session_id": session_id }));
                        tauri::async_runtime::spawn(async {
                            let _ = crate::audio::stop_recording().await;
                            clear_active_recording_owner();
                        });
                        return;
                    }

                    if ACTIVE_RECORDING_OWNER
                        .compare_exchange(0, owner, Ordering::SeqCst, Ordering::SeqCst)
                        .is_err()
                    {
                        log::debug!(
                            "Ignoring {} start because another recording mode is active",
                            start_event
                        );
                        // BUG-05: surface busy state for diagnostics
                        let _ = app.emit(
                            "hotkey-busy",
                            serde_json::json!({
                                "requested": start_event,
                                "active_owner": ACTIVE_RECORDING_OWNER.load(Ordering::SeqCst)
                            }),
                        );
                        return;
                    }
                    is_recording.store(true, Ordering::SeqCst);
                    let session_id = RECORDING_SESSION_ID.fetch_add(1, Ordering::SeqCst) + 1;
                    log::info!("Starting hold_to_record session {} for owner {}", session_id, owner);
                    if app.emit(start_event, serde_json::json!({ "session_id": session_id })).is_err() {
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
                        let now_ms = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_millis() as u64;
                        LAST_STOP_REQUEST_MS.store(now_ms, Ordering::SeqCst);
                        let session_id = RECORDING_SESSION_ID.load(Ordering::SeqCst);
                        log::info!("Emitting {} for session {} (owner={})", stop_event, session_id, owner);
                        let _ = app.emit(stop_event, serde_json::json!({ "session_id": session_id }));
                    }
                } else {
                    let active_owner = ACTIVE_RECORDING_OWNER.load(Ordering::SeqCst);
                    if active_owner == owner {
                        let stop_ms = LAST_STOP_REQUEST_MS.load(Ordering::SeqCst);
                        let now_ms = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_millis() as u64;
                        let elapsed = now_ms.saturating_sub(stop_ms);
                        if elapsed < 2000 {
                            // Legitimate finalization is still running. Do not trigger native cleanup!
                            log::debug!(
                                "Ignoring {} start because previous recording is still finalizing (elapsed={}ms)",
                                start_event,
                                elapsed
                            );
                            let _ = app.emit(
                                "hotkey-busy",
                                serde_json::json!({
                                    "requested": start_event,
                                    "active_owner": active_owner,
                                    "finalizing": true,
                                }),
                            );
                            return;
                        }

                        // More than 2000ms elapsed since stop was emitted, but owner was never cleared.
                        // Frontend dropped the stop or died: this is a truly orphaned owner.
                        let session_id = RECORDING_SESSION_ID.load(Ordering::SeqCst);
                        log::warn!(
                            "Detected orphaned recording owner {} after {}ms (session_id={}); re-emitting {} and initiating native cleanup",
                            owner,
                            elapsed,
                            session_id,
                            stop_event
                        );
                        let _ = app.emit(stop_event, serde_json::json!({ "session_id": session_id }));
                        tauri::async_runtime::spawn(async {
                            let _ = crate::audio::stop_recording().await;
                            clear_active_recording_owner();
                        });
                        return;
                    }

                    if ACTIVE_RECORDING_OWNER
                        .compare_exchange(0, owner, Ordering::SeqCst, Ordering::SeqCst)
                        .is_err()
                    {
                        log::debug!(
                            "Ignoring {} start because another recording mode is active",
                            start_event
                        );
                        let _ = app.emit(
                            "hotkey-busy",
                            serde_json::json!({
                                "requested": start_event,
                                "active_owner": ACTIVE_RECORDING_OWNER.load(Ordering::SeqCst)
                            }),
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
                    let session_id = RECORDING_SESSION_ID.fetch_add(1, Ordering::SeqCst) + 1;
                    log::info!("Starting recording session {} for owner {}", session_id, owner);
                    if app.emit(start_event, serde_json::json!({ "session_id": session_id })).is_err() {
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
                let now_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64;
                LAST_STOP_REQUEST_MS.store(now_ms, Ordering::SeqCst);
                let session_id = RECORDING_SESSION_ID.load(Ordering::SeqCst);
                log::info!("Emitting {} on release for session {} (owner={})", stop_event, session_id, owner);
                let _ = app.emit(stop_event, serde_json::json!({ "session_id": session_id }));
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
        "session_id": RECORDING_SESSION_ID.load(Ordering::SeqCst),
    })
}
