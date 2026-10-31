// Fluence Windows - per-window IPC caller authorization.
//
// App-defined Tauri commands are invokable from any webview; the
// `capabilities/*.json` files only gate *plugin* permissions (fs, dialog,
// updater, process). These helpers enforce least-privilege per caller
// window label ("main" | "overlay" | "wizard") inside the commands
// themselves. Adding `window: tauri::Window` to a command signature is
// transparent to the frontend (Tauri injects it; `invoke()` args unchanged).
//
// Caller inventory (verified against src-tauri/src/*.rs + src/js/*.js +
// src/*.html, app version 1.27.1 from src-tauri/Cargo.toml; table refreshed
// when the last open IPC surface outside the deliberately-open set was
// gated):
//
// | Command                              | Callers              | Gate                     |
// |--------------------------------------|----------------------|--------------------------|
// | get_settings                         | main,overlay,wizard  | open (needs differ)      |
// | update_settings                      | main,wizard          | deny overlay             |
// | update_hotkeys                       | main,wizard          | deny overlay             |
// | set_autostart                        | main                 | main only                |
// | save_api_key                         | main,wizard          | deny overlay; deny Sync* |
// | get_api_key                          | main,wizard          | overlay denied (Task 4)    |
// | delete_api_key                       | (no callers)         | main only; deny Sync*    |
// | sync_get_status/toggle/sign_in/out   | main                 | main only                |
// | sync_run_now                         | main                 | main only                |
// | get_account_stats/activity           | main                 | main only                |
// | get_account_activity                  | main                 | main only (Package B)    |
// | download_*_model (3)                 | main                 | main only                |
// | cancel_offline_download (1)          | main                 | main only                |
// | delete_*_model (3)                   | main                 | main only                |
// | get_*_model_status (3)               | main                 | main only                |
// | list_audio_devices                   | main                 | open (device picker)     |
// | cleanup_debug_recordings             | main                 | main only                |
// | transcribe_audio                     | main,wizard          | main+wizard (raw-key)    |
// | fetch_models                         | main,wizard          | main+wizard (raw-key)    |
// | fetch_stt_models                     | main,wizard          | main+wizard (raw-key)    |
// | test_stt_connection                  | main,wizard          | main+wizard (raw-key)    |
// | test_llm_connection                  | main,wizard          | main+wizard (raw-key)    |
// | execute_agent_command                | main,wizard          | main+wizard (raw-key)    |
// | execute_agent_command_secure         | main,overlay         | server-side key          |
// | get_agents                           | main,overlay         | open (needs differ)      |
// | save/delete_agent                    | main                 | main only                |
// | set_default_agent                    | main                 | main only                |
// | claim_legacy_agent                   | main                 | main only                |
// | get_prompts                          | main                 | open (needs differ)      |
// | save/delete_prompt_style             | main                 | main only                |
// | claim_legacy_prompt_style            | main                 | main only                |
// | set/clear_prompt_override            | main                 | main only                |
// | add/update/delete_dictionary         | main                 | main only                |
// | import_dictionary(_report)           | main                 | main only                |
// | read_import_file                     | main                 | main only                |
// | get/export_dictionary                | main                 | open (read-only)         |
// | set_snippets_enabled                 | main                 | main only (Package B)    |
// | add/update/delete_snippet            | main                 | main only (Package B)    |
// | get_snippets                         | main                 | main only (Package B)    |
// | get_suggestions                      | main                 | main only (Package B)    |
// | accept_suggestion_command            | main                 | main only (Package B)    |
// | dismiss_suggestion_command           | main                 | main only (Package B)    |
// | clear_dismissed_suggestions_command  | main                 | main only (Package B)    |
// | get_dismissed_count                  | main                 | main only (Package B)    |
// | expire_stale_suggestions_command     | main                 | main only (Package B)    |
// | get_learning_suggestions             | main                 | main only (Package B)    |
// | get_auto_accepted_suggestions        | main                 | main only (Package B)    |
// | revert_auto_accepted_suggestion      | main                 | main only (Package B)    |
// | delete/clear_history                 | main                 | main only                |
// | get/save_history                     | main,overlay         | open (overlay saves)     |
// | get_history_stats                    | main                 | main only (Package B)    |
// | get_weekly_activity                  | main                 | main only (Package B)    |
// | start/stop/is_recording              | all                  | open                     |
// | stop_and_transcribe/finish/retry     | overlay              | overlay only             |
// | inject_text                          | overlay              | overlay only (+target    |
// |                                      |                      |  HWND validation, P0)    |
// | capture_target_window                | overlay              | overlay only             |
// | copy_text                            | overlay,main         | overlay+main             |
// | execute_keyboard_action              | overlay              | overlay only             |
// | grab_active_selection                | overlay              | overlay only             |
// | get_foreground_app_icon              | overlay              | overlay only             |
// | show/hide_overlay, set_overlay_style | overlay              | overlay only             |
// | show_main_window                     | main,wizard          | main+wizard              |
// | hide/minimize/toggle_max_main_window | main                 | main only                |
// | show_wizard_window                   | main,wizard          | main+wizard              |
// | minimize/close_wizard                | wizard               | wizard only              |
// | list_installed_apps                  | main                 | main only                |
// | get_hotkey_state                     | all                  | open (read-only)         |
// | get_app_version                      | all                  | open (read-only)         |
//
// Runtime-internal entry points that are NOT IPC commands (and therefore
// ungated by design — no `window` to inspect): `suggestion::upsert_suggestions`
// (called from `auto_learn::learner` and `workflow`),
// `suggestion::accept_suggestion` / `dismiss_suggestion` /
// `clear_dismissed_suggestions` / `expire_stale_suggestions` (called from the
// startup sweep in `main.rs` and from the gated `*_command` wrappers above),
// `dictionary::add_dictionary_entry_internal` (auto-accept monitor thread),
// and `transcribe_audio`'s internal request path. `get_pending_suggestions`
// is likewise an internal helper, reached only through the gated
// `get_suggestions` command.
//
// "Deliberately open" — the remaining ungated `#[tauri::command]`s
// (read-only, cross-window needs, or needed by several callers):
// `settings::get_settings` (main/overlay/wizard needs differ),
// `audio::{list_audio_devices, start_recording, stop_recording,
// is_recording}` (device picker lives in main; recording control is the
// global-hotkey router and overlay flows; update-manager polls
// is_recording), `dictionary::{get_dictionary, export_dictionary}`
// (read-only/export dialog), `history::{get_history, save_history_entry}`
// (overlay saves entries; main lists them), `hotkey::get_hotkey_state`
// (read-only polling from main and wizard), and `get_app_version`
// (read-only banner in both windows).
//
// Model-command counts (verified against main.rs `invoke_handler`, main.rs:
// 262-272): 3 downloads (offline, moonshine v2 small, moonshine v2 medium),
// 1 cancel (`cancel_offline_download`), 3 deletes (same three models),
// 3 status reads. An earlier revision of this table listed a single
// "download/cancel/delete_*model (6)" row; the real total is 7, split above.
//
// Overlay credential reads were narrowed to per-preset `Fluence/LLM_ApiKey/*`
// during Tasks 1-3, then denied entirely by Task 4 (agent mode resolves its
// key server-side via `execute_agent_command_secure`).

pub const MAIN_WINDOW: &str = "main";
pub const OVERLAY_WINDOW: &str = "overlay";
pub const WIZARD_WINDOW: &str = "wizard";

/// True for anything under the Sync credential namespace. The refresh token
/// is managed server-side only (`store/read/delete_sync_refresh_token`);
/// it must never cross the renderer IPC boundary in either direction.
pub fn is_sync_credential_target(target: &str) -> bool {
    target == crate::credentials::SYNC_REFRESH_TOKEN_TARGET || target.starts_with("Fluence/Sync/")
}

/// Deny unless the calling window is in `allowed`. Logs the denial
/// (label only — never secrets or payloads).
pub fn require_caller(window: &tauri::Window, allowed: &[&str]) -> Result<(), String> {
    let label = window.label();
    if allowed.contains(&label) {
        return Ok(());
    }
    log::warn!(
        "IPC denied: window '{}' is not allowed to invoke this command",
        label
    );
    Err("Not allowed from this window".to_string())
}
