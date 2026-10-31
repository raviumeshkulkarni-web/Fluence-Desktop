// Fluence sync - global scheduler + settings UI wiring (frozen v1.2).
//
// One scheduler per process. A background thread owns the pass loop: it waits
// on a command channel for the cadence interval, then runs one v1.2 domain
// pass (dictionary, snippets, stats, settings - history NEVER syncs) and
// emits a `sync-status` Tauri event. The event is delivered to the `main`
// window only - never broadcast to the overlay or wizard webviews - and the
// `account_key` / `last_error` payload fields are redacted at the boundary
// (see `build_status` / `sanitize_status_error`). Scheduling is gated by
// three flags -
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
use tauri::{AppHandle, Emitter, Manager, State};

use crate::sync::auth::{self, AuthSession};
use crate::sync::drive::{Backoff, DomainDriveStore, GoogleDriveStore};
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
/// Longest automatic hold we will take from a `Retry-After` header. A quota
/// reset is bounded by the day; anything past that is a malformed or hostile
/// header and must not be able to park the automatic schedule indefinitely.
/// The manual "Sync now" override is unaffected either way (see
/// [`SchedulerCore::take_run`]).
pub const SYNC_QUOTA_HOLD_MAX_MS: u64 = 24 * 60 * 60 * 1000;
/// How often the scheduler thread wakes while a quota hold is in force, so a
/// queued command - "Sync now" above all - is drained promptly instead of
/// after the whole hold. Each wake is a mutex lock and a clock read; the pass
/// itself is still refused by [`SchedulerCore::take_run`].
const SYNC_QUOTA_HOLD_POLL_MS: u64 = 60_000;
/// Idle wake-up poll cap (disabled / signed out / fatal latch).
const SYNC_IDLE_POLL_MS: u64 = 3_600_000;

const SYNC_CLIENT_SECRET_ENV: &str = "FLUENCE_SYNC_CLIENT_SECRET";
const SYNC_OAUTH_CONFIG_FILE: &str = "sync-oauth.json";
/// The `sync-status` event name. Bound once here so every emission goes
/// through the same scoped helper below (audit remediation S2).
const SYNC_STATUS_EVENT: &str = "sync-status";
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
    /// Partial convergence (F4 Success-with-warning): the pass uploaded and
    /// merged everything it could but skipped some remote state it retains
    /// untouched (currently only the sharded Stats engine reports this).
    /// Converged data advances `last_sync_at` exactly like a success and the
    /// backoff resets (the condition is permanent, never transient); a
    /// clearable `degraded` warning stays visible until a clean pass instead
    /// of a misleading, non-clearable error with a stalled clock.
    Degraded,
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
    /// A domain completed with some remote state skipped as unusable
    /// (currently only the sharded Stats engine reports this, for
    /// oversized/corrupt/future files it retains untouched). Surfaces as a
    /// Degraded pass — Success-with-warning: the converged data advances
    /// last_sync_at like a success while the warning stays visible until a
    /// clean pass heals it.
    pub degraded: bool,
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
    /// F4 Success-with-warning: true while the latest finished pass converged
    /// with skipped remote state (see [`PassOutcomeKind::Degraded`]). Cleared
    /// by the next clean success; never an error, never sticky across one.
    pub degraded: bool,
    /// Human-readable warning carried while [`Self::degraded`] is set.
    pub degraded_detail: Option<String>,
    pub backoff: Backoff,
    pub backoff_active: bool,
    /// The delay gating the next attempt after a retryable failure (the value
    /// `next_delay_ms` returned; the backoff itself pre-steps for later).
    pub retry_delay_ms: u64,
    /// An explicit `Retry-After` delay (ms) surfaced by a throttled Drive
    /// response; consumed by the `Retryable` branch of [`SchedulerCore::finish`]
    /// so a rate-limited API is honored rather than retried too eagerly.
    pending_retry_after_ms: Option<u64>,
    /// The UNCLAMPED (but bounded, see [`SYNC_QUOTA_HOLD_MAX_MS`]) part of a
    /// `Retry-After` header that asks for longer than
    /// [`SYNC_BACKOFF_CAP_MS`]. Consumed by the `Retryable` branch of
    /// [`SchedulerCore::finish`], which turns it into
    /// [`Self::quota_hold_until_ms`].
    pending_quota_hold_ms: Option<u64>,
    /// Absolute epoch millis until which AUTOMATIC passes are refused because
    /// Drive asked for a longer wait than the backoff cap. Deliberately a
    /// deadline rather than an interval: a manual "Sync now" sets
    /// `pending_run`, which [`SchedulerCore::take_run`] honors before it ever
    /// looks at this, so the user override survives a day-long hold.
    pub quota_hold_until_ms: Option<i64>,
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
            degraded: false,
            degraded_detail: None,
            backoff: Backoff::new(
                SYNC_BACKOFF_BASE_MS,
                SYNC_BACKOFF_FACTOR,
                SYNC_BACKOFF_CAP_MS,
            ),
            backoff_active: false,
            retry_delay_ms: SYNC_BACKOFF_BASE_MS,
            pending_retry_after_ms: None,
            pending_quota_hold_ms: None,
            quota_hold_until_ms: None,
            debounce_until_ms: None,
        }
    }

    /// Record an explicit `Retry-After` delay (ms) from a throttled response,
    /// to be honored by the next [`SchedulerCore::finish`] retryable branch.
    ///
    /// The value is clamped to [`SYNC_BACKOFF_CAP_MS`] on the way in. The cap
    /// MUST cover the header too, not just the exponential backoff: `finish`
    /// takes the *max* of the (already capped) backoff delay and this value,
    /// so an unclamped `Retry-After` (Drive sends integer seconds - `3600`
    /// arrives here as 3_600_000 ms) would park the whole scheduler - and
    /// therefore every command, including "Sync now" - for an hour. A capped
    /// hint is honored; an uncapped one is not.
    ///
    /// But `retry_delay_ms` is ALSO the automatic-run interval, and for a
    /// `dailyLimitExceeded` the server asks for a day: honoring only the 60 s
    /// cap meant re-polling every minute, forever, against a server that had
    /// said "come back tomorrow". So a hint that exceeds the cap is recorded
    /// TWICE - clamped for the interval (UI safety), and separately as a
    /// quota hold (honoring the server). The hold gates automatic passes only:
    /// `pending_run` is checked first in both [`SchedulerCore::wait_ms`] and
    /// [`SchedulerCore::take_run`], so "Sync now" still starts a pass at once.
    pub fn note_retry_after(&mut self, ms: Option<u64>) {
        self.pending_retry_after_ms = ms.map(|ms| ms.min(SYNC_BACKOFF_CAP_MS));
        self.pending_quota_hold_ms = ms.filter(|&ms| ms > SYNC_BACKOFF_CAP_MS).map(|ms| {
            // A hint past a whole day is malformed or hostile; bound it so no
            // single response can park the automatic schedule forever.
            ms.min(SYNC_QUOTA_HOLD_MAX_MS)
        });
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
                if *enabled {
                    // (Re-)enabling is a deliberate user action: clear a fatal
                    // latch so the user is never stuck in the "Sync will run
                    // shortly" limbo that previously required an unrelated
                    // local edit or an app restart to escape. Disabling must
                    // NOT clear the latch or schedule a pass.
                    self.wait_for_command = false;
                }
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
                // A fresh grant is a deliberate user action; clear the quota
                // hold with the rest of the backoff state so the pass the
                // sign-in schedules is not refused.
                self.quota_hold_until_ms = None;
            }
            SyncCommand::SignedOut => {
                self.signed_in = false;
                self.pending_run = false;
                self.wait_for_command = false;
                self.backoff_active = false;
                self.debounce_until_ms = None;
                // Signing out resets error state, mirroring SignedIn: a stale
                // error must not linger into the next session's status view.
                self.last_error = None;
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

    /// Apply a command with the caller's clock: `LocalChange` goes through the
    /// debounce window, every other command applies immediately. Used by the
    /// scheduler thread's command drain and by [`Scheduler::command_and_status`]
    /// so both agree on when a local change is allowed to start a pass.
    pub fn apply_with_time(&mut self, cmd: &SyncCommand, now_ms: i64) {
        match cmd {
            SyncCommand::LocalChange => self.apply_debounced(cmd, now_ms),
            _ => self.apply(cmd),
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
        // Quota hold: automatic passes are refused until the deadline, but the
        // thread must keep waking on a short slice so a command queued in the
        // meantime ("Sync now") is drained in seconds rather than after the
        // whole hold. Deliberately placed AFTER the `pending_run` short-circuit
        // above - a pending request must never be delayed by a hold.
        if let Some(hold) = self.quota_hold_until_ms {
            if now_ms < hold {
                return Some(((hold - now_ms) as u64).min(SYNC_QUOTA_HOLD_POLL_MS));
            }
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
            // A quota hold is a DEADLINE, not an interval: it refuses automatic
            // passes outright for as long as Drive asked. The `pending_run`
            // branch above is what preserves the manual override, so "Sync now"
            // still starts a pass mid-hold.
            if self.quota_hold_until_ms.is_some_and(|hold| now_ms < hold) {
                return false;
            }
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
        // Claim any `Retry-After` hint exactly once, whatever the outcome is.
        // Only the `Retryable` arm below spends it; taking it here as well
        // means a hint recorded by a pass that ended any other way (a pass
        // that got past the throttle, a crash, a success) cannot leak into a
        // later pass's retry delay and park the scheduler after the fact.
        let pending_retry_after_ms = self.pending_retry_after_ms.take();
        let pending_quota_hold_ms = self.pending_quota_hold_ms.take();
        match outcome {
            PassOutcomeKind::Success => {
                self.backoff.reset();
                self.backoff_active = false;
                self.last_error = None;
                self.degraded = false;
                self.degraded_detail = None;
                self.quota_hold_until_ms = None;
                if error.is_none() {
                    self.last_sync_at = Some(_now_ms);
                }
            }
            PassOutcomeKind::Retryable => {
                // `next_delay_ms` returns the delay for the NEXT attempt and
                // pre-steps the backoff for a further failure. When the
                // response carried an explicit `Retry-After`, wait at least
                // that long too (never sooner than the header demands). Both
                // operands are already capped - the hint was clamped in
                // `note_retry_after` - so the result cannot exceed the cap.
                let next = self.backoff.next_delay_ms();
                self.retry_delay_ms = pending_retry_after_ms.map_or(next, |r| next.max(r));
                self.backoff_active = true;
                self.last_error = error;
                // ...but a hint LONGER than the cap is a server asking for
                // hours (a `dailyLimitExceeded` says a day), not a blip. Hold
                // automatic passes for the real duration instead of re-polling
                // every 60 s. A still-running hold is left alone so a repeat
                // throttled pass cannot shorten it; an elapsed one is simply
                // ignored by the `now_ms < hold` guards.
                if let Some(hold) = pending_quota_hold_ms {
                    self.quota_hold_until_ms = Some(_now_ms.saturating_add(hold as i64));
                }
            }
            PassOutcomeKind::Rejected => {
                // Permanent rejections must NOT backoff-escalate: reset the
                // backoff and let the next attempt run at the cadence. The
                // pass is surfaced non-success (last_sync_at not advanced).
                self.backoff.reset();
                self.backoff_active = false;
                self.retry_delay_ms = SYNC_BACKOFF_BASE_MS;
                self.quota_hold_until_ms = None;
                self.last_error = error;
            }
            PassOutcomeKind::Degraded => {
                // F4 Success-with-warning: converged data is recorded exactly
                // like a success (last_sync_at advances, backoff resets —
                // the skipped state is permanent, never transient), while the
                // warning stays visible via degraded/degraded_detail instead
                // of a misleading, non-clearable last_error. A warning is not
                // an error: any stale error is cleared with the convergence
                // (a clean Success clears the flag too; a real failure sets
                // last_error as usual).
                self.backoff.reset();
                self.backoff_active = false;
                self.retry_delay_ms = SYNC_BACKOFF_BASE_MS;
                self.quota_hold_until_ms = None;
                self.last_sync_at = Some(_now_ms);
                self.last_error = None;
                self.degraded = true;
                self.degraded_detail = error;
            }
            PassOutcomeKind::Fatal => {
                self.backoff.reset();
                self.backoff_active = false;
                self.retry_delay_ms = SYNC_BACKOFF_BASE_MS;
                self.wait_for_command = true;
                self.quota_hold_until_ms = None;
                self.last_error = error;
            }
            PassOutcomeKind::AuthRequired => {
                self.signed_in = false;
                self.pending_run = false;
                self.backoff_active = false;
                self.retry_delay_ms = SYNC_BACKOFF_BASE_MS;
                self.quota_hold_until_ms = None;
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
    /// Signed-in account email (raw). Deliberately NOT redacted: the Sync
    /// page renders it (`web/src/routes/SyncPage.tsx:117` uses it as the
    /// account label and in the "reconnect as <email>" description;
    /// `src/js/settings.js:2246,2256` does the same, and
    /// `web/src/routes/DashboardPage.tsx:514` compares it to detect an
    /// account switch), and the same value is already reachable through the
    /// `main`-only `sync_get_status` command.
    ///
    /// Its exposure is therefore closed by scoping the emit to the `main`
    /// window (audit remediation S2) rather than by redaction. Keep this
    /// decision in sync with any new payload consumer.
    pub account_key: Option<String>,
    pub running: bool,
    pub last_sync_at: Option<i64>,
    /// Sanitized copy of the scheduler's raw diagnostic (see
    /// [`sanitize_status_error`]). Provider response bodies and request URLs
    /// never appear here.
    pub last_error: Option<String>,
    /// Absolute epoch millis of the next scheduled attempt (null when idle).
    pub next_attempt_ms: Option<i64>,
    /// True when automatic scheduling is paused after a fatal error and the
    /// user must trigger a pass (Sync Now / re-enable) to resume. The UI must
    /// render this honestly - never promise an automatic retry while set.
    pub waiting_for_retry: bool,
    /// UNIT D - growth gauge for stats envelope (existing diagnostics path, no new command)
    pub stats_rows: Option<usize>,
    pub stats_bytes: Option<usize>,
    pub stats_headroom_bytes: Option<usize>,
    /// F4 Success-with-warning: true while the latest finished pass converged
    /// with skipped remote state. The UI must render this as a warning, never
    /// as an error and never as "not synced" — last_sync_at advanced.
    pub degraded: bool,
    /// Human-readable warning carried while `degraded` is set.
    pub degraded_detail: Option<String>,
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
        last_error: core.last_error.as_deref().and_then(sanitize_status_error),
        next_attempt_ms,
        waiting_for_retry: core.wait_for_command,
        stats_rows,
        stats_bytes,
        stats_headroom_bytes,
        degraded: core.degraded,
        degraded_detail: core.degraded_detail.clone(),
    }
}

/// Longest `last_error` string carried in a [`SyncStatus`]. Raw diagnostics
/// stay in the scheduler state and in the log; the payload only has to be
/// actionable, and a bounded size keeps a chatty provider from turning the
/// status into a data blob.
const STATUS_ERROR_MAX_CHARS: usize = 160;

/// Generic fallback used when redaction would leave nothing actionable.
const STATUS_ERROR_FALLBACK: &str = "sync failed";

/// Redact one raw scheduler diagnostic for a [`SyncStatus`] payload.
///
/// The scheduler keeps the full text in `SchedulerCore::last_error` (and logs
/// it on the failing pass), so diagnostics are not lost. This copy is what
/// crosses the IPC boundary, so it must not carry the verbatim provider
/// conversation:
/// - `AuthError::Http` renders as `token endpoint HTTP {status}: {body}`, so
///   everything after `HTTP <digits>` is dropped;
/// - anything shaped like a raw response body (`{`, `[`, `<`) is cut;
/// - every `http(s)://` URL is dropped, together with the empty `(...)`
///   wrapper a transport error leaves behind.
///
/// What survives is deliberately lossy but still machine-matchable: the UI
/// maps on substrings such as `network`, `timeout`, `rate`, `429`,
/// `rejected` and `credential` (see `describeSyncError` / `syncErr`).
fn sanitize_status_error(raw: &str) -> Option<String> {
    if raw.trim().is_empty() {
        return None;
    }
    let stripped = strip_urls(raw);
    let redacted = truncate_after_http_status(stripped.trim());
    let redacted = truncate_raw_body(redacted);
    let message = tidy(truncate(redacted, STATUS_ERROR_MAX_CHARS).trim());
    if message.is_empty() {
        Some(STATUS_ERROR_FALLBACK.to_string())
    } else {
        Some(message)
    }
}

/// Drop every absolute URL in `s`, plus the now-empty `(...)` wrapper a
/// transport error leaves behind (`... for url (https://host/path)`).
fn strip_urls(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(idx) = scheme_index(rest) {
        out.push_str(&rest[..idx]);
        let after = &rest[idx + scheme_len(&rest[idx..])..];
        let end = after
            .find(|c: char| c == ')' || c.is_whitespace())
            .unwrap_or(after.len());
        let mut tail = &after[end..];
        if tail.starts_with(')') {
            tail = &tail[1..];
            // "(url)" collapsed to "()" - drop the orphaned opener too.
            let bare = out.trim_end();
            if bare.ends_with('(') {
                out.truncate(bare.len() - 1);
            }
        }
        rest = tail;
    }
    out.push_str(rest);
    out
}

/// Offset of the next absolute-URL scheme in `s`, if any.
fn scheme_index(s: &str) -> Option<usize> {
    [s.find("https://"), s.find("http://")]
        .into_iter()
        .flatten()
        .min()
}

/// Byte length of the scheme prefix that starts `s`.
fn scheme_len(s: &str) -> usize {
    if s.starts_with("https://") {
        8
    } else {
        7
    }
}

/// Cut `s` after a `HTTP <digits>` status that is followed by a body (the
/// shape `AuthError::Http` renders). The status itself is kept - the UI maps
/// on it.
fn truncate_after_http_status(s: &str) -> &str {
    let mut from = 0;
    while let Some(rel) = s[from..].find("HTTP ") {
        let start = from + rel + 5;
        let digits = s[start..]
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .count();
        if digits == 0 {
            from = start;
            continue;
        }
        let after = s[start + digits..].trim_start();
        if after.starts_with(':') {
            return s[..start + digits].trim_end();
        }
        from = start + digits;
    }
    s
}

/// Cut `s` at the first byte that starts a raw provider response body (a
/// JSON object/array or markup), keeping the description in front of it.
fn truncate_raw_body(s: &str) -> &str {
    match s.find(|c| c == '{' || c == '[' || c == '<') {
        Some(idx) => s[..idx].trim_end(),
        None => s,
    }
}

/// Trim `s` to `max` characters, on a UTF-8 boundary and never mid-word.
fn truncate(s: &str, max: usize) -> &str {
    if s.chars().count() <= max {
        return s;
    }
    let hard = s.char_indices().nth(max).map(|(i, _)| i).unwrap_or(s.len());
    let word_edge = s[..hard].rfind(char::is_whitespace).unwrap_or(hard).max(1);
    &s[..word_edge]
}

/// Collapse the whitespace a cut or a multi-line transport error leaves
/// behind, and drop stray separators.
fn tidy(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace(" :", ":")
        .replace(" ,", ",")
        .trim()
        .to_string()
}

/// Deliver a [`SyncStatus`] to the `main` window, and to nothing else.
///
/// The payload carries the signed-in account key and pass diagnostics, so it
/// is only meaningful for the settings surface. Sending it at app level would
/// broadcast to every webview (`main`, `overlay`, `wizard`), which is exactly
/// what audit remediation S2 removed. Same idiom as `tray.rs`. A missing
/// window is skipped silently: the app runs with the main window closed, and a
/// closed window has no frontend to inform.
fn emit_status_to_main(app: &AppHandle, status: &SyncStatus) {
    if let Some(win) = app.get_webview_window(crate::acl::MAIN_WINDOW) {
        let _ = win.emit(SYNC_STATUS_EVENT, status.clone());
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
        // Seed signed_in from a *usable* credential only: absence or an empty
        // blob must not report signed-in, or every pass would fail with
        // AuthRequired from the token endpoint instead. A credential-store
        // FAILURE is not absence, though - see `seed_credential_state`.
        let seed = seed_credential_state(&crate::credentials::read_sync_refresh_token_checked());
        let mut initial = SchedulerCore::new(settings.sync_enabled, seed.signed_in);
        initial.last_error = seed.last_error;
        let core = Arc::new(Mutex::new(initial));
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

    /// Queue a command AND return the status the scheduler will actually be in
    /// once the command is applied.
    ///
    /// [`Scheduler::command`] only hands the command to the thread over a
    /// channel, so a snapshot taken immediately after it reports PRE-command
    /// state - e.g. `sync_run_now` used to emit and return
    /// `waiting_for_retry: true` even though the `RunNow` it had just sent
    /// clears the fatal latch. This method closes that window without a
    /// blocking handshake: it applies the command's effect to the core that
    /// [`build_status`] reads, then snapshots (and emits) that.
    ///
    /// The command is still queued so the scheduler thread remains the
    /// authority on what runs next, which means it is applied twice. That is
    /// safe for the commands routed through here: `RunNow` only sets
    /// `pending_run` and clears `wait_for_command`/`debounce_until_ms`,
    /// `SetEnabled` only flips `enabled` (plus the same latch/latch-clear
    /// flags), and `SignedIn`'s `backoff.reset()`/`last_error` clears are
    /// idempotent too. None of these transitions consumes a once-only
    /// resource, so the re-apply cannot schedule an extra pass: the thread's
    /// drain loop runs before its `take_run`, so the second application only
    /// keeps the already-pending request pending.
    pub fn command_and_status(&self, cmd: SyncCommand) -> SyncStatus {
        let status = {
            let mut core = self.core.lock().unwrap_or_else(|e| e.into_inner());
            apply_and_snapshot(&mut core, &cmd, chrono::Utc::now().timestamp_millis())
        };
        let _ = self.tx.send(cmd);
        emit_status_to_main(&self.app, &status);
        status
    }

    pub fn status(&self) -> SyncStatus {
        let core = self.core.lock().unwrap_or_else(|e| e.into_inner());
        build_status(&core)
    }

    pub fn emit_status(&self) {
        emit_status_to_main(&self.app, &self.status());
    }
}

/// Apply a command to a core and snapshot the resulting status in one step.
///
/// This is the whole of [`Scheduler::command_and_status`] minus the channel
/// send and the Tauri emit, extracted so the post-command snapshot is
/// unit-testable: building a `Scheduler` needs a live `AppHandle`, which unit
/// tests cannot construct. The caller passes its own clock so the debounce
/// window is stamped from the same instant the caller uses everywhere else.
fn apply_and_snapshot(core: &mut SchedulerCore, cmd: &SyncCommand, now_ms: i64) -> SyncStatus {
    core.apply_with_time(cmd, now_ms);
    build_status(core)
}

/// Initial scheduler state derived from the seed-time credential read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SeedCredential {
    /// What `SchedulerCore::signed_in` starts as.
    pub signed_in: bool,
    /// What `SchedulerCore::last_error` starts as, if anything.
    pub last_error: Option<String>,
}

/// Map the seed-time credential read onto the scheduler's initial state.
///
/// Absence (`Missing`) and an empty blob genuinely mean signed out, so the
/// scheduler starts disarmed with no error - that is the state the UI already
/// renders as "signed out, reconnect".
///
/// A **store failure** is a different thing and must not collapse into it.
/// `Err(..)`/`StoreFailed(..)` means the vault could not be READ (locked,
/// service stopped, unregistered profile) - it says nothing about whether a
/// token is stored. Mapping it to `signed_in = false` (audit finding C5) meant
/// `wait_ms` returned `None`, the thread fell into the 1-hour idle poll with no
/// error and no retry, and the Sync page showed "signed out" plus a reconnect
/// prompt for an account that is actually signed in. Recovery then required a
/// full sign-out/sign-in round trip.
///
/// Instead stay ARMED and carry the diagnostic, exactly as `run_pass` already
/// classifies the same condition via `credential_store_error`: the first pass
/// runs immediately, retries with backoff if the vault is still unreadable,
/// and `last_error` tells the user what actually went wrong. The wording is
/// taken from [`credential_store_diagnostic`], the same sentence `run_pass`'s
/// branch produces, so the seeded message is byte-identical to the one a
/// retryable pass shows - same text, same redaction path.
pub(crate) fn seed_credential_state(
    read: &std::result::Result<crate::credentials::SyncRefreshTokenRead, anyhow::Error>,
) -> SeedCredential {
    use crate::credentials::SyncRefreshTokenRead as Read;
    let store_failure = |message: String| SeedCredential {
        signed_in: true,
        last_error: Some(credential_store_diagnostic(&message)),
    };
    match read {
        Ok(Read::Present(token)) if !token.trim().is_empty() => SeedCredential {
            signed_in: true,
            last_error: None,
        },
        // An empty blob is present-but-unusable: still signed out.
        Ok(Read::Present(_)) | Ok(Read::Missing) => SeedCredential {
            signed_in: false,
            last_error: None,
        },
        Ok(Read::StoreFailed(message)) => store_failure(message.clone()),
        Err(e) => store_failure(e.to_string()),
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
        // Single clock reading shared by the command drain and the run
        // decision below, so the debounce window is stamped with the same
        // instant the timing logic reasons about.
        let now_ms = chrono::Utc::now().timestamp_millis();
        match recv {
            Ok(SyncCommand::Shutdown) => break,
            // `LocalChange` must NOT start a pass the moment it is drained:
            // route it through the debounce window (SYNC_DEBOUNCE_MS) with
            // `apply_with_time`, so a burst of settings saves coalesces into
            // one later pass instead of one full pass per save. Every other
            // command keeps applying immediately.
            Ok(cmd) => core
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .apply_with_time(&cmd, now_ms),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }

        let start = core
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take_run(now_ms);
        if !start {
            continue;
        }
        log::info!("sync pass starting");
        finish_guarded(&core, run_pass);
        // Remote stats rows may have landed: drop the dashboard cache.
        crate::sync::stores::StatsDirtyStore::invalidate_activity_cache();
        log::info!("sync pass finished");
        let status = build_status(&core.lock().unwrap_or_else(|e| e.into_inner()));
        emit_status_to_main(&app, &status);
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
        // Partial convergence (e.g. sharded Stats skipped an
        // oversized/corrupt/future file it retains untouched): F4
        // Success-with-warning. The condition is permanent by construction so
        // it must NOT backoff-escalate like a transient Retryable, and it
        // must NOT stall last_sync_at or park a non-clearable error like a
        // Rejected — the pass converged on everything it could. (Android
        // carries the same Degraded kind with the same converged-gating:
        // skipped + uploaded/merged → Degraded, skipped with nothing
        // converged → Retryable.)
        Ok(outcome) if outcome.degraded => (
            PassOutcomeKind::Degraded,
            Some("stats ledger partially degraded (skipped remote state retained)".to_string()),
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
///
/// This guard only means anything when the build unwinds: the release profile
/// sets `panic = "unwind"` (see `[profile.release]` in Cargo.toml) because
/// `std::panic::catch_unwind` cannot catch anything under `panic = "abort"`,
/// which would make the pass abort the whole process instead of latching.
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
    // The payload sent to the frontend is redacted, so the unfiltered
    // diagnostic has to land in the log here or it is unrecoverable.
    if let Some(err) = error.as_deref() {
        log::warn!("sync pass failed ({kind:?}): {err}");
    }
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
/// Classifies a credential-store read failure.
///
/// Split out of [`run_pass`] so the mapping is unit-testable without a live
/// credential vault or Drive session. A store failure is an OS/environment
/// condition (vault locked, service stopped), never a malformed request, so it
/// is `Retryable` - the pass retries with backoff and the diagnostic (which
/// never contains credential material) reaches the user through `last_error`.
/// A *missing* credential is not an error here: `load_refresh_token_checked`
/// returns `Ok(None)` and `run_pass` maps it to `AuthRequired`.
fn credential_store_error(message: String) -> SyncError {
    SyncError::Retryable(credential_store_diagnostic(&message))
}

/// The one sentence both `run_pass`'s store-failure branch and the seed-time
/// classification put in front of the user. Shared so the two can never drift:
/// the Sync page must not show a different message for the same condition
/// depending on whether it was caught at startup or mid-pass.
fn credential_store_diagnostic(message: &str) -> String {
    format!("credential store read failed: {message}")
}

/// Outcome of [`reconcile_persisted_account_key`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AccountReconcileOutcome {
    /// The key was already correct; nothing was written (S5-e).
    AlreadyBound,
    /// The key was missing or stale and was repaired from the credential
    /// identity (S5-b).
    Repaired,
}

/// Bind the persisted account key to the identity that owns the currently
/// stored credential (audit finding S5).
///
/// This is the recovery half of the sign-in fix. `sync_sign_in` now resolves the
/// account identity before persisting a credential, so new half-persisted states
/// cannot be created; this heals the ones already on disk, from builds that
/// persisted the credential first and failed while fetching the email.
///
/// Non-destructive by construction (S5-d): it only ever *writes* a key derived
/// from the identity the caller got from Drive using the stored credential. It
/// never touches the credential, never clears user data. Sign-out remains the
/// only path that deletes a credential.
///
/// # Consequence callers must not miss
///
/// A repair ALSO sets `sync_enabled = true`. That is a **user preference**, and
/// this is the one place in the recovery path that changes one, so it deserves
/// to be explicit rather than implied by the function's name.
///
/// The justification: the credential exists only because the user completed a
/// browser grant, so a half-persisted install is a *failed* sign-in, not a user
/// who chose to turn sync off. Re-enabling it is what makes the install usable
/// again - without it the scheduler has a credential but no key, `run_pass`
/// cannot address a partition, and the user is back to the original dead-end.
///
/// The edge case this cannot reach: a user who deliberately disabled sync while
/// holding a valid, key-matched credential. That state returns
/// [`AccountReconcileOutcome::AlreadyBound`] above and never rewrites anything,
/// so the preference is preserved. A repair therefore only ever fires for a
/// broken sign-in, and never for a deliberate opt-out.
///
/// Returns whether the key changed, so callers can skip the (atomic) settings
/// write entirely when the install is already healthy.
pub(crate) fn reconcile_persisted_account_key(
    verified_email: &str,
) -> Result<AccountReconcileOutcome, String> {
    let mut settings = crate::settings::load_settings().map_err(|e| e.to_string())?;
    if settings
        .sync_account_key
        .as_deref()
        .is_some_and(|key| crate::sync::metadata::account_keys_match(key, verified_email))
    {
        return Ok(AccountReconcileOutcome::AlreadyBound);
    }
    settings.sync_account_key = Some(verified_email.to_string());
    // The interrupted sign-in never reached its settings write, so sync may still
    // be off even though a credential is present and usable. Enabling it here is
    // what makes the signed-in state usable again; the credential is proof of a
    // completed browser grant.
    settings.sync_enabled = true;
    crate::settings::save_settings(&settings).map_err(|e| e.to_string())?;
    crate::sync::stores::SettingsDirtyStore::activate_account(
        &crate::sync::metadata::account_hash_from_email(verified_email),
    )
    .map_err(|e| e.to_string())?;
    Ok(AccountReconcileOutcome::Repaired)
}

fn run_pass() -> Result<SyncOutcome, SyncError> {
    let settings = crate::settings::load_settings()
        .map_err(|e| SyncError::Fatal(format!("failed to load settings for sync: {e}")))?;
    let persisted_email = settings.sync_account_key.clone();

    let config = sync_config();
    let mut session = AuthSession::new(config);
    // Classify the credential read precisely (audit finding C3):
    // - missing credential (the normal signed-out state) => AuthRequired, so
    //   the UI prompts a reconnect and the scheduler is NOT latched;
    // - a genuine credential-store failure => Retryable (transient OS state),
    //   with a secret-free diagnostic message;
    // - anything else keeps the existing AuthRequired branch below live.
    session
        .load_refresh_token_checked()
        .map_err(credential_store_error)?;
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
    // STAGE 8 — token-bound identity. The partition key derives from the email
    // Drive attributes to THIS access token, never from the persisted setting
    // alone. A pass that cannot establish the identity aborts BEFORE any partition
    // is addressed — so Windows can never upload into (or read from) a partition
    // derived from a stale email. Mirrors Android `DriveIdentity.resolveAccountEmail`.
    let verified_email = match drive.drive_account_email()? {
        Some(e) => e,
        None => {
            return Err(SyncError::Retryable(
                "drive identity unavailable; refusing to sync without it".to_string(),
            ))
        }
    };
    // S5-b: reconcile the persisted account key against the token-bound identity.
    //
    // The persisted key is derived state; the credential that reached this line is
    // what authenticates every request, so it is authoritative. When the key is
    // missing (a sign-in interrupted between the credential write and the settings
    // write) or disagrees (an account switch that got as far as persisting the new
    // credential), trusting the stale key instead of repairing it dead-ends here
    // forever: the old code returned AuthRequired for a missing key, which flipped
    // signed_in false and stopped all future passes while leaving the credential
    // behind, and returned a permanent Retryable on a mismatch.
    //
    // Repair is non-destructive (S5-d): the credential is adopted, never forgotten,
    // and no cached data is dropped beyond what an ordinary account switch drops.
    // Only a mis-binding that Drive itself cannot resolve is reported, and as
    // Retryable (S5-c), never as a terminal AuthRequired.
    match identity_gate(persisted_email.as_deref(), &verified_email) {
        IdentityGate::AlreadyBound => {
            // Already bound to the credential's identity: no write, no log.
        }
        // A DIFFERENT account's key is persisted. This is NOT an interrupted
        // sign-in: the credential's identity and the recorded key disagree about
        // which account the user is. `reconcile_persisted_account_key` would
        // adopt the credential's account, re-point the whole app's partition at
        // it, and flip `sync_enabled = true` — all from a background repair, with
        // no signal beyond a log line the user never sees. Fail closed instead,
        // exactly as Android's `identityDivergenceOutcome` does. Reported
        // Retryable (not a latch, never AuthRequired): the user is not signed
        // out and the pass backs off rather than wedging permanently.
        IdentityGate::RefuseDivergent { persisted } => {
            return Err(SyncError::Retryable(format!(
                "refusing to sync: the stored account ({}) does not match the signed-in Google \
                 account ({}); sign out and back in to continue",
                crate::sync::metadata::account_hash_from_email(&persisted),
                crate::sync::metadata::account_hash_from_email(&verified_email)
            )));
        }
        // No key at all: an interrupted sign-in — the credential was persisted
        // but the settings write never happened. This is the only case the
        // repair (and its `sync_enabled = true`) is for.
        IdentityGate::RepairMissingKey => {
            match reconcile_persisted_account_key(&verified_email) {
                Ok(outcome) if outcome == AccountReconcileOutcome::Repaired => {
                    log::info!(
                        "sync: recovered account identity from the stored credential ({}); \
                         no previous key was recorded",
                        crate::sync::metadata::account_hash_from_email(&verified_email),
                    );
                }
                Ok(_) => {}
                Err(e) => {
                    // Could not even persist the repair. Report as Retryable so the
                    // pass backs off and tries again; never latching and never
                    // reporting the user as signed out.
                    return Err(SyncError::Retryable(e));
                }
            }
        }
    }
    let account_hash = crate::sync::metadata::account_hash_from_email(&verified_email);
    let mut metadata = crate::sync::metadata::SyncMetadata::load();
    // Account switch: update the active-account marker so per-account
    // bookkeeping (lastRev) partitions correctly.
    if metadata.last_account_hash.as_deref() != Some(&account_hash) {
        metadata.last_account_hash = Some(account_hash.clone());
        metadata.save();
        // Same stale-cache class as dictionary.rs W1 - compiled caches
        // keyed to the previous account must be dropped immediately.
        crate::dictionary::invalidate_cache();
        crate::snippets::invalidate_cache();
    }
    let hash = account_hash;

    {
        // F6 mid-pass fence (Android PassAccountGuard parity): the silent 401
        // recovery must not mint for a different account than this pass is
        // syncing for. A sign-out (persisted sign-in gone) or switch (changed)
        // mid-pass aborts as Retryable before any credential use — the next
        // pass runs for the new account (or AuthRequired when signed out).
        // Unreadable settings fail closed the same way. Residual, shared with
        // Android: a pass whose in-memory token never hits a 401 completes
        // without re-checking; tokens are short-lived and the next pass
        // fences, so the window is one pass, never silent divergence.
        let cell = std::rc::Rc::clone(&session_cell);
        let pass_hash = hash.clone();
        drive.set_token_refresher(Box::new(move |_| {
            let current = crate::settings::load_settings()
                .ok()
                .and_then(|s| s.sync_account_key);
            if !pass_account_unchanged(&pass_hash, current.as_deref()) {
                return Err(SyncError::Retryable(
                    "account changed mid-pass; aborting before credential use".to_string(),
                ));
            }
            let mut session = cell.borrow_mut();
            refresh_access_token_silently(&mut session)
        }));
    }

    let mut dict_store = crate::sync::stores::DictionaryDirtyStore;
    let mut snippet_store = crate::sync::stores::SnippetDirtyStore;
    let mut settings_store = crate::sync::stores::SettingsDirtyStore;
    let mut stats_store = crate::sync::stores::StatsDirtyStore;
    let mut agent_store = crate::sync::stores::AgentDirtyStore;
    let mut style_store = crate::sync::stores::StyleDirtyStore;

    let result = crate::sync::frozen::sync_all_domains(
        &mut drive,
        &hash,
        &mut metadata,
        &mut dict_store,
        &mut snippet_store,
        &mut settings_store,
        &mut stats_store,
        &mut agent_store,
        &mut style_store,
    );

    let mut outcome = SyncOutcome::default();
    for o in &result.outcomes {
        if o.pushed {
            outcome.created += 1;
        }
        if o.merged {
            outcome.imported += o.items_merged;
        }
        if o.degraded {
            log::warn!("sync: a domain completed with skipped remote state; converged data still counts as synced");
            outcome.degraded = true;
        }
    }
    // Partial success stays visible via `outcome`; the worst domain error (if
    // any) still determines the pass result so the scheduler escalates on the
    // most actionable signal.
    if let Some(e) = result.error {
        return Err(e);
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

/// F6 mid-pass fence (Android `PassAccountGuard` parity): the account this
/// pass is syncing for, against the CURRENTLY persisted sign-in. A sign-out
/// (None) or switch (different hash) mid-pass must abort before any further
/// credential use — otherwise a stale in-memory session keeps writing the old
/// account's data, or a fresh token mints for somebody else. Pure so the
/// policy is unit-testable without a vault or network.
fn pass_account_unchanged(pass_account_hash: &str, current_email: Option<&str>) -> bool {
    current_email
        .map(crate::sync::metadata::account_hash_from_email)
        .as_deref()
        == Some(pass_account_hash)
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
/// The file is never auto-deleted: existing sign-ins keep working. Prefer the
/// env var for new setups; the file path stays for backwards compatibility.
/// No secret material is ever logged here (lengths only, if at all).
pub fn resolve_client_secret() -> Result<String, String> {
    let env = std::env::var(SYNC_CLIENT_SECRET_ENV).ok();
    let file = oauth_config_path().and_then(|p| std::fs::read_to_string(p).ok());
    if env.is_none() && file.is_some() {
        log::warn!(
            "Using OAuth client secret from sync-oauth.json; prefer FLUENCE_SYNC_CLIENT_SECRET env var for new setups"
        );
    }
    resolve_client_secret_from(env.as_deref(), file.as_deref())
}

/// What a pass must do about the relationship between the PERSISTED account key
/// and the identity Drive attributes to the live access token.
///
/// Extracted as a pure function so the rule is testable directly. The gate used
/// to live inline in `run_pass`, which meant the only test of it drove
/// `reconcile_persisted_account_key` DIRECTLY — bypassing the gate entirely and
/// therefore "certifying" the opposite of shipped behaviour for the divergence
/// case. This is the seam that makes the real rule assertable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum IdentityGate {
    /// The key already names the credential's account: no write, no log.
    AlreadyBound,
    /// No key recorded — an interrupted sign-in (the credential was persisted
    /// but the settings write never landed). This is the ONLY case the repair,
    /// and its `sync_enabled = true`, is for.
    RepairMissingKey,
    /// A DIFFERENT account's key is recorded: the credential and the recorded
    /// key disagree about who the user is. Refuse; never adopt.
    RefuseDivergent { persisted: String },
}

pub(crate) fn identity_gate(persisted_email: Option<&str>, verified_email: &str) -> IdentityGate {
    match persisted_email {
        // F10: full-Unicode fold via the shared helper — NOT
        // `eq_ignore_ascii_case`, which called `ÜSER@…` and `üser@…` two
        // identities.
        Some(key) if crate::sync::metadata::account_keys_match(key, verified_email) => {
            IdentityGate::AlreadyBound
        }
        Some(key) => IdentityGate::RefuseDivergent {
            persisted: key.to_string(),
        },
        None => IdentityGate::RepairMissingKey,
    }
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

/// Absolute path to `rundll32.exe` in the system directory. Resolving it
/// explicitly (instead of relying on PATH) prevents a planted `rundll32`
/// from intercepting the OAuth browser handoff and stealing the auth URL.
#[cfg(windows)]
fn windows_rundll32_path() -> String {
    std::env::var("SystemRoot")
        .map(|root| format!("{}\\System32\\rundll32.exe", root.trim_end_matches('\\')))
        .unwrap_or_else(|_| "C:\\Windows\\System32\\rundll32.exe".to_string())
}

/// Open the authorization URL in the system browser. Windows: route the URL
/// to `ShellExecuteEx` via `rundll32 url.dll,FileProtocolHandler` - no cmd
/// involved, so the `&` query separators in the auth URL are passed through
/// verbatim instead of being split into separate commands.
fn open_browser(url: &str) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        std::process::Command::new(windows_rundll32_path())
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
pub fn sync_get_status(
    window: tauri::Window,
    scheduler: State<'_, Scheduler>,
) -> Result<SyncStatus, String> {
    crate::acl::require_caller(&window, &[crate::acl::MAIN_WINDOW])?;
    Ok(scheduler.status())
}

/// Enable/disable sync and persist the flag. Calling with the current value
/// is idempotent and forces an immediate pass ("sync now").
#[tauri::command]
pub fn sync_toggle(
    window: tauri::Window,
    app: AppHandle,
    scheduler: State<'_, Scheduler>,
    enabled: bool,
) -> Result<SyncStatus, String> {
    crate::acl::require_caller(&window, &[crate::acl::MAIN_WINDOW])?;
    let mut settings = crate::settings::load_settings().map_err(|e| e.to_string())?;
    settings.sync_enabled = enabled;
    crate::settings::save_settings(&settings).map_err(|e| e.to_string())?;
    let status = scheduler.command_and_status(SyncCommand::SetEnabled(enabled));
    let _ = app;
    Ok(status)
}

/// Explicit user-requested sync ("Sync Now"). Unlike `sync_toggle` this never
/// touches persisted settings: it is a pass request, not an enable/disable
/// action. It clears a fatal latch and schedules an immediate pass, so it is
/// the recovery control after a fatal error (audit finding C2: the button
/// previously routed to `sync_toggle`, which could not re-arm the scheduler).
#[tauri::command]
pub fn sync_run_now(
    window: tauri::Window,
    scheduler: State<'_, Scheduler>,
) -> Result<SyncStatus, String> {
    crate::acl::require_caller(&window, &[crate::acl::MAIN_WINDOW])?;
    // `command_and_status` applies RunNow to the core before snapshotting, so
    // the returned/emitted status is post-command: the fatal latch is cleared
    // in the reported state instead of the pre-command state.
    Ok(scheduler.command_and_status(SyncCommand::RunNow))
}

/// PKCE authorization-code sign-in: open the browser, wait for the loopback
/// redirect, exchange the code, persist the refresh token, record the account
/// email, enable sync, and trigger an immediate pass. Blocks the caller until
/// the user finishes in the browser. A client secret is attached only when
/// resolvable at runtime - a Desktop OAuth client is a public PKCE client and
/// must be able to sign in without one.
#[tauri::command]
pub async fn sync_sign_in(
    window: tauri::Window,
    app: AppHandle,
    scheduler: State<'_, Scheduler>,
) -> Result<SyncStatus, String> {
    crate::acl::require_caller(&window, &[crate::acl::MAIN_WINDOW])?;
    let config = sync_config();
    let state = uuid::Uuid::new_v4().to_string();
    let verifier = auth::pkce_verifier();
    let challenge = auth::pkce_s256(&verifier);
    let url = auth::authorization_url(&config, &state, &challenge);
    // Bind the loopback listener BEFORE spawning the browser: the redirect
    // lands on this port and must find something bound, or a fast redirect is
    // refused by the OS and sign-in fails (audit finding S4).
    let listener = auth::bind_redirect_listener(&config.redirect_uri)
        .await
        .map_err(|e| e.to_string())?;
    open_browser(&url).map_err(|e| format!("failed to open the browser: {e}"))?;

    let code = auth::accept_redirect(&listener, &state)
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
        // M1 residual, deliberately NOT fixed by forcing `prompt=consent`.
        //
        // `access_type=offline` (see `authorization_url`) already makes Google
        // issue a refresh token on a first grant. What it cannot force is a
        // *repeat* grant for an account this client already holds a token for:
        // Google may answer that authorization with an access token only, which
        // lands here. `prompt=consent` would guarantee a refresh token on every
        // authorization, but it also shows the consent screen to every user on
        // every sign-in - including first-time users and those switching
        // accounts, who are perfectly served by the current flow. That is a
        // product/UX trade, not a bug fix, so it is not taken unilaterally here.
        //
        // What is in scope is making the outcome actionable rather than opaque:
        // an already-granted account is recovered by revoking Fluence's access
        // in the Google account, which clears the grant and makes the next
        // authorization a first grant again.
        return Err(
            "Google did not return a refresh token. If you have authorized Fluence before, \
             revoke its access at myaccount.google.com/permissions and try signing in again."
                .to_string(),
        );
    }
    let token = session
        .access_token()
        .ok_or("sign-in did not produce an access token")?
        .to_string();
    // S5-a: resolve the account identity BEFORE the credential becomes durable.
    //
    // The old order persisted the refresh token first and fetched the email
    // second, so any failure between the two durably stored a credential with no
    // account key. That half-persisted state is unusable: every later pass reads
    // the credential, cannot resolve an account, and returns AuthRequired - and if
    // the user had previously signed in as a different account, the credential and
    // the persisted key disagree, which dead-ends on identity mismatch.
    //
    // Resolving identity first means a failure here leaves nothing durable at all:
    // no credential, no key, no enabled flag. The user simply retries the
    // browser flow. The remaining credential-write -> settings-write boundary is
    // recovered non-destructively by `reconcile_persisted_account_key` in
    // `run_pass` (S5-b), which never deletes anything.
    let email = fetch_account_email(&token).await?;
    session.persist_refresh_token().map_err(|e| e.to_string())?;

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

    // Pair the persisted `sync_enabled = true` above with the running core:
    // without this, a sign-in that follows a manual disable leaves
    // settings=true but core.enabled=false (no automatic passes, toggle OFF)
    // until the next restart. `SignedIn` deliberately does not touch `enabled`.
    scheduler.command(SyncCommand::SetEnabled(true));
    scheduler.command(SyncCommand::SignedIn);
    scheduler.emit_status();
    let _ = app;
    Ok(scheduler.status())
}

/// Forget the refresh token and account key. `sync_enabled` stays as the user
/// left it; scheduling stops until the next sign-in.
#[tauri::command]
pub fn sync_sign_out(
    window: tauri::Window,
    app: AppHandle,
    scheduler: State<'_, Scheduler>,
) -> Result<SyncStatus, String> {
    crate::acl::require_caller(&window, &[crate::acl::MAIN_WINDOW])?;
    let mut session = AuthSession::new(sync_config());
    // A missing credential here is the normal signed-out case; failures are
    // ignored because sign-out deletes the credential regardless.
    let _ = session.load_refresh_token_checked();
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
pub fn get_account_stats(window: tauri::Window) -> Result<serde_json::Value, String> {
    crate::acl::require_caller(&window, &[crate::acl::MAIN_WINDOW])?;
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
    // has not synced yet). The command-gated `get_history_stats` requires a
    // calling window label; this path has no window to inspect, so it reads
    // the scheduler-free core (see history.rs).
    let local = crate::history::get_history_stats_core()?;
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

    // --- Audit remediation: Sync Now / re-enable must recover the latch -----

    #[test]
    fn set_enabled_clears_fatal_latch_and_permits_a_pass() {
        // The production recovery path before this fix routed "Sync now"
        // through sync_toggle -> SetEnabled, which never cleared the latch.
        let mut c = core();
        c.finish(PassOutcomeKind::Fatal, Some("wedge".into()), NOW);
        assert!(c.wait_for_command);
        assert!(!c.take_run(NOW + 1), "latched: no automatic pass");

        c.apply(&SyncCommand::SetEnabled(true));
        assert!(!c.wait_for_command, "enabling clears the fatal latch");
        assert!(c.pending_run, "enabling schedules an immediate pass");
        assert!(c.take_run(NOW + 2), "the pass is actually permitted");
    }

    #[test]
    fn set_disabled_preserves_latch_and_never_starts_a_pass() {
        let mut c = core();
        c.finish(PassOutcomeKind::Fatal, Some("wedge".into()), NOW);
        c.apply(&SyncCommand::SetEnabled(false));
        assert!(!c.enabled);
        assert!(
            c.wait_for_command,
            "disabling must not clear the fatal latch"
        );
        assert!(!c.pending_run, "disabling must not schedule a pass");
        assert!(!c.take_run(NOW), "no pass while disabled");

        // Clean-slate disable (no latch) also never starts a pass.
        let mut c2 = core();
        c2.apply(&SyncCommand::SetEnabled(false));
        assert!(!c2.pending_run);
        assert!(!c2.take_run(NOW));
    }

    #[test]
    fn set_enabled_while_signed_out_clears_latch_without_starting_a_pass() {
        let mut c = SchedulerCore::new(true, false);
        c.finish(PassOutcomeKind::Fatal, Some("wedge".into()), NOW);
        c.apply(&SyncCommand::SetEnabled(true));
        assert!(!c.wait_for_command, "latch cleared by the explicit action");
        assert!(
            !c.pending_run,
            "no pass may be scheduled without a signed-in account"
        );
        assert!(!c.take_run(NOW));
    }

    #[test]
    fn run_now_clears_latch_even_when_disabled_stays_disabled() {
        // Run Now is a pass request: it re-arms scheduling but must not flip
        // the user's enable/disable choice.
        let mut c = SchedulerCore::new(false, true);
        c.finish(PassOutcomeKind::Fatal, Some("wedge".into()), NOW);
        c.apply(&SyncCommand::RunNow);
        assert!(!c.wait_for_command);
        assert!(!c.enabled, "Run Now must not enable sync");
        assert!(c.pending_run, "the requested pass is scheduled");
    }

    #[test]
    fn auth_required_does_not_latch_and_marks_signed_out() {
        // A missing credential must be recoverable (reconnect prompt), not a
        // permanent fatal latch.
        let mut c = core();
        c.finish(
            PassOutcomeKind::AuthRequired,
            Some("authentication required".into()),
            NOW,
        );
        assert!(!c.wait_for_command, "AuthRequired must not latch");
        assert!(!c.signed_in, "the session is marked signed out");
        c.apply(&SyncCommand::SignedIn);
        assert!(c.take_run(NOW + 1), "signing back in schedules a pass");
    }

    #[test]
    fn credential_store_failure_is_retryable_not_latched() {
        // A genuine credential-store read failure maps to Retryable: backoff,
        // not a permanent wedge.
        let mut c = core();
        c.finish(
            PassOutcomeKind::Retryable,
            Some("credential store read failed: CredReadW failed: ...".into()),
            NOW,
        );
        assert!(!c.wait_for_command);
        assert!(c.backoff_active, "retryable failures use backoff");
        assert!(c.take_run(NOW + 1_000));
    }

    #[test]
    fn credential_store_error_classifies_as_retryable_and_stays_diagnosable() {
        // The map_err join inside run_pass, extracted so it is testable without
        // a live vault: a store failure must be Retryable (never Fatal, which
        // would latch) and must keep a diagnostic message.
        match credential_store_error("CredReadW failed: access denied".to_string()) {
            SyncError::Retryable(msg) => {
                assert!(
                    msg.contains("credential store read failed") && msg.contains("access denied"),
                    "message must stay diagnosable: {msg}"
                );
            }
            other => panic!("a store failure must be Retryable, got {other:?}"),
        }
        // And the pass-level classification keeps it non-success but unlatched.
        let mut c = core();
        let err = credential_store_error("vault stopped".to_string());
        let (kind, msg, _) = classify_pass(Err(err));
        assert_eq!(kind, PassOutcomeKind::Retryable);
        assert!(msg.is_some());
        c.finish(kind, msg, NOW);
        assert!(!c.wait_for_command, "must not latch the scheduler");
    }

    #[test]
    fn sign_out_clears_pending_work() {
        let mut c = core();
        c.apply(&SyncCommand::RunNow);
        c.apply(&SyncCommand::SignedOut);
        assert!(!c.signed_in);
        assert!(!c.take_run(NOW));
    }

    // -- Audit finding C5: a credential-store failure is not a sign-out -----

    #[test]
    fn store_failed_at_seed_time_does_not_park_the_session() {
        use crate::credentials::SyncRefreshTokenRead as Read;
        // The bug: `Err(..)`/`StoreFailed(..)` collapsed to `signed_in=false`,
        // so `wait_ms` returned None, the thread fell into the 1-hour idle poll
        // with no error and no retry, and the Sync page rendered "signed out"
        // plus a reconnect prompt for an account that is signed in.
        for read in [
            Ok(Read::StoreFailed(
                "CredReadW failed: access is denied.".to_string(),
            )),
            Err(anyhow::anyhow!(
                "Credential Manager not supported on this platform"
            )),
        ] {
            let seed = seed_credential_state(&read);
            assert!(
                seed.signed_in,
                "a store failure must not disarm the scheduler: {read:?}"
            );
            let mut c = SchedulerCore::new(true, seed.signed_in);
            c.last_error = seed.last_error.clone();
            let err = seed
                .last_error
                .expect("a store failure must surface its reason, not a sign-out");
            assert!(
                err.contains("credential store read failed"),
                "the diagnostic must name the real problem, got {err:?}"
            );

            // Seeded into a live core: armed, scheduled, and retrying.
            assert_eq!(c.wait_ms(NOW), Some(0), "armed, so not parked");
            assert!(c.take_run(NOW), "the first pass runs immediately");
            assert!(c.signed_in, "still reported as signed in");
            assert!(c.last_error.is_some(), "and it says why");
        }
    }

    #[test]
    fn seeded_store_failure_matches_the_retryable_pass_diagnostic() {
        use crate::credentials::SyncRefreshTokenRead as Read;
        // Same wording as `run_pass`'s `credential_store_error` branch, so the
        // Sync page shows one message rather than two for one condition.
        let msg = "CredReadW failed: Element not found.";
        let seeded = seed_credential_state(&Ok(Read::StoreFailed(msg.to_string())));
        let (kind, error, hint) = classify_pass(Err(credential_store_error(msg.to_string())));
        assert_eq!(kind, PassOutcomeKind::Retryable);
        assert_eq!(hint, None);
        assert_eq!(seeded.last_error.as_deref(), error.as_deref());
        // And it survives the redaction the status payload applies.
        assert!(sanitize_status_error(seeded.last_error.as_deref().unwrap()).is_some());
    }

    #[test]
    fn seeded_absent_credential_still_reports_signed_out_without_an_error() {
        use crate::credentials::SyncRefreshTokenRead as Read;
        // Absence is the real signed-out state and must look exactly like it
        // used to: disarmed, no error, no reconnect prompt manufactured by an
        // error string.
        for read in [
            Ok(Read::Missing),
            Ok(Read::Present("   ".to_string())),
            Ok(Read::Present(String::new())),
        ] {
            let seed = seed_credential_state(&read);
            assert!(!seed.signed_in, "{read:?} is a signed-out account");
            assert_eq!(seed.last_error, None);
            let mut c = SchedulerCore::new(true, seed.signed_in);
            c.last_error = seed.last_error.clone();
            assert_eq!(c.wait_ms(NOW), None, "a signed-out session is parked");
        }
        // A real token seeds armed and clean.
        let seed = seed_credential_state(&Ok(Read::Present("refresh-token".to_string())));
        assert!(seed.signed_in);
        assert_eq!(seed.last_error, None);
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

    /// F6 mid-pass fence policy (Android PassAccountGuard parity): same
    /// account still signed in → proceed; signed out or switched → abort
    /// before further credential use. Case/whitespace-insensitive like the
    /// pass-entry identity check.
    #[test]
    fn mid_pass_fence_only_passes_the_unchanged_account() {
        let hash = crate::sync::metadata::account_hash_from_email("user@example.com");
        assert!(pass_account_unchanged(&hash, Some("user@example.com")));
        assert!(pass_account_unchanged(&hash, Some("  USER@example.com  ")));
        assert!(
            !pass_account_unchanged(&hash, None),
            "sign-out mid-pass must abort"
        );
        assert!(
            !pass_account_unchanged(&hash, Some("other@example.com")),
            "account switch mid-pass must abort"
        );
    }

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
    fn classify_pass_degraded_stats_is_success_with_warning() {
        // F4: this test previously asserted Rejected here — that mapping was
        // the bug (stalled last_sync_at, non-clearable error for a pass that
        // converged). A degraded Stats pass (skipped oversized/corrupt/future
        // remote state, retained by design) must stay visible and non-silent —
        // never silent success — but as a WARNING on a recorded sync, with no
        // backoff escalation (the condition is permanent, unlike a transient
        // Retryable).
        let (kind, err, _) = classify_pass(Ok(SyncOutcome {
            degraded: true,
            ..SyncOutcome::default()
        }));
        assert_eq!(kind, PassOutcomeKind::Degraded);
        assert!(err.unwrap().contains("skipped remote state"));

        // Precedence is unchanged: real failure counts still dominate a
        // degraded flag on the same pass.
        let (kind, _, _) = classify_pass(Ok(SyncOutcome {
            degraded: true,
            retryable_failures: 1,
            ..SyncOutcome::default()
        }));
        assert_eq!(kind, PassOutcomeKind::Retryable);
        let (kind, _, _) = classify_pass(Ok(SyncOutcome {
            degraded: true,
            rejected_failures: 1,
            ..SyncOutcome::default()
        }));
        assert_eq!(kind, PassOutcomeKind::Rejected);
    }

    #[test]
    fn degraded_pass_advances_last_sync_with_a_clearable_warning() {
        // F4: a degraded pass converged (only some remote state was skipped)
        // so it must record last_sync_at like a success — never stall it —
        // while keeping a VISIBLE warning that clears once a pass is clean.
        let mut c = core();
        assert!(c.take_run(NOW));
        // A stale error from an earlier failure must not survive proof of
        // convergence either.
        c.last_error = Some("older boom".to_string());
        c.finish(
            PassOutcomeKind::Degraded,
            Some("stats ledger partially degraded (skipped remote state retained)".into()),
            NOW,
        );
        assert_eq!(
            c.last_sync_at,
            Some(NOW),
            "converged data must be recorded as synced"
        );
        assert!(
            !c.backoff_active,
            "a permanent condition must not escalate backoff"
        );
        assert!(c.degraded, "the warning must stay visible");
        assert!(c.last_error.is_none(), "a warning is not an error");

        assert!(c.take_run(NOW + SYNC_CADENCE_MS as i64));
        c.finish(PassOutcomeKind::Success, None, NOW + SYNC_CADENCE_MS as i64);
        assert!(!c.degraded, "the warning clears once a pass is clean");
        assert_eq!(c.last_sync_at, Some(NOW + SYNC_CADENCE_MS as i64));
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

    // -- M2: Retry-After must obey the backoff cap -------------------------

    #[test]
    fn retry_after_is_clamped_to_the_backoff_cap() {
        // Drive sends integer seconds, so `Retry-After: 3600` reaches the
        // scheduler as 3_600_000 ms. `finish` takes the max of the (already
        // capped) backoff delay and this hint, so an unclamped header would
        // park the scheduler - and every command behind it, including "Sync
        // now" - for an hour. The cap must cover the header too.
        //
        // CHANGED EXPECTATION (audit finding F16). This test used to assert
        // `c.take_run(NOW + SYNC_BACKOFF_CAP_MS)` succeeds, i.e. that the
        // automatic schedule became runnable one cap after an hour-long
        // `Retry-After` - which is precisely the 60-second-ish polling loop
        // #16 was about. The clamp on `retry_delay_ms` is unchanged and still
        // asserted below; what changed is that an above-cap hint now also
        // holds AUTOMATIC passes until the time Drive actually asked for. The
        // manual override that motivated the original cap is untouched and is
        // asserted separately in
        // `quota_hold_does_not_block_a_manual_sync_now`.
        let mut c = core();
        assert!(c.take_run(NOW));
        c.note_retry_after(Some(3_600_000));
        c.finish(PassOutcomeKind::Retryable, Some("rate limited".into()), NOW);
        assert_eq!(
            c.retry_delay_ms, SYNC_BACKOFF_CAP_MS,
            "an above-cap Retry-After is clamped to the backoff cap"
        );

        // The gate is still a real gate: nothing runs before it opens.
        assert!(
            !c.take_run(NOW + SYNC_BACKOFF_CAP_MS as i64 - 1),
            "a capped Retry-After still delays the retry"
        );
        assert!(
            !c.take_run(NOW + SYNC_BACKOFF_CAP_MS as i64),
            "an above-cap Retry-After defers the AUTOMATIC pass past the cap"
        );
        assert!(
            c.take_run(NOW + 3_600_000),
            "the automatic pass becomes runnable at the requested delay"
        );

        // A hint at or under the cap is honored exactly as sent, and sets up
        // no hold at all.
        let mut c = core();
        assert!(c.take_run(NOW));
        c.note_retry_after(Some(SYNC_BACKOFF_CAP_MS));
        c.finish(PassOutcomeKind::Retryable, Some("rate limited".into()), NOW);
        assert_eq!(c.retry_delay_ms, SYNC_BACKOFF_CAP_MS);
        assert_eq!(c.quota_hold_until_ms, None, "a sub-cap hint is not a hold");
        assert!(
            c.take_run(NOW + SYNC_BACKOFF_CAP_MS as i64),
            "a sub-cap Retry-After is reachable one cap later, as before"
        );

        // A zero/absent hint stays absent (no zero-width gate).
        let mut c = core();
        assert!(c.take_run(NOW));
        c.note_retry_after(None);
        c.finish(PassOutcomeKind::Retryable, Some("boom".into()), NOW);
        assert_eq!(c.retry_delay_ms, SYNC_BACKOFF_BASE_MS);
        assert_eq!(c.quota_hold_until_ms, None);
    }

    // -- F16: a quota-shaped 403 must back off for the time Drive asked ----

    /// Drive's own words for an exhausted daily quota, as a response body.
    const DAILY_LIMIT_403: &str = r#"{"error":{"code":403,"message":"Quota exceeded","errors":[{"domain":"usageLimits","reason":"dailyLimitExceeded"}]}}"#;

    #[test]
    fn daily_limit_403_carries_its_retry_after_through_the_classifier() {
        use crate::sync::drive::classify_forbidden_with_retry_after;
        // 24 h, in the integer seconds Drive actually sends.
        let hint = 86_400_000u64;
        let err = classify_forbidden_with_retry_after(DAILY_LIMIT_403, Some(hint));
        assert_eq!(
            err,
            SyncError::Throttled {
                retry_after_ms: Some(hint)
            }
        );

        // ...and the pass that produced it hands the hint to the scheduler.
        let (kind, _, retry_after_ms) = classify_pass(Err(err));
        assert_eq!(kind, PassOutcomeKind::Retryable, "still a retryable pass");
        assert_eq!(retry_after_ms, Some(hint), "the header must survive");
    }

    #[test]
    fn daily_limit_403_without_a_header_stays_a_plain_retryable() {
        use crate::sync::drive::classify_forbidden_with_retry_after;
        let err = classify_forbidden_with_retry_after(DAILY_LIMIT_403, None);
        assert!(
            matches!(err, SyncError::Retryable(_)),
            "no header, no hint - the pre-existing classification is unchanged"
        );
        let (kind, _, retry_after_ms) = classify_pass(Err(err));
        assert_eq!(kind, PassOutcomeKind::Retryable);
        assert_eq!(retry_after_ms, None);
    }

    #[test]
    fn rate_limited_403_and_429_keep_their_headers_too() {
        use crate::sync::drive::classify_forbidden_with_retry_after;
        // 429 was already honored; 403 now matches it.
        let (kind, _, hint) = classify_pass(Err(SyncError::Throttled {
            retry_after_ms: Some(2_000),
        }));
        assert_eq!(kind, PassOutcomeKind::Retryable);
        assert_eq!(hint, Some(2_000));

        let body = r#"{"error":{"code":403,"reason":"userRateLimitExceeded"}}"#;
        let (kind, _, hint) =
            classify_pass(Err(classify_forbidden_with_retry_after(body, Some(30_000))));
        assert_eq!(kind, PassOutcomeKind::Retryable);
        assert_eq!(hint, Some(30_000));

        // A permission-shaped 403 must NOT become a retry loop just because a
        // header happened to ride along.
        let (kind, _, hint) = classify_pass(Err(classify_forbidden_with_retry_after(
            r#"{"error":{"code":403,"reason":"insufficientPermissions"}}"#,
            Some(3_600_000),
        )));
        assert_eq!(kind, PassOutcomeKind::Fatal, "NotOurs is still fatal");
        assert_eq!(hint, None);
    }

    #[test]
    fn daily_limit_defers_automatic_passes_for_hours_not_sixty_seconds() {
        let mut c = core();
        assert!(c.take_run(NOW));
        c.note_retry_after(Some(86_400_000));
        c.finish(PassOutcomeKind::Retryable, Some("rate limited".into()), NOW);

        assert_eq!(
            c.quota_hold_until_ms,
            Some(NOW + 86_400_000),
            "the requested delay, not the backoff cap"
        );
        // The old behavior: a pass every 60 s against a server that said "come
        // back tomorrow". Each of these used to succeed.
        for offset in [1, 60, 600, 3_600, 21_600, 86_399] {
            assert!(
                !c.take_run(NOW + offset * 1000),
                "automatic pass must not run {offset}s into a 24 h hold"
            );
        }
        assert!(c.take_run(NOW + 86_400_000), "the hold expires on time");
    }

    #[test]
    fn quota_hold_does_not_block_a_manual_sync_now() {
        // The whole reason `retry_delay_ms` stays clamped: a day-long sleep
        // must not mean a day-long deaf ear. `RunNow` sets `pending_run`,
        // which both `wait_ms` and `take_run` honor BEFORE the hold.
        let mut c = core();
        assert!(c.take_run(NOW));
        c.note_retry_after(Some(86_400_000));
        c.finish(PassOutcomeKind::Retryable, Some("rate limited".into()), NOW);

        let soon = NOW + 3_600_000;
        // Idle mid-hold the thread still wakes on the responsiveness slice...
        assert_eq!(c.wait_ms(soon), Some(SYNC_QUOTA_HOLD_POLL_MS));
        // ...but a queued command makes it wake immediately.
        c.apply(&SyncCommand::RunNow);
        assert_eq!(c.wait_ms(soon), Some(0));
        assert!(
            c.take_run(soon),
            "\"Sync now\" starts a pass in the middle of a 24 h quota hold"
        );
    }

    #[test]
    fn quota_hold_keeps_the_thread_responsive_while_it_is_active() {
        // Without this the thread would sit in one `recv_timeout` for the whole
        // hold and a queued command would wait for it; with it, the command is
        // drained within the poll slice while no pass runs.
        let mut c = core();
        assert!(c.take_run(NOW));
        c.note_retry_after(Some(86_400_000));
        c.finish(PassOutcomeKind::Retryable, Some("rate limited".into()), NOW);

        let sleep = c.wait_ms(NOW).expect("armed, so never parked");
        assert_eq!(sleep, SYNC_QUOTA_HOLD_POLL_MS, "wakes on a short slice");
        assert!(sleep <= 60_000, "not for the whole hold");
        // Still no pass, however many times it wakes.
        assert!(!c.take_run(NOW + sleep as i64));
        assert!(!c.take_run(NOW + 3_600_000));
    }

    #[test]
    fn quota_hold_is_released_by_any_non_retryable_outcome() {
        for (kind, err) in [
            (PassOutcomeKind::Success, None),
            (PassOutcomeKind::Degraded, Some("degraded".to_string())),
            (PassOutcomeKind::Rejected, Some("rejected".to_string())),
            (PassOutcomeKind::Fatal, Some("boom".to_string())),
            (PassOutcomeKind::AuthRequired, Some("auth".to_string())),
        ] {
            let mut c = core();
            assert!(c.take_run(NOW));
            c.note_retry_after(Some(86_400_000));
            c.finish(PassOutcomeKind::Retryable, Some("rate limited".into()), NOW);
            assert!(c.quota_hold_until_ms.is_some());

            c.apply(&SyncCommand::RunNow);
            assert!(c.take_run(NOW + 1));
            c.finish(kind, err, NOW + 1);
            assert_eq!(
                c.quota_hold_until_ms, None,
                "{kind:?} must release the quota hold"
            );
        }
    }

    #[test]
    fn quota_hold_is_bounded_and_released_by_a_fresh_sign_in() {
        // A hostile or malformed header must not park the automatic schedule
        // forever.
        let mut c = core();
        assert!(c.take_run(NOW));
        c.note_retry_after(Some(u64::MAX / 4));
        c.finish(PassOutcomeKind::Retryable, Some("rate limited".into()), NOW);
        assert_eq!(
            c.quota_hold_until_ms,
            Some(NOW + SYNC_QUOTA_HOLD_MAX_MS as i64),
            "an absurd hint is bounded to a day"
        );
        assert_eq!(SYNC_QUOTA_HOLD_MAX_MS, 24 * 60 * 60 * 1000);

        c.apply(&SyncCommand::SignedIn);
        assert_eq!(
            c.quota_hold_until_ms, None,
            "a deliberate re-sign-in clears the hold with the rest of the backoff"
        );
    }

    #[test]
    fn a_retryable_pass_without_a_hint_keeps_an_existing_hold_rather_than_shortening_it() {
        let mut c = core();
        assert!(c.take_run(NOW));
        c.note_retry_after(Some(86_400_000));
        c.finish(PassOutcomeKind::Retryable, Some("rate limited".into()), NOW);
        let until = c.quota_hold_until_ms;

        // A user-triggered pass mid-hold fails for an unrelated reason and
        // carries no header; the hold must not be pulled forward.
        c.apply(&SyncCommand::RunNow);
        assert!(c.take_run(NOW + 60_000));
        c.note_retry_after(None);
        c.finish(
            PassOutcomeKind::Retryable,
            Some("network".into()),
            NOW + 60_000,
        );
        assert_eq!(c.quota_hold_until_ms, until);
    }

    #[test]
    fn unconsumed_retry_after_never_leaks_into_a_later_pass() {
        // A hint recorded for a pass that did not end retryable must be
        // discarded, or a much later, unrelated failure inherits the wait.
        let mut c = core();
        assert!(c.take_run(NOW));
        c.note_retry_after(Some(30_000));
        c.finish(PassOutcomeKind::Success, None, NOW);
        assert_eq!(
            c.backoff.current_delay_ms(),
            SYNC_BACKOFF_BASE_MS,
            "the converged pass reset the backoff"
        );

        c.apply(&SyncCommand::RunNow);
        assert!(c.take_run(NOW + 1));
        c.finish(PassOutcomeKind::Retryable, Some("boom".into()), NOW + 1);
        assert_eq!(
            c.retry_delay_ms, SYNC_BACKOFF_BASE_MS,
            "the unconsumed hint must not decorate a later retry delay"
        );
    }

    // -- M3: the reported status must be post-command ----------------------

    #[test]
    fn run_now_status_reports_post_command_state() {
        // `sync_run_now` used to snapshot the status right after `command()`
        // had only QUEUED RunNow, so it emitted and returned the pre-command
        // state: `waiting_for_retry` stayed true even though the command it
        // had just sent clears the fatal latch. The reported status must
        // describe the scheduler as it will be once the command is applied.
        let mut c = core();
        c.finish(PassOutcomeKind::Fatal, Some("wedge".into()), NOW);
        assert!(c.wait_for_command, "the fatal pass latched the scheduler");

        let status = apply_and_snapshot(&mut c, &SyncCommand::RunNow, NOW);
        assert!(
            !status.waiting_for_retry,
            "the reported status must not claim a latch RunNow just cleared"
        );
        assert!(
            c.pending_run,
            "the requested pass really is pending on the core"
        );
        assert!(
            status.next_attempt_ms.is_some(),
            "the reported status still promises an attempt"
        );
    }

    #[test]
    fn toggle_status_reports_the_applied_value() {
        // Same staleness class for sync_toggle: the snapshot must carry the
        // enable/disable that was just applied, not the previous value.
        let mut c = core();
        let status = apply_and_snapshot(&mut c, &SyncCommand::SetEnabled(false), NOW);
        assert!(!status.enabled, "the snapshot reports the applied toggle");
        assert!(!c.enabled, "and the core agrees with the snapshot");
    }

    // -- M4: local changes are debounced ----------------------------------

    #[test]
    fn local_change_debounces_the_next_pass() {
        // A local change must not start a full pass the moment it is drained:
        // it waits SYNC_DEBOUNCE_MS, so a burst of edits coalesces.
        let mut c = core();
        c.apply_with_time(&SyncCommand::LocalChange, NOW);
        assert!(c.pending_run, "the change is recorded");
        assert!(
            !c.take_run(NOW + SYNC_DEBOUNCE_MS as i64 - 1),
            "inside the debounce window no pass may start"
        );
        assert!(
            c.take_run(NOW + SYNC_DEBOUNCE_MS as i64),
            "the pass runs once the window elapsed"
        );
    }

    #[test]
    fn debounced_local_changes_coalesce_into_one_pass() {
        // One settings save per keystroke must not become one full sync pass
        // per save: the window slides with the LAST change and the coalesced
        // request produces exactly one pass.
        let mut c = core();
        for i in 0..5 {
            c.apply_with_time(&SyncCommand::LocalChange, NOW + i * 50);
        }
        assert!(
            !c.take_run(NOW + 200),
            "mid-burst: the coalescing window is still open"
        );
        assert!(
            c.take_run(NOW + 200 + SYNC_DEBOUNCE_MS as i64),
            "one pass after the burst settles"
        );
        assert!(
            !c.take_run(NOW + 200 + SYNC_DEBOUNCE_MS as i64 + 1),
            "exactly one pass, not one pass per change"
        );
    }

    #[test]
    fn run_now_cancels_a_pending_debounce() {
        // Debouncing local edits must never delay an explicit user request.
        let mut c = core();
        c.apply_with_time(&SyncCommand::LocalChange, NOW);
        c.apply_with_time(&SyncCommand::RunNow, NOW + 100);
        assert!(
            c.take_run(NOW + 101),
            "Sync now ignores the debounce window"
        );
    }

    // -- S2: the status payload must not leak provider detail or the email --

    #[test]
    fn status_error_strips_provider_bodies_urls_and_overlong_detail() {
        // `AuthError::Http` renders as `token endpoint HTTP {status}: {body}`:
        // the body is a verbatim provider response and must never reach the
        // payload, while the status stays (the UI maps on it).
        let http = sanitize_status_error(
            "token refresh failed: token endpoint HTTP 503: {\"error\": {\"code\": 503, \
             \"message\": \"backend_error_request_id_9f31\", \"status\": \"UNAVAILABLE\"}}",
        )
        .expect("a non-empty error stays visible");
        assert!(http.contains("HTTP 503"), "{http}");
        assert!(http.contains("token refresh failed"), "{http}");
        assert!(!http.contains("backend_error_request_id_9f31"), "{http}");
        assert!(!http.contains("UNAVAILABLE"), "{http}");
        assert!(!http.contains('{') && !http.contains('}'), "{http}");

        // A transport error names the endpoint: not actionable, so it goes.
        let net = sanitize_status_error(
            "token refresh failed: network error: error sending request for url \
             (https://oauth2.googleapis.com/token): connection closed before message \
             completed",
        )
        .expect("a non-empty error stays visible");
        assert!(!net.contains("https://"), "{net}");
        assert!(!net.contains("googleapis"), "{net}");
        assert!(!net.contains("()"), "{net}");
        assert!(net.contains("network error"), "{net}");

        // Markup is cut the same way, and an over-long diagnostic is bounded.
        let markup = sanitize_status_error(
            "<html><head><title>503 Service Unavailable</title></head></html>",
        )
        .expect("a non-empty error stays visible");
        assert!(!markup.contains("<html"), "{markup}");
        let long = sanitize_status_error(&"x".repeat(500)).expect("still a message");
        assert!(
            long.chars().count() <= STATUS_ERROR_MAX_CHARS,
            "{long} exceeds the payload bound"
        );

        // An empty diagnostic reports no error at all.
        assert!(sanitize_status_error("   ").is_none());
    }

    #[test]
    fn status_error_keeps_the_strings_the_ui_maps_on() {
        // The Sync page maps the sanitized text onto user-facing copy
        // (`describeSyncError` in src/js/settings.js / `syncErr` in
        // web/src/routes/SyncPage.tsx), so redaction must not strip the token
        // those regexes match on - only the provider detail.
        let cases = [
            ("retryable sync failure: timeout", "timeout"),
            (
                "retryable sync failure: token refresh failed: network error: connection reset",
                "network",
            ),
            (
                "retryable sync failure: Drive rate limited (userRateLimitExceeded)",
                "rate limited",
            ),
            ("retryable sync failure: Drive HTTP 500", "HTTP 500"),
            ("rejected: Drive HTTP 400", "rejected"),
            ("authentication required. Sign in again", "Sign in again"),
            (
                "retryable sync failure: credential store read failed: CredReadW failed",
                "credential",
            ),
        ];
        for (raw, trigger) in cases {
            let sanitized = sanitize_status_error(raw).expect("a non-empty message survives");
            assert!(
                sanitized.contains(trigger),
                "sanitizing {raw:?} lost the {trigger:?} trigger the UI maps on: {sanitized}"
            );
        }
    }

    #[test]
    fn sync_status_emit_is_scoped_to_the_main_window() {
        // Why this is a structural assertion: `Scheduler::spawn` and
        // `Scheduler::emit_status` need a live `AppHandle`, and an emit needs a
        // running Tauri app with real webviews - none of which can be
        // constructed in this unit-test harness, so no in-process listener can
        // observe what the event was scoped to. The invariant is asserted on
        // this module's own source instead: the event name is bound once, and
        // the single emit call site in the file is the main-window-scoped
        // helper.  (A compiled-in check beats none, and also fails loudly on a
        // future re-introduction of an app-level broadcast.)
        let src = include_str!("scheduler.rs");
        let event_literal = format!("\"{}\"", SYNC_STATUS_EVENT);
        assert_eq!(
            src.matches(event_literal.as_str()).count(),
            1,
            "the event name must be bound exactly once, so every emission funnels through one helper"
        );
        // Escaped so this assertion cannot match its own source text.
        let emit_call = concat!("\x2eemit", "\x28");
        assert_eq!(
            src.matches(emit_call).count(),
            1,
            "the single emit call site must be the main-window-scoped helper"
        );
        assert!(
            src.contains("fn emit_status_to_main")
                && src.contains("get_webview_window(crate::acl::MAIN_WINDOW)"),
            "the helper must resolve the main window by label instead of broadcasting"
        );
    }

    // -- S5: half-persisted sign-in ----------------------------------------
    //
    // A sign-in that persisted the refresh token and then failed while
    // retrieving the account identity left a credential on disk with no account
    // key (and, for a user switching accounts, a key belonging to the wrong
    // account). `run_pass` then returned AuthRequired forever, flipping
    // `signed_in` false and stopping every future pass while the credential
    // silently stayed behind.
    //
    // Two halves fix it. `sync_sign_in` now resolves the identity BEFORE the
    // credential becomes durable (S5-a), so no new state can be half-persisted;
    // `reconcile_persisted_account_key` repairs the ones already on disk
    // non-destructively (S5-b).
    //
    // MUTATION PROBES - how to prove each guard below is load-bearing.
    //
    // Evidence carried in prose decays: a claim that "the test fails if you
    // break it" is worth nothing to the next reviewer once the transcript that
    // ran it is gone. Each probe below is the change that reintroduces the
    // original bug, and the test that must go red. All three were executed and
    // did go red before this table existed. Re-run the one that matches whatever
    // you are changing.
    //
    // These probes deliberately describe the change instead of quoting runnable
    // lines: a quoted call would be counted by the ordering assertion below,
    // which greps this very file for its own call sites.
    //
    // P1 (ordering)  - in `sync_sign_in`, swap the two calls so the credential
    //                 persist happens BEFORE the account lookup.
    //                 -> `s5_sign_in_resolves_identity_before_persisting_the_credential`
    //                    must fail on "fetch < persist".
    // P2 (destructive) - in `reconcile_persisted_account_key`, add a credential
    //                 delete next to the key write (the delete lives in
    //                 `crate::credentials` and takes no arguments).
    //                 -> `s5_credential_without_an_account_key_is_repaired_not_deadended`
    //                    must fail on "must never delete the stored credential".
    //                 (Only visible because the delete seam is armed: the read
    //                 seam alone cannot observe a delete, which is exactly why
    //                 the seam exists.)
    // P3 (boundary)   - in `reconcile_persisted_account_key`, return early when
    //                 the key is absent instead of repairing it.
    //                 -> `s5_credential_without_an_account_key_is_repaired_not_deadended`
    //                    must fail on the repaired assertion.
    //
    // The byte-cap probes for `auth.rs` and the Android reconcile probe for
    // `V1Stores.kt` are recorded next to their own guards.
    //
    // Note on coverage: P1's guard is structural (it compares source offsets) so
    // it pins ordering only for this exact call path. It is deliberately
    // labeled that way rather than dressed up as behavioral - see the push-back
    // recorded with the Package E traceability matrix.

    /// Helpers for seeding a persisted-account-key state without a browser.
    mod s5 {

        /// Write settings with an explicit account key and enabled flag.
        pub(crate) fn seed_settings(key: Option<&str>, enabled: bool) {
            let mut settings = crate::settings::AppSettings::default();
            settings.sync_account_key = key.map(|k| k.to_string());
            settings.sync_enabled = enabled;
            crate::settings::save_settings(&settings).expect("seed settings must write");
        }

        pub(crate) fn current_settings() -> crate::settings::AppSettings {
            crate::settings::load_settings().expect("settings must load")
        }

        /// Arm the credential read seam so a "stored credential" is observable
        /// without touching the real vault.
        pub(crate) fn arm_stored_credential(token: &str) {
            crate::credentials::set_test_sync_refresh_token_read(Some(Ok(
                crate::credentials::SyncRefreshTokenRead::Present(token.to_string()),
            )));
        }

        pub(crate) fn stored_credential() -> Option<String> {
            match crate::credentials::read_sync_refresh_token_checked().expect("seam read") {
                crate::credentials::SyncRefreshTokenRead::Present(t) => Some(t),
                _ => None,
            }
        }

        pub(crate) fn clear() {
            crate::credentials::set_test_sync_refresh_token_read(None);
            crate::credentials::set_test_sync_refresh_token_write(None);
            crate::credentials::set_test_sync_refresh_token_delete(None);
        }

        /// Arm every seam so a credential is observable AND any write/delete
        /// against it is recorded instead of reaching the real vault.
        pub(crate) fn arm_all(token: &str) {
            arm_stored_credential(token);
            crate::credentials::set_test_sync_refresh_token_write(Some(Ok(())));
            crate::credentials::set_test_sync_refresh_token_delete(Some(Ok(())));
        }
    }

    use s5 as s5h;

    /// BOUNDARY 3 (the only boundary that survives S5-a): the credential write
    /// succeeded but the settings write did not, so the key never landed. The
    /// pass must repair itself instead of dead-ending on AuthRequired.
    #[test]
    fn s5_credential_without_an_account_key_is_repaired_not_deadended() {
        let _guard = crate::sync::stores::store_test_guard();
        s5h::arm_all("refresh-token-present");
        s5h::seed_settings(None, false);

        let outcome =
            reconcile_persisted_account_key("user@example.com").expect("reconcile must repair");

        assert_eq!(
            outcome,
            AccountReconcileOutcome::Repaired,
            "a missing key must be repaired, not reported as already bound"
        );
        let settings = s5h::current_settings();
        assert_eq!(
            settings.sync_account_key.as_deref(),
            Some("user@example.com"),
            "the identity the credential actually belongs to must be persisted"
        );
        assert!(
            settings.sync_enabled,
            "a usable credential is proof of a completed browser grant, so sync \
             must become usable again rather than sitting disabled forever"
        );
        // S5-d: the repair is non-destructive. Asserted against the vault-facing
        // seam, not the read seam - reading the read seam proves nothing, because
        // it is a thread-local value the vault never touches and a delete against
        // it is invisible.
        assert!(
            !crate::credentials::test_sync_refresh_token_deleted(),
            "reconciliation must never delete the stored credential (S5-d)"
        );
        assert_eq!(
            s5h::stored_credential().as_deref(),
            Some("refresh-token-present"),
            "the credential must still be readable afterwards"
        );
        s5h::clear();
    }

    /// A pass that runs twice after a repair must not rewrite settings the
    /// second time (S5-e): a healthy install performs zero extra writes.
    #[test]
    fn s5_a_healthy_binding_is_left_alone() {
        let _guard = crate::sync::stores::store_test_guard();
        s5h::arm_all("refresh-token-present");
        s5h::seed_settings(Some("user@example.com"), true);

        let first = reconcile_persisted_account_key("user@example.com").expect("must reconcile");
        assert_eq!(first, AccountReconcileOutcome::AlreadyBound);

        // Case, surrounding whitespace and re-runs all count as already bound.
        let second =
            reconcile_persisted_account_key("  USER@Example.com ").expect("must reconcile");
        assert_eq!(
            second,
            AccountReconcileOutcome::AlreadyBound,
            "the same identity must not trigger a rewrite"
        );
        let settings = s5h::current_settings();
        assert_eq!(
            settings.sync_account_key.as_deref(),
            Some("user@example.com")
        );
        assert!(settings.sync_enabled);
        s5h::clear();
    }

    /// The gate a real pass actually takes. This is the test that matters for
    /// the divergence case: it proves `run_pass` NEVER reaches the repointing
    /// repair when a DIFFERENT account's key is persisted, which is the exact
    /// inverse of the helper-level test above.
    #[test]
    fn the_identity_gate_refuses_a_divergent_key() {
        // F10: the fold is full-Unicode, not ASCII.
        assert_eq!(
            identity_gate(Some("a@example.com"), "b@example.com"),
            IdentityGate::RefuseDivergent {
                persisted: "a@example.com".to_string()
            }
        );
        // Case-insensitively equal -> already bound, nothing written.
        assert_eq!(
            identity_gate(Some("A@Example.com"), "a@example.com"),
            IdentityGate::AlreadyBound
        );
        assert_eq!(
            identity_gate(Some("ÜSER@Example.com"), "üser@example.com"),
            IdentityGate::AlreadyBound
        );
        // No key at all -> the only path that may repair.
        assert_eq!(
            identity_gate(None, "b@example.com"),
            IdentityGate::RepairMissingKey
        );
    }

    /// Account-switch residue: the credential belongs to B but the persisted key
    /// still names A. The old code refused to sync under a stale key forever.
    /// Function-level contract of [`reconcile_persisted_account_key`]: given a
    /// divergent key it DOES repoint it to the credential's identity.
    ///
    /// NOTE this is deliberately NOT the shipped behaviour for that case. The
    /// repair is now reached only through `identity_gate(..) == RepairMissingKey`
    /// (no key at all). A mismatched key takes `RefuseDivergent` and the pass
    /// fails closed instead — see `the_identity_gate_refuses_a_divergent_key`.
    /// This test pins the helper's own contract, which stays correct for the
    /// interrupted-sign-in path that legitimately calls it.
    #[test]
    fn s5_reconcile_helper_repoints_a_divergent_key_when_called_directly() {
        let _guard = crate::sync::stores::store_test_guard();
        s5h::arm_all("refresh-token-B");
        s5h::seed_settings(Some("a@example.com"), true);

        let outcome = reconcile_persisted_account_key("b@example.com").expect("must reconcile");
        assert_eq!(outcome, AccountReconcileOutcome::Repaired);

        let settings = s5h::current_settings();
        assert_eq!(
            settings.sync_account_key.as_deref(),
            Some("b@example.com"),
            "the key must follow the credential that authenticates the pass"
        );
        assert!(
            settings.sync_enabled,
            "a valid switch must not disable sync"
        );
        assert!(
            !crate::credentials::test_sync_refresh_token_deleted(),
            "repointing the key must never delete the credential (S5-d)"
        );
        assert_eq!(
            s5h::stored_credential().as_deref(),
            Some("refresh-token-B"),
            "the credential for the new account must survive untouched"
        );
        s5h::clear();
    }

    /// The repair path must not destroy pre-existing user data: whatever was on
    /// disk before the half-persisted sign-in must still be there.
    #[test]
    fn s5_repair_preserves_unrelated_settings() {
        let _guard = crate::sync::stores::store_test_guard();
        s5h::arm_all("refresh-token-present");
        s5h::seed_settings(None, false);
        // Mutate fields a user actually chose, then repair.
        let mut settings = s5h::current_settings();
        settings.hotkey = "Ctrl+Alt+7".to_string();
        settings.language = "fr".to_string();
        crate::settings::save_settings(&settings).expect("mutate settings");
        let before_hotkey = settings.hotkey.clone();
        let before_language = settings.language.clone();

        reconcile_persisted_account_key("user@example.com").expect("must reconcile");

        let after = s5h::current_settings();
        assert_eq!(
            after.hotkey, before_hotkey,
            "unrelated settings must survive"
        );
        assert_eq!(
            after.language, before_language,
            "unrelated settings must survive"
        );
        s5h::clear();
    }

    /// BOUNDARY 1 + 2: identity retrieval and the credential write both happen
    /// before anything durable exists. `run_pass` cannot observe this ordering
    /// directly (it needs a browser and a live Drive), so this pins the ordering
    /// of the two calls that the boundary analysis depends on: the account
    /// lookup must precede the credential write.
    #[test]
    fn s5_sign_in_resolves_identity_before_persisting_the_credential() {
        let _guard = crate::sync::stores::store_test_guard();
        // Escaped so this assertion cannot match its own source text (the same
        // trick the emit-scope test uses): the literal must not appear anywhere
        // else in this module.
        let persist_call = concat!("session", ".persist_refresh_token()");
        let fetch_call = concat!("fetch_account_email", "(&token)");
        let src = include_str!("scheduler.rs");
        assert_eq!(
            src.matches(fetch_call).count(),
            1,
            "exactly one account-lookup call site is expected"
        );
        assert_eq!(
            src.matches(persist_call).count(),
            1,
            "exactly one sign-in credential-persist call site is expected"
        );
        let persist = src
            .find(persist_call)
            .expect("sign-in must persist the token");
        let fetch = src
            .find(fetch_call)
            .expect("sign-in must fetch the account email");
        assert!(
            fetch < persist,
            "the account identity must be resolved BEFORE the credential is persisted, \
             so a failure during identity retrieval leaves nothing durable"
        );

        // The explainer must sit next to the call it justifies, so a future edit
        // that reorders these two cannot silently drop the reasoning. The window
        // is generous on purpose: the assertion is about adjacency within
        // `sync_sign_in`, not an exact byte distance.
        let window = &src[persist.saturating_sub(2_500)..persist];
        assert!(
            window.contains("S5-a"),
            "the ordering must be documented where a future edit can see it"
        );
    }

    /// A repair that cannot persist must surface as an `Err` the caller maps to
    /// Retryable, so the pass backs off and retries instead of reporting the user
    /// signed out or latching the scheduler. Made deterministic by blocking the
    /// settings path with a file where the directory has to be.
    #[test]
    fn s5_an_unwritable_settings_file_fails_loudly_and_is_not_reported_signed_out() {
        // One guard held for the whole test. Dropping and re-acquiring it
        // mid-test lets another store test interleave and seed/repair the same
        // shared settings file, which made the second reconcile flakily return
        // AlreadyBound instead of Repaired.
        let _guard = crate::sync::stores::store_test_guard();
        s5h::arm_all("refresh-token-present");
        s5h::seed_settings(None, false);

        // Block the write: `save_settings` needs <data_dir>/Fluence to be a
        // directory, so replace it with a file and the create_dir_all fails.
        let data_dir = crate::sync::stores::data_dir();
        let _ = std::fs::remove_dir_all(&data_dir);
        let _ = std::fs::remove_file(&data_dir);
        std::fs::write(&data_dir, b"not a directory").expect("must be able to block the path");

        let healed = reconcile_persisted_account_key("user@example.com");
        assert!(
            healed.is_err(),
            "a repair that cannot persist must be an explicit failure, never a silent Ok"
        );

        // Clean up the blockage, then prove the same call recovers - i.e. the
        // failure above was the injected one, not a permanent condition.
        // Re-seed explicitly: the blocked attempt left no settings file behind,
        // and stating the pre-state removes any dependence on load-time default
        // creation.
        let _ = std::fs::remove_file(&data_dir);
        s5h::seed_settings(None, false);
        let outcome = reconcile_persisted_account_key("user@example.com").expect("must repair");
        assert_eq!(outcome, AccountReconcileOutcome::Repaired);
        s5h::clear();
    }
}
