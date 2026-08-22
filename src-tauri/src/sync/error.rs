// Fluence sync — shared error types for the frozen v1.2 domain engine.
// Previously these lived in the legacy per-record engine; they now stand alone.

use std::fmt;

/// Classification of every sync failure. The scheduler maps these onto its
/// backoff/latch state machine; the domain engine only produces them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncError {
    /// Refresh token missing/revoked — the user must sign in again.
    AuthRequired,
    /// Transient failure (network, 429, 5xx, timeout). Safe to retry.
    Retryable(String),
    /// Permanent client rejection (malformed request, 4xx). Never retried
    /// with escalation; surfaced and retried at the normal cadence.
    Rejected(String),
    /// Local bug or unrecoverable state. Automatic scheduling pauses until
    /// a manual command re-arms it.
    Fatal(String),
    /// 403 — the remote resource is not ours (wrong scope/account). Abort the
    /// pass; never retried with escalation.
    NotOurs,
    /// A concurrent writer changed the remote domain file between our GET
    /// and PUT (detected via Drive `version`). The caller re-fetches,
    /// re-merges and retries. This is the v1.2 replacement for the removed
    /// If-Match/412 machinery.
    StaleVersion(String),
}

impl fmt::Display for SyncError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SyncError::AuthRequired => write!(f, "authentication required"),
            SyncError::Retryable(e) => write!(f, "retryable sync failure: {e}"),
            SyncError::Rejected(e) => write!(f, "rejected: {e}"),
            SyncError::Fatal(e) => write!(f, "fatal sync failure: {e}"),
            SyncError::NotOurs => write!(f, "remote resource is not ours (scope/account mismatch)"),
            SyncError::StaleVersion(e) => write!(f, "remote changed during sync: {e}"),
        }
    }
}

impl std::error::Error for SyncError {}
