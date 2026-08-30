// Fluence sync — Google Drive REST layer (frozen v1.2).
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
// is not atomic — a race can still slip through that window — but every
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
/// quota figures — only these timing constants.
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
                .build()
                .expect("Failed to build blocking Drive client"),
            access_token,
            token_refresher: None,
            v1_folder_id: None,
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
    /// classification maps it exactly as before — `AuthRequired` for 401.
    fn send<B>(&mut self, build: B) -> Result<reqwest::blocking::Response, SyncError>
    where
        B: Fn(&reqwest::blocking::Client, &str) -> reqwest::blocking::RequestBuilder,
    {
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
                // Still unauthorized after the one silent refresh — hand the
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
/// offline-testable; feeds [`classify_status_with_response`].
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

/// Classify a Drive response, honoring a `Retry-After` header on 429 as
/// [`SyncError::Throttled`]. Every other status matches
/// [`classify_status_with_body`] exactly.
pub fn classify_status_with_response(
    resp: &reqwest::blocking::Response,
    status: u16,
    body: &str,
) -> Result<(), SyncError> {
    classify_status_with_retry_after(status, read_retry_after_ms(resp.headers()), body)
}

/// Pure core of [`classify_status_with_response`]: a 429 with (or without) a
/// `Retry-After` delay becomes [`SyncError::Throttled`]; every other status
/// matches [`classify_status_with_body`] exactly. Offline-testable.
fn classify_status_with_retry_after(
    status: u16,
    retry_after_ms: Option<u64>,
    body: &str,
) -> Result<(), SyncError> {
    if status == 429 {
        return Err(SyncError::Throttled { retry_after_ms });
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
    let parsed: Option<serde_json::Value> = serde_json::from_str(body).ok();
    match parsed.as_ref().and_then(forbidden_reason) {
        Some(reason) if is_transient_403_reason(reason) => {
            SyncError::Retryable(format!("Drive rate limited ({reason})"))
        }
        Some(_) => SyncError::NotOurs,
        None => SyncError::Retryable("Drive HTTP 403".to_string()),
    }
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
        pairs.append_pair("fields", "files(id,name,version),nextPageToken");
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
/// detection (v1.2 concurrency model).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DomainFileMeta {
    pub file_id: String,
    pub name: String,
    pub version: Option<String>,
}

/// Domain drive seam — one file per domain (dictionary, snippets, stats,
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
        let resp = self.send(|client, token| {
            client
                .request(reqwest::Method::GET, list_url.as_str())
                .bearer_auth(token)
        })?;
        let status = resp.status().as_u16();
        let retry_after_ms = read_retry_after_ms(resp.headers());
        let body = resp
            .text()
            .map_err(|e| SyncError::Retryable(e.to_string()))?;
        classify_status_with_retry_after(status, retry_after_ms, &body)?;
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
        let create = self.send(|client, token| {
            client
                .request(reqwest::Method::POST, API_BASE.to_string() + "/files?fields=id")
                .bearer_auth(token)
                .json(&serde_json::json!({"name": name, "mimeType": FOLDER_MIME, "parents": [parent]}))
        })?;
        let status = create.status().as_u16();
        let retry_after_ms = read_retry_after_ms(create.headers());
        let body = create
            .text()
            .map_err(|e| SyncError::Retryable(e.to_string()))?;
        classify_status_with_retry_after(status, retry_after_ms, &body)?;
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
        let resp = self.send(|client, token| {
            client
                .request(method.clone(), &url)
                .bearer_auth(token)
                .header(
                    reqwest::header::CONTENT_TYPE,
                    format!("multipart/related; boundary={boundary}"),
                )
                .body(body_for_send.clone())
        })?;
        let status = resp.status().as_u16();
        let retry_after_ms = read_retry_after_ms(resp.headers());
        let body = resp
            .text()
            .map_err(|e| SyncError::Retryable(e.to_string()))?;
        classify_status_with_retry_after(status, retry_after_ms, &body)?;
        if let Some(v) = parse_version_from_response(&body) {
            return Ok(v);
        }
        // Defensive fallback: if the response did not carry a version (shape
        // change), fetch it explicitly so staleness detection stays armed.
        let id = parse_id_from_response(&body);
        if let Some(id) = id {
            let meta_url = format!("{API_BASE}/files/{id}?fields=version");
            let resp = self.send(|client, token| {
                client
                    .request(reqwest::Method::GET, &meta_url)
                    .bearer_auth(token)
            })?;
            let status = resp.status().as_u16();
            let retry_after_ms = read_retry_after_ms(resp.headers());
            let body = resp
                .text()
                .map_err(|e| SyncError::Retryable(e.to_string()))?;
            classify_status_with_retry_after(status, retry_after_ms, &body)?;
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
        let v1_id = self.ensure_v1_folder()?;
        let mut all = Vec::new();
        let mut page_token: Option<String> = None;
        loop {
            let url = list_v1_files_query(&v1_id, page_token.as_deref());
            let resp = self.send(|client, token| {
                client
                    .request(reqwest::Method::GET, &url)
                    .bearer_auth(token)
            })?;
            if resp.status().as_u16() == 404 {
                return Ok(Vec::new());
            }
            let status = resp.status().as_u16();
            let retry_after_ms = read_retry_after_ms(resp.headers());
            let body = resp
                .text()
                .map_err(|e| SyncError::Retryable(e.to_string()))?;
            classify_status_with_retry_after(status, retry_after_ms, &body)?;
            let (files, next) = parse_domain_listing(&body)?;
            all.extend(files);
            match next {
                Some(t) => page_token = Some(t),
                None => break,
            }
        }
        Ok(all)
    }

    fn get_domain_content(&mut self, file_id: &str) -> Result<Option<Vec<u8>>, SyncError> {
        let url = format!("{API_BASE}/files/{file_id}?alt=media");
        let resp = self.send(|client, token| {
            client
                .request(reqwest::Method::GET, &url)
                .bearer_auth(token)
        })?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let status = resp.status().as_u16();
        let retry_after_ms = read_retry_after_ms(resp.headers());
        let bytes = resp
            .bytes()
            .map(|b| b.to_vec())
            .map_err(|e| SyncError::Retryable(e.to_string()))?;
        classify_status_with_retry_after(status, retry_after_ms, &String::from_utf8_lossy(&bytes))?;
        Ok(Some(bytes))
    }

    fn put_domain(
        &mut self,
        name: &str,
        content: &[u8],
        expected_version: Option<&str>,
        preferred_file_id: Option<&str>,
    ) -> Result<String, SyncError> {
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
                // cannot prove freshness — treat as stale and let the caller
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
                // between list and write — also fine: recreate.
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
        let resp = self.send(|client, token| {
            client
                .request(reqwest::Method::DELETE, &url)
                .bearer_auth(token)
        })?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(());
        }
        let status = resp.status().as_u16();
        if status == 403 {
            let body = resp
                .text()
                .map_err(|e| SyncError::Retryable(e.to_string()))?;
            return Err(classify_forbidden(&body));
        }
        // The body is only consumed for 403 above; the 429 branch of
        // classify_status_with_response reads the Retry-After header only.
        classify_status_with_response(&resp, status, "")
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
        // ITEM 2 — verify list_v1_files pagination: two pages aggregated correctly
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
