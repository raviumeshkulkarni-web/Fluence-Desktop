// Fluence sync — global scheduler + settings UI wiring (spec §21, §27 row 7).
//
// One scheduler per process. A background thread owns the pass loop: it waits
// on a command channel for the cadence interval, then runs one engine pass
// over every record kind (History, Dictionary, Snippet, Settings) and emits a
// `sync-status` Tauri event. Scheduling is gated by three flags — sync
// enabled (settings), signed in (refresh token present), and a fatal-error
// latch (only a manual command re-arms automatic scheduling). A "sync now"
// request while a pass is running coalesces into exactly one follow-up pass
// (single-flight). Retryable failures advance the drive `Backoff` (1000 ms ×2,
// cap 60 s); successful passes reset it.
//
// Secrets and persistence follow the security contract: the client secret is
// never committed — it comes from the `FLUENCE_SYNC_CLIENT_SECRET` environment
// variable or `Fluence/sync-oauth.json` ({"client_secret": "..."}) at runtime.
// The access token is memory-only; the refresh token lives in the OS
// credential store (`credentials::Fluence/Sync/RefreshToken`). No transcript
// text is ever logged or surfaced.

use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

use crate::dictionary::sync_store::DictionarySyncStore;
use crate::history::HistorySyncStore;
use crate::snippets::sync_store::SnippetSyncStore;
use crate::sync::auth::{self, AuthSession};
use crate::sync::drive::{Backoff, GoogleDriveStore};
use crate::sync::engine::{self, SyncError, SyncOutcome};
use crate::sync::settings_store::SyncSettingsStore;
use crate::sync::wire::RecordType;

// ---------------------------------------------------------------------------
// Public constants (reported to the user; Exp 4 fixed the client ID and port).
// ---------------------------------------------------------------------------

/// Google OAuth client ID recorded in Exp 4 — public by design.
pub const SYNC_CLIENT_ID: &str =
    "236666538373-005rdohmcf6cgh0in10v5v8nhcc1m85k.apps.googleusercontent.com";
/// Loopback redirect port validated in Exp 4.
pub const SYNC_REDIRECT_PORT: u16 = 58611;
/// Automatic pass cadence: one pass every 5 minutes while enabled + signed in.
pub const SYNC_CADENCE_MS: u64 = 300_000;
/// Backoff after a retryable failure: 1000 ms base, ×2, capped at 60 s.
pub const SYNC_BACKOFF_BASE_MS: u64 = 1_000;
pub const SYNC_BACKOFF_FACTOR: u32 = 2;
pub const SYNC_BACKOFF_CAP_MS: u64 = 60_000;
/// Idle wake-up poll cap (disabled / signed out / fatal latch).
const SYNC_IDLE_POLL_MS: u64 = 3_600_000;

const SYNC_CLIENT_SECRET_ENV: &str = "FLUENCE_SYNC_CLIENT_SECRET";
const SYNC_OAUTH_CONFIG_FILE: &str = "sync-oauth.json";
const SYNC_USERINFO_URL: &str = "https://www.googleapis.com/oauth2/v3/userinfo";
const SYNC_SECRET_MISSING_MSG: &str = "sync client secret is not configured — set the \
    FLUENCE_SYNC_CLIENT_SECRET environment variable or create Fluence/sync-oauth.json \
    with {\"client_secret\": \"...\"}";

// ---------------------------------------------------------------------------
// Command channel + pure scheduler core (unit-testable without threads).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncCommand {
    /// Manual pass now (coalesces while running; re-arms after a fatal error).
    RunNow,
    /// User toggled sync in settings.
    SetEnabled(bool),
    /// Sign-in succeeded: signed_in = true, immediate pass, backoff reset.
    SignedIn,
    /// Sign-out: scheduling stops until the next sign-in.
    SignedOut,
    /// Stop the background thread.
    Shutdown,
}

/// How a finished pass affects future scheduling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PassOutcomeKind {
    Success,
    /// Pass ran but reported retryable failures, or ended on a retryable error.
    Retryable,
    /// Fatal or NotOurs error — automatic scheduling stops until a command.
    Fatal,
    /// 401 — the refresh token is gone; the user must sign in again.
    AuthRequired,
}

/// Pure scheduling state machine. All transitions are deterministic given the
/// command stream and injected wall-clock millis, so the thread logic is
/// fully unit-testable.
#[derive(Debug)]
pub struct SchedulerCore {
    pub enabled: bool,
    pub signed_in: bool,
    pub running: bool,
    pub pending_run: bool,
    pub wait_for_command: bool,
    pub last_attempt_ms: Option<i64>,
    pub last_sync_at: Option<i64>,
    pub last_error: Option<String>,
    pub backoff: Backoff,
    pub backoff_active: bool,
    /// The delay gating the next attempt after a retryable failure (the value
    /// `next_delay_ms` returned; the backoff itself pre-steps for later).
    pub retry_delay_ms: u64,
}

impl SchedulerCore {
    pub fn new(enabled: bool, signed_in: bool) -> Self {
        Self {
            enabled,
            signed_in,
            running: false,
            pending_run: false,
            wait_for_command: false,
            last_attempt_ms: None,
            last_sync_at: None,
            last_error: None,
            backoff: Backoff::new(
                SYNC_BACKOFF_BASE_MS,
                SYNC_BACKOFF_FACTOR,
                SYNC_BACKOFF_CAP_MS,
            ),
            backoff_active: false,
            retry_delay_ms: SYNC_BACKOFF_BASE_MS,
        }
    }

    pub fn apply(&mut self, cmd: &SyncCommand) {
        match cmd {
            SyncCommand::RunNow => {
                self.pending_run = true;
                self.wait_for_command = false;
            }
            SyncCommand::SetEnabled(enabled) => {
                self.enabled = *enabled;
                if *enabled && self.signed_in {
                    self.pending_run = true;
                }
            }
            SyncCommand::SignedIn => {
                self.signed_in = true;
                self.wait_for_command = false;
                self.last_error = None;
                self.pending_run = true;
                self.backoff.reset();
                self.backoff_active = false;
                self.retry_delay_ms = SYNC_BACKOFF_BASE_MS;
            }
            SyncCommand::SignedOut => {
                self.signed_in = false;
                self.pending_run = false;
                self.wait_for_command = false;
                self.backoff_active = false;
            }
            SyncCommand::Shutdown => {}
        }
    }

    /// How long the thread should sleep before the next opportunity to run a
    /// pass; `None` = no automatic run is possible (block on the channel).
    pub fn wait_ms(&self, now_ms: i64) -> Option<u64> {
        if !self.enabled || !self.signed_in || self.wait_for_command {
            return None;
        }
        if self.running {
            return Some(SYNC_CADENCE_MS);
        }
        if self.pending_run {
            return Some(0);
        }
        let interval = if self.backoff_active {
            self.retry_delay_ms
        } else {
            SYNC_CADENCE_MS
        };
        match self.last_attempt_ms {
            None => Some(0),
            Some(last) => Some(interval.saturating_sub((now_ms - last).max(0) as u64)),
        }
    }

    /// Start a pass if one is due: `true` claims the run (sets `running`,
    /// records `last_attempt_ms`). Single-flight: while `running`, this is
    /// always `false` and `pending_run` keeps coalescing requests.
    pub fn take_run(&mut self, now_ms: i64) -> bool {
        if self.running || !self.enabled || !self.signed_in || self.wait_for_command {
            return false;
        }
        if !self.pending_run {
            let interval = if self.backoff_active {
                self.retry_delay_ms
            } else {
                SYNC_CADENCE_MS
            };
            if let Some(last) = self.last_attempt_ms {
                if now_ms - last < interval as i64 {
                    return false;
                }
            }
        }
        self.pending_run = false;
        self.running = true;
        self.last_attempt_ms = Some(now_ms);
        true
    }

    /// Record a finished pass. `pending_run` set during the pass survives, so
    /// a coalesced "sync now" produces exactly one follow-up pass.
    pub fn finish(&mut self, outcome: PassOutcomeKind, error: Option<String>, _now_ms: i64) {
        self.running = false;
        match outcome {
            PassOutcomeKind::Success => {
                self.backoff.reset();
                self.backoff_active = false;
                self.last_error = None;
                if error.is_none() {
                    self.last_sync_at = Some(_now_ms);
                }
            }
            PassOutcomeKind::Retryable => {
                // `next_delay_ms` returns the delay for the NEXT attempt and
                // pre-steps the backoff for a further failure.
                self.retry_delay_ms = self.backoff.next_delay_ms();
                self.backoff_active = true;
                self.last_error = error;
            }
            PassOutcomeKind::Fatal => {
                self.backoff.reset();
                self.backoff_active = false;
                self.retry_delay_ms = SYNC_BACKOFF_BASE_MS;
                self.wait_for_command = true;
                self.last_error = error;
            }
            PassOutcomeKind::AuthRequired => {
                self.signed_in = false;
                self.pending_run = false;
                self.backoff_active = false;
                self.retry_delay_ms = SYNC_BACKOFF_BASE_MS;
                self.last_error = error;
            }
        }
    }
}

/// Status snapshot serialized to the frontend and emitted as `sync-status`.
#[derive(Debug, Clone, Serialize)]
pub struct SyncStatus {
    pub enabled: bool,
    pub signed_in: bool,
    pub account_key: Option<String>,
    pub running: bool,
    pub last_sync_at: Option<i64>,
    pub last_error: Option<String>,
    /// Absolute epoch millis of the next scheduled attempt (null when idle).
    pub next_attempt_ms: Option<i64>,
}

fn build_status(core: &SchedulerCore) -> SyncStatus {
    let now_ms = chrono::Utc::now().timestamp_millis();
    let next_attempt_ms = core.wait_ms(now_ms).map(|ms| now_ms + ms as i64);
    let account_key = crate::settings::load_settings()
        .ok()
        .and_then(|s| s.sync_account_key);
    SyncStatus {
        enabled: core.enabled,
        signed_in: core.signed_in,
        account_key,
        running: core.running,
        last_sync_at: core.last_sync_at,
        last_error: core.last_error.clone(),
        next_attempt_ms,
    }
}

// ---------------------------------------------------------------------------
// Scheduler handle + background thread.
// ---------------------------------------------------------------------------

/// Managed handle registered with Tauri (`app.manage`); commands read status
/// through it and push commands through its channel.
pub struct Scheduler {
    core: Arc<Mutex<SchedulerCore>>,
    tx: Sender<SyncCommand>,
    app: AppHandle,
}

impl Scheduler {
    /// Start the background thread and return the managed handle. Initial
    /// state is derived from persisted settings (sync_enabled) and the
    /// presence of a stored refresh token.
    pub fn spawn(app: AppHandle) -> Self {
        let settings = crate::settings::load_settings().unwrap_or_default();
        let signed_in = crate::credentials::read_sync_refresh_token().is_ok();
        let core = Arc::new(Mutex::new(SchedulerCore::new(
            settings.sync_enabled,
            signed_in,
        )));
        let (tx, rx) = channel();
        let thread_app = app.clone();
        let thread_core = core.clone();
        std::thread::Builder::new()
            .name("sync-scheduler".to_string())
            .spawn(move || scheduler_thread(thread_app, thread_core, rx))
            .expect("failed to spawn sync scheduler thread");
        Self { core, tx, app }
    }

    pub fn command(&self, cmd: SyncCommand) {
        let _ = self.tx.send(cmd);
    }

    pub fn status(&self) -> SyncStatus {
        let core = self.core.lock().unwrap_or_else(|e| e.into_inner());
        build_status(&core)
    }

    pub fn emit_status(&self) {
        let _ = self.app.emit("sync-status", self.status());
    }
}

fn scheduler_thread(app: AppHandle, core: Arc<Mutex<SchedulerCore>>, rx: Receiver<SyncCommand>) {
    loop {
        let wait = core
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .wait_ms(chrono::Utc::now().timestamp_millis());
        let recv = match wait {
            Some(ms) => rx.recv_timeout(Duration::from_millis(ms)),
            None => rx.recv_timeout(Duration::from_millis(SYNC_IDLE_POLL_MS)),
        };
        match recv {
            Ok(SyncCommand::Shutdown) => break,
            Ok(cmd) => core.lock().unwrap_or_else(|e| e.into_inner()).apply(&cmd),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }

        let now_ms = chrono::Utc::now().timestamp_millis();
        let start = core
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take_run(now_ms);
        if !start {
            continue;
        }
        log::info!("sync pass starting");
        let (kind, error) = match run_pass() {
            Ok(outcome) if outcome.retryable_failures > 0 => (
                PassOutcomeKind::Retryable,
                Some(format!(
                    "{} retryable operation(s) failed this pass",
                    outcome.retryable_failures
                )),
            ),
            Ok(_) => (PassOutcomeKind::Success, None),
            Err(SyncError::AuthRequired) => (
                PassOutcomeKind::AuthRequired,
                Some("authentication required — sign in again".to_string()),
            ),
            Err(SyncError::Retryable(e)) => (PassOutcomeKind::Retryable, Some(e.to_string())),
            Err(SyncError::Fatal(e)) => (PassOutcomeKind::Fatal, Some(e.to_string())),
            Err(SyncError::NotOurs) => {
                (PassOutcomeKind::Fatal, Some(SyncError::NotOurs.to_string()))
            }
        };
        core.lock()
            .unwrap_or_else(|e| e.into_inner())
            .finish(kind, error, now_ms);
        log::info!("sync pass finished ({:?})", kind);
        let _ = app.emit(
            "sync-status",
            build_status(&core.lock().unwrap_or_else(|e| e.into_inner())),
        );
    }
}

// ---------------------------------------------------------------------------
// The pass driver (spec §30.1: one `engine::run` per kind, in order).
// ---------------------------------------------------------------------------

/// Record kinds in pass order. The Settings pass runs last so its mirrored
/// value (snippets_enabled) wins the §30.3 toggle race on a single pass.
const KINDS: [RecordType; 4] = [
    RecordType::History,
    RecordType::Dictionary,
    RecordType::Snippet,
    RecordType::Settings,
];

/// Build the OAuth config. The client secret is resolved at runtime and only
/// attached when present — never hard-coded, never persisted.
fn build_config(secret: Option<String>) -> auth::OAuthConfig {
    let mut config = auth::OAuthConfig::google(SYNC_CLIENT_ID.to_string(), SYNC_REDIRECT_PORT);
    config.client_secret = secret;
    config
}

fn sync_config() -> auth::OAuthConfig {
    build_config(resolve_client_secret().ok())
}

/// The real pass: refresh-token → access token → one engine pass per kind,
/// stores rebuilt from their persisted state each time. Returns the first
/// kind-level error, aborting the pass (nothing further mutates).
fn run_pass() -> Result<SyncOutcome, SyncError> {
    let settings = crate::settings::load_settings()
        .map_err(|e| SyncError::Fatal(format!("failed to load settings for sync: {e}")))?;
    let account = settings.sync_account_key.clone();

    let config = sync_config();
    if config.client_secret.is_none() {
        // Exp 4: Google requires the client secret at the token endpoint even
        // with PKCE; without it no refresh can ever succeed. Fail fast with a
        // clear, transcript-free message instead of retry-bombing.
        return Err(SyncError::Fatal(SYNC_SECRET_MISSING_MSG.to_string()));
    }
    let mut session = AuthSession::new(config);
    session.load_refresh_token().map_err(SyncError::Fatal)?;
    if session.refresh_token.is_none() {
        return Err(SyncError::AuthRequired);
    }
    let token = ensure_access_token(&mut session)?;

    let mut drive = GoogleDriveStore::new(token);
    let mut history = HistorySyncStore::new();
    let mut dictionary = DictionarySyncStore::new();
    let mut snippets = SnippetSyncStore::new();
    let mut settings_store = SyncSettingsStore::new(sync_settings_path());

    let mut outcome = SyncOutcome::default();
    for kind in KINDS {
        let o = match kind {
            RecordType::History => engine::run(
                kind,
                account.as_deref(),
                &mut history,
                &mut drive,
                &mut session,
            ),
            RecordType::Dictionary => engine::run(
                kind,
                account.as_deref(),
                &mut dictionary,
                &mut drive,
                &mut session,
            ),
            RecordType::Snippet => engine::run(
                kind,
                account.as_deref(),
                &mut snippets,
                &mut drive,
                &mut session,
            ),
            RecordType::Settings => engine::run(
                kind,
                account.as_deref(),
                &mut settings_store,
                &mut drive,
                &mut session,
            ),
        }?;
        outcome.imported += o.imported;
        outcome.created += o.created;
        outcome.reuploaded += o.reuploaded;
        outcome.patches += o.patches;
        outcome.tombstoned_local += o.tombstoned_local;
        outcome.quarantined += o.quarantined;
        outcome.hard_deleted += o.hard_deleted;
        outcome.retryable_failures += o.retryable_failures;
    }

    // §30.3: mirror the synced snippets_enabled row into the live toggle.
    settings_store.mirror_enabled(&mut |enabled| {
        let _ = crate::snippets::set_snippets_enabled(enabled);
    });
    Ok(outcome)
}

/// Access token with an in-memory refresh when needed (spec §24). A 400/401
/// from the token endpoint means the refresh token was revoked — the user
/// must sign in again.
fn ensure_access_token(session: &mut AuthSession) -> Result<String, SyncError> {
    if let Some(token) = session.access_token() {
        return Ok(token.to_string());
    }
    let Some(refresh) = session.refresh_token.clone() else {
        return Err(SyncError::AuthRequired);
    };
    let config = session.config.clone();
    match tauri::async_runtime::block_on(auth::refresh_access_token(
        &config,
        &crate::http_client::CLIENT,
        &refresh,
    )) {
        Ok(response) => {
            session.store_tokens(&response);
            session
                .access_token()
                .map(str::to_string)
                .ok_or(SyncError::AuthRequired)
        }
        Err(auth::AuthError::Http { status, .. }) if status == 400 || status == 401 => {
            Err(SyncError::AuthRequired)
        }
        Err(e) => Err(SyncError::Retryable(format!("token refresh failed: {e}"))),
    }
}

fn sync_settings_path() -> Option<std::path::PathBuf> {
    let mut path = dirs::data_local_dir()?;
    path.push("Fluence");
    path.push("sync-settings.json");
    Some(path)
}

// ---------------------------------------------------------------------------
// Client secret resolution (runtime only — never committed) and account key.
// ---------------------------------------------------------------------------

fn oauth_config_path() -> Option<std::path::PathBuf> {
    let mut path = dirs::data_local_dir()?;
    path.push("Fluence");
    path.push(SYNC_OAUTH_CONFIG_FILE);
    Some(path)
}

/// Pure resolution: env var wins over the config file JSON. Both absent →
/// error with the setup hint.
pub fn resolve_client_secret_from(
    env: Option<&str>,
    config_json: Option<&str>,
) -> Result<String, String> {
    if let Some(v) = env.map(str::trim).filter(|v| !v.is_empty()) {
        return Ok(v.to_string());
    }
    if let Some(json) = config_json {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(json) {
            if let Some(secret) = value.get("client_secret").and_then(|v| v.as_str()) {
                let secret = secret.trim();
                if !secret.is_empty() {
                    return Ok(secret.to_string());
                }
            }
        }
    }
    Err(SYNC_SECRET_MISSING_MSG.to_string())
}

/// Read the secret from the environment, falling back to
/// `Fluence/sync-oauth.json` in the app data directory.
pub fn resolve_client_secret() -> Result<String, String> {
    let env = std::env::var(SYNC_CLIENT_SECRET_ENV).ok();
    let file = oauth_config_path().and_then(|p| std::fs::read_to_string(p).ok());
    resolve_client_secret_from(env.as_deref(), file.as_deref())
}

/// Extract the account key (email) from the Google userinfo response.
pub fn parse_account_email(json: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    let email = value.get("email")?.as_str()?;
    if email.trim().is_empty() {
        None
    } else {
        Some(email.to_string())
    }
}

/// Open the authorization URL in the system browser. Windows: `cmd /c start`
/// — no shell plugin, no new Tauri capability needed.
fn open_browser(url: &str) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        std::process::Command::new("cmd")
            .arg("/c")
            .arg("start")
            .arg("")
            .arg(url)
            .spawn()
            .map(|_| ())
    }
    #[cfg(not(windows))]
    {
        std::process::Command::new("xdg-open")
            .arg(url)
            .spawn()
            .map(|_| ())
    }
}

// ---------------------------------------------------------------------------
// Tauri commands (command names are part of the stable frontend contract).
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn sync_get_status(scheduler: State<'_, Scheduler>) -> Result<SyncStatus, String> {
    Ok(scheduler.status())
}

/// Enable/disable sync and persist the flag. Calling with the current value
/// is idempotent and forces an immediate pass ("sync now").
#[tauri::command]
pub fn sync_toggle(
    app: AppHandle,
    scheduler: State<'_, Scheduler>,
    enabled: bool,
) -> Result<SyncStatus, String> {
    let mut settings = crate::settings::load_settings().map_err(|e| e.to_string())?;
    settings.sync_enabled = enabled;
    crate::settings::save_settings(&settings).map_err(|e| e.to_string())?;
    scheduler.command(SyncCommand::SetEnabled(enabled));
    scheduler.emit_status();
    let _ = app;
    Ok(scheduler.status())
}

/// PKCE authorization-code sign-in: resolve the secret, open the browser,
/// wait for the loopback redirect, exchange the code, persist the refresh
/// token, record the account email, enable sync, and trigger an immediate
/// pass. Blocks the caller until the user finishes in the browser.
#[tauri::command]
pub async fn sync_sign_in(
    app: AppHandle,
    scheduler: State<'_, Scheduler>,
) -> Result<SyncStatus, String> {
    let secret = resolve_client_secret().map_err(|e| e.to_string())?;
    let config = build_config(Some(secret));
    let state = uuid::Uuid::new_v4().to_string();
    let verifier = auth::pkce_verifier();
    let challenge = auth::pkce_s256(&verifier);
    let url = auth::authorization_url(&config, &state, &challenge);
    open_browser(&url).map_err(|e| format!("failed to open the browser: {e}"))?;

    let code = auth::listen_for_redirect(&config.redirect_uri, &state)
        .await
        .map_err(|e| e.to_string())?;
    let mut session = AuthSession::new(config);
    let response = auth::exchange_code(
        &session.config,
        &crate::http_client::CLIENT,
        &code,
        &verifier,
    )
    .await
    .map_err(|e| e.to_string())?;
    session.store_tokens(&response);
    if session.refresh_token.is_none() {
        return Err("Google did not return a refresh token for this client".to_string());
    }
    session.persist_refresh_token().map_err(|e| e.to_string())?;

    let token = session
        .access_token()
        .ok_or("sign-in did not produce an access token")?
        .to_string();
    let email = fetch_account_email(&token).await?;

    let mut settings = crate::settings::load_settings().map_err(|e| e.to_string())?;
    settings.sync_enabled = true;
    settings.sync_account_key = Some(email);
    crate::settings::save_settings(&settings).map_err(|e| e.to_string())?;

    scheduler.command(SyncCommand::SignedIn);
    scheduler.emit_status();
    let _ = app;
    Ok(scheduler.status())
}

/// Forget the refresh token and account key. `sync_enabled` stays as the user
/// left it; scheduling stops until the next sign-in.
#[tauri::command]
pub fn sync_sign_out(
    app: AppHandle,
    scheduler: State<'_, Scheduler>,
) -> Result<SyncStatus, String> {
    let mut session = AuthSession::new(sync_config());
    let _ = session.load_refresh_token();
    session.forget_refresh_token().map_err(|e| e.to_string())?;

    let mut settings = crate::settings::load_settings().map_err(|e| e.to_string())?;
    settings.sync_account_key = None;
    crate::settings::save_settings(&settings).map_err(|e| e.to_string())?;

    scheduler.command(SyncCommand::SignedOut);
    scheduler.emit_status();
    let _ = app;
    Ok(scheduler.status())
}

/// Account key fetch: userinfo email with the memory-only access token. The
/// response is parsed without ever logging transcript or token material.
async fn fetch_account_email(access_token: &str) -> Result<String, String> {
    let response = crate::http_client::CLIENT
        .get(SYNC_USERINFO_URL)
        .bearer_auth(access_token)
        .send()
        .await
        .map_err(|e| format!("account lookup failed: {e}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "account lookup failed (HTTP {})",
            response.status().as_u16()
        ));
    }
    let text = response
        .text()
        .await
        .map_err(|e| format!("account lookup failed: {e}"))?;
    parse_account_email(&text).ok_or_else(|| "account lookup did not return an email".to_string())
}

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn core() -> SchedulerCore {
        SchedulerCore::new(true, true)
    }

    const NOW: i64 = 1_000_000;

    // -- single-flight coalescing -------------------------------------------

    #[test]
    fn run_now_while_running_coalesces_into_one_followup() {
        let mut c = core();
        assert!(c.take_run(NOW), "first pass starts");
        c.apply(&SyncCommand::RunNow);
        c.apply(&SyncCommand::RunNow);
        assert!(c.pending_run, "requests coalesce while running");
        assert!(
            !c.take_run(NOW + 1_000),
            "no second pass may start while one is running"
        );
        c.finish(PassOutcomeKind::Success, None, NOW + 2_000);
        assert!(!c.running);
        assert!(c.pending_run, "coalesced request survives the pass");
        assert!(c.take_run(NOW + 2_000), "exactly one follow-up pass");
        assert!(!c.pending_run, "coalesced request consumed once");
    }

    #[test]
    fn run_now_while_idle_starts_immediately() {
        let mut c = core();
        c.apply(&SyncCommand::RunNow);
        assert!(c.take_run(NOW));
    }

    // -- cadence gating -----------------------------------------------------

    #[test]
    fn cadence_prevents_back_to_back_passes() {
        let mut c = core();
        assert!(c.take_run(NOW));
        c.finish(PassOutcomeKind::Success, None, NOW + 100);
        assert!(
            !c.take_run(NOW + SYNC_CADENCE_MS as i64 - 1),
            "a pass before the cadence elapses is refused"
        );
        assert!(
            c.take_run(NOW + SYNC_CADENCE_MS as i64),
            "the next pass is due once the cadence elapsed"
        );
    }

    #[test]
    fn disabled_or_signed_out_never_runs() {
        let mut c = SchedulerCore::new(false, true);
        c.apply(&SyncCommand::RunNow);
        assert!(!c.take_run(NOW));
        assert_eq!(c.wait_ms(NOW), None);

        let mut c = SchedulerCore::new(true, false);
        c.apply(&SyncCommand::RunNow);
        assert!(!c.take_run(NOW));
        assert_eq!(c.wait_ms(NOW), None);
    }

    #[test]
    fn enable_toggle_schedules_immediate_pass() {
        let mut c = SchedulerCore::new(false, true);
        c.apply(&SyncCommand::SetEnabled(true));
        assert!(c.take_run(NOW));

        let mut c = SchedulerCore::new(true, true);
        c.apply(&SyncCommand::SetEnabled(false));
        assert!(!c.take_run(NOW), "disabling cancels pending runs");
    }

    // -- backoff gating -----------------------------------------------------

    #[test]
    fn retryable_failure_backs_off_then_resets_on_success() {
        let mut c = core();
        assert!(c.take_run(NOW));
        c.finish(PassOutcomeKind::Retryable, Some("boom".into()), NOW);
        assert_eq!(
            c.retry_delay_ms, SYNC_BACKOFF_BASE_MS,
            "the first retry waits the base delay"
        );
        assert_eq!(
            c.wait_ms(NOW),
            Some(SYNC_BACKOFF_BASE_MS),
            "the next attempt is gated by the backoff delay"
        );
        assert!(!c.take_run(NOW + 500));
        assert!(c.take_run(NOW + SYNC_BACKOFF_BASE_MS as i64));

        c.finish(PassOutcomeKind::Retryable, Some("boom".into()), NOW);
        assert_eq!(c.retry_delay_ms, 2_000, "delay doubles");

        assert!(
            c.take_run(NOW + 3_000),
            "second retry waits its doubled delay"
        );
        c.finish(PassOutcomeKind::Success, None, NOW + 3_100);
        assert!(!c.backoff_active, "success resets the backoff");
        assert_eq!(c.backoff.current_delay_ms(), SYNC_BACKOFF_BASE_MS);
        assert!(c.last_sync_at.is_some());
        assert!(c.last_error.is_none());
    }

    #[test]
    fn backoff_caps_at_max_delay() {
        let mut c = core();
        let mut t = NOW;
        for _ in 0..10 {
            assert!(c.take_run(t));
            c.finish(PassOutcomeKind::Retryable, Some("x".into()), t);
            t += c.retry_delay_ms as i64;
        }
        assert_eq!(c.retry_delay_ms, SYNC_BACKOFF_CAP_MS);
        assert_eq!(
            c.wait_ms(t - 1),
            Some(1),
            "one ms before the gate elapses, 1 ms remains"
        );
        assert_eq!(
            c.wait_ms(t),
            Some(0),
            "the pass is due once the gate elapsed"
        );
        assert!(c.take_run(t), "a capped-delay retry is allowed once due");
    }

    // -- status transitions -------------------------------------------------

    #[test]
    fn auth_required_stops_scheduling_until_sign_in() {
        let mut c = core();
        assert!(c.take_run(NOW));
        c.finish(PassOutcomeKind::AuthRequired, Some("reauth".into()), NOW);
        assert!(!c.signed_in, "401 marks the session as signed out");
        assert!(!c.take_run(NOW + 999_999), "no automatic passes");
        c.apply(&SyncCommand::SignedIn);
        assert!(c.signed_in);
        assert!(c.take_run(NOW), "sign-in schedules an immediate pass");
        assert_eq!(c.backoff.current_delay_ms(), SYNC_BACKOFF_BASE_MS);
    }

    #[test]
    fn fatal_error_stops_until_manual_run() {
        let mut c = core();
        assert!(c.take_run(NOW));
        c.finish(PassOutcomeKind::Fatal, Some("nope".into()), NOW);
        assert!(c.wait_for_command);
        assert!(!c.take_run(NOW + 999_999), "no automatic retry after fatal");
        c.apply(&SyncCommand::RunNow);
        assert!(
            c.take_run(NOW + 999_999),
            "a manual run re-arms automatic scheduling"
        );
    }

    #[test]
    fn sign_out_clears_pending_work() {
        let mut c = core();
        c.apply(&SyncCommand::RunNow);
        c.apply(&SyncCommand::SignedOut);
        assert!(!c.signed_in);
        assert!(!c.take_run(NOW));
    }

    #[test]
    fn partial_retryable_pass_keeps_last_sync_at() {
        let mut c = core();
        assert!(c.take_run(NOW));
        c.finish(PassOutcomeKind::Retryable, Some("2 failed".into()), NOW);
        assert!(c.backoff_active);
        assert!(
            c.last_sync_at.is_none(),
            "a pass with failures is not recorded as synced"
        );
    }

    // -- client secret resolution -------------------------------------------

    #[test]
    fn secret_env_wins_over_config_file() {
        let r = resolve_client_secret_from(
            Some("env-secret"),
            Some(r#"{"client_secret": "file-secret"}"#),
        );
        assert_eq!(r.unwrap(), "env-secret");
    }

    #[test]
    fn secret_falls_back_to_config_file() {
        let r = resolve_client_secret_from(None, Some(r#"{"client_secret": "file-secret"}"#));
        assert_eq!(r.unwrap(), "file-secret");
    }

    #[test]
    fn secret_ignores_blank_env_and_malformed_file() {
        assert!(resolve_client_secret_from(Some("   "), None).is_err());
        assert!(resolve_client_secret_from(None, Some("not json")).is_err());
        assert!(resolve_client_secret_from(None, Some(r#"{"client_secret": "  "}"#)).is_err());
        assert!(resolve_client_secret_from(None, None).is_err());
    }

    // -- sign-in config building --------------------------------------------

    #[test]
    fn google_config_carries_secret_only_when_resolved() {
        let with_secret = build_config(Some("s".to_string()));
        assert_eq!(with_secret.client_id, SYNC_CLIENT_ID);
        assert_eq!(
            with_secret.redirect_uri,
            format!("http://localhost:{}/", SYNC_REDIRECT_PORT)
        );
        assert_eq!(with_secret.client_secret.as_deref(), Some("s"));

        let without = build_config(None);
        assert!(without.client_secret.is_none());
    }

    #[test]
    fn authorization_url_uses_pkce_and_loopback() {
        let config = build_config(None);
        let url = auth::authorization_url(&config, "state-1", "challenge-1");
        assert!(url.contains("response_type=code"));
        assert!(url.contains("client_id="));
        assert!(url.contains(&format!(
            "redirect_uri=http%3A%2F%2Flocalhost%3A{}%2F",
            SYNC_REDIRECT_PORT
        )));
        assert!(url.contains("code_challenge=challenge-1"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("state=state-1"));
    }

    // -- account key parsing -------------------------------------------------

    #[test]
    fn account_email_parses_from_userinfo() {
        assert_eq!(
            parse_account_email(r#"{"email": "me@example.com", "name": "Me"}"#),
            Some("me@example.com".to_string())
        );
        assert_eq!(parse_account_email("not json"), None);
        assert_eq!(parse_account_email(r#"{}"#), None);
        assert_eq!(parse_account_email(r#"{"email": ""}"#), None);
    }
}
