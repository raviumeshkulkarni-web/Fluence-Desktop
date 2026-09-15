// Fluence Windows - per-window IPC caller authorization.
//
// App-defined Tauri commands are invokable from any webview; the
// `capabilities/*.json` files only gate *plugin* permissions (fs, dialog,
// updater, process). These helpers enforce least-privilege per caller
// window label ("main" | "overlay" | "wizard") inside the commands
// themselves. Adding `window: tauri::Window` to a command signature is
// transparent to the frontend (Tauri injects it; `invoke()` args unchanged).
//
// Caller inventory (verified against src/js/*.js + web/src, v1.22.x):
//
// | Command                              | Callers              | Gate                     |
// |--------------------------------------|----------------------|--------------------------|
// | get_settings                         | main,overlay,wizard  | open (needs differ)      |
// | update_settings                      | main,wizard          | deny overlay             |
// | update_hotkeys                       | main,wizard          | deny overlay             |
// | set_autostart                        | main                 | main only                |
// | save_api_key                         | main,wizard          | deny overlay; deny Sync* |
// | get_api_key                          | main,wizard,overlay* | *overlay: LLM/preset only|
// | delete_api_key                       | (no callers)         | main only; deny Sync*    |
// | sync_get_status/toggle/sign_in/out   | main                 | main only                |
// | get_account_stats/activity/weekly    | main                 | open (read-only stats)   |
// | download/cancel/delete_*model (6)    | main                 | main only                |
// | *_model_status (3)                   | main                 | open (read-only)         |
// | add/update/delete/import_dictionary  | main                 | main only                |
// | get/export_dictionary                | main                 | open (read-only)         |
// | delete/clear_history                 | main                 | main only                |
// | get/save_history                     | main,overlay         | open (overlay saves)     |
// | start/stop/is_recording              | all                  | open                     |
// | stop_and_transcribe/finish/retry     | overlay              | open (workflow owns key) |
// | transcribe_audio/fetch/test (5)      | main,wizard          | main+wizard (raw-key)    |
// | execute_agent_command                | overlay              | open until Task 4        |
// | inject/copy/keyboard/grab_selection  | main,overlay         | open (core function)     |
// | overlay/window/hotkey-state/icon     | caller window        | open                     |
// | cleanup_debug_recordings             | (new, uncalled)      | main only                |
//
// *Overlay `get_api_key` is narrowed to per-preset `Fluence/LLM_ApiKey/*`
// reads (agent mode, until Task 4 moves it server-side). STT, Sync, and
// legacy global slots are denied for overlay.

pub const MAIN_WINDOW: &str = "main";
pub const OVERLAY_WINDOW: &str = "overlay";
pub const WIZARD_WINDOW: &str = "wizard";

/// True for anything under the Sync credential namespace. The refresh token
/// is managed server-side only (`store/read/delete_sync_refresh_token`);
/// it must never cross the renderer IPC boundary in either direction.
pub fn is_sync_credential_target(target: &str) -> bool {
    target == crate::credentials::SYNC_REFRESH_TOKEN_TARGET
        || target.starts_with("Fluence/Sync/")
}

/// Deny unless the calling window is in `allowed`. Logs the denial
/// (label only — never secrets or payloads).
pub fn require_caller(window: &tauri::Window, allowed: &[&str]) -> Result<(), String> {
    let label = window.label();
    if allowed.iter().any(|a| *a == label) {
        return Ok(());
    }
    log::warn!(
        "IPC denied: window '{}' is not allowed to invoke this command",
        label
    );
    Err("Not allowed from this window".to_string())
}

/// Overlay's temporary credential rule (see table): per-preset LLM reads
/// only. Everything else — STT, Sync, legacy global slots — is denied.
pub fn overlay_may_read_credential(target: &str) -> bool {
    target.starts_with(crate::credentials::LLM_API_KEY_TARGET)
        && *target != *crate::credentials::LLM_API_KEY_TARGET
        && !is_sync_credential_target(target)
}
