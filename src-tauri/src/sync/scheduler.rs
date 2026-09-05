// Fluence sync - global scheduler + settings UI wiring (frozen v1.2).
//
// One scheduler per process. A background thread owns the pass loop: it waits
// on a command channel for the cadence interval, then runs one v1.2 domain
// pass (dictionary, snippets, stats, settings - history NEVER syncs) and
// emits a `sync-status` Tauri event. Scheduling is gated by three flags -
// sync enabled (settings), signed in (refresh token present), and a
// fatal-error latch (only a manual command re-arms automatic scheduling). A
// "sync now" or local-change request that arrives while a pass is running is
// recorded as `pending_run` and produces exactly one follow-up pass after the
// current pass finishes (single-flight with requeue). Retryable failures
// advance the drive `Backoff` (1000 ms ×2, cap 60 s); successful passes reset it.
//
// Secrets and persistence follow the security contract: the client secret is
// never committed - it comes from the `FLUENCE_SYNC_CLIENT_SECRET` environment
// variable or `Fluence/sync-oauth.json` ({"client_secret": "..."}) at runtime.
// The access token is memory-only; the refresh token lives in the OS
// credential store (`credentials::Fluence/Sync/RefreshToken`). No transcript
// text is ever logged or surfaced.

use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

use crate::sync::auth::{self, AuthSession};
use crate::sync::drive::{Backoff, GoogleDriveStore};
use crate::sync::error::SyncError;

// ---------------------------------------------------------------------------
// Public constants (reported to the user; Exp 4 fixed the client ID and port).
// ---------------------------------------------------------------------------

/// Google OAuth client ID recorded in Exp 4 - public by design.
pub const SYNC_CLIENT_ID: &str =
    "236666538373-8s13ahi71df7q9soql435fk2fol6up86.apps.googleusercontent.com";
/// Loopback redirect port validated in Exp 4.
pub const SYNC_REDIRECT_PORT: u16 = 58611;
/// Automatic pass cadence: one pass every 15 minutes while enabled + signed in (frozen v1.1).
pub const SYNC_CADENCE_MS: u64 = 900_000;
/// Debounce for local changes: 300ms after last local mutation before syncing.
pub const SYNC_DEBOUNCE_MS: u64 = 300;
/// Backoff after a retryable failure: 1000 ms base, ×2, capped at 60 s.
pub const SYNC_BACKOFF_BASE_MS: u64 = 1_000;
pub const SYNC_BACKOFF_FACTOR: u32 = 2;
pub const SYNC_BACKOFF_CAP_MS: u64 = 60_000;
/// Idle wake-up poll cap (disabled / signed out / fatal latch).
const SYNC_IDLE_POLL_MS: u64 = 3_600_000;

const SYNC_CLIENT_SECRET_ENV: &str = "FLUENCE_SYNC_CLIENT_SECRET";
const SYNC_OAUTH_CONFIG_FILE: &str = "sync-oauth.json";
/// Drive `about` endpoint: returns the signed-in user's email address under the
/// `drive.file` scope alone (no `openid`/`email` scope needed for the account
/// key used by sync).
const SYNC_ABOUT_URL: &str = "https://www.googleapis.com/drive/v3/about?fields=user";
const SYNC_SECRET_MISSING_MSG: &str = "sync client secret is not configured. Set the \
    FLUENCE_SYNC_CLIENT_SECRET environment variable or create Fluence/sync-oauth.json \
    with {\"client_secret\": \"...\"}";

// ---------------------------------------------------------------------------
// Command channel + pure scheduler core (unit-testable without threads).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncCommand {
    /// Manual pass now (coalesces while running; re-arms after a fatal error).
    RunNow,
    /// Local data changed - debounce 300ms before syncing (frozen v1.1).
    LocalChange,
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
    /// Permanent client rejections were surfaced - non-success, but unlike
    /// `Retryable` the backoff is NOT escalated: the next attempt runs at the
    /// cadence (§23 / Phase 0 remediation).
    Rejected,
    /// Fatal or NotOurs error - automatic scheduling stops until a command.
    Fatal,
    /// 401 - the refresh token is gone; the user must sign in again.
    AuthRequired,
}

/// Aggregated outcome of one full pass across the four domains.
#[derive(Debug, Default)]
pub struct SyncOutcome {
    pub created: usize,
    pub imported: usize,
    pub retryable_failures: usize,
    pub rejected_failures: usize,
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
    /// An explicit `Retry-After` delay (ms) surfaced by a throttled Drive
    /// response; consumed by the `Retryable` branch of [`SchedulerCore::finish`]
    /// so a rate-limited API is honored rather than retried too eagerly.
    pending_retry_after_ms: Option<u64>,
    pub debounce_until_ms: Option<i64>,
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
            pending_retry_after_ms: None,
            debounce_until_ms: None,
        }
    }

    /// Record an explicit `Retry-After` delay (ms) from a throttled response,
    /// to be honored by the next [`SchedulerCore::finish`] retryable branch.
    pub fn note_retry_after(&mut self, ms: Option<u64>) {
        self.pending_retry_after_ms = ms;
    }

    pub fn apply(&mut self, cmd: &SyncCommand) {
        // For time-dependent commands we need now_ms; callers should use apply_with_time for LocalChange
        match cmd {
            SyncCommand::RunNow => {
                self.pending_run = true;
                self.wait_for_command = false;
                self.debounce_until_ms = None;
            }
            SyncCommand::LocalChange => {
                // Debounce handling without explicit now is done via apply_debounced
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
                self.debounce_until_ms = None;
            }
            SyncCommand::SignedOut => {
                self.signed_in = false;
                self.pending_run = false;
                self.wait_for_command = false;
                self.backoff_active = false;
                self.debounce_until_ms = None;
            }
            SyncCommand::Shutdown => {}
        }
    }

    /// Debounced local change: schedule a run after SYNC_DEBOUNCE_MS
    pub fn apply_debounced(&mut self, cmd: &SyncCommand, now_ms: i64) {
        if let SyncCommand::LocalChange = cmd {
            self.pending_run = true;
            self.wait_for_command = false;
            self.debounce_until_ms = Some(now_ms + SYNC_DEBOUNCE_MS as i64);
        } else {
            self.apply(cmd);
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
        if let Some(debounce) = self.debounce_until_ms {
            if now_ms < debounce {
                return Some((debounce - now_ms) as u64);
            }
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
        if let Some(debounce) = self.debounce_until_ms {
            if now_ms < debounce {
                return false;
            }
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
        self.debounce_until_ms = None;
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
                // pre-steps the backoff for a further failure. When the
                // response carried an explicit `Retry-After`, wait at least
                // that long too (never sooner than the header demands).
                let next = self.backoff.next_delay_ms();
                self.retry_delay_ms = self
                    .pending_retry_after_ms
                    .take()
                    .map_or(next, |r| next.max(r));
                self.backoff_active = true;
                self.last_error = error;
            }
            PassOutcomeKind::Rejected => {
                // Permanent rejections must NOT backoff-escalate: reset the
                // backoff and let the next attempt run at the cadence. The
                // pass is surfaced non-success (last_sync_at not advanced).
                self.backoff.reset();
                self.backoff_active = false;
                self.retry_delay_ms = SYNC_BACKOFF_BASE_MS;
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
    /// UNIT D - growth gauge for stats envelope (existing diagnostics path, no new command)
    pub stats_rows: Option<usize>,
    pub stats_bytes: Option<usize>,
    pub stats_headroom_bytes: Option<usize>,
}

fn build_status(core: &SchedulerCore) -> SyncStatus {
    let now_ms = chrono::Utc::now().timestamp_millis();
    let next_attempt_ms = core.wait_ms(now_ms).map(|ms| now_ms + ms as i64);
    let account_key = crate::settings::load_settings()
        .ok()
        .and_then(|s| s.sync_account_key);
    let (stats_rows, stats_bytes, stats_headroom_bytes) = account_key
        .as_deref()
        .map(|email| crate::sync::metadata::account_hash_from_email(email))
        .map(|hash| crate::sync::stores::StatsDirtyStore::gauge_for_account(&hash))
        .map(|(r, b, h)| (Some(r), Some(b), Some(h)))
        .unwrap_or((None, None, None));
    SyncStatus {
        enabled: core.enabled,
        signed_in: core.signed_in,
        account_key,
        running: core.running,
        last_sync_at: core.last_sync_at,
        last_error: core.last_error.clone(),
        next_attempt_ms,
        stats_rows,
        stats_bytes,
        stats_headroom_bytes,
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
        finish_guarded(&core, run_pass);
        log::info!("sync pass finished");
        let _ = app.emit(
            "sync-status",
            build_status(&core.lock().unwrap_or_else(|e| e.into_inner())),
        );
    }
}

/// Map a finished pass to the scheduling outcome. Extracted so both the
/// thread and the tests share one classification. The third element is the
/// optional explicit `Retry-After` delay (ms) to honor on a retryable pass.
fn classify_pass(
    result: Result<SyncOutcome, SyncError>,
) -> (PassOutcomeKind, Option<String>, Option<u64>) {
    let retry_hint = |e: &SyncError| match e {
        SyncError::Throttled { retry_after_ms } => *retry_after_ms,
        _ => None,
    };
    match result {
        Ok(outcome) if outcome.retryable_failures > 0 => (
            PassOutcomeKind::Retryable,
            Some(format!(
                "{} retryable operation(s) failed this pass",
                outcome.retryable_failures
            )),
            None,
        ),
        Ok(outcome) if outcome.rejected_failures > 0 => (
            PassOutcomeKind::Rejected,
            Some(format!(
                "{} operation(s) permanently rejected this pass",
                outcome.rejected_failures
            )),
            None,
        ),
        Ok(_) => (PassOutcomeKind::Success, None, None),
        Err(e) => match e {
            SyncError::AuthRequired => (
                PassOutcomeKind::AuthRequired,
                Some("authentication required. Sign in again".to_string()),
                None,
            ),
            SyncError::StaleVersion(e) => (PassOutcomeKind::Retryable, Some(e.to_string()), None),
            SyncError::Retryable(e) => (PassOutcomeKind::Retryable, Some(e.to_string()), None),
            SyncError::Throttled { .. } => {
                let hint = retry_hint(&e);
                (PassOutcomeKind::Retryable, Some(e.to_string()), hint)
            }
            SyncError::Rejected(e) => (PassOutcomeKind::Rejected, Some(e.to_string()), None),
            SyncError::Fatal(e) => (PassOutcomeKind::Fatal, Some(e.to_string()), None),
            SyncError::NotOurs => (
                PassOutcomeKind::Fatal,
                Some(SyncError::NotOurs.to_string()),
                None,
            ),
        },
    }
}

/// Run one pass and commit its outcome to the core - including when the pass
/// panics (a bug mid-pass must not wedge the single-flight latch; the panic
/// surfaces as a fatal error and scheduling waits for a manual command).
fn finish_guarded<F>(core: &Mutex<SchedulerCore>, pass: F)
where
    F: FnOnce() -> Result<SyncOutcome, SyncError>,
{
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(pass));
    let (kind, error, retry_after_ms) = match result {
        Ok(r) => classify_pass(r),
        Err(_) => (
            PassOutcomeKind::Fatal,
            Some("sync pass crashed internally - automatic scheduling is paused; use \"Sync now\" to retry".to_string()),
            None,
        ),
    };
    let now_ms = chrono::Utc::now().timestamp_millis();
    let mut core = core.lock().unwrap_or_else(|e| e.into_inner());
    core.note_retry_after(retry_after_ms);
    core.finish(kind, error, now_ms);
}

// ---------------------------------------------------------------------------
// The pass driver (frozen v1.2: one domain pass over dictionary, snippets,
// stats, settings - transcription history is platform-local and never syncs).
// ---------------------------------------------------------------------------

/// Build the OAuth config. The client secret is resolved at runtime and only
/// attached when present - never hard-coded, never persisted.
fn build_config(secret: Option<String>) -> auth::OAuthConfig {
    let mut config = auth::OAuthConfig::google(SYNC_CLIENT_ID.to_string(), SYNC_REDIRECT_PORT);
    config.client_secret = secret;
    config
}

fn sync_config() -> auth::OAuthConfig {
    build_config(resolve_client_secret().ok())
}

/// The real pass: frozen v1.2 domain sync (dictionary, snippets, stats,
/// settings). History stays local (never uploaded). Returns aggregated outcome.
fn run_pass() -> Result<SyncOutcome, SyncError> {
    let settings = crate::settings::load_settings()
        .map_err(|e| SyncError::Fatal(format!("failed to load settings for sync: {e}")))?;
    let account_email = settings.sync_account_key.clone();
    let account_hash = account_email
        .as_deref()
        .map(crate::sync::metadata::account_hash_from_email);

    let config = sync_config();
    let mut session = AuthSession::new(config);
    session.load_refresh_token().map_err(SyncError::Fatal)?;
    if session.refresh_token.is_none() {
        return Err(SyncError::AuthRequired);
    }
    let token = ensure_access_token(&mut session)?;
    // Silent 401 recovery: when Drive rejects the access token mid-pass,
    // refresh it once through the stored refresh token and retry the request.
    // A refresh-grant rejection (revoked/expired refresh token) surfaces
    // AuthRequired so the pass stops - the user must reconnect. The session
    // is shared through Rc<RefCell> so the refresher closure can outlive the
    // local binding and still adopt rotated refresh tokens.
    let session_cell = std::rc::Rc::new(std::cell::RefCell::new(session));
    let mut drive = GoogleDriveStore::new(token);
    {
        let cell = std::rc::Rc::clone(&session_cell);
        drive.set_token_refresher(Box::new(move |_| {
            let mut session = cell.borrow_mut();
            refresh_access_token_silently(&mut session)
        }));
    }
    let mut metadata = crate::sync::metadata::SyncMetadata::load();
    // Account switch: update the active-account marker so per-account
    // bookkeeping (lastRev) partitions correctly.
    if let Some(hash) = account_hash.clone() {
        if metadata.last_account_hash.as_deref() != Some(&hash) {
            metadata.last_account_hash = Some(hash.clone());
            metadata.save();
            // Same stale-cache class as dictionary.rs W1 - compiled caches
            // keyed to the previous account must be dropped immediately.
            crate::dictionary::invalidate_cache();
            crate::snippets::invalidate_cache();
        }
    }
    let Some(hash) = account_hash else {
        return Err(SyncError::AuthRequired);
    };

    let mut dict_store = crate::sync::stores::DictionaryDirtyStore;
    let mut snippet_store = crate::sync::stores::SnippetDirtyStore;
    let mut settings_store = crate::sync::stores::SettingsDirtyStore;
    let mut stats_store = crate::sync::stores::StatsDirtyStore;

    let outcomes = crate::sync::frozen::sync_all_domains(
        &mut drive,
        &hash,
        &mut metadata,
        &mut dict_store,
        &mut snippet_store,
        &mut settings_store,
        &mut stats_store,
    )?;

    let mut outcome = SyncOutcome::default();
    for o in outcomes {
        if o.pushed {
            outcome.created += 1;
        }
        if o.merged {
            outcome.imported += o.items_merged;
        }
    }
    Ok(outcome)
}

/// Access token with an in-memory refresh when needed (spec §24). A 400/401
/// from the token endpoint means the refresh token was revoked - the user
/// must sign in again.
fn ensure_access_token(session: &mut AuthSession) -> Result<String, SyncError> {
    if let Some(token) = session.access_token() {
        return Ok(token.to_string());
    }
    refresh_access_token_silently(session)
}

/// Refresh the Drive access token with the session's stored refresh token.
/// Shared by the pass-start `ensure_access_token` and the drive layer's
/// single silent 401 recovery: both must recover ordinary token expiry
/// without any user motion, and both must stop (AuthRequired) the moment the
/// refresh grant itself is rejected (revoked/expired refresh token).
fn refresh_access_token_silently(session: &mut AuthSession) -> Result<String, SyncError> {
    let (config, refresh) = {
        let config = session.config.clone();
        let Some(refresh) = session.refresh_token.clone() else {
            return Err(SyncError::AuthRequired);
        };
        (config, refresh)
    };
    match tauri::async_runtime::block_on(auth::refresh_access_token(
        &config,
        &crate::http_client::CLIENT,
        &refresh,
    )) {
        Ok(response) => {
            session.store_tokens(&response);
            // RFC 6749 §6 / Google rotation: a refresh grant MAY return a new
            // refresh token. Dropping it would orphan the stored credential
            // and force a needless re-auth on the next pass, so persist
            // rotations through the same Credential Manager path as sign-in.
            if needs_refresh_token_persist(Some(&refresh), &response) {
                session
                    .persist_refresh_token()
                    .map_err(SyncError::Retryable)?;
            }
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

/// Whether a successful token grant must update the stored refresh token:
/// true only when the provider returned a rotated token different from the
/// one already held/persisted. Pure - the Credential Manager round-trip is
/// OS integration, not unit-testable here.
fn needs_refresh_token_persist(previous: Option<&str>, response: &auth::TokenResponse) -> bool {
    match &response.refresh_token {
        Some(rotated) => previous != Some(rotated.as_str()),
        None => false,
    }
}

// ---------------------------------------------------------------------------
// Client secret resolution (runtime only - never committed) and account key.
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

/// Extract the account key (email) from the Drive `about` response
/// (`{"user": {"emailAddress": "..."}}`).
pub fn parse_account_email(json: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    let email = value.get("user")?.get("emailAddress")?.as_str()?;
    if email.trim().is_empty() {
        None
    } else {
        Some(email.to_string())
    }
}

/// The URL-open invocation for Windows (testable).
#[cfg(windows)]
fn windows_browser_command(url: &str) -> Vec<String> {
    vec!["url.dll,FileProtocolHandler".to_string(), url.to_string()]
}

/// Open the authorization URL in the system browser. Windows: route the URL
/// to `ShellExecuteEx` via `rundll32 url.dll,FileProtocolHandler` - no cmd
/// involved, so the `&` query separators in the auth URL are passed through
/// verbatim instead of being split into separate commands.
fn open_browser(url: &str) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        std::process::Command::new("rundll32.exe")
            .args(windows_browser_command(url))
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

/// PKCE authorization-code sign-in: open the browser, wait for the loopback
/// redirect, exchange the code, persist the refresh token, record the account
/// email, enable sync, and trigger an immediate pass. Blocks the caller until
/// the user finishes in the browser. A client secret is attached only when
/// resolvable at runtime - a Desktop OAuth client is a public PKCE client and
/// must be able to sign in without one.
#[tauri::command]
pub async fn sync_sign_in(
    app: AppHandle,
    scheduler: State<'_, Scheduler>,
) -> Result<SyncStatus, String> {
    let config = sync_config();
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
    let account_hash = settings
        .sync_account_key
        .as_deref()
        .map(crate::sync::metadata::account_hash_from_email)
        .ok_or_else(|| "sign-in did not establish an account".to_string())?;
    crate::sync::stores::SettingsDirtyStore::activate_account(&account_hash)
        .map_err(|e| e.to_string())?;
    crate::dictionary::invalidate_cache();
    crate::snippets::invalidate_cache();

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
    crate::dictionary::invalidate_cache();
    crate::snippets::invalidate_cache();

    scheduler.command(SyncCommand::SignedOut);
    scheduler.emit_status();
    let _ = app;
    Ok(scheduler.status())
}

/// Account key fetch: Drive `about` email with the memory-only access token.
/// The token carries only the `drive.appdata` scope, so the OpenID userinfo
/// endpoint (which needs `openid`/`email`) cannot be used. The response is
/// parsed without ever logging transcript or token material.
async fn fetch_account_email(access_token: &str) -> Result<String, String> {
    let response = crate::http_client::CLIENT
        .get(SYNC_ABOUT_URL)
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

/// Account-level combined statistics (frozen v1.2 product contract).
///
/// Signed in: totals derive from the merged account event ledger - the same
/// union set every device converges to - so Windows + Android contributions
/// sum naturally (X + Y). Signed out, or before the first successful sync
/// populates the ledger, falls back to platform-local history-derived numbers.
///
/// Transcription history itself is never consulted for account mode and never
/// leaves the device; clearing history cannot reduce these totals because
/// they come from the append-only stats ledger.
#[tauri::command]
pub fn get_account_stats() -> Result<serde_json::Value, String> {
    let now_ms = chrono::Utc::now().timestamp_millis();
    let week_start = crate::history::utc_week_start_ms(now_ms);
    let month_start = crate::history::utc_month_start_ms(now_ms);

    let account_hash = crate::settings::load_settings()
        .ok()
        .and_then(|s| s.sync_account_key)
        .map(|email| crate::sync::metadata::account_hash_from_email(&email));

    if let Some(hash) = account_hash {
        let rows = crate::sync::stores::StatsDirtyStore::account_event_rows(&hash);
        if !rows.is_empty() {
            let mut total_words = 0i64;
            let mut total_chars = 0i64;
            let mut total_duration_ms = 0i64;
            let mut weekly_words = 0i64;
            let mut weekly_duration_ms = 0i64;
            let mut weekly_count = 0i64;
            let mut monthly_words = 0i64;
            let mut monthly_count = 0i64;
            let mut weekly_timestamps: Vec<String> = Vec::new();
            for (ts, dur, words, chars) in &rows {
                total_words += words;
                total_chars += chars;
                total_duration_ms += dur;
                if *ts >= week_start {
                    weekly_words += words;
                    weekly_duration_ms += dur;
                    weekly_count += 1;
                    weekly_timestamps.push(
                        chrono::DateTime::from_timestamp_millis(*ts)
                            .map(|dt| dt.to_rfc3339())
                            .unwrap_or_default(),
                    );
                }
                if *ts >= month_start {
                    monthly_words += words;
                    monthly_count += 1;
                }
            }
            return Ok(serde_json::json!({
                "source": "account",
                "total_entries": rows.len() as i64,
                "total_chars": total_chars,
                "total_duration_ms": total_duration_ms,
                "total_words": total_words,
                "weekly_count": weekly_count,
                "weekly_duration_ms": weekly_duration_ms,
                "weekly_words": weekly_words,
                "monthly_count": monthly_count,
                "monthly_words": monthly_words,
                "week_start_ms": week_start,
                "month_start_ms": month_start,
                "weekly_timestamps": weekly_timestamps,
            }));
        }
    }

    // Fallback: platform-local view (signed out, or signed in but the ledger
    // has not synced yet).
    let local = crate::history::get_history_stats()?;
    let mut value = serde_json::to_value(&local).map_err(|e| e.to_string())?;
    if let Some(obj) = value.as_object_mut() {
        obj.insert(
            "source".to_string(),
            serde_json::Value::String("local".to_string()),
        );
        obj.insert(
            "weekly_timestamps".to_string(),
            serde_json::Value::Array(Vec::new()),
        );
    }
    Ok(value)
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

    #[test]
    fn rejected_pass_is_non_success_without_backoff_escalation() {
        let mut c = core();
        assert!(c.take_run(NOW));
        // Warm the backoff up so the reset is observable.
        c.finish(PassOutcomeKind::Retryable, Some("boom".into()), NOW);
        c.finish(PassOutcomeKind::Retryable, Some("boom".into()), NOW);
        assert!(c.backoff_active);
        assert_eq!(c.retry_delay_ms, 2_000, "backoff escalated");

        assert!(c.take_run(NOW + 3_000));
        c.finish(
            PassOutcomeKind::Rejected,
            Some("4 permanently rejected".into()),
            NOW + 3_100,
        );
        assert!(!c.backoff_active, "Rejected resets the backoff");
        assert_eq!(c.retry_delay_ms, SYNC_BACKOFF_BASE_MS);
        assert!(
            c.last_sync_at.is_none(),
            "a rejected pass is not recorded as synced"
        );
        assert!(c.last_error.is_some(), "the rejection is surfaced");
        assert!(
            !c.take_run(NOW + 3_200),
            "no immediate rerun (cadence, not backoff, gates the next attempt)"
        );
        assert!(c.take_run(NOW + SYNC_CADENCE_MS as i64 + 3_000));
    }

    // -- refresh-token rotation persistence ----------------------------------

    fn token_response(refresh: Option<&str>) -> auth::TokenResponse {
        auth::TokenResponse {
            access_token: "at-1".to_string(),
            refresh_token: refresh.map(str::to_string),
            expires_in_secs: 3600,
        }
    }

    #[test]
    fn rotated_refresh_token_must_be_persisted() {
        assert!(
            needs_refresh_token_persist(Some("rt-old"), &token_response(Some("rt-new"))),
            "a rotated token must go to the credential store, or the old one is orphaned"
        );
        assert!(needs_refresh_token_persist(
            None,
            &token_response(Some("rt-new"))
        ));
    }

    #[test]
    fn unrotated_refresh_response_keeps_previous_token() {
        // No rotation: the previously persisted token stays authoritative.
        assert!(!needs_refresh_token_persist(
            Some("rt-old"),
            &token_response(None)
        ));
        assert!(!needs_refresh_token_persist(None, &token_response(None)));
        // Provider echoed the same token: no redundant credential write.
        assert!(!needs_refresh_token_persist(
            Some("rt-old"),
            &token_response(Some("rt-old"))
        ));
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
    fn sign_in_never_requires_a_client_secret() {
        // Regression (production hardening): `sync_sign_in` used
        // `resolve_client_secret()?` and refused to start when no secret was
        // present. End users have neither FLUENCE_SYNC_CLIENT_SECRET nor
        // Fluence/sync-oauth.json, so the shipped desktop app could never sign
        // in. A Desktop OAuth client is a public PKCE client: the token
        // exchange must work with no secret, exactly like the background
        // scheduler's `sync_config()` already did. The worst-case resolution
        // (env and file both absent) must still produce a valid config.
        let config = build_config(resolve_client_secret_from(None, None).ok());
        assert!(
            config.client_secret.is_none(),
            "public client has no secret"
        );
        assert_eq!(config.client_id, SYNC_CLIENT_ID);
        assert_eq!(
            config.redirect_uri,
            format!("http://localhost:{}/", SYNC_REDIRECT_PORT)
        );
        assert_eq!(
            config.scope,
            "https://www.googleapis.com/auth/drive.appdata"
        );
        // The authorization-code exchange for that config must omit the
        // client_secret field entirely, as Google's public-client flow expects.
        let body = auth::token_request_body(&config, "code-1", "verifier-1");
        assert!(!body.iter().any(|(k, _)| k == "client_secret"));
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
        assert!(url.contains("prompt=select_account"));
    }

    #[test]
    #[cfg(windows)]
    fn open_browser_routes_the_url_to_the_shell_protocol_handler() {
        // Regression: `cmd /c start "" <url>` split the auth URL at its `&`
        // query separators (dropping client_id) and Rust's re-quoting of the
        // embedded quotes surfaced as `\https://...`. The URL must go to
        // rundll32's protocol handler verbatim, never through cmd.
        let url =
            "https://accounts.google.com/o/oauth2/v2/auth?response_type=code&client_id=x&scope=y";
        assert_eq!(
            windows_browser_command(url),
            vec!["url.dll,FileProtocolHandler".to_string(), url.to_string()]
        );
    }

    // -- account key parsing -------------------------------------------------

    #[test]
    fn account_email_parses_from_drive_about() {
        assert_eq!(
            parse_account_email(r#"{"user": {"emailAddress": "me@example.com"}}"#),
            Some("me@example.com".to_string())
        );
        assert_eq!(parse_account_email("not json"), None);
        assert_eq!(parse_account_email(r#"{}"#), None);
        assert_eq!(
            parse_account_email(r#"{"user": {"emailAddress": ""}}"#),
            None
        );
    }

    // -- pass classification + panic safety (Phase 9 hardening) -------------

    fn ok_outcome(failures: usize) -> SyncOutcome {
        SyncOutcome {
            retryable_failures: failures,
            ..SyncOutcome::default()
        }
    }

    #[test]
    fn classify_pass_maps_every_engine_result() {
        assert_eq!(
            classify_pass(Ok(ok_outcome(0))),
            (PassOutcomeKind::Success, None, None)
        );
        let (kind, err, _) = classify_pass(Ok(ok_outcome(3)));
        assert_eq!(kind, PassOutcomeKind::Retryable);
        assert!(err.unwrap().contains("3 retryable"));

        let (kind, err, _) = classify_pass(Ok(SyncOutcome {
            rejected_failures: 2,
            ..SyncOutcome::default()
        }));
        assert_eq!(kind, PassOutcomeKind::Rejected);
        assert!(err.unwrap().contains("2 operation(s) permanently rejected"));

        let (kind, _, _) = classify_pass(Err(SyncError::Rejected("permanent 400".into())));
        assert_eq!(kind, PassOutcomeKind::Rejected);

        assert_eq!(
            classify_pass(Err(SyncError::AuthRequired)),
            (
                PassOutcomeKind::AuthRequired,
                Some("authentication required. Sign in again".to_string()),
                None
            )
        );
        let (kind, _, _) = classify_pass(Err(SyncError::Retryable("boom".into())));
        assert_eq!(kind, PassOutcomeKind::Retryable);
        let (kind, _, _) = classify_pass(Err(SyncError::Fatal("nope".into())));
        assert_eq!(kind, PassOutcomeKind::Fatal);
        // 403 drive.file → NotOurs → fatal latch; never retried automatically.
        let (kind, _, _) = classify_pass(Err(SyncError::NotOurs));
        assert_eq!(kind, PassOutcomeKind::Fatal);
    }

    #[test]
    fn throttled_error_surfaces_retry_after_hint() {
        let (kind, err, hint) = classify_pass(Err(SyncError::Throttled {
            retry_after_ms: Some(5_000),
        }));
        assert_eq!(kind, PassOutcomeKind::Retryable);
        assert!(err.unwrap().contains("rate limited"));
        assert_eq!(hint, Some(5_000), "the Retry-After delay is surfaced");

        // A 429 without a usable header falls back to a plain retryable.
        let (kind, _, hint) = classify_pass(Err(SyncError::Throttled {
            retry_after_ms: None,
        }));
        assert_eq!(kind, PassOutcomeKind::Retryable);
        assert_eq!(hint, None);
    }

    #[test]
    fn retry_after_header_is_honored_by_the_backoff() {
        // When Drive says "retry after 5s" on the first 429, the scheduler must
        // wait at least 5s - not the 1s base backoff - before the next attempt.
        let mut c = core();
        assert!(c.take_run(NOW));
        c.note_retry_after(Some(5_000));
        c.finish(PassOutcomeKind::Retryable, Some("rate limited".into()), NOW);
        assert_eq!(
            c.retry_delay_ms, 5_000,
            "Retry-After dominates the base backoff (1s)"
        );

        // A later retry with no hint uses the (now doubled) backoff normally,
        // proving the hint is consumed once and not sticky.
        assert!(!c.take_run(NOW + 4_999));
        assert!(c.take_run(NOW + 5_000));
        c.finish(PassOutcomeKind::Retryable, Some("boom".into()), NOW + 5_000);
        assert_eq!(
            c.retry_delay_ms, 2_000,
            "no hint → normal exponential backoff"
        );
    }

    #[test]
    fn panic_in_pass_releases_single_flight_latch() {
        let core = Arc::new(Mutex::new(SchedulerCore::new(true, true)));
        core.lock()
            .unwrap_or_else(|e| e.into_inner())
            .apply(&SyncCommand::RunNow);
        assert!(
            core.lock().unwrap_or_else(|e| e.into_inner()).take_run(NOW),
            "the pass starts"
        );

        // A panicking pass must not wedge `running`: the guard records a
        // fatal outcome and releases the latch.
        finish_guarded(&core, || panic!("injected mid-pass crash"));
        {
            let c = core.lock().unwrap_or_else(|e| e.into_inner());
            assert!(!c.running, "the single-flight latch is released");
            assert!(c.wait_for_command, "a crash pauses automatic scheduling");
            let err = c.last_error.as_deref().unwrap_or_default();
            assert!(err.contains("crashed"), "the crash is surfaced: {err}");
        }

        // A manual command re-arms scheduling; the next pass runs normally.
        core.lock()
            .unwrap_or_else(|e| e.into_inner())
            .apply(&SyncCommand::RunNow);
        let ok = core
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take_run(NOW + 1);
        assert!(ok, "manual run starts after a crash");
    }

    #[test]
    fn successful_pass_after_crash_resets_state() {
        let core = Arc::new(Mutex::new(SchedulerCore::new(true, true)));
        finish_guarded(&core, || panic!("boom"));
        assert!(
            core.lock()
                .unwrap_or_else(|e| e.into_inner())
                .wait_for_command,
            "the crash latches scheduling"
        );

        // The user clicks "Sync now" (which clears the latch), and the
        // manual pass succeeds: backoff resets and cadence resumes.
        core.lock()
            .unwrap_or_else(|e| e.into_inner())
            .apply(&SyncCommand::RunNow);
        finish_guarded(&core, || Ok(ok_outcome(0)));
        let c = core.lock().unwrap_or_else(|e| e.into_inner());
        assert!(!c.wait_for_command);
        assert!(c.last_error.is_none());
        assert!(!c.backoff_active);
        assert_eq!(c.backoff.current_delay_ms(), SYNC_BACKOFF_BASE_MS);
    }
}
