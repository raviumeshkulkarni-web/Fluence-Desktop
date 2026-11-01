// Fluence Windows - Settings module
// Manages persistent configuration in a JSON file in the app data directory.

use anyhow::Result;
// Only the production branch of `settings_path` resolves the real Windows known
// folder, so the import is test-excluded there too.
#[cfg(not(test))]
use dirs::data_local_dir;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    #[serde(default = "default_preset")]
    pub preset: String, // "groq" | "openai" | "custom" | "mistral"
    #[serde(default = "default_base_url")]
    pub base_url: String,
    #[serde(default = "default_model")]
    pub model: String,
    #[serde(default = "default_api_key_saved")]
    pub api_key_saved: bool, // whether credential is stored in Credential Manager
}

fn default_preset() -> String {
    "groq".to_string()
}
fn default_base_url() -> String {
    "https://api.groq.com/openai".to_string()
}
fn default_model() -> String {
    "whisper-large-v3".to_string()
}
fn default_api_key_saved() -> bool {
    false
}

impl Default for ProviderConfig {
    fn default() -> Self {
        Self {
            preset: default_preset(),
            base_url: default_base_url(),
            model: default_model(),
            api_key_saved: default_api_key_saved(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSettings {
    #[serde(default = "default_hotkey")]
    pub hotkey: String,
    #[serde(default = "default_recording_mode")]
    pub recording_mode: String, // "push_to_toggle" | "hold_to_record"
    #[serde(default = "default_overlay_position")]
    pub overlay_position: String, // "center" | "bottom_left" | "bottom_right" | "top_left" | "top_center" | "top_right"
    #[serde(default = "default_overlay_style")]
    pub overlay_style: String, // "full" | "compact" | "bubble"
    /// Waveform animation of the recording overlay
    /// ("waves" | "bars"). Frontend normalizes unknown
    /// values back to "waves", so legacy and hand-edited files stay safe.
    /// Default keeps the shipped layered-ripple look: existing users see
    /// nothing change until they pick another style in Floating Bubble.
    /// Device-local by design (not added to the sync allowlist).
    #[serde(default = "default_waveform_style")]
    pub waveform_style: String,
    /// Soft halo around the overlay border (Android pill-glow parity).
    /// Default ON: dropping the halo is one toggle away in Floating Bubble.
    #[serde(default = "default_true")]
    pub overlay_glow: bool,
    /// Foreground-app pill (icon + name floating above the overlay).
    /// Default ON preserves today's behavior; OFF skips its fetch, poll,
    /// and display without touching overlay layout or recording flow.
    #[serde(default = "default_true")]
    pub show_app_pill: bool,
    #[serde(default)]
    pub audio_device_id: Option<String>,
    #[serde(default = "default_stt_provider")]
    pub stt_provider: ProviderConfig,
    #[serde(default = "default_llm_provider")]
    pub llm_provider: ProviderConfig,
    /// Dedicated provider for AI post-processing cleanup. Split out of
    /// `llm_provider` so Agent Mode and cleanup can use different keys /
    /// models. Files saved before this field existed inherit the current
    /// LLM provider on load (see migrate_missing_cleaner_provider), so
    /// existing users keep the exact cleanup behavior they had.
    #[serde(default = "default_llm_provider")]
    pub cleaner_provider: ProviderConfig,
    #[serde(default = "default_false")]
    pub auto_start: bool,
    #[serde(default = "default_true")]
    pub sound_on_complete: bool,
    #[serde(default = "default_theme")]
    pub theme: String, // "dark" | "light"
    #[serde(default = "default_agent_mode_threshold_ms")]
    #[allow(dead_code)]
    pub agent_mode_threshold_ms: u64,
    #[serde(default = "default_true")]
    pub first_run: bool,
    #[serde(default = "default_language")]
    pub language: String,
    #[serde(default = "default_ui_language")]
    pub ui_language: String,
    #[serde(default = "default_agent_hotkey")]
    pub agent_hotkey: String, // e.g. "Ctrl+Shift+A"
    #[serde(default = "default_agent_recording_mode")]
    pub agent_recording_mode: String, // "push_to_toggle" | "hold_to_record"
    #[serde(default = "default_ai_polish_style")]
    pub ai_polish_style: String,
    #[serde(default = "default_true")]
    pub auto_grab_highlight: bool,
    #[serde(default = "default_true")]
    pub auto_learn_enabled: bool,
    /// Master switch for automatic suggestion acceptance. Default OFF.
    /// Local-only by design: intentionally NOT in the settings sync
    /// allowlist, so no device can opt another into autonomous writes.
    #[serde(default = "default_false")]
    pub auto_accept_enabled: bool,
    #[serde(default = "default_false")]
    pub duck_enabled: bool,
    #[serde(default = "default_duck_level")]
    pub duck_level: f32,
    #[serde(default = "default_offline_engine")]
    pub offline_engine: String, // "sensevoice" | "moonshine_v2_small" | "moonshine_v2_medium"
    // (legacy "moonshine_base" is auto-migrated on load, see below)
    /// Legacy online-streaming toggle. Online streaming support has been
    /// removed: this flag is parsed but no longer read by any code path,
    /// so existing settings files keep working unchanged.
    #[serde(default = "default_false")]
    pub stt_streaming_enabled: bool,
    #[serde(default = "default_false")]
    pub sync_enabled: bool,
    // NOTE: kept as the raw account email on purpose. It is hashed into the
    // per-account id for sync partitioning AND shown in Settings → Sync.
    // Hashing it at rest would destroy the display value and the hash input,
    // so we store it plaintext (local-only) instead. No key material here.
    #[serde(default)]
    pub sync_account_key: Option<String>,
    /// Diagnostic voice-capture retention. Default OFF: the app never writes
    /// `debug_recordings/*.flac` unless this is explicitly enabled. Cleanup
    /// only runs via the explicit `cleanup_debug_recordings` command — never
    /// automatically — so no user data is deleted without user control.
    #[serde(default = "default_false")]
    pub debug_recordings_enabled: bool,
    /// Keep the transcribed text on the clipboard after paste injection so
    /// the user can paste it again with Ctrl+V (fallback when the target
    /// field rejected the paste, and for multi-paste). OFF keeps today's
    /// behavior: the clipboard returns to whatever the user had copied
    /// before dictating. Default ON follows issue #10. Device-local by
    /// design (not added to the sync allowlist).
    #[serde(default = "default_true")]
    pub copy_transcript_to_clipboard: bool,
}

fn default_hotkey() -> String {
    "Ctrl+Shift+Space".to_string()
}
fn default_recording_mode() -> String {
    "push_to_toggle".to_string()
}
fn default_overlay_position() -> String {
    "bottom_right".to_string()
}
fn default_overlay_style() -> String {
    "full".to_string()
}
fn default_waveform_style() -> String {
    "waves".to_string()
}
fn default_false() -> bool {
    false
}
fn default_true() -> bool {
    true
}
fn default_theme() -> String {
    "dark".to_string()
}
fn default_agent_mode_threshold_ms() -> u64 {
    800
}
fn default_language() -> String {
    "en".to_string()
}
fn default_ui_language() -> String {
    "system".to_string()
}
fn default_agent_hotkey() -> String {
    "Ctrl+Shift+A".to_string()
}
fn default_agent_recording_mode() -> String {
    "push_to_toggle".to_string()
}
fn default_ai_polish_style() -> String {
    "none".to_string()
}
fn default_duck_level() -> f32 {
    0.0
}
fn default_offline_engine() -> String {
    "sensevoice".to_string()
}

fn default_stt_provider() -> ProviderConfig {
    ProviderConfig {
        preset: "groq".to_string(),
        base_url: "https://api.groq.com/openai".to_string(),
        model: "whisper-large-v3".to_string(),
        api_key_saved: false,
    }
}
fn default_llm_provider() -> ProviderConfig {
    ProviderConfig {
        preset: "groq".to_string(),
        base_url: "https://api.groq.com/openai".to_string(),
        model: "llama-3.3-70b-versatile".to_string(),
        api_key_saved: false,
    }
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            hotkey: default_hotkey(),
            recording_mode: default_recording_mode(),
            overlay_position: default_overlay_position(),
            overlay_style: default_overlay_style(),
            waveform_style: default_waveform_style(),
            overlay_glow: default_true(),
            show_app_pill: default_true(),
            audio_device_id: None,
            stt_provider: default_stt_provider(),
            llm_provider: default_llm_provider(),
            cleaner_provider: default_llm_provider(),
            auto_start: default_false(),
            sound_on_complete: default_true(),
            theme: default_theme(),
            agent_mode_threshold_ms: default_agent_mode_threshold_ms(),
            first_run: default_true(),
            language: default_language(),
            ui_language: default_ui_language(),
            agent_hotkey: default_agent_hotkey(),
            agent_recording_mode: default_agent_recording_mode(),
            ai_polish_style: default_ai_polish_style(),
            auto_grab_highlight: default_true(),
            auto_learn_enabled: default_true(),
            auto_accept_enabled: default_false(),
            duck_enabled: default_false(),
            duck_level: default_duck_level(),
            offline_engine: default_offline_engine(),
            stt_streaming_enabled: default_false(),
            sync_enabled: default_false(),
            sync_account_key: None,
            debug_recordings_enabled: default_false(),
            copy_transcript_to_clipboard: default_true(),
        }
    }
}

pub fn settings_path() -> PathBuf {
    // TEST-ONLY redirect. Under `cfg(test)` this resolves inside the per-process
    // temp data dir, so no test can read or overwrite the real settings.json —
    // which the delete paths would otherwise do, since they resolve the active
    // account through this file.
    //
    // PRODUCTION below is byte-identical to the original and deliberately does
    // NOT go through `stores::base_data_dir()`: that helper honours
    // FLUENCE_DATA_DIR outside `cfg(test)`, so routing this through it would
    // have made a release build start respecting that variable for settings,
    // which it never did before.
    #[cfg(test)]
    {
        return crate::sync::stores::data_dir().join("settings.json");
    }
    #[cfg(not(test))]
    {
        let mut path = data_local_dir().unwrap_or_else(|| PathBuf::from("."));
        path.push("Fluence");
        path.push("settings.json");
        path
    }
}

/// One-time migration for the retired Moonshine v1 batch engine (removed
/// like on Android: English-only, ~2x the size of its v2 Small successor,
/// no unique capability). Maps legacy saves to the English successor so
/// existing users keep an English offline engine instead of falling back
/// to multilingual SenseVoice via the workflow parse fallback. Idempotent:
/// returns true only when a migration was applied.
fn migrate_retired_offline_engine(settings: &mut AppSettings) -> bool {
    if settings.offline_engine == "moonshine_base" {
        settings.offline_engine = "moonshine_v2_small".to_string();
        return true;
    }
    false
}

/// One-time migration for the new dedicated AI-cleaner provider. Files
/// saved before `cleaner_provider` existed have no such key: copy the
/// current LLM provider over so cleanup keeps working exactly as before
/// (same endpoint, same key slot family, same model) until the user picks
/// something else in Providers > AI cleaner. Idempotent: returns true only
/// when a migration was applied.
fn migrate_missing_cleaner_provider(raw: &str, settings: &mut AppSettings) -> bool {
    let has_key = serde_json::from_str::<serde_json::Value>(raw)
        .ok()
        .map(|v| v.get("cleaner_provider").is_some())
        .unwrap_or(false);
    if has_key {
        return false;
    }
    settings.cleaner_provider = settings.llm_provider.clone();
    true
}

/// Post-parse migrations applied on every settings load. Returns true
/// when any migration changed the settings (the caller persists once).
/// Pure over (raw_json, settings): unit-testable without touching the
/// real settings file, so no test seam, env var, or cross-test locking
/// is needed.
fn apply_load_migrations(raw: &str, settings: &mut AppSettings) -> bool {
    let mut migrated = false;
    if migrate_retired_offline_engine(settings) {
        log::info!("Migrated retired offline engine 'moonshine_base' to 'moonshine_v2_small'");
        migrated = true;
    }
    if migrate_missing_cleaner_provider(raw, settings) {
        log::info!("AI cleaner provider missing: inherited current LLM provider");
        migrated = true;
    }
    migrated
}

pub fn load_settings() -> Result<AppSettings> {
    let path = settings_path();
    log::debug!("Loading settings from path: {:?}", path);
    if !path.exists() {
        log::info!("Settings file does not exist, creating default settings");
        let settings = AppSettings::default();
        save_settings(&settings)?;
        return Ok(settings);
    }
    let data = fs::read_to_string(&path)?;
    match serde_json::from_str::<AppSettings>(&data) {
        Ok(mut settings) => {
            log::debug!("Successfully loaded settings");
            if apply_load_migrations(&data, &mut settings) {
                // Persist once so the mapping is not re-applied on every
                // load. A failed write is harmless: the in-memory value is
                // already migrated and the next load retries idempotently.
                if let Err(e) = save_settings(&settings) {
                    log::warn!("Failed to persist migrated settings: {:?}", e);
                }
            }
            Ok(settings)
        }
        Err(e) => {
            log::error!("Failed to deserialize settings JSON: {:?}", e);
            // Atomic recovery: rename corrupt file and return fresh defaults
            let corrupt_path = path.with_extension("json.corrupt.json");
            // Ensure unique name if .corrupt already exists
            let mut target = corrupt_path.clone();
            let mut counter = 1;
            while target.exists() {
                target = path.with_extension(format!("json.corrupt.{}.json", counter));
                counter += 1;
            }
            if let Err(rename_err) = fs::rename(&path, &target) {
                log::error!("Failed to rename corrupt settings file: {}", rename_err);
                // Fallback: try copy+delete
                if fs::copy(&path, &target).is_ok() {
                    let _ = fs::remove_file(&path);
                }
            }
            log::warn!(
                "Corrupted settings file renamed to {:?}. Creating fresh defaults.",
                target
            );
            let settings = AppSettings::default();
            // Preserve first_run=false if the corrupt file existed (was not a fresh install)
            // so we do not reshown wizard after a mid-write corruption.
            // Note: we cannot parse the corrupt file, so we conservatively keep first_run false
            // to avoid disrupting returning users; fresh installs have no file at all.
            let mut repaired = settings;
            repaired.first_run = false;
            let _ = save_settings(&repaired);
            Ok(repaired)
        }
    }
}

pub fn save_settings(settings: &AppSettings) -> Result<()> {
    let path = settings_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let data = serde_json::to_string_pretty(settings)?;
    // Atomic write: tmp + sync_all + rename (matches suggestion.rs + ducking.rs).
    //
    // The tmp name MUST be unique per caller. A shared name meant two concurrent
    // writers both wrote the same tmp, then both renamed it: the loser renamed a
    // tmp the winner had already consumed and failed with os error 2, or renamed a
    // partially-written tmp onto the destination. Production reaches this too —
    // the scheduler thread and the settings UI can both persist — and it was the
    // root cause of the intermittent audio perf-test flake (load_settings writes
    // settings.json while sync tests write the same shared test data dir).
    let tmp_path = {
        static TMP_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let seq = TMP_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        // Same directory as the destination so the rename stays same-volume atomic.
        let mut name = path.clone().into_os_string();
        name.push(format!(".tmp.{}.{}", std::process::id(), seq));
        PathBuf::from(name)
    };
    let write_result = (|| -> Result<()> {
        fs::write(&tmp_path, &data)?;
        // Flush via a WRITE handle. `fs::File::open` is O_RDONLY and on Windows
        // `sync_all` -> `FlushFileBuffers` needs `GENERIC_WRITE`, so the old
        // `File::open(..) { let _ = f.sync_all(); }` performed no flush while
        // claiming to. Best effort, non-fatal: this can only add durability.
        if let Ok(f) = fs::OpenOptions::new().write(true).open(&tmp_path) {
            let _ = f.sync_all();
        }
        fs::rename(&tmp_path, &path)?;
        Ok(())
    })();
    if write_result.is_err() {
        // Best effort: never leave a stray tmp behind on the failure path.
        let _ = fs::remove_file(&tmp_path);
    }
    write_result
}

/// The `inject_text` clipboard-restore decision, pure so it is
/// unit-testable. Some(true) keeps the transcript on the clipboard
/// (restore skipped, issue #10); Some(false) restores the user's
/// pre-injection clipboard; None (failed settings read) also restores,
/// the fail-safe that matches pre-feature behavior.
pub fn should_restore_clipboard(settings: Option<&AppSettings>) -> bool {
    !settings
        .map(|s| s.copy_transcript_to_clipboard)
        .unwrap_or(false)
}

// Tauri commands
#[tauri::command]
pub fn get_settings() -> Result<AppSettings, String> {
    let mut settings = load_settings().map_err(|e| e.to_string())?;
    // `get_settings` is deliberately UNGATED — the wizard and overlay need it —
    // so it must not be a wider path to the signed-in Google email than the
    // `main`-only `sync-status` emit it was supposed to be narrower than. The
    // S2 remediation scoped `account_key` to the main window precisely because
    // the raw email is PII; returning it here re-opened exactly that exposure to
    // any script that reached the overlay or wizard webview.
    //
    // Nothing in the frontend reads `sync_account_key` from this command — the
    // Sync page gets it from `sync_get_status`, which is main-only. `sync_enabled`
    // IS read (`settings.js` binds it to a checkbox) and is only a boolean, so
    // it stays. Internal callers use `load_settings()` directly and are
    // unaffected by this redaction.
    settings.sync_account_key = None;
    Ok(settings)
}

#[tauri::command]
pub fn update_settings(
    app: tauri::AppHandle,
    window: tauri::Window,
    settings: AppSettings,
    scheduler: tauri::State<'_, crate::sync::scheduler::Scheduler>,
) -> Result<(), String> {
    crate::acl::require_caller(
        &window,
        &[crate::acl::MAIN_WINDOW, crate::acl::WIZARD_WINDOW],
    )?;
    let current = load_settings().ok();
    let old_account = current.as_ref().and_then(|s| s.sync_account_key.clone());
    // This command receives the frontend's FULL `AppSettings`, which is
    // routinely a STALE copy — the UI does not refresh after `sync_toggle` or a
    // sign-in. Persisting `sync_enabled` / `sync_account_key` straight from it
    // therefore REVERTS them: toggle sync off, then change any unrelated
    // setting, and the debounced flush writes `sync_enabled: true` back a few
    // hundred ms later. The running scheduler is never told, so nothing looks
    // wrong until a RESTART, at which point automatic sync resumes against an
    // explicit user opt-out. The same path re-adopts a stale account key after
    // sign-out, so the Sync page offers to "reconnect" as an account the user
    // already left.
    //
    // Both fields are owned by dedicated commands (`sync_toggle`,
    // `sync_sign_in`, `sync_sign_out`, and the scheduler's own reconcile), which
    // write settings directly and are unaffected. A general settings flush must
    // not touch them. If the current file cannot be read we fall through to the
    // incoming object rather than blocking the user's write entirely.
    let mut settings = settings;
    if let Some(ref c) = current {
        settings.sync_enabled = c.sync_enabled;
        settings.sync_account_key = c.sync_account_key.clone();
    }
    save_settings(&settings).map_err(|e| e.to_string())?;
    // Account switch via any path (frontend, file, scheduler) must drop
    // compiled caches that were keyed to the previous account - otherwise
    // W1/W8 stale-cache shows foreign snippets/dictionary until next write.
    if old_account != settings.sync_account_key {
        crate::dictionary::invalidate_cache();
        crate::snippets::invalidate_cache();
    }
    scheduler.command(crate::sync::scheduler::SyncCommand::LocalChange);

    let lang = crate::i18n::resolve(&settings.ui_language);
    let _ = crate::tray::refresh_menu(&app);
    {
        use tauri::Manager;
        if let Some(win) = app.get_webview_window("main") {
            let _ = win.set_title(crate::i18n::tr(&lang, "Fluence Settings"));
        }
        if let Some(win) = app.get_webview_window("wizard") {
            let _ = win.set_title(crate::i18n::tr(&lang, "Welcome to Fluence"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Highest-priority safety default: a settings.json written before the
    /// streaming feature existed (no `stt_streaming_enabled` key at all)
    /// must load with streaming OFF, so no existing install can silently
    /// start streaming audio to a realtime endpoint after an update.
    #[test]
    fn legacy_settings_without_streaming_flag_default_off() {
        let legacy = r#"{
            "hotkey": "Ctrl+Shift+Space",
            "language": "fr",
            "stt_provider": {
                "preset": "groq",
                "base_url": "https://api.groq.com/openai",
                "model": "whisper-large-v3",
                "api_key_saved": true
            }
        }"#;
        let settings: AppSettings =
            serde_json::from_str(legacy).expect("legacy settings must parse");
        assert!(
            !settings.stt_streaming_enabled,
            "streaming must default OFF for legacy settings files"
        );
        // Everything else still parses around the missing key.
        assert_eq!(settings.language, "fr");
        assert_eq!(settings.stt_provider.preset, "groq");
    }

    #[test]
    fn streaming_flag_round_trips_when_present() {
        let on = r#"{"stt_streaming_enabled": true}"#;
        let off = r#"{"stt_streaming_enabled": false}"#;
        let enabled: AppSettings = serde_json::from_str(on).unwrap();
        let disabled: AppSettings = serde_json::from_str(off).unwrap();
        assert!(enabled.stt_streaming_enabled);
        assert!(!disabled.stt_streaming_enabled);
        assert!(!AppSettings::default().stt_streaming_enabled);
    }

    /// Settings files written before copy_transcript_to_clipboard existed
    /// (issue #10) must parse and load the requested default: ON, so the
    /// paste-again fallback works for existing installs. An explicit OFF
    /// in the file is honored (restore-the-user's-clipboard behavior).
    #[test]
    fn legacy_settings_without_copy_transcript_flag_default_on() {
        let legacy = r#"{"language": "fr"}"#;
        let settings: AppSettings =
            serde_json::from_str(legacy).expect("legacy settings must parse");
        assert!(
            settings.copy_transcript_to_clipboard,
            "copy-to-clipboard must default ON for legacy settings files"
        );
        assert_eq!(settings.language, "fr");
        let off: AppSettings =
            serde_json::from_str(r#"{"copy_transcript_to_clipboard": false}"#).unwrap();
        assert!(!off.copy_transcript_to_clipboard);
    }

    /// The restore-gate decision used by `inject_text`: ON keeps the
    /// transcript on the clipboard (restore skipped), OFF restores, and a
    /// failed settings read (None) restores too. Also asserts the
    /// `Default` impl agrees with the serde default so the two cannot
    /// drift (fresh installs take the ON path via `Default`).
    #[test]
    fn clipboard_restore_gate_failsafe_restores() {
        let mut settings = AppSettings::default();
        assert!(
            settings.copy_transcript_to_clipboard,
            "fresh-install default must be ON (issue #10)"
        );
        assert!(!should_restore_clipboard(Some(&settings)));
        settings.copy_transcript_to_clipboard = false;
        assert!(should_restore_clipboard(Some(&settings)));
        assert!(
            should_restore_clipboard(None),
            "unreadable settings must keep today's restore behavior"
        );
    }

    /// Removed experiment keys (exp_omit_temperature, exp_verbose_json_filter,
    /// exp_agc_boost, exp_silence_gate) must not break parsing of settings
    /// files written while they existed (serde ignores unknown keys). The
    /// silence gate itself runs unconditionally in the audio stop path.
    #[test]
    fn removed_experiment_flag_keys_still_parse() {
        let old = r#"{"exp_omit_temperature": true, "exp_verbose_json_filter": true, "exp_agc_boost": true, "exp_silence_gate": true}"#;
        let settings: AppSettings = serde_json::from_str(old).unwrap();
        assert_eq!(settings.language, "en");
    }

    /// Files saved before `cleaner_provider` existed inherit the current
    /// LLM provider (same endpoint/model/key family), so cleanup behavior
    /// is unchanged; files that already carry the key pass through
    /// untouched; the mapping is idempotent.
    #[test]
    fn missing_cleaner_provider_inherits_llm_provider() {
        let raw = r#"{"llm_provider": {"preset": "openai", "base_url": "https://api.openai.com", "model": "gpt-4o", "api_key_saved": true}}"#;
        let mut legacy: AppSettings = serde_json::from_str(raw).unwrap();
        // Serde default fills the hardcoded default first...
        assert_eq!(legacy.cleaner_provider.preset, "groq");
        // ...then the migration copies the user's LLM provider over.
        assert!(migrate_missing_cleaner_provider(raw, &mut legacy));
        assert_eq!(legacy.cleaner_provider.preset, "openai");
        assert_eq!(legacy.cleaner_provider.model, "gpt-4o");
        // Second load is a no-op (key now present after persist).
        let resaved = serde_json::to_string(&legacy).unwrap();
        let mut again: AppSettings = serde_json::from_str(&resaved).unwrap();
        assert!(!migrate_missing_cleaner_provider(&resaved, &mut again));
        assert_eq!(again.cleaner_provider.preset, "openai");
    }

    /// Load-path wiring: the composed post-parse migration inherits the
    /// LLM provider for keyless files (its true return drives the
    /// caller's one-time persist) and is a no-op once the key exists.
    /// Pure strings + structs: no settings file, no env, no cross-test
    /// interference.
    #[test]
    fn load_migrations_inherit_cleaner_provider_for_legacy_files() {
        let raw = r#"{"llm_provider": {"preset": "openai", "base_url": "https://api.openai.com", "model": "gpt-4o", "api_key_saved": true}}"#;
        let mut legacy: AppSettings = serde_json::from_str(raw).unwrap();
        assert!(apply_load_migrations(raw, &mut legacy));
        assert_eq!(legacy.cleaner_provider.preset, "openai");
        assert_eq!(legacy.cleaner_provider.model, "gpt-4o");
        let resaved = serde_json::to_string(&legacy).unwrap();
        let mut again: AppSettings = serde_json::from_str(&resaved).unwrap();
        assert!(!apply_load_migrations(&resaved, &mut again));
        assert_eq!(again.cleaner_provider.preset, "openai");
    }

    /// Retired Moonshine v1 batch engine: legacy saves migrate to the
    /// English successor (v2 Small), never to multilingual SenseVoice;
    /// current values pass through untouched; the mapping is idempotent.
    #[test]
    fn retired_moonshine_base_engine_migrates_to_v2_small() {
        let mut legacy: AppSettings =
            serde_json::from_str(r#"{"offline_engine": "moonshine_base"}"#).unwrap();
        assert!(migrate_retired_offline_engine(&mut legacy));
        assert_eq!(legacy.offline_engine, "moonshine_v2_small");
        // Second load is a no-op.
        assert!(!migrate_retired_offline_engine(&mut legacy));

        for current in ["sensevoice", "moonshine_v2_small", "moonshine_v2_medium"] {
            let mut settings: AppSettings =
                serde_json::from_str(&format!(r#"{{"offline_engine": "{current}"}}"#)).unwrap();
            assert!(!migrate_retired_offline_engine(&mut settings));
            assert_eq!(settings.offline_engine, current);
        }
    }

    /// Regression test for the shared-tmp-path race in `save_settings`.
    ///
    /// `save_settings` writes via tmp-then-rename. While the tmp name was fixed,
    /// two concurrent writers both wrote the same tmp and then both renamed it:
    /// the loser renamed a tmp the winner had already consumed (os error 2), or
    /// renamed a half-written tmp onto the destination. This reproduced 10/10
    /// before the fix and is what made the audio perf tests flake — they call
    /// `load_settings`, which writes settings.json, while sync tests write the
    /// same shared per-process test data dir.
    ///
    /// Production is exposed the same way: the scheduler thread and the settings
    /// UI can both persist settings.
    #[test]
    fn concurrent_saves_of_the_same_settings_file_all_succeed() {
        // Serialize against the other tests that mutate the shared on-disk
        // settings file under the cfg(test) data root.
        let _guard = crate::sync::stores::store_test_guard();

        let threads = 8;
        let writes_each = 25;
        let mut handles = Vec::new();
        for i in 0..threads {
            handles.push(std::thread::spawn(move || {
                let mut settings = AppSettings::default();
                settings.language = format!("lang-{i}");
                for _ in 0..writes_each {
                    // The real code path, not a reimplementation.
                    save_settings(&settings)
                        .unwrap_or_else(|e| panic!("save {i} must not fail: {e}"));
                }
                settings
            }));
        }

        // Every writer must complete without error: this is the assertion that
        // failed 10/10 before the fix.
        let settled: Vec<_> = handles
            .into_iter()
            .map(|h| h.join().expect("no writer thread may panic"))
            .collect();
        assert_eq!(settled.len(), threads);

        // Destination must survive the race: present, valid JSON, and holding one
        // of the written values — never a truncated, empty, or torn file.
        let path = settings_path();
        let raw = fs::read_to_string(&path).expect("settings.json must exist after racing saves");
        let parsed: AppSettings =
            serde_json::from_str(&raw).expect("settings.json must parse after racing saves");
        let valid: Vec<String> = (0..threads).map(|i| format!("lang-{i}")).collect();
        assert!(
            valid.contains(&parsed.language),
            "settled language {:?} must be one of the complete writes {:?}, not a torn write",
            parsed.language,
            valid
        );
    }
}
