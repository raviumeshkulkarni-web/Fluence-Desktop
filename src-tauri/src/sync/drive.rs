// Fluence sync - Google Drive REST layer (frozen v1.2).
//
// Implements the `DomainDriveStore` trait against the Drive v3 API using the
// shared reqwest blocking client. Error mapping:
// - `401`                  -> one silent token refresh + request retry, then
//                             `AuthRequired` (reauth) if still rejected
// - `403`                  -> quota/transient reason -> `Retryable`; other
//                            reasons -> `NotOurs`; unparseable body fails
//                            safe to `Retryable` (see `classify_forbidden`)
// - `429` / 5xx / timeout  -> `Retryable` (scheduler backs off)
// - fetch-404              -> `Ok(None)` (domain treated as absent this pass)
// - partial responses      -> `Rejected`, pass aborted before any mutation
//
// Concurrency model (v1.2): Drive API v3 does NOT honor If-Match on media
// updates, so optimistic concurrency is implemented with the per-file
// monotonically increasing `version` revision number instead:
//
//   LIST (id+version) -> GET content -> merge -> PUT(expected_version)
//
// `put_domain` re-checks the live version immediately before writing and
// returns `SyncError::StaleVersion` when another device changed the file in
// the meantime; the engine re-fetches, re-merges and retries. Check-then-write
// is not atomic - a race can still slip through that window - but every
// device keeps its merged state locally, so the next pass converges. This is
// deliberate: deterministic self-healing convergence, not transactional
// guarantees.
//
// `Backoff` is the configurable exponential backoff used by the scheduler
// between passes; it is a pure value type so its schedule is unit-testable.

use crate::sync::error::SyncError;

// Frozen v1.1 drive.appdata layout: appDataFolder/fluence/v1/{dictionary,snippets,stats,settings}.json
pub const APPDATA_FOLDER_ALIAS: &str = "appDataFolder";
pub const FLUENCE_FOLDER_NAME: &str = "fluence";
pub const V1_FOLDER_NAME: &str = "v1";
pub const DICT_FILE: &str = "dictionary.json";
pub const SNIPPETS_FILE: &str = "snippets.json";
pub const STATS_FILE: &str = "stats.json";
pub const SETTINGS_FILE: &str = "settings.json";
pub const DOMAIN_FILES: [&str; 4] = [DICT_FILE, SNIPPETS_FILE, STATS_FILE, SETTINGS_FILE];

// Phase 6 STAGE 3: additive, account-partitioned domains.
//
// These names are deliberately NOT added to `DOMAIN_FILES`. That array is the
// allowlist an already-shipped client uses to decide what to read from `v1`,
// so extending it would make an old client start reading files that are not
// there and would change existing behaviour. Partitioned files live one level
// deeper, under `fluence/v1/acct-<accountHash>/`, and are reached only via
// `account_partition::relative_path` with a token-derived account hash.
//
// The shared path contract lives in `super::account_partition`; the constants
// here are re-exports so the Drive layer has one obvious place to look.
pub use super::account_partition::{AGENTS_FILE, STYLES_FILE};

/// Every domain file name Fluence knows about, partitioned ones included.
///
/// Used only by *new* code that must reason about the full set. Existing
/// v1 read paths must keep using [`DOMAIN_FILES`].
pub const ALL_DOMAIN_FILES: [&str; 6] = [
    DICT_FILE,
    SNIPPETS_FILE,
    STATS_FILE,
    SETTINGS_FILE,
    AGENTS_FILE,
    STYLES_FILE,
];

/// Whether a domain file lives inside a per-account partition rather than
/// directly in `v1`. Pure; delegates to the shared path contract.
pub fn is_partitioned_domain_file(file_name: &str) -> bool {
    super::account_partition::is_partitioned(file_name)
}

const FOLDER_MIME: &str = "application/vnd.google-apps.folder";
const API_BASE: &str = "https://www.googleapis.com/drive/v3";
/// Uploads (multipart create/update) must hit the upload host, not the
/// metadata host.
const UPLOAD_BASE: &str = "https://www.googleapis.com/upload/drive/v3";

/// Hard cap on a domain payload we will read or write. Legitimate envelopes
/// are tens of KB; anything near this bound is corruption or abuse.
pub const MAX_DOMAIN_BYTES: usize = 8 * 1024 * 1024;

/// URL for a multipart create against an upload base. Pure so the host and
/// query are testable offline.
pub fn create_upload_url(upload_base: &str) -> String {
    format!("{upload_base}/files?uploadType=multipart&fields=id,version")
}

/// URL for a multipart update against an upload base. Pure so the host is
/// testable offline. Returns the new file metadata (including `version`) in
/// the response body.
pub fn update_media_url(upload_base: &str, file_id: &str) -> String {
    format!("{upload_base}/files/{file_id}?uploadType=multipart&fields=version")
}

/// Exponential backoff (base 1000ms, factor 2, cap 60000ms). No hardcoded
/// quota figures - only these timing constants.
#[derive(Debug, Clone)]
pub struct Backoff {
    base_ms: u64,
    factor: u32,
    cap_ms: u64,
    delay_ms: u64,
}

impl Default for Backoff {
    fn default() -> Self {
        Self::new(1000, 2, 60_000)
    }
}

impl Backoff {
    pub fn new(base_ms: u64, factor: u32, cap_ms: u64) -> Self {
        Self {
            base_ms,
            factor,
            cap_ms,
            delay_ms: base_ms,
        }
    }

    /// The next wait, then step up for the following failure.
    pub fn next_delay_ms(&mut self) -> u64 {
        let current = self.delay_ms;
        self.delay_ms = (self.delay_ms * u64::from(self.factor)).min(self.cap_ms);
        current
    }

    /// Reset after a successful pass.
    pub fn reset(&mut self) {
        self.delay_ms = self.base_ms;
    }

    pub fn current_delay_ms(&self) -> u64 {
        self.delay_ms
    }
}

/// A hook that re-issues a Drive access token after a 401. Implementations
/// refresh through the OAuth session's stored refresh token; a permanent
/// rejection (revoked refresh token, account removed) returns
/// `AuthRequired` so the pass stops instead of looping.
pub type TokenRefresher = Box<dyn FnMut(&str) -> Result<String, SyncError>>;

/// Test-only fault injection: when `__fault.json` exists in the Fluence data dir,
/// the next Drive operation can be forced to fail without touching the network.
/// This is hotspot-safe: only Fluence's Drive client reads it, the agent's own
/// internet is unaffected. Format: {"remaining":2,"status":429,"retry_after_secs":2,"is_timeout":false}
/// If `remaining` >0 the fault is consumed (decremented) and the requested
/// error is returned. Absent/malformed file = no fault.
///
/// DIRECT PRODUCTION: gated to debug builds only - release builds never read
/// the file, so `PRODUCTION` is not affected by a stray `__fault.json`.
fn maybe_inject_fault(op: &str) -> Option<SyncError> {
    if !cfg!(debug_assertions) {
        return None;
    }
    let data_dir = crate::sync::stores::data_dir();
    let path = data_dir.join("__fault.json");
    let raw = std::fs::read_to_string(&path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    // op filter: if fault specifies "op", only inject for matching op
    if let Some(filter) = v.get("op").and_then(|x| x.as_str()) {
        if filter != op && filter != "send" && filter != "any" {
            return None;
        }
    }
    // corrupt/duplicate faults are handled by separate helpers, not as SyncError
    if v.get("corrupt").and_then(|x| x.as_bool()).unwrap_or(false)
        || v.get("duplicate")
            .and_then(|x| x.as_bool())
            .unwrap_or(false)
    {
        return None;
    }
    let remaining = v.get("remaining").and_then(|x| x.as_u64()).unwrap_or(1);
    if remaining == 0 {
        return None;
    }
    // consume one
    let mut new_v = v.clone();
    if let Some(obj) = new_v.as_object_mut() {
        obj.insert("remaining".to_string(), serde_json::json!(remaining - 1));
        let _ = std::fs::write(
            &path,
            serde_json::to_string_pretty(&new_v).unwrap_or(raw.clone()),
        );
        if remaining - 1 == 0 {
            let _ = std::fs::remove_file(&path);
        }
    }
    let status = v.get("status").and_then(|x| x.as_u64()).unwrap_or(429) as u16;
    let retry_after = v.get("retry_after_secs").and_then(|x| x.as_u64());
    let is_timeout = v
        .get("is_timeout")
        .and_then(|x| x.as_bool())
        .unwrap_or(false);
    let injected = if is_timeout {
        SyncError::Retryable(format!("injected timeout for {op}"))
    } else if status == 429 {
        SyncError::Throttled {
            retry_after_ms: retry_after.map(|s| s * 1000),
        }
    } else if status == 401 {
        SyncError::AuthRequired
    } else if (500..=599).contains(&status) {
        SyncError::Retryable(format!("injected Drive HTTP {status} for {op}"))
    } else {
        SyncError::Retryable(format!("injected fault {status} for {op}"))
    };
    log::warn!(
        "FAULT INJECTED op={} remaining={} -> {:?}",
        op,
        remaining,
        injected
    );
    Some(injected)
}

/// Corrupt-body injection: makes `get_domain_content` return malformed bytes
/// instead of the real Drive content. Format: {"remaining":1,"op":"get_domain_content","corrupt":true,"corrupt_body":"not json"}
/// If `corrupt_body` absent, defaults to `b"corrupt"` (invalid JSON). Consumes `remaining`.
/// DIRECT PRODUCTION: debug-only.
fn maybe_inject_corrupt_body(op: &str) -> Option<Vec<u8>> {
    if !cfg!(debug_assertions) {
        return None;
    }
    let data_dir = crate::sync::stores::data_dir();
    let path = data_dir.join("__fault.json");
    let raw = std::fs::read_to_string(&path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    if !v.get("corrupt").and_then(|x| x.as_bool()).unwrap_or(false) {
        return None;
    }
    if let Some(filter) = v.get("op").and_then(|x| x.as_str()) {
        if filter != op && filter != "any" {
            return None;
        }
    }
    let remaining = v.get("remaining").and_then(|x| x.as_u64()).unwrap_or(1);
    if remaining == 0 {
        return None;
    }
    let mut new_v = v.clone();
    if let Some(obj) = new_v.as_object_mut() {
        obj.insert("remaining".to_string(), serde_json::json!(remaining - 1));
        let _ = std::fs::write(
            &path,
            serde_json::to_string_pretty(&new_v).unwrap_or(raw.clone()),
        );
        if remaining - 1 == 0 {
            let _ = std::fs::remove_file(&path);
        }
    }
    let body = v
        .get("corrupt_body")
        .and_then(|x| x.as_str())
        .unwrap_or("corrupt");
    log::warn!(
        "FAULT INJECTED corrupt_body op={} remaining={} body={:?}",
        op,
        remaining,
        body
    );
    Some(body.as_bytes().to_vec())
}

/// Duplicate-file injection: makes `list_v1_files` return an extra file with
/// the same name but different id/version, simulating Drive's duplicate.
/// Format: {"remaining":1,"op":"list_v1_files","duplicate":true,"duplicate_name":"dictionary.json"}
/// DIRECT PRODUCTION: debug-only.
fn maybe_inject_duplicate(op: &str) -> bool {
    if !cfg!(debug_assertions) {
        return false;
    }
    let data_dir = crate::sync::stores::data_dir();
    let path = data_dir.join("__fault.json");
    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(_) => return false,
    };
    let v: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(_) => return false,
    };
    if !v
        .get("duplicate")
        .and_then(|x| x.as_bool())
        .unwrap_or(false)
    {
        return false;
    }
    if let Some(filter) = v.get("op").and_then(|x| x.as_str()) {
        if filter != op && filter != "any" {
            return false;
        }
    }
    let remaining = v.get("remaining").and_then(|x| x.as_u64()).unwrap_or(1);
    if remaining == 0 {
        return false;
    }
    let mut new_v = v.clone();
    if let Some(obj) = new_v.as_object_mut() {
        obj.insert("remaining".to_string(), serde_json::json!(remaining - 1));
        let _ = std::fs::write(
            &path,
            serde_json::to_string_pretty(&new_v).unwrap_or(raw.clone()),
        );
        if remaining - 1 == 0 {
            let _ = std::fs::remove_file(&path);
        }
    }
    log::warn!("FAULT INJECTED duplicate op={} remaining={}", op, remaining);
    true
}

/// The active account's Drive connection. Holds a memory-only access token;
/// failures are classified by [`classify_status`]. Uses the blocking client
/// because the domain engine runs on a worker thread.
pub struct GoogleDriveStore {
    client: reqwest::blocking::Client,
    access_token: String,
    /// Optional silent token recovery for a single 401 retry per pass.
    token_refresher: Option<TokenRefresher>,
    /// Cached appDataFolder/fluence/v1 folder id.
    v1_folder_id: Option<String>,
    /// Per-account partition folder ids, keyed by folder name. Keyed (not a
    /// single field) so an account switch cannot reuse the previous account's
    /// folder id across the partition boundary.
    partition_folder_ids: std::collections::HashMap<String, String>,
    /// Upload host for multipart writes.
    upload_base: String,
}

impl GoogleDriveStore {
    pub fn new(access_token: String) -> Self {
        Self::with_upload_base(access_token, UPLOAD_BASE.to_string())
    }

    /// Test seam: point upload writes at an injectable upload base.
    pub fn with_upload_base(access_token: String, upload_base: String) -> Self {
        Self {
            client: reqwest::blocking::Client::builder()
                .connect_timeout(std::time::Duration::from_secs(8))
                .timeout(std::time::Duration::from_secs(30))
                // S3: explicit redirect policy. Chosen: `Policy::none()`.
                //
                // Every request this client makes is a `bearer_auth` call to
                // the Drive metadata/upload hosts, so the strongest safe
                // default is to follow nothing at all - a redirect can then
                // never carry the token anywhere. reqwest 0.12.28's default
                // (`limited(10)`, `https_only = false`) follows a same-host
                // `https` -> `http` hop with `Authorization` still attached,
                // because it only strips that header when the HOST changes.
                //
                // Drive's appDataFolder CRUD never legitimately redirects, so
                // nothing production-shaped is lost. A stray 3xx is not an
                // error: `stop()` semantics mean it surfaces as a response,
                // which the existing classification maps to a permanent
                // `SyncError::Rejected` (see the redirect test below and
                // `classify_status_maps_per_sec_23`) - never the opaque
                // transport error that `transport_err` would report as
                // `Retryable` and back off on forever.
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .expect("Failed to build blocking Drive client"),
            access_token,
            token_refresher: None,
            v1_folder_id: None,
            partition_folder_ids: std::collections::HashMap::new(),
            upload_base,
        }
    }

    /// Arm the one-silent-refresh-401-recovery for this pass.
    pub fn set_token_refresher(&mut self, refresher: TokenRefresher) {
        self.token_refresher = Some(refresher);
    }

    /// Build, send, and (once per store) silently recover a 401 by refreshing
    /// the access token and replaying the request with the fresh token. The
    /// keyed retry is transport-level: `build` receives the CURRENT bearer
    /// token so a replay naturally uses the refreshed one. A second 401 (or
    /// any non-401) is returned untouched so the caller's status
    /// classification maps it exactly as before - `AuthRequired` for 401.
    fn send<B>(&mut self, build: B) -> Result<reqwest::blocking::Response, SyncError>
    where
        B: Fn(&reqwest::blocking::Client, &str) -> reqwest::blocking::RequestBuilder,
    {
        if let Some(e) = maybe_inject_fault("send") {
            log::warn!("FAULT INJECTED send -> {:?}", e);
            return Err(e);
        }
        let mut attempts = 0;
        loop {
            let resp = build(&self.client, &self.access_token)
                .send()
                .map_err(transport_err)?;
            if resp.status() != reqwest::StatusCode::UNAUTHORIZED {
                return Ok(resp);
            }
            attempts += 1;
            if attempts > 1 {
                // Still unauthorized after the one silent refresh - hand the
                // 401 back; the caller's classification surfaces AuthRequired.
                return Ok(resp);
            }
            let Some(refresher) = self.token_refresher.as_mut() else {
                return Ok(resp);
            };
            let fresh = refresher(&self.access_token)?;
            self.access_token = fresh;
        }
    }
}

/// Map a reqwest transport error to the engine's retryable kind. Timeouts and
/// connect failures are transient; the scheduler backs off and retries.
fn transport_err(e: reqwest::Error) -> SyncError {
    if e.is_timeout() {
        SyncError::Retryable("timeout".to_string())
    } else {
        SyncError::Retryable(e.to_string())
    }
}

/// Classify a Drive HTTP status into the engine's error kinds.
/// Timeouts and transport failures arrive as `Retryable` via the caller.
///
/// Strict: only 2xx is `Ok`. Every other 4xx (and 3xx) that is not already
/// mapped to `AuthRequired`/`NotOurs`/`Retryable` is a permanent client
/// rejection → `Rejected`: surfaced non-success, never backoff-escalated. Op
/// level handling (e.g. fetch-404 → absent) happens BEFORE this call.
pub fn classify_status(status: u16) -> Result<(), SyncError> {
    match status {
        200..=299 => Ok(()),
        401 => Err(SyncError::AuthRequired),
        403 => Err(SyncError::NotOurs),
        429 => Err(SyncError::Retryable("rate limited".to_string())),
        500..=599 => Err(SyncError::Retryable(format!("Drive HTTP {status}"))),
        _ => Err(SyncError::Rejected(format!("Drive HTTP {status}"))),
    }
}

/// Google returns 403 with these error reasons for TRANSIENT quota/backend
/// throttling. Everything else (e.g. `insufficientPermissions`) is treated as
/// a genuine scope/account mismatch.
const TRANSIENT_403_REASONS: [&str; 6] = [
    "userRateLimitExceeded",
    "rateLimitExceeded",
    "dailyLimitExceeded",
    "sharedLimitExceeded",
    "quotaExceeded",
    "backendError",
];

/// Classify a Drive response using its body. Only 403 inspects the body (to
/// separate transient quota errors from genuine ownership mismatches); every
/// other status matches [`classify_status`] exactly.
pub fn classify_status_with_body(status: u16, body: &str) -> Result<(), SyncError> {
    match status {
        403 => Err(classify_forbidden(body)),
        _ => classify_status(status),
    }
}

/// Read a Drive `429 Retry-After` header into an explicit delay in ms.
/// Drive sends integer seconds (RFC 7231 `Retry-After`); an HTTP-date form is
/// intentionally ignored (cannot be mapped to a relative delay cheaply), the
/// scheduler falls back to its exponential backoff in that case.
///
/// `None` when the header is absent, unparseable, or non-positive. Pure and
/// offline-testable; feeds [`classify_status_with_retry_after`].
pub fn read_retry_after_ms(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    let value = headers
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim();
    let secs = value.parse::<u64>().ok()?;
    if secs == 0 {
        return None;
    }
    // Saturating mul so a hostile huge value cannot overflow.
    Some(secs.saturating_mul(1000))
}

/// Classify a Drive response from its status, `Retry-After` header and body: a
/// 429 - and a quota-shaped 403 - with (or without) a `Retry-After` delay
/// becomes [`SyncError::Throttled`]; every other status matches
/// [`classify_status_with_body`] exactly. Pure and offline-testable.
fn classify_status_with_retry_after(
    status: u16,
    retry_after_ms: Option<u64>,
    body: &str,
) -> Result<(), SyncError> {
    if status == 429 {
        return Err(SyncError::Throttled { retry_after_ms });
    }
    // A quota-shaped 403 carries the SAME Retry-After semantics as a 429 -
    // `dailyLimitExceeded` asks the client to come back tomorrow - so it must
    // not fall through to `classify_status_with_body`, which drops the header.
    if status == 403 {
        return Err(classify_forbidden_with_retry_after(body, retry_after_ms));
    }
    classify_status_with_body(status, body)
}

/// Whether a Drive error reason denotes a transient quota/backend failure
/// (same retryable path as 429). Pure, offline-testable.
fn is_transient_403_reason(reason: &str) -> bool {
    TRANSIENT_403_REASONS.contains(&reason)
}

/// Extract `error.reason`, falling back to the first `error.errors[].reason`.
/// `None` when absent or the body is not a Drive error JSON object.
fn forbidden_reason(value: &serde_json::Value) -> Option<&str> {
    let error = value.get("error")?;
    error.get("reason").and_then(|r| r.as_str()).or_else(|| {
        error
            .get("errors")?
            .as_array()?
            .iter()
            .find_map(|e| e.get("reason").and_then(|r| r.as_str()))
    })
}

/// Classify an HTTP 403 body. A quota-shaped reason takes the retryable
/// backoff path; a parsed non-quota reason stays `NotOurs`.
///
/// Fail-safe default: a 403 whose body cannot be parsed as a Drive error JSON
/// is classified `Retryable`, NOT `NotOurs`. Deliberate: `NotOurs` latches the
/// scheduler into a fleet-wide fatal stop until manual intervention, while a
/// wrongly-retried 403 merely costs one backoff attempt. On unknown bodies we
/// prefer availability over strictness.
pub fn classify_forbidden(body: &str) -> SyncError {
    classify_forbidden_with_retry_after(body, None)
}

/// [`classify_forbidden`], but carrying the response's `Retry-After`.
///
/// A quota-shaped 403 that arrived with an explicit delay becomes
/// [`SyncError::Throttled`] so the scheduler honors it. Without this the
/// `dailyLimitExceeded` header is dropped on the floor and the scheduler
/// re-polls on its 60 s exponential cap forever against a server that asked
/// for a day. With `retry_after_ms: None` the classification is byte-for-byte
/// identical to [`classify_forbidden`] - the only behavior change is that a
/// present header now survives.
pub fn classify_forbidden_with_retry_after(body: &str, retry_after_ms: Option<u64>) -> SyncError {
    let parsed: Option<serde_json::Value> = serde_json::from_str(body).ok();
    match parsed.as_ref().and_then(forbidden_reason) {
        Some(reason) if is_transient_403_reason(reason) => match retry_after_ms {
            // `Throttled` routes to exactly the same retryable scheduling
            // path as `Retryable` (see `frozen.rs`'s failure tally); it only
            // carries the header so the delay is honored.
            Some(_) => SyncError::Throttled { retry_after_ms },
            None => SyncError::Retryable(format!("Drive rate limited ({reason})")),
        },
        Some(_) => SyncError::NotOurs,
        None => SyncError::Retryable("Drive HTTP 403".to_string()),
    }
}

// ---------------------------------------------------------------------------
// Bounded response bodies (audit finding F14).
//
// `MAX_DOMAIN_BYTES` used to be enforced on UPLOAD only, and on download only
// AFTER the whole body had already been buffered - so one oversized (or
// simply lying) remote still cost us a full unbounded allocation, and the
// `resp.text()` calls on listing/metadata/error responses had no ceiling at
// all. Two guards close that:
//   1. the declared `Content-Length` is checked BEFORE a byte is read;
//   2. the body is streamed under a hard ceiling, so an absent, chunked or
//      simply wrong `Content-Length` still costs a bounded allocation.
// Both are needed: the header is only a claim, the ceiling is the guarantee.
// ---------------------------------------------------------------------------

/// Bytes of a FAILURE body we will keep. Drive's error JSON is a few hundred
/// bytes; past this it is abuse or a proxy error page and carries no
/// classification signal worth buffering. Success bodies use the far larger
/// domain cap instead (see [`read_bounded_body`]).
pub const MAX_DIAGNOSTIC_BODY_BYTES: usize = 64 * 1024;

/// A response body read under a hard byte ceiling.
#[derive(Debug)]
pub enum BoundedBody {
    /// The whole body arrived and is <= the cap.
    Complete(Vec<u8>),
    /// The body exceeded the cap and was abandoned mid-stream. At most
    /// `cap + 1` bytes were ever held.
    Truncated,
}

/// Whether a declared `Content-Length` already exceeds `cap`.
///
/// Pre-read half of the guard: when the length is declared we refuse before a
/// single body byte is buffered. `None` - absent, chunked, or simply a lie -
/// falls through to the streaming ceiling, which is what actually holds.
pub fn content_length_over_cap(content_length: Option<u64>, cap: usize) -> Option<u64> {
    content_length.filter(|&len| len > cap as u64)
}

/// Stream `reader` into a `Vec`, abandoning it the moment the body passes
/// `cap`.
///
/// Streaming half of the guard. `Ok(None)` = exceeded `cap`; `Ok(Some(_))` =
/// the complete body, guaranteed <= `cap`. At most `cap + 1` bytes are ever
/// held, so an unbounded or hostile body costs a bounded allocation. Reads
/// through any `Read`, so it is exercisable offline with a cursor.
pub fn read_capped<R: std::io::Read>(
    reader: &mut R,
    cap: usize,
) -> std::io::Result<Option<Vec<u8>>> {
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 16 * 1024];
    loop {
        // `buf.len() <= cap` holds here (we return as soon as it exceeds), so
        // `room >= 1` and the read never asks for a zero-length buffer.
        let room = cap + 1 - buf.len();
        let want = room.min(chunk.len());
        let n = reader.read(&mut chunk[..want])?;
        if n == 0 {
            return Ok(Some(buf));
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.len() > cap {
            return Ok(None);
        }
    }
}

/// [`read_bounded_body_capped`] with the cap chosen from the response status.
///
/// A success body is domain state (listing, media, about) and gets the full
/// domain cap - `list_v1_files` legitimately returns ~1000 rows of
/// `id,name,version,size`, well past a diagnostic-sized cap. A failure body is
/// only read to CLASSIFY the failure, so it gets the small diagnostic cap and a
/// hostile error page can never make us buffer megabytes.
pub fn read_bounded_body(resp: &mut reqwest::blocking::Response) -> Result<BoundedBody, SyncError> {
    let cap = if resp.status().is_success() {
        MAX_DOMAIN_BYTES
    } else {
        MAX_DIAGNOSTIC_BODY_BYTES
    };
    read_bounded_body_capped(resp, cap)
}

/// The whole two-guard read as a pure function of (declared length, stream).
///
/// Guard 1 checks the declared `Content-Length` before a byte is read; guard 2
/// streams under the ceiling for the case where that header is absent, chunked
/// or simply wrong. `Ok(None)` means "do not use this body" for either reason.
/// Split out of [`read_bounded_body_capped`] so both guards are exercisable
/// offline over a plain stream, with the byte counter proving guard 1 read
/// nothing at all.
pub fn read_body_capped<R: std::io::Read>(
    content_length: Option<u64>,
    reader: &mut R,
    cap: usize,
) -> std::io::Result<Option<Vec<u8>>> {
    if content_length_over_cap(content_length, cap).is_some() {
        return Ok(None);
    }
    read_capped(reader, cap)
}

/// [`read_bounded_body`] with an explicit cap: the `Content-Length` pre-check,
/// then the streaming ceiling.
pub fn read_bounded_body_capped(
    resp: &mut reqwest::blocking::Response,
    cap: usize,
) -> Result<BoundedBody, SyncError> {
    match read_body_capped(resp.content_length(), resp, cap) {
        Ok(Some(bytes)) => Ok(BoundedBody::Complete(bytes)),
        Ok(None) => {
            log::warn!("sync: response body passed the {cap} byte cap, abandoned unread");
            Ok(BoundedBody::Truncated)
        }
        Err(e) => Err(SyncError::Retryable(e.to_string())),
    }
}

/// Decode a body read by [`read_bounded_body`] for parsing or classification.
///
/// A truncated SUCCESS body cannot yield valid state, so it is `Rejected` -
/// never decoded, never handed to the engine. A truncated FAILURE body
/// decodes to an empty string so the status and headers alone still classify
/// the failure, which keeps the documented fail-safe (an unparseable 403 is
/// `Retryable`, never the `NotOurs` fatal latch).
pub fn decoded_bounded_body(status: u16, body: &BoundedBody) -> Result<String, SyncError> {
    match body {
        BoundedBody::Complete(bytes) => Ok(String::from_utf8_lossy(bytes).into_owned()),
        BoundedBody::Truncated if (200..=299).contains(&status) => Err(SyncError::Rejected(
            format!("response body exceeds {MAX_DOMAIN_BYTES} byte cap"),
        )),
        BoundedBody::Truncated => Ok(String::new()),
    }
}

/// Read + classify one response: status and `Retry-After` are read off the
/// headers FIRST (so a throttled answer is honored even when the body is
/// never looked at), the body is bounded by [`read_bounded_body`], and the
/// decoded text is handed to the classifier. Returns the decoded body so the
/// caller can parse it on the success path.
fn read_and_classify(resp: &mut reqwest::blocking::Response) -> Result<String, SyncError> {
    let status = resp.status().as_u16();
    let retry_after_ms = read_retry_after_ms(resp.headers());
    let body = decoded_bounded_body(status, &read_bounded_body(resp)?)?;
    classify_status_with_retry_after(status, retry_after_ms, &body)?;
    Ok(body)
}

/// The files query for a folder listing, `trashed=false`, with optional
/// pagination token. Pure so the escaping is testable offline.
fn list_files_query(folder_id: &str, page_token: Option<&str>, spaces: &str) -> String {
    let q = format!("'{folder_id}' in parents and trashed = false");
    let mut url = url::Url::parse(&format!("{API_BASE}/files")).expect("valid base");
    {
        let mut pairs = url.query_pairs_mut();
        pairs.append_pair("q", &q);
        pairs.append_pair("spaces", spaces);
        pairs.append_pair("fields", "files(id,name,version,size),nextPageToken");
        pairs.append_pair("pageSize", "1000");
        if let Some(token) = page_token {
            pairs.append_pair("pageToken", token);
        }
    }
    url.to_string()
}

/// Query for listing files inside the v1 folder (domain files). Public for
/// tests. Pure.
pub fn list_v1_files_query(v1_folder_id: &str, page_token: Option<&str>) -> String {
    list_files_query(v1_folder_id, page_token, APPDATA_FOLDER_ALIAS)
}

/// Extract `id` from a create/folder JSON response. Pure, testable offline.
/// An empty id is rejected (Android parity, `optString("id").ifEmpty { null }`):
/// committing `Some("")` would make the engine believe a remote file exists.
pub fn parse_id_from_response(json: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(json)
        .ok()
        .and_then(|v| v.get("id")?.as_str().map(str::to_string))
        .filter(|s| !s.is_empty())
}

/// Extract the `version` field Drive returns for a file. Drive serializes
/// int64 fields as strings but tolerate bare numbers defensively. Pure.
pub fn parse_version_from_response(json: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    let v = value.get("version")?;
    match v {
        serde_json::Value::String(s) if !s.is_empty() => Some(s.clone()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// Metadata for a domain file inside appDataFolder/fluence/v1. `version` is
/// Drive's monotonically increasing per-file revision used for staleness
/// detection (v1.2 concurrency model). `size` is the Drive-reported byte
/// size when the listing carries it (F5: lets the engine skip downloading an
/// oversized file it would only discard); `None` for folders, Google-native
/// files, and listings that omit it — callers then download and check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DomainFileMeta {
    pub file_id: String,
    pub name: String,
    pub version: Option<String>,
    pub size: Option<u64>,
}

/// Domain drive seam - one file per domain (dictionary, snippets, stats,
/// settings) under appDataFolder/fluence/v1. Handles duplicate files
/// (returns all duplicates for the caller to merge) and corruption skip
/// (invalid envelope treated as absent, not a failure).
pub trait DomainDriveStore {
    fn ensure_v1_folder(&mut self) -> Result<String, SyncError>;
    /// List all files in the v1 folder with their current versions.
    fn list_v1_files(&mut self) -> Result<Vec<DomainFileMeta>, SyncError>;
    /// Download a domain file's bytes. `Ok(None)` = absent (404).
    fn get_domain_content(&mut self, file_id: &str) -> Result<Option<Vec<u8>>, SyncError>;
    /// Upload a domain envelope.
    ///
    /// `expected_version` is the version the caller based its merge on:
    /// - `Some(v)` and the live file still has version `v` → update, return
    ///   the new version.
    /// - `Some(v)` and the live file has a different version (or none) →
    ///   `Err(StaleVersion)`; the caller re-fetches and re-merges.
    /// - `None` (caller believes the file does not exist) and a file DOES
    ///   exist → `Err(StaleVersion)`; never clobber a concurrently created
    ///   domain.
    /// - `None` and no file exists → create, return the new version.
    fn put_domain(
        &mut self,
        name: &str,
        content: &[u8],
        expected_version: Option<&str>,
        preferred_file_id: Option<&str>,
    ) -> Result<String, SyncError>;
    fn delete_domain_file(&mut self, file_id: &str) -> Result<(), SyncError>;

    // ── Phase 6: account-partitioned domains (Agents/Styles) ─────────────
    //
    // These mirror the flat-domain operations above, scoped to a per-account
    // subfolder `fluence/v1/acct-<segment>/`. The flat methods are frozen and
    // untouched; partitioned domains never address `v1` directly.

    /// Resolve (or create) the account partition folder inside `v1`.
    ///
    /// Fail-closed twice: `folder_name` must be a well-formed partition segment
    /// (`acct-` + 64 lowercase hex, see `account_partition`), otherwise no
    /// folder is created and no network call is made for it.
    fn ensure_partition_folder(&mut self, folder_name: &str) -> Result<String, SyncError>;
    /// List files inside a partition folder, with versions for CAS.
    fn list_partition_files(&mut self, folder_id: &str) -> Result<Vec<DomainFileMeta>, SyncError>;
    /// Upload a partitioned domain envelope. Same CAS contract as
    /// [`DomainDriveStore::put_domain`], but the create parent is the partition
    /// folder, so a create can never land in a pooled location.
    fn put_partitioned_domain(
        &mut self,
        folder_id: &str,
        name: &str,
        content: &[u8],
        expected_version: Option<&str>,
        preferred_file_id: Option<&str>,
    ) -> Result<String, SyncError>;

    /// The email address Drive attributes the current access token to
    /// (`about?fields=user`). `Ok(None)` means Drive answered without one.
    ///
    /// This is the token-bound identity the partition key MUST derive from. A
    /// pass that cannot establish it must not address any partition.
    fn drive_account_email(&mut self) -> Result<Option<String>, SyncError>;
}

/// Extract Drive's `size` field (int64 serialized as a string; bare numbers
/// tolerated defensively, like `version`). `None` when absent or unparseable
/// (folders, Google-native files, trimmed listings) — never an error: the
/// caller downloads and checks the bytes instead (F5).
pub fn parse_size_from_response(value: &serde_json::Value) -> Option<u64> {
    match value.get("size") {
        Some(serde_json::Value::String(s)) => s.parse::<u64>().ok(),
        Some(serde_json::Value::Number(n)) => n.as_u64(),
        _ => None,
    }
}

/// Parse a domain listing page extracting `id`, `name` and `version`.
/// Corrupt or partial pages are treated as failures (caller aborts before mutation).
pub fn parse_domain_listing(
    json: &str,
) -> Result<(Vec<DomainFileMeta>, Option<String>), SyncError> {
    let value: serde_json::Value = match serde_json::from_str(json) {
        Ok(v) => v,
        Err(_) => return Err(SyncError::Rejected("corrupt domain listing".to_string())),
    };
    let next = value
        .get("nextPageToken")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let mut files = Vec::new();
    if let Some(list) = value.get("files").and_then(|v| v.as_array()) {
        for item in list {
            let id = item.get("id").and_then(|v| v.as_str());
            let name = item.get("name").and_then(|v| v.as_str());
            let version = item.get("version").and_then(|v| match v {
                serde_json::Value::String(s) if !s.is_empty() => Some(s.clone()),
                serde_json::Value::Number(n) => Some(n.to_string()),
                _ => None,
            });
            match (id, name) {
                (Some(id), Some(name)) if !id.is_empty() && !name.is_empty() => {
                    files.push(DomainFileMeta {
                        file_id: id.to_string(),
                        name: name.to_string(),
                        version,
                        size: parse_size_from_response(item),
                    })
                }
                _ => return Err(SyncError::Rejected("partial domain listing".to_string())),
            }
        }
    } else {
        return Err(SyncError::Rejected(
            "domain listing missing files".to_string(),
        ));
    }
    Ok((files, next))
}

/// Whether a domain file name is one of the 4 valid domain files.
pub fn is_domain_file(name: &str) -> bool {
    DOMAIN_FILES.contains(&name)
}

/// Whether `name` is a well-formed account partition folder segment
/// (`acct-` + 64 lowercase hex). The transport boundary re-checks this so that
/// even a caller bypassing `account_partition::folder_name` cannot address an
/// arbitrary folder through the partitioned path.
pub fn is_partition_folder_name(name: &str) -> bool {
    let Some(hash) = name.strip_prefix(super::account_partition::FOLDER_PREFIX) else {
        return false;
    };
    crate::account_scope::valid_account_hash(hash)
}

impl GoogleDriveStore {
    /// Find or create a folder `name` under `parent` ("appDataFolder" or a
    /// folder id). Caches nothing except at the v1 level.
    fn ensure_folder(&mut self, name: &str, parent: &str) -> Result<String, SyncError> {
        let q = format!(
            "name = '{}' and mimeType = '{FOLDER_MIME}' and '{}' in parents and trashed = false",
            name.replace('\'', "\\'"),
            parent
        );
        let list_url = url::Url::parse_with_params(
            &format!("{API_BASE}/files"),
            [("q", q.as_str()), ("spaces", APPDATA_FOLDER_ALIAS)],
        )
        .expect("valid url");
        let mut resp = self.send(|client, token| {
            client
                .request(reqwest::Method::GET, list_url.as_str())
                .bearer_auth(token)
        })?;
        let body = read_and_classify(&mut resp)?;
        // Folder listings reuse the same parser shape (id/name); versions are
        // simply absent for folders.
        let (folders, _) = Self::parse_file_listing_lenient(&body)?;
        if let Some(first) = folders
            .into_iter()
            .filter(|f| f.name == name)
            .min_by(|a, b| a.file_id.cmp(&b.file_id))
        {
            return Ok(first.file_id);
        }
        let mut create = self.send(|client, token| {
            client
                .request(reqwest::Method::POST, API_BASE.to_string() + "/files?fields=id")
                .bearer_auth(token)
                .json(&serde_json::json!({"name": name, "mimeType": FOLDER_MIME, "parents": [parent]}))
        })?;
        let body = read_and_classify(&mut create)?;
        parse_id_from_response(&body)
            .ok_or_else(|| SyncError::Retryable("folder create missing id".to_string()))
    }

    /// Simple id/name listing parser used for folder lookups (no version
    /// field required). Partial pages are failures.
    fn parse_file_listing_lenient(
        json: &str,
    ) -> Result<(Vec<DomainFileMeta>, Option<String>), SyncError> {
        let value: serde_json::Value = match serde_json::from_str(json) {
            Ok(v) => v,
            Err(_) => return Err(SyncError::Rejected("corrupt listing response".to_string())),
        };
        let mut files = Vec::new();
        if let Some(list) = value.get("files").and_then(|v| v.as_array()) {
            for item in list {
                match (
                    item.get("id").and_then(|v| v.as_str()),
                    item.get("name").and_then(|v| v.as_str()),
                ) {
                    (Some(id), Some(name)) if !id.is_empty() && !name.is_empty() => {
                        files.push(DomainFileMeta {
                            file_id: id.to_string(),
                            name: name.to_string(),
                            version: None,
                            size: parse_size_from_response(item),
                        })
                    }
                    _ => return Err(SyncError::Rejected("partial listing response".to_string())),
                }
            }
        } else {
            return Err(SyncError::Rejected(
                "listing response missing files".to_string(),
            ));
        }
        Ok((files, None))
    }

    /// Multipart/related write (create or update) returning the post-write
    /// version. Drive's `uploadType=multipart` requires RFC 2387
    /// `multipart/related`; a `multipart/form-data` body makes Drive drop the
    /// metadata part, which for creates inside appDataFolder means a
    /// parentless write => 403 insufficientFilePermissions.
    fn multipart_write(
        &mut self,
        method: reqwest::Method,
        url: String,
        name: &str,
        content: &[u8],
        parent_folder_id: Option<&str>,
    ) -> Result<String, SyncError> {
        let mut metadata = serde_json::json!({ "name": name });
        if let Some(pid) = parent_folder_id {
            metadata["parents"] = serde_json::json!([pid]);
        }
        let boundary = format!("fluence_{}", uuid::Uuid::new_v4().simple());
        let mut body: Vec<u8> = Vec::with_capacity(content.len() + 512);
        body.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Type: application/json; charset=UTF-8\r\n\r\n{metadata}\r\n--{boundary}\r\nContent-Type: application/json\r\n\r\n"
            )
            .as_bytes(),
        );
        body.extend_from_slice(content);
        body.extend_from_slice(format!("\r\n--{boundary}--").as_bytes());
        let body_for_send = body.clone();
        let mut resp = self.send(|client, token| {
            client
                .request(method.clone(), &url)
                .bearer_auth(token)
                .header(
                    reqwest::header::CONTENT_TYPE,
                    format!("multipart/related; boundary={boundary}"),
                )
                .body(body_for_send.clone())
        })?;
        let body = read_and_classify(&mut resp)?;
        if let Some(v) = parse_version_from_response(&body) {
            return Ok(v);
        }
        // Defensive fallback: if the response did not carry a version (shape
        // change), fetch it explicitly so staleness detection stays armed.
        let id = parse_id_from_response(&body);
        if let Some(id) = id {
            let meta_url = format!("{API_BASE}/files/{id}?fields=version");
            let mut resp = self.send(|client, token| {
                client
                    .request(reqwest::Method::GET, &meta_url)
                    .bearer_auth(token)
            })?;
            let body = read_and_classify(&mut resp)?;
            if let Some(v) = parse_version_from_response(&body) {
                return Ok(v);
            }
        }
        Err(SyncError::Retryable(
            "write succeeded but no file version was returned".to_string(),
        ))
    }
}

impl DomainDriveStore for GoogleDriveStore {
    fn ensure_v1_folder(&mut self) -> Result<String, SyncError> {
        if let Some(id) = &self.v1_folder_id {
            return Ok(id.clone());
        }
        let fluence_id = self.ensure_folder(FLUENCE_FOLDER_NAME, APPDATA_FOLDER_ALIAS)?;
        let v1_id = self.ensure_folder(V1_FOLDER_NAME, &fluence_id)?;
        self.v1_folder_id = Some(v1_id.clone());
        Ok(v1_id)
    }

    fn list_v1_files(&mut self) -> Result<Vec<DomainFileMeta>, SyncError> {
        if let Some(e) = maybe_inject_fault("list_v1_files") {
            return Err(e);
        }
        let is_duplicate_test = maybe_inject_duplicate("list_v1_files");
        let v1_id = self.ensure_v1_folder()?;
        let mut all = Vec::new();
        let mut page_token: Option<String> = None;
        loop {
            let url = list_v1_files_query(&v1_id, page_token.as_deref());
            let mut resp = self.send(|client, token| {
                client
                    .request(reqwest::Method::GET, &url)
                    .bearer_auth(token)
            })?;
            if resp.status().as_u16() == 404 {
                return Ok(Vec::new());
            }
            let body = read_and_classify(&mut resp)?;
            let (files, next) = parse_domain_listing(&body)?;
            all.extend(files);
            match next {
                Some(t) => page_token = Some(t),
                None => break,
            }
        }
        if is_duplicate_test {
            // Simulate Drive returning two files with same name but different ids/versions.
            // The engine must fetch both, merge their entries, and not lose records.
            if let Some(first) = all.iter().find(|f| f.name == DICT_FILE).cloned() {
                let dup = DomainFileMeta {
                    file_id: format!("{}-dup", first.file_id),
                    name: first.name.clone(),
                    version: Some("999".to_string()),
                    size: first.size,
                };
                log::warn!(
                    "FAULT INJECTED duplicate list_v1_files: cloning {} -> {}",
                    first.file_id,
                    dup.file_id
                );
                all.push(dup);
            } else if !all.is_empty() {
                let first = all[0].clone();
                let dup = DomainFileMeta {
                    file_id: format!("{}-dup", first.file_id),
                    name: first.name.clone(),
                    version: Some("999".to_string()),
                    size: first.size,
                };
                log::warn!(
                    "FAULT INJECTED duplicate list_v1_files: cloning {} -> {}",
                    first.file_id,
                    dup.file_id
                );
                all.push(dup);
            } else {
                // No existing files - create a synthetic duplicate pair for the test.
                // Engine should handle two valid files with same name.
                let dup1 = DomainFileMeta {
                    file_id: "dup-1".to_string(),
                    name: DICT_FILE.to_string(),
                    version: Some("1".to_string()),
                    size: None,
                };
                let dup2 = DomainFileMeta {
                    file_id: "dup-2".to_string(),
                    name: DICT_FILE.to_string(),
                    version: Some("2".to_string()),
                    size: None,
                };
                log::warn!("FAULT INJECTED duplicate list_v1_files: synthetic pair dup-1/dup-2");
                all.push(dup1);
                all.push(dup2);
            }
        }
        Ok(all)
    }

    fn get_domain_content(&mut self, file_id: &str) -> Result<Option<Vec<u8>>, SyncError> {
        if let Some(e) = maybe_inject_fault("get_domain_content") {
            return Err(e);
        }
        if let Some(corrupt) = maybe_inject_corrupt_body("get_domain_content") {
            return Ok(Some(corrupt));
        }
        // Synthetic duplicate content: when list_v1_files injected a dup file,
        // return a valid but distinct dictionary envelope for that dup id.
        // DIRECT PRODUCTION: debug-only - release never returns synthetic data.
        if cfg!(debug_assertions) && (file_id.ends_with("-dup") || file_id.starts_with("dup-")) {
            let dup_content = serde_json::json!({
                "v": 1,
                "entries": [{
                    "syncId": "00000000-0000-4000-8000-000000000001",
                    "businessKey": "duplicateTest",
                    "spoken": "duplicateTest",
                    "corrected": "DuplicateAA",
                    "isEnabled": true,
                    "updatedAt": 1788100000000i64,
                    "deletedAt": null,
                    "deviceId": "dup-device-0000"
                }]
            });
            let bytes = serde_json::to_vec(&dup_content).unwrap();
            log::warn!(
                "DUPLICATE INJECTED get_domain_content file_id={} returning synthetic valid",
                file_id
            );
            return Ok(Some(bytes));
        }
        let url = format!("{API_BASE}/files/{file_id}?alt=media");
        let mut resp = self.send(|client, token| {
            client
                .request(reqwest::Method::GET, &url)
                .bearer_auth(token)
        })?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let status = resp.status().as_u16();
        let retry_after_ms = read_retry_after_ms(resp.headers());
        match read_bounded_body(&mut resp)? {
            BoundedBody::Complete(bytes) => {
                classify_status_with_retry_after(
                    status,
                    retry_after_ms,
                    &String::from_utf8_lossy(&bytes),
                )?;
                Ok(Some(bytes))
            }
            // Over-cap on the success path (audit finding F14): `Rejected`,
            // which is exactly what the engine raises for an oversized remote,
            // so the pass aborts BEFORE any mutation and the remote is left
            // untouched. `Ok(None)` is deliberately NOT used here - the engine
            // reads "absent" as "safe to (re)create", which would overwrite the
            // very file we just refused to read.
            BoundedBody::Truncated if (200..=299).contains(&status) => Err(SyncError::Rejected(
                format!("remote domain body exceeds {MAX_DOMAIN_BYTES} byte cap"),
            )),
            // Over-cap on the failure path: the body is only diagnostic, so
            // discard it and classify from the status + headers alone.
            BoundedBody::Truncated => {
                classify_status_with_retry_after(status, retry_after_ms, "")?;
                Ok(None)
            }
        }
    }

    fn put_domain(
        &mut self,
        name: &str,
        content: &[u8],
        expected_version: Option<&str>,
        preferred_file_id: Option<&str>,
    ) -> Result<String, SyncError> {
        if let Some(e) = maybe_inject_fault("put_domain") {
            return Err(e);
        }
        if content.len() > MAX_DOMAIN_BYTES {
            return Err(SyncError::Rejected(format!(
                "refusing to upload {} byte domain payload",
                content.len()
            )));
        }
        let v1_id = self.ensure_v1_folder()?;
        let files = self.list_v1_files()?;
        let existing = preferred_file_id
            .and_then(|id| files.iter().find(|f| f.file_id == id))
            .or_else(|| files.iter().find(|f| f.name == name));
        match existing {
            Some(meta) => {
                // Concurrency check: the live version must still match what
                // the caller merged against. A missing live version means we
                // cannot prove freshness - treat as stale and let the caller
                // re-read (fail-safe, never fail-open).
                let live = meta.version.as_deref();
                let fresh = match (expected_version, live) {
                    (Some(exp), Some(cur)) => exp == cur,
                    (Some(_), None) => false,
                    (None, Some(_)) => false,
                    (None, None) => true,
                };
                if !fresh {
                    return Err(SyncError::StaleVersion(
                        live.unwrap_or("<none>").to_string(),
                    ));
                }
                self.multipart_write(
                    reqwest::Method::PATCH,
                    update_media_url(&self.upload_base, &meta.file_id),
                    name,
                    content,
                    None,
                )
            }
            None => {
                // File absent. Creating is always safe unless the caller
                // expected to UPDATE an existing file whose row vanished
                // between list and write - also fine: recreate.
                self.multipart_write(
                    reqwest::Method::POST,
                    create_upload_url(&self.upload_base),
                    name,
                    content,
                    Some(&v1_id),
                )
            }
        }
    }

    fn delete_domain_file(&mut self, file_id: &str) -> Result<(), SyncError> {
        let url = format!("{API_BASE}/files/{file_id}");
        let mut resp = self.send(|client, token| {
            client
                .request(reqwest::Method::DELETE, &url)
                .bearer_auth(token)
        })?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(());
        }
        // `read_and_classify` inspects the body for EVERY status, not just
        // 403: only 403 changes classification based on it
        // (`classify_forbidden`), and the bounded read costs a body we were
        // already reading anyway.
        read_and_classify(&mut resp)?;
        Ok(())
    }

    fn ensure_partition_folder(&mut self, folder_name: &str) -> Result<String, SyncError> {
        // Defensive shape check at the transport boundary: even if a caller
        // passes an unvalidated string, nothing outside `v1/acct-<64hex>/`
        // can be addressed. The name carries no separators or dots by
        // construction, so it cannot escape `v1`.
        if !is_partition_folder_name(folder_name) {
            return Err(SyncError::Rejected(format!(
                "refusing to address non-partition folder {folder_name:?}"
            )));
        }
        // Per-account cache, keyed by folder name: reusing one account's folder
        // id for another account would cross the partition boundary.
        if let Some(id) = self.partition_folder_ids.get(folder_name) {
            return Ok(id.clone());
        }
        let v1_id = self.ensure_v1_folder()?;
        let id = self.ensure_folder(folder_name, &v1_id)?;
        self.partition_folder_ids
            .insert(folder_name.to_string(), id.clone());
        Ok(id)
    }

    fn list_partition_files(&mut self, folder_id: &str) -> Result<Vec<DomainFileMeta>, SyncError> {
        if let Some(e) = maybe_inject_fault("list_partition_files") {
            return Err(e);
        }
        let mut all = Vec::new();
        let mut page_token: Option<String> = None;
        loop {
            let url = list_files_query(folder_id, page_token.as_deref(), APPDATA_FOLDER_ALIAS);
            let mut resp = self.send(|client, token| {
                client
                    .request(reqwest::Method::GET, &url)
                    .bearer_auth(token)
            })?;
            if resp.status().as_u16() == 404 {
                return Ok(Vec::new());
            }
            let body = read_and_classify(&mut resp)?;
            let (files, next) = parse_domain_listing(&body)?;
            all.extend(files);
            match next {
                Some(t) => page_token = Some(t),
                None => break,
            }
        }
        Ok(all)
    }

    fn put_partitioned_domain(
        &mut self,
        folder_id: &str,
        name: &str,
        content: &[u8],
        expected_version: Option<&str>,
        preferred_file_id: Option<&str>,
    ) -> Result<String, SyncError> {
        if let Some(e) = maybe_inject_fault("put_partitioned_domain") {
            return Err(e);
        }
        if content.len() > MAX_DOMAIN_BYTES {
            return Err(SyncError::Rejected(format!(
                "refusing to upload {} byte domain payload",
                content.len()
            )));
        }
        let files = self.list_partition_files(folder_id)?;
        let existing = preferred_file_id
            .and_then(|id| files.iter().find(|f| f.file_id == id))
            .or_else(|| files.iter().find(|f| f.name == name));
        match existing {
            Some(meta) => {
                let live = meta.version.as_deref();
                let fresh = match (expected_version, live) {
                    (Some(exp), Some(cur)) => exp == cur,
                    (Some(_), None) => false,
                    (None, Some(_)) => false,
                    (None, None) => true,
                };
                if !fresh {
                    return Err(SyncError::StaleVersion(
                        live.unwrap_or("<none>").to_string(),
                    ));
                }
                self.multipart_write(
                    reqwest::Method::PATCH,
                    update_media_url(&self.upload_base, &meta.file_id),
                    name,
                    content,
                    None,
                )
            }
            None => {
                // Create parented to the PARTITION folder: a create can never
                // land in a pooled location.
                self.multipart_write(
                    reqwest::Method::POST,
                    create_upload_url(&self.upload_base),
                    name,
                    content,
                    Some(folder_id),
                )
            }
        }
    }

    fn drive_account_email(&mut self) -> Result<Option<String>, SyncError> {
        if let Some(e) = maybe_inject_fault("drive_account_email") {
            return Err(e);
        }
        let url = format!("{API_BASE}/about?fields=user");
        let mut resp = self.send(|client, token| {
            client
                .request(reqwest::Method::GET, &url)
                .bearer_auth(token)
        })?;
        let body = read_and_classify(&mut resp)?;
        let value: serde_json::Value = serde_json::from_str(&body)
            .map_err(|_| SyncError::Rejected("corrupt about response".to_string()))?;
        Ok(value
            .get("user")
            .and_then(|u| u.get("emailAddress"))
            .and_then(|e| e.as_str())
            // Same blank predicate as `scheduler::parse_account_email`, which
            // the sign-in path uses. These two parsers disagreed: sign-in
            // rejected a whitespace-only address (`trim().is_empty()`) while
            // every PASS accepted it, and an all-whitespace address normalises
            // to the empty string — so the pass went on to hash `""` and address
            // the partition for it, deriving an identity Drive never returned.
            // One predicate, so both paths fail closed together.
            .filter(|s| !s.trim().is_empty())
            .map(str::to_string))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    /// A tiny loopback server that answers `status_for(attempt)` requests and
    /// then closes. Connection-through-close forces reqwest to open a fresh
    /// TCP connection per request, so each retry lands as a new accept.
    fn loopback_server(
        responses: Vec<(u16, &'static str)>,
    ) -> (std::net::SocketAddr, std::thread::JoinHandle<()>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            for (status, body) in responses {
                let (mut stream, _) = listener.accept().unwrap();
                let mut buf = [0u8; 4096];
                let mut head = Vec::new();
                loop {
                    let n = stream.read(&mut buf).unwrap();
                    if n == 0 {
                        break;
                    }
                    head.extend_from_slice(&buf[..n]);
                    if head.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                write!(
                    stream,
                    "HTTP/1.1 {status} {reason}\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n{body}",
                    reason = if status == 401 { "Unauthorized" } else if status == 400 { "Bad Request" } else { "OK" },
                    len = body.len(),
                )
                .unwrap();
            }
        });
        (addr, handle)
    }

    #[test]
    fn send_retries_once_with_silent_token_refresh_on_401() {
        let (addr, server) = loopback_server(vec![(401, ""), (200, "ok")]);
        let url = format!("http://{addr}/fluency?alt=media");
        let mut store = GoogleDriveStore::with_upload_base(
            "stale-token".to_string(),
            "http://127.0.0.1:0".to_string(),
        );
        let calls = std::rc::Rc::new(std::cell::Cell::new(0usize));
        let calls_for_closure = std::rc::Rc::clone(&calls);
        store.set_token_refresher(Box::new(move |old: &str| {
            assert_eq!(old, "stale-token", "refresher receives the rejected token");
            calls_for_closure.set(calls_for_closure.get() + 1);
            Ok("fresh-token".to_string())
        }));

        let resp = store
            .send(|client, token| {
                client
                    .request(reqwest::Method::GET, &url)
                    .bearer_auth(token)
            })
            .unwrap();
        server.join().unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        assert_eq!(calls.get(), 1, "exactly one silent refresh");
        assert_eq!(
            store.access_token, "fresh-token",
            "store adopts the fresh token"
        );
    }

    #[test]
    fn send_surfaces_401_after_one_refresh_without_looping() {
        let (addr, server) = loopback_server(vec![(401, ""), (401, "")]);
        let url = format!("http://{addr}/fluency?alt=media");
        let mut store = GoogleDriveStore::with_upload_base(
            "stale-token".to_string(),
            "http://127.0.0.1:0".to_string(),
        );
        let calls = std::rc::Rc::new(std::cell::Cell::new(0usize));
        let calls_for_closure = std::rc::Rc::clone(&calls);
        store.set_token_refresher(Box::new(move |_old: &str| {
            calls_for_closure.set(calls_for_closure.get() + 1);
            Ok("fresh-token".to_string())
        }));

        let resp = store
            .send(|client, token| {
                client
                    .request(reqwest::Method::GET, &url)
                    .bearer_auth(token)
            })
            .unwrap();
        server.join().unwrap();
        assert_eq!(
            resp.status(),
            reqwest::StatusCode::UNAUTHORIZED,
            "still-401 after one refresh surfaces so the caller maps AuthRequired"
        );
        assert_eq!(
            calls.get(),
            1,
            "a rejected re-auth loops to AuthRequired, never spins"
        );
    }

    #[test]
    fn send_without_refresher_passes_401_through_untouched() {
        let (addr, server) = loopback_server(vec![(401, "")]);
        let url = format!("http://{addr}/fluency?alt=media");
        let mut store = GoogleDriveStore::with_upload_base(
            "stale-token".to_string(),
            "http://127.0.0.1:0".to_string(),
        );
        let resp = store
            .send(|client, token| {
                client
                    .request(reqwest::Method::GET, &url)
                    .bearer_auth(token)
            })
            .unwrap();
        server.join().unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn send_does_not_refresh_on_non_401_status() {
        let (addr, server) = loopback_server(vec![(400, "bad")]);
        let url = format!("http://{addr}/fluency?alt=media");
        let mut store = GoogleDriveStore::with_upload_base(
            "stale-token".to_string(),
            "http://127.0.0.1:0".to_string(),
        );
        let calls = std::rc::Rc::new(std::cell::Cell::new(0usize));
        let calls_for_closure = std::rc::Rc::clone(&calls);
        store.set_token_refresher(Box::new(move |_old: &str| {
            calls_for_closure.set(calls_for_closure.get() + 1);
            Ok("should-not-be-used".to_string())
        }));

        let resp = store
            .send(|client, token| {
                client
                    .request(reqwest::Method::GET, &url)
                    .bearer_auth(token)
            })
            .unwrap();
        server.join().unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
        assert_eq!(calls.get(), 0, "refresh is reserved for 401 responses");
    }

    /// Serves exactly one request with a `302` and a `Location` header, then
    /// closes. Used to prove the Drive blocking client really carries an
    /// explicit redirect policy: reqwest's default would follow this hop.
    fn redirecting_server(location: String) -> std::net::SocketAddr {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            write!(
                stream,
                "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            )
            .unwrap();
        });
        addr
    }

    #[test]
    fn send_surfaces_a_redirect_instead_of_following_it() {
        // S3: the Drive client follows NOTHING (`redirect::Policy::none()`), so
        // the bearer token can never ride a redirect. The 3xx is handed back
        // as a response instead of an error, which is what lets the existing
        // classification map it to a permanent `Rejected` - and it must not
        // panic or reach the `Location` target.
        let (target_addr, target_server) = loopback_server(vec![(200, "must-not-be-fetched")]);
        let origin = redirecting_server(format!("http://{target_addr}/redirected"));
        let mut store = GoogleDriveStore::with_upload_base(
            "live-token".to_string(),
            "http://127.0.0.1:0".to_string(),
        );
        let resp = store
            .send(|client, token| {
                client
                    .request(
                        reqwest::Method::GET,
                        format!("http://{origin}/file?alt=media"),
                    )
                    .bearer_auth(token)
            })
            .expect("a refused redirect is a response, not a transport error");
        let status = resp.status().as_u16();
        drop(resp);

        assert_eq!(status, 302, "the redirect target is not requested");
        assert!(
            matches!(
                classify_status_with_retry_after(status, None, ""),
                Err(SyncError::Rejected(_))
            ),
            "a refused hop must classify as permanent, not retryable"
        );
        assert!(
            !target_server.is_finished(),
            "the Location target was never contacted"
        );
    }

    #[test]
    fn backoff_doubles_up_to_cap_and_resets() {
        let mut b = Backoff::default();
        assert_eq!(b.current_delay_ms(), 1000);
        assert_eq!(b.next_delay_ms(), 1000);
        assert_eq!(b.next_delay_ms(), 2000);
        assert_eq!(b.next_delay_ms(), 4000);
        assert_eq!(b.next_delay_ms(), 8000);
        assert_eq!(b.next_delay_ms(), 16000);
        assert_eq!(b.next_delay_ms(), 32000);
        assert_eq!(b.next_delay_ms(), 60000, "capped");
        assert_eq!(b.next_delay_ms(), 60000, "stays capped");
        b.reset();
        assert_eq!(b.current_delay_ms(), 1000);
    }

    #[test]
    fn backoff_respects_custom_parameters() {
        let mut b = Backoff::new(500, 3, 5000);
        assert_eq!(b.next_delay_ms(), 500);
        assert_eq!(b.next_delay_ms(), 1500);
        assert_eq!(b.next_delay_ms(), 4500);
        assert_eq!(b.next_delay_ms(), 5000, "capped at 5000");
    }

    #[test]
    fn read_retry_after_parses_integer_seconds() {
        let mut h = reqwest::header::HeaderMap::new();
        assert_eq!(read_retry_after_ms(&h), None, "absent header -> None");

        h.insert(reqwest::header::RETRY_AFTER, "5".parse().unwrap());
        assert_eq!(read_retry_after_ms(&h), Some(5_000));

        h.insert(reqwest::header::RETRY_AFTER, "0".parse().unwrap());
        assert_eq!(
            read_retry_after_ms(&h),
            None,
            "zero (immediate) is not a delay"
        );

        h.insert(reqwest::header::RETRY_AFTER, "abc".parse().unwrap());
        assert_eq!(read_retry_after_ms(&h), None, "non-numeric -> None");
    }

    #[test]
    fn throttled_429_carries_the_retry_after_delay() {
        // A 429 with a parsed delay surfaces Throttled { Some(delay) }.
        assert!(matches!(
            classify_status_with_retry_after(429, Some(5_000), ""),
            Err(SyncError::Throttled {
                retry_after_ms: Some(5_000)
            })
        ));
        // A 429 with no usable header surfaces Throttled { None } (scheduler
        // falls back to its exponential backoff).
        assert!(matches!(
            classify_status_with_retry_after(429, None, ""),
            Err(SyncError::Throttled {
                retry_after_ms: None
            })
        ));
        // Non-429 statuses are unaffected (e.g. 500 stays Retryable).
        assert!(matches!(
            classify_status_with_retry_after(500, Some(5_000), ""),
            Err(SyncError::Retryable(_))
        ));
    }

    #[test]
    fn classify_status_maps_per_sec_23() {
        assert!(matches!(classify_status(401), Err(SyncError::AuthRequired)));
        assert!(matches!(classify_status(403), Err(SyncError::NotOurs)));
        assert!(matches!(classify_status(429), Err(SyncError::Retryable(_))));
        assert!(matches!(classify_status(500), Err(SyncError::Retryable(_))));
        assert!(matches!(classify_status(502), Err(SyncError::Retryable(_))));
        assert!(classify_status(200).is_ok());
        assert!(classify_status(204).is_ok(), "any 2xx is success");
        assert!(classify_status(304).is_err(), "3xx is never treated as OK");
        assert!(
            matches!(classify_status(304), Err(SyncError::Rejected(_))),
            "3xx -> Rejected"
        );
        assert!(
            matches!(classify_status(400), Err(SyncError::Rejected(_))),
            "400 is a permanent client rejection -> Rejected"
        );
        assert!(
            matches!(classify_status(404), Err(SyncError::Rejected(_))),
            "transport-level 404 -> Rejected (fetch-404 is handled op-level)"
        );
    }

    #[test]
    fn forbidden_quota_reason_takes_the_retryable_backoff_path() {
        // Top-level `error.reason` shape.
        assert!(matches!(
            classify_status_with_body(
                403,
                r#"{"error":{"code":403,"message":"Rate limit exceeded","reason":"userRateLimitExceeded"}}"#
            ),
            Err(SyncError::Retryable(_))
        ));
        // Nested `error.errors[].reason` shape.
        assert!(matches!(
            classify_status_with_body(
                403,
                r#"{"error":{"code":403,"errors":[{"domain":"usageLimits","reason":"dailyLimitExceeded"}]}}"#
            ),
            Err(SyncError::Retryable(_))
        ));
        for reason in [
            "rateLimitExceeded",
            "sharedLimitExceeded",
            "quotaExceeded",
            "backendError",
        ] {
            let body = format!(r#"{{"error":{{"code":403,"reason":"{reason}"}}}}"#);
            assert!(
                matches!(classify_forbidden(&body), SyncError::Retryable(_)),
                "{reason} must retry, not latch NotOurs"
            );
        }
    }

    #[test]
    fn forbidden_permission_reason_stays_not_ours() {
        assert_eq!(
            classify_status_with_body(
                403,
                r#"{"error":{"code":403,"message":"insufficient permissions","reason":"insufficientPermissions"}}"#,
            ),
            Err(SyncError::NotOurs)
        );
        // Well-formed but unrecognized reason: still ownership-shaped.
        assert_eq!(
            classify_forbidden(r#"{"error":{"code":403,"reason":"appNotAuthorizedToFile"}}"#),
            SyncError::NotOurs
        );
    }

    #[test]
    fn forbidden_unparseable_body_fails_safe_to_retryable() {
        for body in ["", "<html>Bad Gateway</html>", "not json", "{}"] {
            assert!(
                matches!(classify_forbidden(body), SyncError::Retryable(_)),
                "unparseable 403 body {body:?} must not latch NotOurs"
            );
        }
    }

    // --- Audit finding F14: bounded response bodies -----------------------
    //
    // Every case below is constructed from plain values - a declared length, a
    // cursor over a body - so none of it touches a socket.

    /// A reader that yields exactly `len` bytes of `b'x'` one byte at a time,
    /// while counting what was actually pulled. Lets a test prove the read was
    /// ABANDONED at the ceiling instead of draining the whole body.
    struct CountingBody {
        remaining: usize,
        read_so_far: usize,
    }

    impl CountingBody {
        fn new(len: usize) -> Self {
            Self {
                remaining: len,
                read_so_far: 0,
            }
        }
    }

    impl std::io::Read for CountingBody {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let n = buf.len().min(self.remaining);
            for slot in buf.iter_mut().take(n) {
                *slot = b'x';
            }
            self.remaining -= n;
            self.read_so_far += n;
            Ok(n)
        }
    }

    #[test]
    fn content_length_over_cap_refuses_a_declared_oversize_before_reading() {
        // Declared larger than the cap -> refused, and the value is reported
        // so the log/diagnostic can name the size.
        assert_eq!(
            content_length_over_cap(Some(MAX_DOMAIN_BYTES as u64 + 1), MAX_DOMAIN_BYTES),
            Some(MAX_DOMAIN_BYTES as u64 + 1)
        );
        // Exactly at the cap is allowed (the cap is inclusive).
        assert_eq!(
            content_length_over_cap(Some(MAX_DOMAIN_BYTES as u64), MAX_DOMAIN_BYTES),
            None
        );
        assert_eq!(content_length_over_cap(Some(0), MAX_DOMAIN_BYTES), None);
        // Absent / chunked / lying: the header cannot help, the streaming
        // ceiling is what holds. This is the TOCTOU the listing `size` guard
        // cannot cover.
        assert_eq!(content_length_over_cap(None, MAX_DOMAIN_BYTES), None);
    }

    #[test]
    fn read_capped_returns_the_whole_body_when_it_fits() {
        let body = b"{\"v\":1,\"entries\":[]}".to_vec();
        let mut reader = std::io::Cursor::new(body.clone());
        let got = read_capped(&mut reader, MAX_DOMAIN_BYTES)
            .expect("read cannot fail")
            .expect("body is under the cap");
        assert_eq!(got, body);
    }

    #[test]
    fn a_declared_oversize_is_refused_without_reading_a_single_byte() {
        // Guard 1, end to end: Drive says how big it is, so nothing is read.
        // `CountingBody` counts the pulls, which is the whole point - an
        // over-cap remote must not cost an allocation at all.
        let cap = 128 * 1024usize;
        let mut reader = CountingBody::new(cap * 4);
        let got =
            read_body_capped(Some(cap as u64 * 4 + 1), &mut reader, cap).expect("read cannot fail");
        assert!(got.is_none(), "a declared oversize must be refused");
        assert_eq!(
            reader.read_so_far, 0,
            "the pre-read guard must fire before the stream is touched"
        );

        // A declared length UNDER the cap is not a reason to stop early.
        let body = b"small".to_vec();
        let mut reader = std::io::Cursor::new(body.clone());
        assert_eq!(
            read_body_capped(Some(body.len() as u64), &mut reader, cap).expect("read cannot fail"),
            Some(body)
        );
    }

    #[test]
    fn read_capped_stops_a_lying_content_length_body_at_the_ceiling() {
        // A body that is 3 MiB over the cap, with NO declared length to catch
        // it first: the reader must stop at the ceiling, not buffer 11 MiB.
        let cap = 256 * 1024usize;
        let mut reader = CountingBody::new(cap * 2 + 3);
        let got = read_capped(&mut reader, cap).expect("read cannot fail");
        assert!(got.is_none(), "an over-cap body must report truncation");
        assert!(
            reader.read_so_far <= cap + 1,
            "buffered {} bytes for a {cap} cap: the ceiling did not hold",
            reader.read_so_far
        );
    }

    #[test]
    fn read_capped_holds_the_ceiling_on_a_bounded_reader_too() {
        // Same guarantee on a reader that hands back big chunks: the loop has
        // to size each read by the room left under the cap, not by its own
        // buffer size.
        let cap = 100_000usize;
        let mut reader = std::io::Cursor::new(vec![b'y'; cap * 3]);
        assert!(read_capped(&mut reader, cap)
            .expect("read cannot fail")
            .is_none());
    }

    #[test]
    fn read_capped_accepts_a_body_of_exactly_the_cap() {
        let cap = 4096usize;
        let mut reader = std::io::Cursor::new(vec![b'z'; cap]);
        let got = read_capped(&mut reader, cap)
            .expect("read cannot fail")
            .expect("cap is inclusive");
        assert_eq!(got.len(), cap);
    }

    #[test]
    fn decoded_bounded_body_rejects_a_truncated_success_body() {
        // An over-cap SUCCESS body cannot yield valid state, and must never be
        // handed to the engine as if it had been read.
        assert!(matches!(
            decoded_bounded_body(200, &BoundedBody::Truncated),
            Err(SyncError::Rejected(_))
        ));
    }

    #[test]
    fn decoded_bounded_body_still_classifies_a_truncated_failure_body() {
        // Fail-safe preserved: with no body to inspect the 403 is unparseable,
        // which the documented policy classifies `Retryable` - never the
        // `NotOurs` fatal latch that would need a manual sign-out.
        let text =
            decoded_bounded_body(403, &BoundedBody::Truncated).expect("classify from status");
        assert_eq!(text, "");
        assert!(matches!(
            classify_status_with_retry_after(403, None, &text),
            Err(SyncError::Retryable(_))
        ));
        // The header alone must NOT promote it to `Throttled`: without a body
        // we cannot tell a quota 403 from a permission 403, and guessing
        // `Throttled` on a permission failure would re-introduce the retry
        // loop that `NotOurs` exists to prevent. Exponential backoff is the
        // right answer for an unreadable error.
        assert!(
            matches!(
                classify_status_with_retry_after(403, Some(86_400_000), &text),
                Err(SyncError::Retryable(_))
            ),
            "an unreadable 403 body falls back to plain retryable backoff"
        );
        // ...but a 429 needs no body at all, so its header still survives.
        assert!(matches!(
            classify_status_with_retry_after(429, Some(86_400_000), &text),
            Err(SyncError::Throttled {
                retry_after_ms: Some(86_400_000)
            })
        ));
    }

    #[test]
    fn oversized_remote_domain_body_is_rejected_and_never_returned() {
        // Mirrors the `get_domain_content` success-path decision, end to end
        // over a constructed body: an over-cap remote yields `Rejected` (the
        // engine's own oversized outcome, which aborts the pass BEFORE any
        // mutation and leaves the remote intact) - and specifically NOT
        // `Ok(None)`, which the engine reads as "absent" and would then
        // overwrite.
        let status = 200u16;
        let read = read_capped(
            &mut CountingBody::new(MAX_DOMAIN_BYTES + 1),
            MAX_DOMAIN_BYTES,
        )
        .expect("read cannot fail");
        assert!(read.is_none(), "{MAX_DOMAIN_BYTES}+1 bytes cannot complete");
        assert!(matches!(
            decoded_bounded_body(status, &BoundedBody::Truncated),
            Err(SyncError::Rejected(_))
        ));
    }

    #[test]
    fn diagnostic_body_cap_is_small_but_listing_bodies_keep_the_domain_cap() {
        // A 403's error JSON is a few hundred bytes; a proxy error page is
        // not, and must not be buffered as if it were domain state.
        assert_eq!(MAX_DIAGNOSTIC_BODY_BYTES, 64 * 1024);
        assert!(
            MAX_DIAGNOSTIC_BODY_BYTES < MAX_DOMAIN_BYTES,
            "failure bodies must be capped tighter than domain bodies"
        );
        // A page of 1000 listing rows fits the success-path cap comfortably,
        // which is why the cap is chosen from the status, not fixed.
        let listing = vec![b'0'; 200 * 1024];
        assert!(listing.len() < MAX_DOMAIN_BYTES);
    }

    // --- Audit finding F16: Retry-After on a quota-shaped 403 --------------

    #[test]
    fn quota_403_with_retry_after_keeps_the_header_instead_of_a_plain_retryable() {
        // The bug: `classify_forbidden` returned a bare `Retryable`, so the
        // `Retry-After` was dropped and the scheduler re-polled on its 60 s cap
        // against a server that had asked for a day.
        let body = r#"{"error":{"code":403,"reason":"dailyLimitExceeded"}}"#;
        assert_eq!(
            classify_forbidden_with_retry_after(body, Some(86_400_000)),
            SyncError::Throttled {
                retry_after_ms: Some(86_400_000)
            }
        );
        // The nested `errors[].reason` shape carries it too.
        let nested =
            r#"{"error":{"errors":[{"domain":"usageLimits","reason":"dailyLimitExceeded"}]}}"#;
        assert!(matches!(
            classify_forbidden_with_retry_after(nested, Some(3_600_000)),
            SyncError::Throttled {
                retry_after_ms: Some(3_600_000)
            }
        ));
    }

    #[test]
    fn quota_403_without_retry_after_is_byte_identical_to_the_old_classification() {
        // No header -> unchanged. Nothing about the pre-existing contract moves.
        for body in [
            r#"{"error":{"code":403,"reason":"dailyLimitExceeded"}}"#,
            r#"{"error":{"code":403,"reason":"userRateLimitExceeded"}}"#,
            "<html>Bad Gateway</html>",
        ] {
            assert_eq!(
                classify_forbidden_with_retry_after(body, None),
                classify_forbidden(body)
            );
            assert!(matches!(
                classify_forbidden_with_retry_after(body, None),
                SyncError::Retryable(_)
            ));
        }
    }

    #[test]
    fn non_quota_403_ignores_retry_after_and_stays_not_ours() {
        // A `Retry-After` on a permission failure must not turn the fatal
        // ownership latch into a retry loop.
        let body = r#"{"error":{"code":403,"reason":"insufficientPermissions"}}"#;
        assert_eq!(
            classify_forbidden_with_retry_after(body, Some(3_600_000)),
            SyncError::NotOurs
        );
        assert_eq!(
            classify_status_with_retry_after(403, Some(3_600_000), body),
            Err(SyncError::NotOurs)
        );
    }

    #[test]
    fn retry_after_survives_the_full_status_path_for_429_and_quota_403() {
        assert_eq!(
            classify_status_with_retry_after(429, Some(2_000), ""),
            Err(SyncError::Throttled {
                retry_after_ms: Some(2_000)
            })
        );
        assert_eq!(
            classify_status_with_retry_after(
                429,
                None,
                r#"{"error":{"code":429,"reason":"rateLimitExceeded"}}"#
            ),
            Err(SyncError::Throttled {
                retry_after_ms: None
            })
        );
        // 5xx / 401 / 400 are untouched by the header.
        assert!(matches!(
            classify_status_with_retry_after(500, Some(2_000), ""),
            Err(SyncError::Retryable(_))
        ));
        assert_eq!(
            classify_status_with_retry_after(401, Some(2_000), ""),
            Err(SyncError::AuthRequired)
        );
        assert!(matches!(
            classify_status_with_retry_after(400, Some(2_000), ""),
            Err(SyncError::Rejected(_))
        ));
        assert!(classify_status_with_retry_after(200, Some(2_000), "").is_ok());
    }

    #[test]
    fn upload_urls_hit_the_upload_host_not_metadata() {
        let base = "https://example.test/upload/drive/v3";
        assert_eq!(
            create_upload_url(base),
            "https://example.test/upload/drive/v3/files?uploadType=multipart&fields=id,version"
        );
        assert_eq!(
            update_media_url(base, "file-9"),
            "https://example.test/upload/drive/v3/files/file-9?uploadType=multipart&fields=version"
        );
        assert_eq!(
            create_upload_url(UPLOAD_BASE),
            "https://www.googleapis.com/upload/drive/v3/files?uploadType=multipart&fields=id,version"
        );
    }

    #[test]
    fn list_v1_query_scopes_to_appdata_and_requests_versions() {
        let q = list_v1_files_query("folder-1", None);
        assert!(q.contains("q=%27folder-1%27+in+parents+and+trashed+%3D+false"));
        assert!(q.contains("spaces=appDataFolder"));
        assert!(q.contains("version"));
        // F5: the listing must carry sizes so oversized files can be skipped
        // without downloading them every pass.
        assert!(q.contains("size"));
        assert!(q.contains("pageSize=1000"));
        let q2 = list_v1_files_query("folder-1", Some("next-1"));
        assert!(q2.contains("pageToken=next-1"));
    }

    #[test]
    fn parse_domain_listing_extracts_versions() {
        let (files, next) = parse_domain_listing(
            r#"{"files":[{"id":"a","name":"dictionary.json","version":"7"},{"id":"b","name":"stats.json"}],"nextPageToken":"tok"}"#,
        )
        .expect("valid listing");
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].version.as_deref(), Some("7"));
        assert_eq!(files[1].version, None);
        assert_eq!(next.as_deref(), Some("tok"));

        let (numeric, _) =
            parse_domain_listing(r#"{"files":[{"id":"a","name":"x.json","version":42}]}"#)
                .expect("numeric version parses");
        assert_eq!(numeric[0].version.as_deref(), Some("42"));
    }

    #[test]
    fn parse_domain_listing_extracts_sizes() {
        // F5: Drive serializes int64 sizes as strings; bare numbers tolerated.
        // Absent or garbage sizes are None (folders, trimmed listings) — never
        // a listing failure: the caller downloads and checks instead.
        let (files, _) = parse_domain_listing(
            r#"{"files":[{"id":"a","name":"dictionary.json","version":"7","size":"123"},{"id":"b","name":"stats.json","size":456},{"id":"c","name":"snippets.json"}]}"#,
        )
        .expect("valid listing");
        assert_eq!(files.len(), 3);
        assert_eq!(files[0].size, Some(123));
        assert_eq!(files[1].size, Some(456));
        assert_eq!(files[2].size, None);

        let (garbage, _) =
            parse_domain_listing(r#"{"files":[{"id":"a","name":"x.json","size":"huge"}]}"#)
                .expect("garbage size must not fail the listing");
        assert_eq!(garbage[0].size, None);
    }

    #[test]
    fn parse_domain_listing_partial_response_is_a_failure() {
        let err = parse_domain_listing(r#"{"files":[{"id":"a"}]}"#).expect_err("missing name");
        assert!(matches!(err, SyncError::Rejected(_)));
        let err = parse_domain_listing("not json").expect_err("corrupt response");
        assert!(matches!(err, SyncError::Rejected(_)));
        let err = parse_domain_listing(r#"{"nextPageToken":"tok"}"#).expect_err("missing files");
        assert!(matches!(err, SyncError::Rejected(_)));
    }

    #[test]
    fn parse_version_handles_string_number_and_garbage() {
        assert_eq!(
            parse_version_from_response(r#"{"version":"9"}"#).as_deref(),
            Some("9")
        );
        assert_eq!(
            parse_version_from_response(r#"{"version":9}"#).as_deref(),
            Some("9")
        );
        assert_eq!(parse_version_from_response(r#"{"id":"x"}"#), None);
        assert_eq!(parse_version_from_response("garbage"), None);
    }

    #[test]
    fn paginated_list_aggregates_across_pages() {
        // ITEM 2 - verify list_v1_files pagination: two pages aggregated correctly
        let page1 = r#"{"files":[{"id":"a","name":"dictionary.json","version":"1"},{"id":"b","name":"dictionary.json","version":"2"}],"nextPageToken":"tok123"}"#;
        let (files1, next1) = parse_domain_listing(page1).expect("page1 parses");
        assert_eq!(files1.len(), 2);
        assert_eq!(next1.as_deref(), Some("tok123"));
        let page2 = r#"{"files":[{"id":"c","name":"dictionary.json","version":"3"}]}"#;
        let (files2, next2) = parse_domain_listing(page2).expect("page2 parses");
        assert_eq!(files2.len(), 1);
        assert_eq!(next2, None);
        let mut all = files1;
        all.extend(files2);
        assert_eq!(all.len(), 3);
        // Verify query includes pageToken when present
        let q = list_v1_files_query("v1-id", Some("tok123"));
        assert!(q.contains("pageToken=tok123"));
        let q2 = list_v1_files_query("v1-id", None);
        assert!(!q2.contains("pageToken"));
    }

    #[test]
    fn parse_id_from_response_handles_folder_and_file() {
        assert_eq!(
            parse_id_from_response(r#"{"id":"folder-9","name":"fluence"}"#).as_deref(),
            Some("folder-9")
        );
        assert_eq!(
            parse_id_from_response(r#"{"id":"file-7"}"#).as_deref(),
            Some("file-7")
        );
        assert!(parse_id_from_response("garbage").is_none());
        assert!(parse_id_from_response(r#"{"error":{"code":403}}"#).is_none());
        assert!(
            parse_id_from_response(r#"{"id":""}"#).is_none(),
            "empty id is not a confirmed create"
        );
    }
}
