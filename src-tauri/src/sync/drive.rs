// Fluence sync — Google Drive REST layer (spec §23).
//
// Implements the `DriveStore` trait against the Drive v3 API using the shared
// reqwest client. Error mapping (§23):
// - `401`                  -> `AuthRequired` (reauth)
// - `403` drive.file scope -> `NotOurs` (abort the pass — ABSENCE never runs
//                            on an unconfirmed listing, never retry-bomb)
// - `429` / 5xx / timeout  -> `Retryable` (scheduler backs off)
// - fetch-404 during fetch -> `Ok(None)` (drop the file this pass)
// - partial responses      -> `Rejected`, pass aborted before any mutation
//
// `Backoff` is the configurable exponential backoff used by the scheduler
// between passes; it is a pure value type so its schedule is unit-testable.

use crate::sync::engine::{DriveStore, FileMeta, SyncError};

/// The shared sync folder created lazily under the active account (§14).
pub const FOLDER_NAME: &str = "Fluence Transcribe";

const FOLDER_MIME: &str = "application/vnd.google-apps.folder";
const API_BASE: &str = "https://www.googleapis.com/drive/v3";
/// Uploads (multipart create, media patch) must hit the upload host, not the
/// metadata host (§23 / Phase 0 remediation).
const UPLOAD_BASE: &str = "https://www.googleapis.com/upload/drive/v3";

/// URL for a multipart create against an upload base. Pure so the host and
/// query are testable offline.
pub fn create_upload_url(upload_base: &str) -> String {
    format!("{upload_base}/files?uploadType=multipart&fields=id")
}

/// URL for a media patch against an upload base. Pure so the host is testable
/// offline.
pub fn update_media_url(upload_base: &str, file_id: &str) -> String {
    format!("{upload_base}/files/{file_id}?uploadType=media")
}

/// Exponential backoff (base 1000ms, factor 2, cap 60000ms). No hardcoded
/// quota figures — only these timing constants, per §23/§28.
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

/// The active account's Drive connection. Holds a memory-only access token;
/// a `403`/`401`/`429`/5xx/timeout is classified per §23 by
/// [`classify_status`]. Uses the blocking client because the `DriveStore`
/// trait is synchronous — the engine runs on a worker thread.
pub struct GoogleDriveStore {
    client: reqwest::blocking::Client,
    access_token: String,
    folder_id: Option<String>,
    /// Upload host for create/media writes (§23).
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
            folder_id: None,
            upload_base,
        }
    }

    fn bearer(&self, method: reqwest::Method, url: &str) -> reqwest::blocking::RequestBuilder {
        self.client
            .request(method, url)
            .bearer_auth(&self.access_token)
    }
}

/// Classify a Drive HTTP status into the engine's error kinds (§23).
/// Timeouts and transport failures arrive as `Retryable` via the caller.
///
/// Strict: only 2xx is `Ok`. Every other 4xx (and 3xx) that is not already
/// mapped to `AuthRequired`/`NotOurs`/`Retryable` is a permanent client
/// rejection → `Rejected`: surfaced non-success, never backoff-escalated. Op
/// level handling (e.g. fetch-404 → `Ok(None)`) happens BEFORE this call.
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

/// Classify an `update_content` status. A 404 means the remote file is
/// already gone — the tombstone is already satisfied, an idempotent success
/// (§23 / Phase 1 remediation). Every other status follows [`classify_status`].
pub fn classify_update_status(status: u16) -> Result<(), SyncError> {
    if status == 404 {
        Ok(())
    } else {
        classify_status(status)
    }
}

/// The files query for the sync folder, `trashed=false`, with optional
/// pagination token. Pure so the escaping is testable offline.
pub fn list_files_query(folder_id: &str, page_token: Option<&str>) -> String {
    let q = format!("'{folder_id}' in parents and trashed = false");
    let mut url = url::Url::parse(&format!("{API_BASE}/files")).expect("valid base");
    {
        let mut pairs = url.query_pairs_mut();
        pairs.append_pair("q", &q);
        pairs.append_pair("spaces", "drive");
        pairs.append_pair("fields", "files(id,name,trashed),nextPageToken");
        pairs.append_pair("pageSize", "1000");
        pairs.append_pair("supportsAllDrives", "false");
        if let Some(token) = page_token {
            pairs.append_pair("pageToken", token);
        }
    }
    url.to_string()
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

/// Parse a `files(id,name,trashed)` listing page. A partial/corrupt page is a
/// failure — never a silently-empty result — so the caller aborts the pass
/// before any mutation (§23 / Phase 2).
pub fn parse_file_listing(json: &str) -> Result<(Vec<FileMeta>, Option<String>), SyncError> {
    let value: serde_json::Value = match serde_json::from_str(json) {
        Ok(v) => v,
        Err(_) => return Err(SyncError::Rejected("corrupt listing response".to_string())),
    };
    let next = value
        .get("nextPageToken")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let mut files = Vec::new();
    if let Some(list) = value.get("files").and_then(|v| v.as_array()) {
        for item in list {
            match (
                item.get("id").and_then(|v| v.as_str()),
                item.get("name").and_then(|v| v.as_str()),
            ) {
                // Empty id/name are rejected (Android parity): a partial
                // entry invalidates the page (§23 / Phase 2).
                (Some(id), Some(name)) if !id.is_empty() && !name.is_empty() => {
                    files.push(FileMeta {
                        file_id: id.to_string(),
                        name: name.to_string(),
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
    Ok((files, next))
}

impl DriveStore for GoogleDriveStore {
    fn find_or_create_folder(&mut self) -> Result<(), SyncError> {
        if self.folder_id.is_some() {
            return Ok(());
        }
        let q = format!(
            "name = '{}' and mimeType = '{FOLDER_MIME}' and trashed = false",
            FOLDER_NAME.replace('\'', "\\'")
        );
        let list_url =
            url::Url::parse_with_params(&format!("{API_BASE}/files"), [("q", q.as_str())])
                .expect("valid url");
        let response = self
            .bearer(reqwest::Method::GET, list_url.as_str())
            .send()
            .map_err(|e| {
                if e.is_timeout() {
                    SyncError::Retryable("timeout".to_string())
                } else {
                    SyncError::Retryable(e.to_string())
                }
            })?;
        classify_status(response.status().as_u16())?;
        let body = response
            .text()
            .map_err(|e| SyncError::Retryable(e.to_string()))?;
        let (folders, _) = parse_file_listing(&body)?;
        if let Some(first) = folders.first() {
            self.folder_id = Some(first.file_id.clone());
            return Ok(());
        }

        let create = self
            .bearer(
                reqwest::Method::POST,
                &format!("{API_BASE}/files?fields=id"),
            )
            .json(&serde_json::json!({
                "name": FOLDER_NAME,
                "mimeType": FOLDER_MIME,
            }))
            .send()
            .map_err(|e| SyncError::Retryable(e.to_string()))?;
        classify_status(create.status().as_u16())?;
        let body = create
            .text()
            .map_err(|e| SyncError::Retryable(e.to_string()))?;
        let id = parse_id_from_response(&body)
            .ok_or_else(|| SyncError::Retryable("folder create response missing id".to_string()))?;
        self.folder_id = Some(id);
        Ok(())
    }

    fn list_files(&mut self) -> Result<Vec<FileMeta>, SyncError> {
        self.find_or_create_folder()?;
        let folder_id = self
            .folder_id
            .as_ref()
            .expect("folder id cached by find_or_create_folder");
        let mut all = Vec::new();
        let mut page_token: Option<String> = None;
        loop {
            let url = list_files_query(folder_id, page_token.as_deref());
            let response = self
                .bearer(reqwest::Method::GET, &url)
                .send()
                .map_err(|e| {
                    if e.is_timeout() {
                        SyncError::Retryable("timeout".to_string())
                    } else {
                        SyncError::Retryable(e.to_string())
                    }
                })?;
            match classify_status(response.status().as_u16()) {
                // 403 drive.file scope: abort the whole pass — ABSENCE must
                // never run on an unconfirmed listing (§23 / Phase 2).
                Err(SyncError::NotOurs) => return Err(SyncError::NotOurs),
                other => other?,
            }
            let body = response
                .text()
                .map_err(|e| SyncError::Retryable(e.to_string()))?;
            let (files, next) = parse_file_listing(&body)?;
            all.extend(files);
            match next {
                Some(token) => page_token = Some(token),
                None => break,
            }
        }
        Ok(all)
    }

    fn get_content(&mut self, id: &str) -> Result<Option<Vec<u8>>, SyncError> {
        let url = format!("{API_BASE}/files/{id}?alt=media");
        let response = self
            .bearer(reqwest::Method::GET, &url)
            .send()
            .map_err(|e| {
                if e.is_timeout() {
                    SyncError::Retryable("timeout".to_string())
                } else {
                    SyncError::Retryable(e.to_string())
                }
            })?;
        let status = response.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Ok(None); // fetch-404 during VALIDATE: drop file from group this pass
        }
        match classify_status(status.as_u16()) {
            Err(SyncError::NotOurs) => return Ok(None), // drop file this pass
            other => other?,
        }
        response
            .bytes()
            .map(|b| Some(b.to_vec()))
            .map_err(|e| SyncError::Retryable(e.to_string()))
    }

    fn create_file(
        &mut self,
        name: &str,
        record: &crate::sync::wire::WireRecord,
    ) -> Result<String, SyncError> {
        self.find_or_create_folder()?;
        let folder_id = self
            .folder_id
            .as_ref()
            .expect("folder id cached by find_or_create_folder");
        let metadata = serde_json::json!({
            "name": name,
            "parents": [folder_id],
        })
        .to_string();
        let content = record.to_json();
        let part_meta = reqwest::blocking::multipart::Part::text(metadata)
            .mime_str("application/json")
            .map_err(|e| SyncError::Retryable(e.to_string()))?;
        let part_bytes = reqwest::blocking::multipart::Part::bytes(content.into_bytes())
            .file_name(name.to_string())
            .mime_str("application/octet-stream")
            .map_err(|e| SyncError::Retryable(e.to_string()))?;
        let form = reqwest::blocking::multipart::Form::new()
            .part("metadata", part_meta)
            .part("file", part_bytes);
        let response = self
            .bearer(reqwest::Method::POST, &create_upload_url(&self.upload_base))
            .multipart(form)
            .send()
            .map_err(|e| {
                if e.is_timeout() {
                    SyncError::Retryable("timeout".to_string())
                } else {
                    SyncError::Retryable(e.to_string())
                }
            })?;
        classify_status(response.status().as_u16())?;
        let body = response
            .text()
            .map_err(|e| SyncError::Retryable(e.to_string()))?;
        parse_id_from_response(&body)
            .ok_or_else(|| SyncError::Retryable("create response missing id".to_string()))
    }

    fn update_content(
        &mut self,
        id: &str,
        record: &crate::sync::wire::WireRecord,
    ) -> Result<(), SyncError> {
        let url = update_media_url(&self.upload_base, id);
        let response = self
            .bearer(reqwest::Method::PATCH, &url)
            .body(record.to_json())
            .send()
            .map_err(|e| {
                if e.is_timeout() {
                    SyncError::Retryable("timeout".to_string())
                } else {
                    SyncError::Retryable(e.to_string())
                }
            })?;
        classify_update_status(response.status().as_u16())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn upload_urls_hit_the_upload_host_not_metadata() {
        let base = "https://example.test/upload/drive/v3";
        assert_eq!(
            create_upload_url(base),
            "https://example.test/upload/drive/v3/files?uploadType=multipart&fields=id"
        );
        assert_eq!(
            update_media_url(base, "file-9"),
            "https://example.test/upload/drive/v3/files/file-9?uploadType=media"
        );
        // The production default keeps Google's upload host.
        assert_eq!(
            create_upload_url(UPLOAD_BASE),
            "https://www.googleapis.com/upload/drive/v3/files?uploadType=multipart&fields=id"
        );
    }

    #[test]
    fn update_status_404_is_idempotent_success() {
        // A tombstone PATCH that 404s means the remote file is already gone:
        // the tombstone is satisfied — never retried, never rejected.
        assert!(classify_update_status(404).is_ok(), "already gone -> Ok");
        assert!(classify_update_status(200).is_ok());
        assert!(
            matches!(classify_update_status(400), Err(SyncError::Rejected(_))),
            "other 4xx still rejected"
        );
        assert!(matches!(
            classify_update_status(500),
            Err(SyncError::Retryable(_))
        ));
    }

    #[test]
    fn list_files_query_escapes_folder_and_adds_pagination() {
        let q1 = list_files_query("folder-1", None);
        assert!(q1.contains("q=%27folder-1%27+in+parents+and+trashed+%3D+false"));
        assert!(q1.contains("fields=files%28id%2Cname%2Ctrashed%29%2CnextPageToken"));
        assert!(q1.contains("pageSize=1000"));
        assert!(!q1.contains("pageToken"));

        let q2 = list_files_query("folder-1", Some("next-1"));
        assert!(q2.contains("pageToken=next-1"));
    }

    #[test]
    fn parse_file_listing_extracts_files_and_page_token() {
        let (files, next) = parse_file_listing(
            r#"{"files":[{"id":"a","name":"a.json","trashed":false},{"id":"b","name":"b.json","trashed":true}],"nextPageToken":"tok"}"#,
        )
        .expect("valid listing");
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].file_id, "a");
        assert_eq!(files[0].name, "a.json");
        assert_eq!(next.as_deref(), Some("tok"));

        let (files, next) = parse_file_listing(r#"{"files":[]}"#).expect("valid listing");
        assert!(files.is_empty());
        assert!(next.is_none());
    }

    #[test]
    fn parse_file_listing_partial_response_is_a_failure() {
        // A partial/corrupt page is an error, never a silent empty result:
        // the pass must abort before mutation (§23 / Phase 2).
        let err = parse_file_listing(r#"{"files":[{"id":"a"}]}"#) // name missing
            .expect_err("partial response is a failure");
        assert!(matches!(err, SyncError::Rejected(_)));
        let err = parse_file_listing(r#"{"files":[{"id":"a","name":"a.json"},{"name":"b"}]}"#)
            .expect_err("one malformed entry invalidates the page");
        assert!(matches!(err, SyncError::Rejected(_)));
        let err = parse_file_listing("not json").expect_err("corrupt response is a failure");
        assert!(matches!(err, SyncError::Rejected(_)));
        let err = parse_file_listing(r#"{"nextPageToken":"tok"}"#)
            .expect_err("missing files array is a failure");
        assert!(matches!(err, SyncError::Rejected(_)));
        let err = parse_file_listing(r#"{"files":[{"id":"","name":"a.json"}]}"#)
            .expect_err("empty id is a failure");
        assert!(matches!(err, SyncError::Rejected(_)));
        let err = parse_file_listing(r#"{"files":[{"id":"a","name":""}]}"#)
            .expect_err("empty name is a failure");
        assert!(matches!(err, SyncError::Rejected(_)));
        let (files, next) = parse_file_listing(r#"{"files":[],"nextPageToken":""}"#)
            .expect("empty token is treated as no more pages");
        assert!(files.is_empty());
        assert!(next.is_none(), "empty page token must not re-fetch");
    }

    #[test]
    fn parse_id_from_response_handles_folder_and_file() {
        assert_eq!(
            parse_id_from_response(r#"{"id":"folder-9","name":"Fluence Transcribe"}"#).as_deref(),
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
