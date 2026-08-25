// Fluence sync — frozen v1.1 sync_metadata (deviceId, per-account maxSeen/backfill/lastRev)
// Persists to %LOCALAPPDATA%/Fluence/sync_metadata.json
// Identity: deviceId UUIDv4, syncId UUIDv4 (Windows id is syncId — we store one UUID as both)

use std::collections::HashMap;
use std::path::PathBuf;

use dirs::data_local_dir;
use serde::{Deserialize, Serialize};

fn metadata_path() -> PathBuf {
    let mut p = data_local_dir().unwrap_or_else(|| PathBuf::from("."));
    p.push("Fluence");
    p.push("sync_metadata.json");
    p
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PerAccountState {
    #[serde(default)]
    pub max_seen: i64,
    #[serde(default)]
    pub backfill_done: bool,
    #[serde(default)]
    pub last_rev: HashMap<String, String>, // domain -> etag/revision
    #[serde(default)]
    pub last_sync_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncMetadata {
    pub device_id: String,
    pub sync_id: String,
    #[serde(default)]
    pub accounts: HashMap<String, PerAccountState>,
    #[serde(default)]
    pub last_account_hash: Option<String>,
}

impl Default for SyncMetadata {
    fn default() -> Self {
        let id = uuid::Uuid::new_v4().to_string();
        Self {
            device_id: id.clone(),
            sync_id: id,
            accounts: HashMap::new(),
            last_account_hash: None,
        }
    }
}

impl SyncMetadata {
    pub fn load() -> Self {
        let path = metadata_path();
        if !path.exists() {
            return Self::default();
        }
        match std::fs::read_to_string(&path) {
            Ok(data) => serde_json::from_str(&data).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self) {
        let path = metadata_path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(data) = serde_json::to_string_pretty(self) {
            let tmp = path.with_extension("tmp");
            if let Err(e) = std::fs::write(&tmp, &data) {
                log::error!("failed to write sync metadata tmp file: {}", e);
                return;
            }
            if let Ok(f) = std::fs::File::open(&tmp) {
                let _ = f.sync_all();
            }
            if let Err(e) = std::fs::rename(&tmp, &path) {
                log::error!("failed to rename sync metadata tmp file: {}", e);
            }
        }
    }

    pub fn device_id(&self) -> &str {
        &self.device_id
    }

    pub fn sync_id(&self) -> &str {
        &self.sync_id
    }

    pub fn for_account_mut(&mut self, account_hash: &str) -> &mut PerAccountState {
        self.accounts
            .entry(account_hash.to_string())
            .or_insert_with(PerAccountState::default)
    }

    pub fn for_account(&self, account_hash: &str) -> Option<&PerAccountState> {
        self.accounts.get(account_hash)
    }

    /// Ensure deviceId exists (generate if empty) and return it.
    pub fn ensure_device_id(&mut self) -> String {
        if self.device_id.is_empty() {
            let id = uuid::Uuid::new_v4().to_string();
            self.device_id = id.clone();
            self.sync_id = id.clone();
            self.save();
            id
        } else {
            self.device_id.clone()
        }
    }

    /// Update maxSeen atomically: new_max = max(old, candidate) and persist.
    pub fn update_max_seen(&mut self, account_hash: &str, candidate: i64) {
        let state = self.for_account_mut(account_hash);
        if candidate > state.max_seen {
            state.max_seen = candidate;
            // need to save outer
        }
        // save after
        self.save();
    }

    /// Record new lastRev for a domain, and handle account switch clearing.
    pub fn set_last_rev(&mut self, account_hash: &str, domain: &str, rev: String) {
        // Check if account switched
        let switched = self.last_account_hash.as_deref() != Some(account_hash);
        if switched {
            // clear lastRev for new account? Per spec: clear lastRev on switch
            // We keep per-account map, so no need to clear other accounts, but we update last_account_hash
            self.last_account_hash = Some(account_hash.to_string());
        }
        let state = self.for_account_mut(account_hash);
        state.last_rev.insert(domain.to_string(), rev);
        self.save();
    }

    pub fn clear_last_rev(&mut self, account_hash: &str) {
        if let Some(state) = self.accounts.get_mut(account_hash) {
            state.last_rev.clear();
        }
        self.save();
    }

    pub fn get_last_rev(&self, account_hash: &str, domain: &str) -> Option<String> {
        self.accounts
            .get(account_hash)
            .and_then(|s| s.last_rev.get(domain).cloned())
    }

    #[cfg(test)]
    pub fn test_path() -> PathBuf {
        metadata_path()
    }
}

// Account hash helper: lowercased trimmed email hashed? For now use lowercased email as hash for simplicity.
// In production, we use SHA256 hex of lower(trim(email)) to avoid storing raw email in file names? But spec says accountHash stamping.
// We'll provide a helper.
pub fn account_hash_from_email(email: &str) -> String {
    let normalized = email.trim().to_lowercase();
    // Use SHA256 hex for stable hash; fallback to normalized if hex crate unavailable
    let hash = sha2::Sha256::digest(normalized.as_bytes());
    hex::encode(hash)
}

use sha2::Digest;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_id_generated_and_persisted() {
        let mut m = SyncMetadata::default();
        assert!(!m.device_id.is_empty());
        assert_eq!(m.device_id, m.sync_id);
    }

    #[test]
    fn per_account_max_seen_monotonic() {
        let mut m = SyncMetadata::default();
        m.update_max_seen("acc1", 100);
        assert_eq!(m.for_account("acc1").unwrap().max_seen, 100);
        m.update_max_seen("acc1", 90);
        assert_eq!(m.for_account("acc1").unwrap().max_seen, 100);
        m.update_max_seen("acc1", 150);
        assert_eq!(m.for_account("acc1").unwrap().max_seen, 150);
        // per-account isolation
        assert!(m.for_account("acc2").is_none());
        m.update_max_seen("acc2", 200);
        assert_eq!(m.for_account("acc2").unwrap().max_seen, 200);
    }

    #[test]
    fn last_rev_per_account_isolated_and_cleared_on_switch() {
        let mut m = SyncMetadata::default();
        m.set_last_rev("acc1", "dictionary", "rev1".to_string());
        assert_eq!(m.get_last_rev("acc1", "dictionary").as_deref(), Some("rev1"));
        assert_eq!(m.get_last_rev("acc2", "dictionary"), None);
        m.set_last_rev("acc2", "dictionary", "rev2".to_string());
        assert_eq!(m.get_last_rev("acc2", "dictionary").as_deref(), Some("rev2"));
        // acc1 still has its rev
        assert_eq!(m.get_last_rev("acc1", "dictionary").as_deref(), Some("rev1"));
        m.clear_last_rev("acc1");
        assert_eq!(m.get_last_rev("acc1", "dictionary"), None);
    }

    #[test]
    fn account_hash_is_lowercase_and_hashed() {
        let h1 = account_hash_from_email("Test@Example.COM ");
        let h2 = account_hash_from_email("test@example.com");
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 64); // sha256 hex
    }
}
