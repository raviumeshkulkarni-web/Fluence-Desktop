# Fluence v1.15.0 Release Notes

Fluence v1.15.0 introduces end-to-end Google Drive cloud synchronization for Windows, enabling seamless cross-device syncing across transcription history, custom dictionary corrections, voice snippets, and application settings. This release also introduces a resilient quarantine isolation system with resolve UX, backed by comprehensive failure-injection testing and a cross-platform Phase 10 acceptance framework.

---

## 🌟 What's New in v1.15.0

### 1. Google Drive Cloud Sync
- **Pure Multi-Store Sync Engine**:
  - Implements a decentralized, deterministic file-per-UUID sync protocol (`wire.rs`, `engine.rs`).
  - Supports four distinct record stores:
    - **Transcription History** (`history`): Syncs transcription logs with content hashes and metadata without leaking private transcripts in logs.
    - **Custom Dictionary** (`dictionary`): Syncs spoken-to-corrected phrase mappings and expansions with transactional tombstone-and-replace edit semantics.
    - **Voice Snippets** (`snippet`): Syncs custom snippet triggers and expansions.
    - **App Settings** (`settings`): Syncs user preferences per account namespace.
  - Complete soft-delete tombstone propagation and conflict prevention without centralized server dependencies or clock skew vulnerabilities.
- **Secure PKCE Authentication**:
  - Direct Google OAuth 2.0 loopback flow with PKCE (Proof Key for Code Exchange) using SHA-256 verifiers via the system default browser.
  - Secure credential storage in the native **Windows Credential Manager** for refresh tokens and auth state.
  - Automatic, transparent access token refreshes with graceful handling of expired sessions.
- **Background Sync Scheduler**:
  - Intelligent periodic background synchronization (default 5-minute cadence).
  - Single-flight execution latch preventing race conditions or overlapping sync passes.
  - Exponential backoff and jitter for transient network failures.
  - Instant manual trigger via the **Sync Now** button.
- **Dedicated Sync Settings Dashboard**:
  - New **Sync** tab in Settings with Google Account profile details, connection state, and one-click Sign In / Sign Out.
  - Real-time status indicator displaying live progress ("Syncing right now…", "Last synced at [time]", or clear actionable error messages).
  - Granular store synchronization controls.

---

### 2. Quarantine List & Resolve UX + Layer-6 Hardening
- **Quarantine Isolation**:
  - Corrupted remote files, malformed JSON, schema deviations, or UUID/content conflicts are automatically isolated in local quarantine rather than clobbering local records or corrupting cloud storage.
  - Clear, user-friendly failure reason badges (e.g., *Corrupt file*, *Content differs from synced copy*, *Schema invalid*, *Collision*).
- **Quarantine Resolution UI**:
  - **Restore**: Allows users to re-queue a quarantined record for re-evaluation and safe re-import.
  - **Discard**: Removes the quarantine entry locally with confirmation dialog without deleting or modifying remote Drive files.
- **Layer-6 Failure-Injection & Chaos Testing**:
  - Extensive automated test suite covering network dropouts, malformed wire files, schema version mismatches, cross-account collision attempts, and edge-case payload corruptions.
  - Verified across 307 unit and integration tests with zero failures.

---

### 3. Phase 10 Cross-Platform Acceptance Checklist
- **Acceptance Scenarios (S1–S6)**:
  - Documented and structured test matrix in `docs/sync/phase10-checklist.md` covering:
    - **S1**: First sync upload (Windows → Drive).
    - **S2**: Cross-client import (Android → Windows).
    - **S3**: Edit convergence (concurrent modification handling).
    - **S4**: Delete propagation (tombstone-wins semantics).
    - **S5**: Offline resilience and automatic retry.
    - **S6**: Sign-out and re-authentication lifecycle.
- **Experiment 1 Verification**: Cross-account isolation and duplicate UUID validation across clients.
- **Release Gates**: Updater artifact signing via `TAURI_SIGNING_PRIVATE_KEY` / `latest.json` manifest requirements and installer smoke tests.

---

## 🔒 Security & Privacy
- **Zero Plaintext Credentials**: All OAuth tokens are secured in the Windows Credential Manager.
- **Local Isolation**: Quarantine operations never purge remote cloud assets without explicit intent.
- **Scoped Drive Access**: Sync operations are strictly confined to the application's designated Google Drive storage namespace.

---

## 📦 Upgrade Notes
- In-place upgrades are supported from v1.14.x.
- Google Drive sync requires granting Drive permissions via the **Sync** tab in Settings.
