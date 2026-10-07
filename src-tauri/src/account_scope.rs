//! Account-scoped custom agents and styles — **local storage only**.
//!
//! # Why separate files, not sibling keys
//!
//! Adding an `account_agents` key inside `agents.json` would be additive for a
//! *new* build but **destructive for an old one**: `serde` drops unknown fields
//! on round-trip, so any build that predates this change would load
//! `agents.json`, fail to recognise the key, and silently rewrite the file
//! without it — destroying every account's data. In a mixed-version fleet that
//! is silent, unrecoverable data loss.
//!
//! A separate file per account removes the hazard entirely: an old build does
//! not know the file exists, so it never reads, rewrites, or truncates it. The
//! legacy file is left byte-identical.
//!
//! # Ownership rules (D1)
//!
//! - Legacy (`agents.json` / `prompts.json`) records are **device-local** and
//!   stay exactly where they are. They are never adopted into an account, never
//!   re-stamped, and never attributed to a signed-in user — their original ids,
//!   names and hints are preserved byte-for-byte.
//! - Account records live only in that account's file, keyed by the existing
//!   64-hex `account_hash` convention.
//! - Reads are `legacy ∪ current account`. Unowned legacy rows remain visible
//!   to any signed-in account, matching the frozen
//!   `syncAccount == null || == hash` predicate used by dictionary/snippets.
//! - `default_id` and `package_overrides` stay **device-local** in the legacy
//!   file (D1c) and are never copied into an account file.
//!
//! # Record shape
//!
//! Account records carry the same fields as dictionary items (`sync_id`,
//! `updated_at`, `device_id`, `deleted_at`) so that adding Drive sync later is
//! purely additive. `deleted_at` is the sync contract's tombstone: it makes a
//! delete permanent rather than letting an older copy resurrect.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// File-name convention for an account-scoped store. Kept deliberately
/// distinct from the legacy stem so no glob or rewrite can confuse the two.
const ACCOUNT_SUFFIX: &str = "account-";

fn account_path(base: &Path, stem: &str, account_hash: &str) -> PathBuf {
    // Validate HERE, not only at the call sites, so no future caller can reach
    // the filesystem with an unvalidated hash.
    assert!(
        valid_account_hash(account_hash),
        "account hash must be 64 lowercase hex characters"
    );
    let file_name = format!("{stem}.{ACCOUNT_SUFFIX}{account_hash}.json");
    base.join(file_name)
}

/// Resolved through the shared hermetic resolver, NOT raw
/// `dirs::data_local_dir()`.
///
/// Under `cfg(test)` that resolver points at a per-process temp directory, so
/// no test can write the real `%LOCALAPPDATA%\Fluence` — the same protection
/// `dictionary.rs` / `snippets.rs` / `metadata.rs` rely on. Calling
/// `dirs::data_local_dir()` directly here would reintroduce exactly the
/// test-writes-production-files failure this resolver exists to prevent.
fn agents_base_dir() -> PathBuf {
    crate::sync::stores::base_data_dir()
}

fn prompts_base_dir() -> PathBuf {
    crate::sync::stores::base_data_dir()
}

/// `#[cfg(test)]` accessors for the private account-file path resolver.
///
/// The routed delete resolves the active account through the account file, so a
/// durability-gate test must be able to make THAT file unwritable without
/// reaching for a private helper. Read-only shims: they add no behaviour and are
/// compiled out of release builds.
#[cfg(test)]
pub(crate) fn account_path_for_test(base: &Path, stem: &str, account_hash: &str) -> PathBuf {
    account_path(base, stem, account_hash)
}

#[cfg(test)]
pub(crate) fn prompts_base_dir_for_test() -> PathBuf {
    prompts_base_dir()
}

/// Strictly the documented convention: the account hash is SHA-256 hex, so
/// exactly 64 lowercase hex characters.
///
/// Being strict here matters beyond tidiness. A looser validator would let a
/// *truncated* or otherwise malformed hash through and silently create a
/// separate account file, which surfaces much later as apparent data loss. The
/// allowlist also makes path traversal and name collision with the legacy /
/// `.corrupt` / `.tmp` siblings unrepresentable.
///
/// Shared with the remote partition gate (`sync::account_partition`) so a hash
/// accepted for a Drive path is by construction also accepted for local account
/// storage. Two validators would be free to drift.
pub(crate) fn valid_account_hash(hash: &str) -> bool {
    hash.len() == 64
        && hash
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// An account-owned custom agent. Legacy device-local agents keep the narrower
/// [`crate::agents::CustomAgent`] shape; this one carries sync metadata.
///
/// `camelCase` matches the frozen wire convention used by dictionary items
/// (`syncId`, `updatedAt`, `deviceId`, `deletedAt`), so the eventual Drive
/// domain is additive rather than a second, differently-named format.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AccountAgent {
    pub id: String,
    pub name: String,
    pub hint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sync_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_id: Option<String>,
    /// Tombstone. `Some(ts)` means deleted at `ts`; the row is retained so a
    /// later merge cannot resurrect it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deleted_at: Option<i64>,
    /// Local-only: this row has a mutation the ledger has not accepted yet.
    ///
    /// This is the same lifecycle `StatEventRow::dirty` already uses, and it
    /// is deliberately NOT part of the wire (`AgentItem` has no such field).
    /// It must exist as an explicit flag rather than being derived from
    /// missing sync metadata: a record that has synced once carries a
    /// `sync_id`/`updated_at` forever, so "never stamped" cannot also mean
    /// "edited since last sync". Deriving it that way made every edit after
    /// the first sync permanently invisible to upload.
    #[serde(default, skip_serializing_if = "is_false")]
    pub dirty: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct AccountAgentsStore {
    #[serde(default)]
    pub custom_agents: Vec<AccountAgent>,
}

/// An account-owned custom style. `camelCase` for the same wire reason as
/// [`AccountAgent`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AccountStyle {
    pub id: String,
    pub name: String,
    pub hint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sync_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deleted_at: Option<i64>,
    /// Local-only pending-mutation flag. See [`AccountAgent::dirty`].
    #[serde(default, skip_serializing_if = "is_false")]
    pub dirty: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct AccountStylesStore {
    #[serde(default)]
    pub custom_styles: Vec<AccountStyle>,
}

fn read_json<T: for<'de> Deserialize<'de> + Default>(path: &Path) -> T {
    match std::fs::read_to_string(path) {
        Ok(data) => match serde_json::from_str::<T>(&data) {
            Ok(v) => v,
            Err(e) => {
                // Back up ONLY this account file. The legacy file is a different
                // path and is never touched here, so a corrupt account file
                // cannot damage device-local data.
                log::warn!("Corrupt account store {}: {e:?}", path.display());
                backup_corrupt_account(path);
                T::default()
            }
        },
        Err(_) => T::default(),
    }
}

fn backup_corrupt_account(path: &Path) {
    let mut target = path.with_extension("json.corrupt.json");
    let mut counter = 1;
    while target.exists() {
        target = path.with_extension(format!("json.corrupt.{counter}.json"));
        counter += 1;
    }
    if std::fs::rename(path, &target).is_err() && std::fs::copy(path, &target).is_ok() {
        let _ = std::fs::remove_file(path);
    }
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let data = serde_json::to_string_pretty(value).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, &data).map_err(|e| e.to_string())?;
    if let Ok(f) = std::fs::File::open(&tmp) {
        let _ = f.sync_all();
    }
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())
}

pub(crate) fn load_account_agents(account_hash: &str) -> AccountAgentsStore {
    if !valid_account_hash(account_hash) {
        return AccountAgentsStore::default();
    }
    read_json(&account_path(&agents_base_dir(), "agents", account_hash))
}

pub(crate) fn save_account_agents(
    account_hash: &str,
    store: &AccountAgentsStore,
) -> Result<(), String> {
    if !valid_account_hash(account_hash) {
        return Err("invalid account hash".to_string());
    }
    write_json(
        &account_path(&agents_base_dir(), "agents", account_hash),
        store,
    )
}

pub(crate) fn load_account_styles(account_hash: &str) -> AccountStylesStore {
    if !valid_account_hash(account_hash) {
        return AccountStylesStore::default();
    }
    read_json(&account_path(&prompts_base_dir(), "styles", account_hash))
}

pub(crate) fn save_account_styles(
    account_hash: &str,
    store: &AccountStylesStore,
) -> Result<(), String> {
    if !valid_account_hash(account_hash) {
        return Err("invalid account hash".to_string());
    }
    write_json(
        &account_path(&prompts_base_dir(), "styles", account_hash),
        store,
    )
}

/// Create or update an agent in the signed-in account's own store.
///
/// Marks the row `dirty` on BOTH paths, so a local edit to an
/// already-synchronized record is re-stamped with a fresh revision and
/// uploaded on the next pass. `sync_id`/`updated_at`/`device_id` are left
/// untouched here on purpose: stamping belongs to the sync pass, which is
/// where the monotonic clock and this device's id live. Refuses an invalid
/// hash: a record must never be written somewhere without a verified
/// identity behind it.
pub(crate) fn upsert_account_agent(account_hash: &str, id: &str, name: &str, hint: &str) {
    if !valid_account_hash(account_hash) {
        return;
    }
    let mut store = load_account_agents(account_hash);
    if let Some(e) = store.custom_agents.iter_mut().find(|e| e.id == id) {
        e.name = name.to_string();
        e.hint = hint.to_string();
        e.dirty = true;
    } else {
        store.custom_agents.push(AccountAgent {
            id: id.to_string(),
            name: name.to_string(),
            hint: hint.to_string(),
            sync_id: None,
            updated_at: None,
            device_id: None,
            deleted_at: None,
            dirty: true,
        });
    }
    let _ = save_account_agents(account_hash, &store);
}

/// Soft-delete an agent in the account's own store: writes a tombstone so the
/// delete propagates instead of being resurrected. A same-id legacy row is
/// deliberately left alone. No-op for an invalid hash.
///
/// Returns true only when the tombstone was found AND durably persisted.
/// Callers drop the same-id legacy shadow only on success: a failed tombstone
/// write must preserve the legacy copy, or the record ends up in neither
/// store with no tombstone to propagate the loss.
pub(crate) fn delete_account_agent(account_hash: &str, id: &str) -> bool {
    if !valid_account_hash(account_hash) {
        return false;
    }
    let mut store = load_account_agents(account_hash);
    let Some(e) = store.custom_agents.iter_mut().find(|e| e.id == id) else {
        return false;
    };
    e.deleted_at = Some(chrono::Utc::now().timestamp_millis());
    e.dirty = true;
    save_account_agents(account_hash, &store).is_ok()
}

/// Outcome of one explicit legacy claim.
///
/// `skipped_ids` are legacy records whose stable id ALREADY exists in the
/// account store. They are left untouched on BOTH sides: the owned copy is
/// authoritative and already synced, so overwriting it would destroy the user's
/// data on every other device. The legacy copy stays put (and stays shadowed by
/// the account copy) so nothing is silently discarded.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaimOutcome {
    pub claimed_ids: Vec<String>,
    pub skipped_ids: Vec<String>,
    /// Deliberately NOT claimed, with the legacy copy deliberately left in place.
    ///
    /// Kept distinct from `skipped_ids` because the two mean opposite things to
    /// the user: "skipped" means the record is already theirs, "refused" means
    /// this record cannot be adopted and the app will say why. Reasons, all
    /// deterministic:
    ///
    ///  - `builtin-id` — a reserved id that must never become an ownable record.
    ///  - `tombstoned` — the account holds a DELETE for this id. A delete is
    ///    never resurrected by an ownership transfer; see the tombstone policy
    ///    on `claim_legacy_account_agents`.
    ///  - `duplicate-legacy-id` — the legacy store holds this id more than once,
    ///    so there is no single record to adopt and picking one would silently
    ///    destroy the other. Refusing keeps both.
    pub refused_ids: Vec<(String, &'static str)>,
}

impl ClaimOutcome {
    pub fn is_empty(&self) -> bool {
        self.claimed_ids.is_empty() && self.skipped_ids.is_empty() && self.refused_ids.is_empty()
    }
}

/// Adopt legacy Agents into `account_hash` — ONE explicit user action.
///
/// This is an ownership transition and the ONLY path that performs it:
///
///  - it never runs on its own; nothing here is reachable from a read path or
///    from sync, only from an explicit user action;
///  - it never infers the target account. `account_hash` is supplied by the
///    caller from the currently authenticated identity, and an absent or invalid
///    hash claims NOTHING — so signing out, or an unverifiable identity, cannot
///    adopt anything;
///  - it never overwrites an owned record; a legacy id that already exists is
///    skipped and reported.
///
/// Claimed rows are written exactly as [`upsert_account_agent`] writes one:
/// `sync_id`/`updated_at` null and `dirty` true, so the ordinary pass stamps and
/// uploads them. No new sync path, envelope, or ownership mechanism is
/// introduced, and the legacy store is only ever READ here.
///
/// The claim COPIES, it never MOVES: the caller removes NOTHING. The legacy row
/// is deliberately left on disk because the account store is a whole-document
/// read-modify-write and writers that do not share this function's critical
/// section — the sync pass notably loads the document, performs a Drive round
/// trip, and only then writes a payload built from that pre-network snapshot —
/// can discard a committed claim. Deleting the legacy copy would turn that
/// outcome into permanent loss (the record in NEITHER store); keeping it degrades
/// it to a re-claimable shadow instead.
///
/// Legacy removal happens on exactly one path, and it is NOT here: the delete
/// routes drop the same-id shadow only after the account tombstone is confirmed
/// durable (`delete_custom_agent_routed` / `delete_custom_style_routed`).
pub(crate) fn claim_legacy_account_agents(
    account_hash: Option<&str>,
    legacy: &[crate::agents::CustomAgent],
    only_ids: Option<&std::collections::HashSet<String>>,
) -> ClaimOutcome {
    let Some(hash) = account_hash.filter(|h| valid_account_hash(h)) else {
        return ClaimOutcome::default();
    };
    // CLAIM-SPECIFIC CRITICAL SECTION.
    //
    // The account store is a whole-document read-modify-write, so "one load and
    // one save" is itself an RMW cycle — batching does not avoid that. This is
    // NOT the general account-store concurrency fix (B2/R1 remains open); it is
    // the minimum needed to keep this operation from LOSING A RECORD, because it
    // is the only writer whose success is followed by a destructive delete of
    // the other copy.
    //
    // Without it: `AgentDirtyStore::stamp_account` (stores.rs) holds this same
    // lock, loads, stamps, and saves. An unguarded claim could append + save,
    // report the id claimed, delete the legacy copy — and then the stamper
    // would save its own older snapshot, discarding the claim. The record would
    // exist in NEITHER store.
    //
    // Reentrant (see io_lock.rs), so nesting is safe, and it is dropped before
    // any caller touches the legacy store — no lock is ever held across the
    // legacy cleanup or any network/Drive work.
    let _io = crate::sync::io_lock::io_lock_guard();

    let mut outcome = ClaimOutcome::default();
    if legacy.is_empty() {
        return outcome;
    }
    let mut store = load_account_agents(hash);
    // Each id is DECIDED once, so a duplicated legacy id yields exactly one
    // refusal rather than one per row.
    let mut decided = std::collections::HashSet::new();
    for rec in legacy {
        if let Some(only) = only_ids {
            if !only.contains(&rec.id) {
                continue;
            }
        }
        if !decided.insert(rec.id.clone()) {
            continue;
        }
        // A reserved id must never become an ownable, syncable custom record.
        if rec.id == crate::agents::ID_BUILT_IN {
            outcome.refused_ids.push((rec.id.clone(), "builtin-id"));
            continue;
        }
        // Duplicate ids in the legacy store: refuse rather than pick one. The
        // cleanup step deletes by id, so claiming would delete BOTH copies while
        // reporting one as adopted — silent destruction of a record nobody chose.
        if legacy.iter().filter(|o| o.id == rec.id).count() > 1 {
            outcome
                .refused_ids
                .push((rec.id.clone(), "duplicate-legacy-id"));
            continue;
        }
        // TOMBSTONE POLICY (deterministic, stated once): a DELETE wins. An
        // explicit claim does NOT resurrect a deleted record, because reviving it
        // would make an ownership transfer undo a deletion the user performed on
        // another device, and would change tombstone semantics. The legacy copy
        // is left untouched and the refusal is REPORTED, so this is never the
        // silent side effect of an `ownedIds` set.
        if let Some(existing) = store.custom_agents.iter().find(|a| a.id == rec.id) {
            if existing.deleted_at.is_some() {
                outcome.refused_ids.push((rec.id.clone(), "tombstoned"));
            } else {
                outcome.skipped_ids.push(rec.id.clone());
            }
            continue;
        }
        store.custom_agents.push(AccountAgent {
            id: rec.id.clone(),
            name: rec.name.clone(),
            hint: rec.hint.clone(),
            sync_id: None,
            updated_at: None,
            device_id: None,
            deleted_at: None,
            dirty: true,
        });
        outcome.claimed_ids.push(rec.id.clone());
    }
    if !outcome.claimed_ids.is_empty() {
        // Ownership MUST be durable before the caller is allowed to drop the
        // legacy copy. If this write fails the record still exists in the legacy
        // store and nowhere else, so reporting it as claimed would let the caller
        // delete the only copy — a silent data loss. Report nothing claimed and
        // let the legacy row stand; the user can retry.
        if save_account_agents(hash, &store).is_err() {
            outcome.claimed_ids.clear();
        }
    }
    outcome
}

/// Style counterpart of [`claim_legacy_account_agents`], with identical rules.
pub(crate) fn claim_legacy_account_styles(
    account_hash: Option<&str>,
    legacy: &[crate::prompts::CustomStyle],
    only_ids: Option<&std::collections::HashSet<String>>,
) -> ClaimOutcome {
    let Some(hash) = account_hash.filter(|h| valid_account_hash(h)) else {
        return ClaimOutcome::default();
    };
    // Same claim-specific critical section as the agent path, contending with the
    // already-locked `StyleDirtyStore::stamp_account`. Still not B2/R1.
    let _io = crate::sync::io_lock::io_lock_guard();

    let mut outcome = ClaimOutcome::default();
    if legacy.is_empty() {
        return outcome;
    }
    let mut store = load_account_styles(hash);
    // Each id decided once — see the agent counterpart.
    let mut decided = std::collections::HashSet::new();
    for rec in legacy {
        if let Some(only) = only_ids {
            if !only.contains(&rec.id) {
                continue;
            }
        }
        if !decided.insert(rec.id.clone()) {
            continue;
        }
        if crate::prompts::is_builtin_style(&rec.id) {
            outcome.refused_ids.push((rec.id.clone(), "builtin-id"));
            continue;
        }
        if legacy.iter().filter(|o| o.id == rec.id).count() > 1 {
            outcome
                .refused_ids
                .push((rec.id.clone(), "duplicate-legacy-id"));
            continue;
        }
        // DELETE wins over an ownership transfer — see the agent path.
        if let Some(existing) = store.custom_styles.iter().find(|s| s.id == rec.id) {
            if existing.deleted_at.is_some() {
                outcome.refused_ids.push((rec.id.clone(), "tombstoned"));
            } else {
                outcome.skipped_ids.push(rec.id.clone());
            }
            continue;
        }
        store.custom_styles.push(AccountStyle {
            id: rec.id.clone(),
            name: rec.name.clone(),
            hint: rec.hint.clone(),
            sync_id: None,
            updated_at: None,
            device_id: None,
            deleted_at: None,
            dirty: true,
        });
        outcome.claimed_ids.push(rec.id.clone());
    }
    if !outcome.claimed_ids.is_empty() {
        // Same fail-safe as agents: ownership must be durable before the caller
        // drops the legacy copy, or a failed write loses the only copy.
        if save_account_styles(hash, &store).is_err() {
            outcome.claimed_ids.clear();
        }
    }
    outcome
}

/// Style counterpart of [`upsert_account_agent`].
pub(crate) fn upsert_account_style(account_hash: &str, id: &str, name: &str, hint: &str) {
    if !valid_account_hash(account_hash) {
        return;
    }
    let mut store = load_account_styles(account_hash);
    if let Some(e) = store.custom_styles.iter_mut().find(|e| e.id == id) {
        e.name = name.to_string();
        e.hint = hint.to_string();
        e.dirty = true;
    } else {
        store.custom_styles.push(AccountStyle {
            id: id.to_string(),
            name: name.to_string(),
            hint: hint.to_string(),
            sync_id: None,
            updated_at: None,
            device_id: None,
            deleted_at: None,
            dirty: true,
        });
    }
    let _ = save_account_styles(account_hash, &store);
}

/// Style counterpart of [`delete_account_agent`]: tombstone, never removal.
///
/// Same durability contract: true only when the tombstone persisted, so the
/// caller keeps the legacy shadow on any failure.
pub(crate) fn delete_account_style(account_hash: &str, id: &str) -> bool {
    if !valid_account_hash(account_hash) {
        return false;
    }
    let mut store = load_account_styles(account_hash);
    let Some(e) = store.custom_styles.iter_mut().find(|e| e.id == id) else {
        return false;
    };
    e.deleted_at = Some(chrono::Utc::now().timestamp_millis());
    e.dirty = true;
    save_account_styles(account_hash, &store).is_ok()
}

/// A record visible to the user, tagged with where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VisibleRecord {
    /// Device-local record from the legacy `agents.json` / `prompts.json`.
    /// Never adopted, never attributed to an account (D1a).
    Legacy {
        id: String,
        name: String,
        hint: String,
    },
    /// Record owned by the active account.
    Account(AccountAgent),
}

// ---------------------------------------------------------------------------
// Phase 6 STAGE 5/6 — three-state local admission (mirrors Android
// `AccountScope.Admission`). Provenance derives from WHERE the record
// physically lives, never from who is signed in: the account file is
// hash-partitioned by the token-derived identity, so "in this file" IS "owned
// by this account"; the legacy pre-account store is genuinely unknown
// provenance.
// ---------------------------------------------------------------------------

/// Provenance of a locally stored Agent/Style record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admission {
    /// Visible, selectable, executable, and syncable.
    Owned,
    /// Visible and executable on this device. NEVER auto-uploaded, NEVER
    /// silently reassigned.
    DeviceLocal,
    /// Provenance unknown. While an account is signed in it is NOT executable
    /// and NOT uploaded. Preserved intact; becomes Owned only through an
    /// explicit assignment, which does not exist yet.
    Unassigned,
}

impl Admission {
    /// Whether a record in this state may be shown AND run. Unassigned is
    /// withheld only while an account is signed in: an agent carries an
    /// executable system prompt, so running one of unknown provenance under a
    /// signed-in identity is the cross-account confusion this phase prevents.
    /// Offline there is no identity to confuse, so withholding would only
    /// brick the app.
    pub fn is_admissible(self) -> bool {
        self != Admission::Unassigned
    }

    /// Whether a listed record may be SHOWN. Deliberately broader than
    /// `is_admissible`: hiding unassigned records makes intact legacy data
    /// look deleted. Shown-but-not-runnable preserves the security property
    /// (never executed, never uploaded) without the destructive perception.
    pub fn is_displayable(self) -> bool {
        true
    }

    /// Only Owned records are ever uploaded. Never DeviceLocal, never
    /// Unassigned.
    pub fn is_syncable(self) -> bool {
        self == Admission::Owned
    }
}

/// True only when there is positively no account. A non-blank value that is
/// not a valid hash is an UNVERIFIABLE identity, not an absent one, and must
/// be handled fail-closed — never allowed to unlock unknown-provenance
/// records.
fn signed_out(account_hash: Option<&str>) -> bool {
    match account_hash {
        None => true,
        Some(h) if h.trim().is_empty() => true,
        _ => false,
    }
}

/// Classify one agent record against the active account.
pub fn admit_agent(record: &VisibleRecord, account_hash: Option<&str>) -> Admission {
    if signed_out(account_hash) {
        return Admission::DeviceLocal;
    }
    match record {
        VisibleRecord::Account(_) => Admission::Owned,
        VisibleRecord::Legacy { .. } => Admission::Unassigned,
    }
}

/// Style counterpart of [`admit_agent`], with identical rules.
pub fn admit_style(record: &VisibleStyle, account_hash: Option<&str>) -> Admission {
    if signed_out(account_hash) {
        return Admission::DeviceLocal;
    }
    match record {
        VisibleStyle::Account(_) => Admission::Owned,
        VisibleStyle::Legacy { .. } => Admission::Unassigned,
    }
}

/// The single admission gate for agents: raw union in, admitted records out.
/// Every runtime consumer must read through this (or a snapshot built from
/// it) rather than walking the union directly.
pub fn admitted_agents(union: &[VisibleRecord], account_hash: Option<&str>) -> Vec<VisibleRecord> {
    union
        .iter()
        .filter(|r| admit_agent(r, account_hash).is_admissible())
        .cloned()
        .collect()
}

/// Style counterpart of [`admitted_agents`].
pub fn admitted_styles(union: &[VisibleStyle], account_hash: Option<&str>) -> Vec<VisibleStyle> {
    union
        .iter()
        .filter(|r| admit_style(r, account_hash).is_admissible())
        .cloned()
        .collect()
}

/// Read-union of device-local legacy records and the active account's records.
///
/// # Precedence: account wins on id collision
///
/// A signed-in user sees the record they own for a given id, not a shadowed
/// device-local one. D1a keeps unowned legacy data *visible*; it does not make
/// unowned data outrank account-owned data.
///
/// A shadowed legacy record is **not** deleted, mutated, or adopted. It stays
/// exactly as it is in the legacy file and becomes visible again when no account
/// is signed in, or when a different account is signed in.
///
/// # Ordering
///
/// Account records first (in stored order), then legacy records, deduplicated by
/// id in favour of the account copy. This is deterministic and independent of
/// which side happened to be loaded first.
pub fn union_agents(
    legacy: &[crate::agents::CustomAgent],
    account: &[AccountAgent],
) -> Vec<VisibleRecord> {
    let mut out: Vec<VisibleRecord> = Vec::with_capacity(legacy.len() + account.len());
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for a in account {
        if seen.insert(a.id.clone()) {
            out.push(VisibleRecord::Account(a.clone()));
        }
    }
    for l in legacy {
        if seen.insert(l.id.clone()) {
            out.push(VisibleRecord::Legacy {
                id: l.id.clone(),
                name: l.name.clone(),
                hint: l.hint.clone(),
            });
        }
    }
    out
}

/// Style counterpart of [`union_agents`], with identical precedence rules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VisibleStyle {
    Legacy {
        id: String,
        name: String,
        hint: String,
    },
    Account(AccountStyle),
}

pub fn union_styles(
    legacy: &[crate::prompts::CustomStyle],
    account: &[AccountStyle],
) -> Vec<VisibleStyle> {
    let mut out: Vec<VisibleStyle> = Vec::with_capacity(legacy.len() + account.len());
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for s in account {
        if seen.insert(s.id.clone()) {
            out.push(VisibleStyle::Account(s.clone()));
        }
    }
    for l in legacy {
        if seen.insert(l.id.clone()) {
            out.push(VisibleStyle::Legacy {
                id: l.id.clone(),
                name: l.name.clone(),
                hint: l.hint.clone(),
            });
        }
    }
    out
}

/// Load the union of device-local legacy agents and the active account's
/// agents, for the read paths.
///
/// `account_hash` is `None` when signed out, which yields the legacy records
/// only — local-first behaviour, no account involvement at all. A rejected or
/// corrupt account store contributes nothing and never touches legacy data.
/// A single read snapshot of visible agents.
///
/// # Why a snapshot
///
/// Wiring condition 1: the union must be read **once** and the same object
/// threaded through listing and resolution. Holding both in one value makes a
/// re-read between display and resolution unrepresentable, rather than a rule
/// callers must remember.
pub struct VisibleAgents {
    union: Vec<VisibleRecord>,
    account_hash: Option<String>,
}

impl VisibleAgents {
    /// Read the union once. `account_hash` is `None` when signed out, which
    /// yields device-local legacy records only.
    pub fn load(account_hash: Option<&str>) -> Self {
        Self {
            union: load_visible_agents(account_hash),
            account_hash: account_hash.map(str::to_string),
        }
    }

    /// Build a snapshot from an already-loaded union (tests, and callers that
    /// already hold one).
    pub fn from_union(union: Vec<VisibleRecord>) -> Self {
        Self {
            union,
            account_hash: None,
        }
    }

    /// Build a snapshot from an already-loaded union with a known account.
    /// Production snapshots always carry the verified hash; a `None` hash
    /// withholds account records (see `has_verified_account`).
    pub fn from_union_with_account(union: Vec<VisibleRecord>, account_hash: Option<&str>) -> Self {
        Self {
            union,
            account_hash: account_hash.map(str::to_string),
        }
    }

    /// An account record only means something with a valid account hash. The
    /// production loader already yields no account records without one, but
    /// enforcing it here too means a hand-assembled union, or a snapshot
    /// reused across a sign-out, still cannot leak account records into a
    /// session with no verified identity.
    fn has_verified_account(&self) -> bool {
        match &self.account_hash {
            Some(h) => valid_account_hash(h),
            None => false,
        }
    }

    /// Records a runtime consumer may execute: the admission gate. Display,
    /// selection, execution, previews and any LLM path resolve through this
    /// snapshot, so no consumer can reach an unadmitted record by going around
    /// it.
    pub fn admitted(&self) -> Vec<VisibleRecord> {
        let hash = self.account_hash.as_deref();
        self.union
            .iter()
            .filter(|r| {
                // Defence in depth alongside `admitted_agents`: account records
                // require a verified hash regardless of how the union was built.
                if !self.has_verified_account() && matches!(r, VisibleRecord::Account(_)) {
                    return false;
                }
                admit_agent(r, hash).is_admissible()
            })
            .cloned()
            .collect()
    }

    /// `is_known_id` over this snapshot: builtin, or an ADMITTED record.
    /// An unassigned legacy record is displayable but not known-executable, so
    /// a default or saved selection pointing at one cannot run it.
    pub fn is_known(&self, agent_id: &str) -> bool {
        agent_id == crate::agents::ID_BUILT_IN
            || self.admitted().iter().any(|r| visible_id(r) == agent_id)
    }

    /// The visible union, in display order.
    pub fn records(&self) -> &[VisibleRecord] {
        &self.union
    }

    /// Resolve an id against this snapshot, mirroring the existing
    /// `resolve_active_agent` contract: built-in for the built-in id, an absent
    /// id, an unknown id, **or a tombstoned record**.
    ///
    /// A tombstone wins its id in the union, so treating it as unresolvable is
    /// what stops a deleted agent producing a usable hint — and also stops it
    /// falling through to a shadowed device-local record.
    ///
    /// Gated on the admitted set: an Unassigned record resolves to builtin
    /// here, so it can never supply a hint to execution or any LLM path.
    pub fn resolve(&self, agent_id: Option<&str>) -> crate::agents::ResolvedAgent {
        let builtin = crate::agents::ResolvedAgent {
            id: crate::agents::ID_BUILT_IN.to_string(),
            is_builtin: true,
            hint: None,
        };
        let admitted = self.admitted();
        let found = match resolve_in(&admitted, agent_id) {
            None => return builtin,
            Some(r) => r,
        };
        match found {
            VisibleRecord::Legacy { id, hint, .. } => {
                let clean = crate::agents::sanitize_hint(hint);
                crate::agents::ResolvedAgent {
                    id: id.clone(),
                    is_builtin: false,
                    hint: if clean.is_empty() { None } else { Some(clean) },
                }
            }
            VisibleRecord::Account(a) => {
                // A deleted agent is never executable and never leaks a hint.
                if a.deleted_at.is_some() {
                    return builtin;
                }
                let clean = crate::agents::sanitize_hint(&a.hint);
                crate::agents::ResolvedAgent {
                    id: a.id.clone(),
                    is_builtin: false,
                    hint: if clean.is_empty() { None } else { Some(clean) },
                }
            }
        }
    }

    /// Hint lookup for the execution path. `None` means "use the built-in",
    /// exactly what the previous `resolve_hint` contract returned.
    pub fn hint(&self, agent_id: Option<&str>) -> Option<String> {
        self.resolve(agent_id).hint
    }

    /// Admission state of a listed record, so the UI can render it honestly.
    /// An account record without a verified account reports Unassigned: it has
    /// no owner to be attributed to, and must not present as runnable.
    pub fn admission_of(&self, record: &VisibleRecord) -> Admission {
        if !self.has_verified_account() && matches!(record, VisibleRecord::Account(_)) {
            return Admission::Unassigned;
        }
        admit_agent(record, self.account_hash.as_deref())
    }

    /// True when this listed record may actually be run: admitted AND not a
    /// tombstone.
    pub fn is_runnable(&self, record: &VisibleRecord) -> bool {
        if !self.admission_of(record).is_admissible() {
            return false;
        }
        match record {
            VisibleRecord::Legacy { .. } => true,
            VisibleRecord::Account(a) => a.deleted_at.is_none(),
        }
    }

    /// True when this listed record is an unclaimed pre-account record the user
    /// may explicitly adopt into the signed-in account.
    ///
    /// Derived from the SAME admission gate as `is_runnable` rather than a second
    /// sign-in check, so the affordance can never disagree with the policy: a
    /// record is claimable exactly when it is a device-local legacy row that the
    /// active account reports as `Unassigned` — i.e. signed in (signed out, the
    /// same row is `DeviceLocal`, fully usable, and there is nothing to adopt).
    pub fn is_claimable(&self, record: &VisibleRecord) -> bool {
        matches!(record, VisibleRecord::Legacy { .. })
            && self.admission_of(record) == Admission::Unassigned
    }

    /// Project onto the existing `AgentsView` shape so the IPC contract and the
    /// frontend are unchanged. Union order is preserved, so the account-wins
    /// collision outcome is what the user sees.
    ///
    /// **Tombstoned account records are omitted.** A deleted agent must not be
    /// rendered as a selectable, listed agent, and the view is the user-visible
    /// surface. Hints are sanitized exactly as the legacy projection did, so an
    /// account-sourced hint can never bypass `sanitize_hint` on its way to the UI.
    pub fn into_view(self, default_id: String) -> crate::agents::AgentsView {
        // `claimable` is computed here from `is_claimable`, the same admission gate
        // the policy uses, so the affordance can never disagree with the rule.
        let snapshot = &self;
        let custom_agents = self
            .union
            .iter()
            .filter_map(|r| match r {
                VisibleRecord::Legacy { id, name, hint } => Some(crate::agents::CustomAgentView {
                    id: id.clone(),
                    name: name.clone(),
                    hint: crate::agents::sanitize_hint(hint),
                    claimable: snapshot.is_claimable(r),
                }),
                VisibleRecord::Account(a) => {
                    // Deleted agents are never listed as selectable.
                    if a.deleted_at.is_some() {
                        return None;
                    }
                    Some(crate::agents::CustomAgentView {
                        id: a.id.clone(),
                        name: a.name.clone(),
                        hint: crate::agents::sanitize_hint(&a.hint),
                        // An owned record is never claimable: it is already owned.
                        claimable: false,
                    })
                }
            })
            .collect();
        crate::agents::AgentsView {
            builtin_id: crate::agents::ID_BUILT_IN.to_string(),
            builtin_name: crate::agents::NAME_BUILT_IN.to_string(),
            builtin_description: "The all-rounder. Edits, rewrites, and answers questions."
                .to_string(),
            default_id,
            custom_agents,
        }
    }
}

pub fn load_visible_agents(account_hash: Option<&str>) -> Vec<VisibleRecord> {
    let legacy = crate::agents::load_store().custom_agents;
    let account = match account_hash {
        Some(h) if valid_account_hash(h) => load_account_agents(h).custom_agents,
        _ => Vec::new(),
    };
    union_agents(&legacy, &account)
}

/// Style counterpart of [`load_visible_agents`].
pub fn load_visible_styles(account_hash: Option<&str>) -> Vec<VisibleStyle> {
    let legacy = crate::prompts::load_store().custom_styles;
    let account = match account_hash {
        Some(h) if valid_account_hash(h) => load_account_styles(h).custom_styles,
        _ => Vec::new(),
    };
    union_styles(&legacy, &account)
}

/// Style counterpart of [`VisibleAgents`]: a single read snapshot so display,
/// selection and execution cannot diverge by re-reading between them.
pub struct VisibleStyles {
    union: Vec<VisibleStyle>,
    account_hash: Option<String>,
}

fn visible_style_id(r: &VisibleStyle) -> &str {
    match r {
        VisibleStyle::Legacy { id, .. } => id,
        VisibleStyle::Account(s) => &s.id,
    }
}

impl VisibleStyles {
    /// Read the union once. `None` yields legacy only (signed out).
    pub fn load(account_hash: Option<&str>) -> Self {
        Self {
            union: load_visible_styles(account_hash),
            account_hash: account_hash.map(str::to_string),
        }
    }

    /// Build from an already-loaded union with a known account.
    pub fn from_union_with_account(union: Vec<VisibleStyle>, account_hash: Option<&str>) -> Self {
        Self {
            union,
            account_hash: account_hash.map(str::to_string),
        }
    }

    fn has_verified_account(&self) -> bool {
        match &self.account_hash {
            Some(h) => valid_account_hash(h),
            None => false,
        }
    }

    /// Styles a runtime consumer may execute: the admission gate.
    pub fn admitted(&self) -> Vec<VisibleStyle> {
        let hash = self.account_hash.as_deref();
        self.union
            .iter()
            .filter(|r| {
                if !self.has_verified_account() && matches!(r, VisibleStyle::Account(_)) {
                    return false;
                }
                admit_style(r, hash).is_admissible()
            })
            .cloned()
            .collect()
    }

    /// True for builtin ids and admitted custom styles. An unassigned legacy
    /// style is displayable but not known-executable.
    pub fn is_known(&self, style_id: &str) -> bool {
        crate::prompts::is_builtin_style(style_id)
            || self
                .admitted()
                .iter()
                .any(|r| visible_style_id(r) == style_id)
    }

    /// Resolve a custom style id to its hint for execution. Tombstoned and
    /// unadmitted records yield `None` (caller falls back, never stuck).
    pub fn resolve_hint(&self, style_id: &str) -> Option<String> {
        let found = self
            .admitted()
            .into_iter()
            .find(|r| visible_style_id(r) == style_id)?;
        let (hint, deleted) = match &found {
            VisibleStyle::Legacy { hint, .. } => (hint.clone(), false),
            VisibleStyle::Account(s) => (s.hint.clone(), s.deleted_at.is_some()),
        };
        if deleted {
            return None;
        }
        let clean = crate::prompts::sanitize_custom_prompt(&hint);
        if clean.is_empty() {
            None
        } else {
            Some(clean)
        }
    }

    /// Admission state of a listed record (G7 lesson: account records without
    /// a verified account report Unassigned, never runnable).
    pub fn admission_of(&self, record: &VisibleStyle) -> Admission {
        if !self.has_verified_account() && matches!(record, VisibleStyle::Account(_)) {
            return Admission::Unassigned;
        }
        admit_style(record, self.account_hash.as_deref())
    }

    /// True when this listed record may actually be run.
    pub fn is_runnable(&self, record: &VisibleStyle) -> bool {
        if !self.admission_of(record).is_admissible() {
            return false;
        }
        match record {
            VisibleStyle::Legacy { .. } => true,
            VisibleStyle::Account(s) => s.deleted_at.is_none(),
        }
    }

    /// Style counterpart of [`VisibleAgents::is_claimable`], off the same gate.
    pub fn is_claimable(&self, record: &VisibleStyle) -> bool {
        matches!(record, VisibleStyle::Legacy { .. })
            && self.admission_of(record) == Admission::Unassigned
    }

    /// Project the union onto the prompts-board view shape, with per-row
    /// claimability. Style counterpart of [`VisibleAgents::into_view`].
    ///
    /// Walks `union` under `is_displayable`, NOT `admitted`. `admitted` is the
    /// EXECUTION gate and, by design, withholds `Unassigned` rows while an
    /// account is signed in — correct for runtime, wrong for a board: a listing
    /// built from it hides pre-account data the moment the user signs in, which
    /// is exactly the "it vanished" perception `is_displayable` was written to
    /// prevent, and it leaves nothing to offer a claim action on. This mirrors
    /// `VisibleAgents::into_view`, which already lists agents that way, so the
    /// two boards no longer disagree.
    ///
    /// Claims are still impossible for a displayed-but-unclaimed row: it stays
    /// inadmissible for execution, unowned for sync, and `claimable` only when an
    /// account is signed in.
    pub fn into_view_list(&self) -> Vec<crate::prompts::CustomStyleView> {
        self.union
            .iter()
            .filter(|r| self.admission_of(r).is_displayable())
            .filter_map(|r| match r {
                VisibleStyle::Legacy { id, name, hint } => Some(crate::prompts::CustomStyleView {
                    id: id.clone(),
                    name: name.clone(),
                    // Not sanitised here: `load_custom_styles_for` never did, and
                    // the board must keep rendering stored text verbatim.
                    hint: hint.clone(),
                    claimable: self.is_claimable(r),
                }),
                VisibleStyle::Account(s) => {
                    if s.deleted_at.is_some() {
                        return None;
                    }
                    Some(crate::prompts::CustomStyleView {
                        id: s.id.clone(),
                        name: s.name.clone(),
                        hint: s.hint.clone(),
                        claimable: false,
                    })
                }
            })
            .collect()
    }
}

/// The active account's hash, or `None` when signed out.
///
/// Wiring rule (Reviewer 2, binding): callers must pass this real value or
/// `None`. It is never a placeholder, so a junk file can never be minted.
pub fn active_account_hash() -> Option<String> {
    crate::sync::metadata::current_account_hash().filter(|h| valid_account_hash(h))
}

/// The id of a visible union record, whichever variant it is.
pub fn visible_id(r: &VisibleRecord) -> &str {
    match r {
        VisibleRecord::Legacy { id, .. } => id,
        VisibleRecord::Account(a) => &a.id,
    }
}

/// `is_known_id` evaluated over the union, preserving the existing rule: the
/// built-in id is always known, plus any id visible in the union.
///
/// Production counterpart of the Android `AccountScope.isKnownAgentIn`. It takes
/// the union as a parameter and never re-loads, so the set the UI lists and the
/// set resolution consults are the same object by construction — they cannot
/// diverge.
pub fn is_known_in(union: &[VisibleRecord], agent_id: &str) -> bool {
    agent_id == crate::agents::ID_BUILT_IN || union.iter().any(|r| visible_id(r) == agent_id)
}

/// Resolve an agent id against the union, mirroring the existing
/// `resolve_active_agent` contract: `None` for the built-in id, an absent id, or
/// an unknown id — all of which mean "use the built-in".
///
/// Order-independent by construction: the union is already deduplicated by id,
/// so at most one record per id exists. That is what stops a tombstoned record
/// falling through to a shadowed legacy record and resurrecting a delete.
///
/// Production counterpart of the Android `AccountScope.resolveAgentIn`.
pub fn resolve_in<'a>(
    union: &'a [VisibleRecord],
    agent_id: Option<&str>,
) -> Option<&'a VisibleRecord> {
    let requested = agent_id?;
    if requested == crate::agents::ID_BUILT_IN {
        return None;
    }
    union.iter().find(|r| visible_id(r) == requested)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- resolution over the union (is_known_id / resolve_active_agent) ----

    #[test]
    fn is_known_id_over_union_legacy_only() {
        let u = union_agents(&[legacy_agent("agent:1", "L1")], &[]);
        assert!(is_known_in(&u, crate::agents::ID_BUILT_IN));
        assert!(is_known_in(&u, "agent:1"));
        assert!(!is_known_in(&u, "agent:missing"));
    }

    #[test]
    fn is_known_id_over_union_account_only() {
        let u = union_agents(&[], &[a("agent:A", "A1")]);
        assert!(is_known_in(&u, "agent:A"));
        assert!(!is_known_in(&u, "agent:1"));
    }

    #[test]
    fn is_known_id_over_union_both() {
        let u = union_agents(&[legacy_agent("agent:1", "L1")], &[a("agent:2", "A2")]);
        assert!(is_known_in(&u, "agent:1"));
        assert!(is_known_in(&u, "agent:2"));
        assert!(!is_known_in(&u, "agent:3"));
    }

    #[test]
    fn is_known_id_collision_resolves_to_account_record() {
        // Same id, different records: the account one is what the user sees, so
        // that is the hint the execution path must use.
        let u = union_agents(
            &[legacy_agent("agent:X", "Legacy")],
            &[a("agent:X", "Account")],
        );
        assert!(is_known_in(&u, "agent:X"));
        let visible = u.iter().find(|r| visible_id(r) == "agent:X").unwrap();
        assert!(matches!(visible, VisibleRecord::Account(_)));
    }

    #[test]
    fn union_never_signed_in_is_legacy_only() {
        // `None` hash => no account records at all; local-first preserved.
        let u = union_agents(&[legacy_agent("agent:1", "L1")], &[]);
        assert_eq!(1, u.len());
        assert!(matches!(u[0], VisibleRecord::Legacy { .. }));
    }

    #[test]
    fn a_to_b_to_a_visible_sets() {
        // A's record visible only as A; B's only as B; each returns on return.
        let a_rec = a("agent:shared", "A-agent");
        let b_rec = a("agent:shared", "B-agent");
        let legacy = vec![legacy_agent("agent:dev", "Device")];

        let as_a = union_agents(&legacy, &[a_rec.clone()]);
        assert!(matches!(
            as_a.iter().find(|r| visible_id(r) == "agent:shared"),
            Some(VisibleRecord::Account(x)) if x.name == "A-agent"
        ));

        let as_b = union_agents(&legacy, &[b_rec.clone()]);
        assert!(matches!(
            as_b.iter().find(|r| visible_id(r) == "agent:shared"),
            Some(VisibleRecord::Account(x)) if x.name == "B-agent"
        ));

        // Back to A: A's record, not B's.
        let back = union_agents(&legacy, &[a_rec]);
        assert!(matches!(
            back.iter().find(|r| visible_id(r) == "agent:shared"),
            Some(VisibleRecord::Account(x)) if x.name == "A-agent"
        ));
        // The device-local record is visible to both, untouched.
        assert!(is_known_in(&as_a, "agent:dev"));
        assert!(is_known_in(&as_b, "agent:dev"));
    }

    #[test]
    fn corrupt_account_store_never_hides_valid_legacy() {
        // A rejected store yields no account records; legacy still resolves.
        let u = union_agents(&[legacy_agent("agent:1", "L1")], &[]);
        assert!(is_known_in(&u, "agent:1"));
        assert_eq!(1, u.len());
    }

    // ---- production path: VisibleAgents snapshot (Phase 5) -----------------

    /// Test hash. Production snapshots always carry the verified account hash;
    /// a `None` hash withholds account records, so tests exercising account
    /// records pass one explicitly (mirrors Android `VisibleAgentsSnapshotTest`).
    const TEST_HASH: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn snap(legacy: Vec<crate::agents::CustomAgent>, account: Vec<AccountAgent>) -> VisibleAgents {
        VisibleAgents::from_union(union_agents(&legacy, &account))
    }

    fn snap_signed_in(
        legacy: Vec<crate::agents::CustomAgent>,
        account: Vec<AccountAgent>,
    ) -> VisibleAgents {
        VisibleAgents::from_union_with_account(union_agents(&legacy, &account), Some(TEST_HASH))
    }

    /// Condition 2: null must preserve builtin behaviour, never an error.
    #[test]
    fn resolve_null_maps_to_builtin_not_an_error() {
        let s = snap(vec![legacy_agent("agent:1", "L1")], vec![]);
        let r = s.resolve(None);
        assert!(r.is_builtin);
        assert_eq!(crate::agents::ID_BUILT_IN, r.id);
        assert!(r.hint.is_none());
        assert!(s.hint(None).is_none(), "no hint means run the built-in");
    }

    #[test]
    fn resolve_builtin_unknown_and_absent_all_map_to_builtin() {
        let s = snap(vec![legacy_agent("agent:1", "L1")], vec![]);
        for id in [Some(crate::agents::ID_BUILT_IN), Some("agent:nope"), None] {
            let r = s.resolve(id);
            assert!(r.is_builtin, "id {id:?} must map to builtin");
            assert!(r.hint.is_none());
        }
    }

    #[test]
    fn resolve_visible_record_returns_its_hint() {
        // STAGE 6: a legacy record of unknown provenance does not resolve while
        // signed in. Only the owned record supplies a hint.
        let s = snap_signed_in(
            vec![legacy_agent("agent:1", "L1")],
            vec![a("agent:2", "A2")],
        );
        let r = s.resolve(Some("agent:1"));
        assert!(
            r.is_builtin,
            "unassigned legacy must not resolve while signed in"
        );
        assert!(r.hint.is_none());
        let r2 = s.resolve(Some("agent:2"));
        assert_eq!("agent:2", r2.id);
        assert!(!r2.is_builtin);
    }

    /// Condition 3: a tombstoned record must never yield a usable hint.
    #[test]
    fn tombstoned_record_never_yields_a_usable_hint() {
        let dead = AccountAgent {
            deleted_at: Some(1_700_000_000_000),
            ..a("agent:2", "gone")
        };
        let s = snap_signed_in(vec![legacy_agent("agent:1", "L1")], vec![dead]);
        let r = s.resolve(Some("agent:2"));
        assert!(r.is_builtin, "a deleted agent must not resolve as custom");
        assert!(r.hint.is_none(), "a deleted agent must never leak a hint");
        assert!(s.hint(Some("agent:2")).is_none());
    }

    #[test]
    fn tombstone_does_not_fall_through_to_shadowed_legacy() {
        // The tombstone wins the id, so a legacy record of the same id must NOT
        // become the resolved agent — that would resurrect a deleted agent.
        let dead = AccountAgent {
            deleted_at: Some(42),
            ..a("agent:X", "deleted")
        };
        let s = snap_signed_in(vec![legacy_agent("agent:X", "Legacy Agent")], vec![dead]);
        let r = s.resolve(Some("agent:X"));
        assert!(r.is_builtin);
        assert!(r.hint.is_none());
    }

    /// Condition 1 in practice: one snapshot serves both listing and resolution.
    ///
    /// STAGE 6 sharpened this invariant. Listing and resolution share ONE
    /// snapshot, but display is deliberately a superset of what can run: an
    /// unassigned legacy record is listed (so it does not look deleted) while
    /// being non-runnable. The invariant is therefore about RUNNABILITY — every
    /// record the UI marks runnable must be known and must resolve — not about
    /// every listed id being executable.
    #[test]
    fn one_snapshot_serves_listing_and_resolution() {
        let s = snap_signed_in(
            vec![legacy_agent("agent:1", "L1")],
            vec![a("agent:2", "A2")],
        );
        let listed: Vec<&str> = s.records().iter().map(|r| visible_id(r)).collect();
        assert_eq!(vec!["agent:2", "agent:1"], listed);
        for id in listed {
            let record = s.records().iter().find(|r| visible_id(r) == id).unwrap();
            assert_eq!(
                s.is_runnable(record),
                s.is_known(id) && !s.resolve(Some(id)).is_builtin,
                "runnability must agree for {id}",
            );
        }
        assert!(s.is_runnable(
            &s.records()
                .iter()
                .find(|r| visible_id(r) == "agent:2")
                .unwrap()
        ));
        assert!(!s.is_runnable(
            &s.records()
                .iter()
                .find(|r| visible_id(r) == "agent:1")
                .unwrap()
        ));
    }

    #[test]
    fn is_known_covers_builtin_and_admitted_ids_only() {
        // STAGE 6: a legacy record is displayable but not executable, so it
        // must not be reported as known. Were it "known", a saved default
        // pointing at it could cause an unknown-provenance prompt to run.
        let s = snap_signed_in(
            vec![legacy_agent("agent:1", "L1")],
            vec![a("agent:2", "A2")],
        );
        assert!(s.is_known(crate::agents::ID_BUILT_IN));
        assert!(
            !s.is_known("agent:1"),
            "unassigned legacy must not be executable"
        );
        assert!(s.is_known("agent:2"));
        assert!(!s.is_known("agent:other"));
    }

    // Condition 4/6: D1 — legacy stays visible, never adopted, never leaked.
    #[test]
    fn never_signed_in_snapshot_is_legacy_only() {
        let s = VisibleAgents::from_union(union_agents(&[legacy_agent("agent:1", "L1")], &[]));
        assert_eq!(1, s.records().len());
        assert!(matches!(s.records()[0], VisibleRecord::Legacy { .. }));
        assert!(!s.resolve(Some("agent:1")).is_builtin);
    }

    #[test]
    fn snapshot_exposes_no_other_account_records() {
        // Only what was passed in is visible; B's records are simply absent.
        let s = VisibleAgents::from_union_with_account(
            union_agents(&[], &[a("agent:A", "A-only")]),
            Some(TEST_HASH),
        );
        assert!(s.is_known("agent:A"));
        assert!(!s.is_known("agent:B"));
        assert!(s.resolve(Some("agent:B")).is_builtin);
    }

    #[test]
    fn a_to_b_to_a_snapshots_each_resolve_their_own_record() {
        let device = legacy_agent("agent:dev", "Device");
        let hash_b = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        let as_a = VisibleAgents::from_union_with_account(
            union_agents(&[device.clone()], &[a("agent:s", "A-agent")]),
            Some(TEST_HASH),
        );
        let as_b = VisibleAgents::from_union_with_account(
            union_agents(&[device.clone()], &[a("agent:s", "B-agent")]),
            Some(hash_b),
        );
        let back = VisibleAgents::from_union_with_account(
            union_agents(&[device], &[a("agent:s", "A-agent")]),
            Some(TEST_HASH),
        );
        assert_eq!(Some("hint-A-agent".to_string()), as_a.hint(Some("agent:s")));
        assert_eq!(Some("hint-B-agent".to_string()), as_b.hint(Some("agent:s")));
        assert_eq!(Some("hint-A-agent".to_string()), back.hint(Some("agent:s")));
        // The shared legacy device record stays LISTED in each snapshot but is
        // not executable under any signed-in account.
        for s in [&as_a, &as_b, &back] {
            assert!(s.records().iter().any(|r| visible_id(r) == "agent:dev"));
            assert!(!s.is_known("agent:dev"));
        }
    }

    /// Production path: the IPC view keeps its existing shape and D1c holds.
    #[test]
    fn into_view_preserves_shape_and_device_local_default_id() {
        let s = snap(
            vec![legacy_agent("agent:1", "L1")],
            vec![a("agent:2", "A2")],
        );
        let view = s.into_view("builtin".to_string());
        assert_eq!(crate::agents::ID_BUILT_IN, view.builtin_id);
        assert_eq!(crate::agents::NAME_BUILT_IN, view.builtin_name);
        // default_id passes straight through: still device-local, never
        // account-scoped (D1c).
        assert_eq!("builtin", view.default_id);
        // Account record first (account-wins ordering), then device-local.
        let ids: Vec<&str> = view.custom_agents.iter().map(|x| x.id.as_str()).collect();
        assert_eq!(vec!["agent:2", "agent:1"], ids);
    }

    #[test]
    fn into_view_collision_renders_the_account_record() {
        let s = snap(
            vec![legacy_agent("agent:X", "Legacy Agent")],
            vec![a("agent:X", "Account Agent")],
        );
        let view = s.into_view("builtin".to_string());
        assert_eq!(1, view.custom_agents.len());
        assert_eq!("Account Agent", view.custom_agents[0].name);
    }

    #[test]
    fn empty_snapshot_still_resolves_builtin() {
        let s = VisibleAgents::from_union(vec![]);
        assert!(s.resolve(Some("agent:1")).is_builtin);
        assert!(s.resolve(None).is_builtin);
        assert!(!s.is_known("agent:1"));
    }

    // ---- STAGE 5/6: three-state admission ----------------------------------
    //
    // Mirrors Android `ThreeStateAdmissionTest`. Classification derives from
    // WHERE the record lives, never from who is signed in: signing in must not
    // be capable of promoting an unknown-provenance record to Owned.

    fn owned_record(id: &str) -> VisibleRecord {
        VisibleRecord::Account(a(id, "Owned"))
    }

    fn legacy_record(id: &str) -> VisibleRecord {
        VisibleRecord::Legacy {
            id: id.to_string(),
            name: "L".to_string(),
            hint: "h".to_string(),
        }
    }

    #[test]
    fn admission_classifies_by_storage_location() {
        assert_eq!(
            Admission::Owned,
            admit_agent(&owned_record("x1"), Some(TEST_HASH))
        );
        assert_eq!(
            Admission::Unassigned,
            admit_agent(&legacy_record("l1"), Some(TEST_HASH))
        );
    }

    #[test]
    fn admission_does_not_depend_on_which_account_is_signed_in() {
        let hash_b = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        for h in [TEST_HASH, hash_b] {
            assert_eq!(Admission::Owned, admit_agent(&owned_record("x1"), Some(h)));
            assert_eq!(
                Admission::Unassigned,
                admit_agent(&legacy_record("l1"), Some(h))
            );
        }
    }

    #[test]
    fn signed_out_withholds_nothing() {
        // Only a POSITIVELY absent account counts as signed out. There is no
        // identity to confuse, so withholding would only brick offline use.
        for h in [None, Some(""), Some("   ")] {
            assert_eq!(Admission::DeviceLocal, admit_agent(&owned_record("x1"), h));
            assert_eq!(Admission::DeviceLocal, admit_agent(&legacy_record("l1"), h));
        }
    }

    #[test]
    fn malformed_hash_does_not_unlock_unassigned_records() {
        // A non-blank invalid hash is an UNVERIFIABLE identity, not an absent
        // one. Treating it as signed out would make every legacy agent
        // executable under an identity nobody can name.
        let hash_a = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let bad: Vec<String> = vec![
            "not-a-hash".to_string(),
            hash_a[..63].to_string(),
            format!("{hash_a}0"),
            format!("g{}", &hash_a[1..]),
            hash_a.to_uppercase(),
        ];
        for h in &bad {
            assert_eq!(
                Admission::Unassigned,
                admit_agent(&legacy_record("l1"), Some(h)),
                "must stay withheld: {h}"
            );
            assert!(!admit_agent(&legacy_record("l1"), Some(h)).is_admissible());
        }
    }

    #[test]
    fn malformed_hash_still_admits_records_from_an_account_store() {
        // Fail-closed must not over-reach: a record already inside a
        // hash-partitioned account file was written under a validated hash.
        for h in [
            "not-a-hash",
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        ] {
            assert_eq!(Admission::Owned, admit_agent(&owned_record("x1"), Some(h)));
        }
    }

    #[test]
    fn only_owned_is_syncable() {
        assert!(Admission::Owned.is_syncable());
        assert!(
            !Admission::DeviceLocal.is_syncable(),
            "device-local must never auto-upload"
        );
        assert!(
            !Admission::Unassigned.is_syncable(),
            "unassigned must never upload"
        );
    }

    #[test]
    fn unassigned_never_becomes_syncable_just_by_signing_in() {
        let before = admit_agent(&legacy_record("l1"), None);
        let after = admit_agent(&legacy_record("l1"), Some(TEST_HASH));
        assert!(!before.is_syncable());
        assert!(!after.is_syncable());
        assert_eq!(Admission::Unassigned, after);
    }

    #[test]
    fn displayable_and_admissible_are_deliberately_different() {
        // Guards the footgun: a consumer checking only is_admissible would hide
        // unassigned records and reintroduce the "my agents vanished" bug.
        assert!(Admission::Unassigned.is_displayable());
        assert!(!Admission::Unassigned.is_admissible());
    }

    #[test]
    fn into_view_omits_tombstoned_account_records() {
        // A deleted agent must never render as a listed, selectable agent.
        let s = snap(
            vec![legacy_agent("agent:1", "L1")],
            vec![
                a("agent:2", "A2"),
                AccountAgent {
                    deleted_at: Some(42),
                    ..a("agent:3", "gone")
                },
            ],
        );
        let view = s.into_view("builtin".to_string());
        let ids: Vec<&str> = view.custom_agents.iter().map(|x| x.id.as_str()).collect();
        assert_eq!(vec!["agent:2", "agent:1"], ids);
        assert!(!ids.contains(&"agent:3"));
    }

    #[test]
    fn into_view_sanitizes_account_hints() {
        // An account-sourced hint must go through the same sanitisation as a
        // legacy one, so it cannot bypass the legacy projection's guarantees.
        let s = snap(
            vec![],
            vec![AccountAgent {
                hint: "  spaced hint  ".to_string(),
                ..a("agent:1", "A")
            }],
        );
        let view = s.into_view("builtin".to_string());
        assert_eq!("spaced hint", view.custom_agents[0].hint);
    }

    #[test]
    fn into_view_tombstone_does_not_shadow_a_legacy_sibling_into_the_list() {
        // The tombstone wins the id in the union, and it is omitted from the
        // view, so the shadowed device-local record does NOT reappear.
        let s = snap(
            vec![legacy_agent("agent:X", "Legacy Agent")],
            vec![AccountAgent {
                deleted_at: Some(9),
                ..a("agent:X", "deleted")
            }],
        );
        let view = s.into_view("builtin".to_string());
        assert!(
            view.custom_agents.is_empty(),
            "a deleted id must not resurface its shadowed legacy record"
        );
    }

    // ---- read-union: account wins on id collision, legacy never adopted ----

    fn legacy_agent(id: &str, name: &str) -> crate::agents::CustomAgent {
        crate::agents::CustomAgent {
            id: id.into(),
            name: name.into(),
            hint: format!("hint-{name}"),
        }
    }

    fn legacy_style(id: &str, name: &str) -> crate::prompts::CustomStyle {
        crate::prompts::CustomStyle {
            id: id.into(),
            name: name.into(),
            hint: format!("hint-{name}"),
        }
    }

    fn names(v: &[VisibleRecord]) -> Vec<&str> {
        v.iter()
            .map(|r| match r {
                VisibleRecord::Legacy { name, .. } => name.as_str(),
                VisibleRecord::Account(a) => a.name.as_str(),
            })
            .collect()
    }

    #[test]
    fn union_legacy_only() {
        let out = union_agents(&[legacy_agent("agent:1", "L1")], &[]);
        assert_eq!(vec!["L1"], names(&out));
        assert!(matches!(out[0], VisibleRecord::Legacy { .. }));
    }

    #[test]
    fn union_account_only() {
        let out = union_agents(&[], &[a("agent:1", "A1")]);
        assert_eq!(vec!["A1"], names(&out));
        assert!(matches!(out[0], VisibleRecord::Account(_)));
    }

    #[test]
    fn union_both_distinct_ids() {
        let out = union_agents(&[legacy_agent("agent:1", "L1")], &[a("agent:2", "A2")]);
        assert_eq!(vec!["A2", "L1"], names(&out));
    }

    #[test]
    fn union_same_id_account_wins() {
        // The decided rule: a signed-in user sees the record they OWN, not a
        // shadowed device-local one.
        let out = union_agents(
            &[legacy_agent("agent:X", "Legacy Agent")],
            &[a("agent:X", "Account Agent")],
        );
        assert_eq!(vec!["Account Agent"], names(&out));
        assert!(matches!(out[0], VisibleRecord::Account(_)));
        assert_eq!(
            1,
            out.len(),
            "the shadowed legacy record must not duplicate"
        );
    }

    #[test]
    fn union_shadowed_legacy_record_is_not_mutated_or_adopted() {
        // Pure function: the input is untouched, so shadowing cannot delete,
        // mutate, or adopt the legacy record.
        let legacy = vec![legacy_agent("agent:X", "Legacy Agent")];
        let before = legacy.clone();
        let _ = union_agents(&legacy, &[a("agent:X", "Account Agent")]);
        assert_eq!(before, legacy, "union must not mutate its input");
    }

    #[test]
    fn union_shadowed_legacy_returns_when_no_account_signed_in() {
        let legacy = vec![legacy_agent("agent:X", "Legacy Agent")];
        let signed_in = union_agents(&legacy, &[a("agent:X", "Account Agent")]);
        assert_eq!(vec!["Account Agent"], names(&signed_in));
        // Signed out: no account records, so the legacy record is visible again.
        let signed_out = union_agents(&legacy, &[]);
        assert_eq!(vec!["Legacy Agent"], names(&signed_out));
    }

    #[test]
    fn union_multiple_account_records() {
        let out = union_agents(
            &[],
            &[a("agent:1", "A1"), a("agent:2", "A2"), a("agent:3", "A3")],
        );
        assert_eq!(vec!["A1", "A2", "A3"], names(&out));
    }

    #[test]
    fn union_empty_inputs() {
        assert!(union_agents(&[], &[]).is_empty());
    }

    #[test]
    fn union_deduplicates_within_account_records() {
        let out = union_agents(&[], &[a("agent:1", "first"), a("agent:1", "second")]);
        assert_eq!(1, out.len());
        assert_eq!(vec!["first"], names(&out));
    }

    #[test]
    fn union_preserves_tombstones() {
        // A tombstoned account record still appears, so a future merge sees the
        // delete instead of resurrecting it.
        let dead = AccountAgent {
            deleted_at: Some(1_700_000_000_000),
            ..a("agent:2", "gone")
        };
        let out = union_agents(&[legacy_agent("agent:1", "L1")], &[dead]);
        assert_eq!(2, out.len());
        match &out[0] {
            VisibleRecord::Account(x) => assert!(x.deleted_at.is_some()),
            other => panic!("expected account record, got {other:?}"),
        }
    }

    #[test]
    fn union_tombstone_shadows_legacy_even_when_deleted() {
        // The account's delete wins the id, so the legacy record does not
        // reappear for this account — but stays intact for a signed-out read.
        let dead = AccountAgent {
            deleted_at: Some(42),
            ..a("agent:X", "deleted")
        };
        let legacy = vec![legacy_agent("agent:X", "Legacy Agent")];
        let out = union_agents(&legacy, &[dead]);
        assert_eq!(1, out.len());
        assert!(matches!(&out[0], VisibleRecord::Account(x) if x.deleted_at == Some(42)));
        assert_eq!(vec!["Legacy Agent"], names(&union_agents(&legacy, &[])));
    }

    #[test]
    fn union_is_deterministic() {
        let legacy = vec![legacy_agent("agent:1", "L1"), legacy_agent("agent:X", "LX")];
        let acct = vec![a("agent:2", "A2"), a("agent:X", "AX")];
        assert_eq!(union_agents(&legacy, &acct), union_agents(&legacy, &acct));
        let again = union_agents(&legacy, &acct);
        assert_eq!(union_agents(&legacy, &acct), again);
    }

    #[test]
    fn union_fabricates_nothing() {
        // Every output id must exist in the inputs, and the count never exceeds
        // the input count — no synthesized records.
        let legacy = vec![legacy_agent("agent:1", "L1")];
        let acct = vec![a("agent:2", "A2")];
        let out = union_agents(&legacy, &acct);
        assert_eq!(legacy.len() + acct.len(), out.len());
        let input_ids: std::collections::HashSet<&str> = legacy
            .iter()
            .map(|x| x.id.as_str())
            .chain(acct.iter().map(|x| x.id.as_str()))
            .collect();
        for r in &out {
            let id = match r {
                VisibleRecord::Legacy { id, .. } => id.as_str(),
                VisibleRecord::Account(x) => x.id.as_str(),
            };
            assert!(input_ids.contains(id), "union invented id {id}");
        }
    }

    // ---- style union mirrors the agent rules ----

    #[test]
    fn style_union_same_id_account_wins() {
        let out = union_styles(
            &[legacy_style("custom:X", "Legacy Style")],
            &[s("custom:X", "Account Style")],
        );
        assert_eq!(1, out.len());
        match &out[0] {
            VisibleStyle::Account(x) => assert_eq!("Account Style", x.name),
            other => panic!("expected account style, got {other:?}"),
        }
    }

    #[test]
    fn style_union_both_and_empty() {
        let out = union_styles(&[legacy_style("custom:1", "L1")], &[s("custom:2", "A2")]);
        assert_eq!(2, out.len());
        assert!(union_styles(&[], &[]).is_empty());
        assert_eq!(
            1,
            union_styles(&[legacy_style("custom:1", "L1")], &[]).len()
        );
    }

    #[test]
    fn style_union_preserves_tombstone() {
        let dead = AccountStyle {
            deleted_at: Some(7),
            ..s("custom:1", "gone")
        };
        let out = union_styles(&[], &[dead]);
        assert!(matches!(&out[0], VisibleStyle::Account(x) if x.deleted_at == Some(7)));
    }

    fn a(id: &str, name: &str) -> AccountAgent {
        AccountAgent {
            id: id.into(),
            name: name.into(),
            hint: format!("hint-{name}"),
            sync_id: Some(format!("sync-{id}")),
            updated_at: Some(100),
            device_id: Some("dev-a".into()),
            deleted_at: None,
            dirty: false,
        }
    }

    fn s(id: &str, name: &str) -> AccountStyle {
        AccountStyle {
            id: id.into(),
            name: name.into(),
            hint: format!("hint-{name}"),
            sync_id: Some(format!("sync-{id}")),
            updated_at: Some(100),
            device_id: Some("dev-a".into()),
            deleted_at: None,
            dirty: false,
        }
    }

    // ---- gate 3: account files are distinct per account ----

    #[test]
    fn account_files_are_distinct_per_account() {
        let dir = std::env::temp_dir();
        let h1 = "a".repeat(64);
        let h2 = "b".repeat(64);
        let a1 = account_path(&dir, "agents", &h1);
        let a2 = account_path(&dir, "agents", &h2);
        assert_ne!(a1, a2);
        assert!(a1.to_string_lossy().contains(&h1));
        assert!(a2.to_string_lossy().contains(&h2));
    }

    #[test]
    fn account_file_never_collides_with_the_legacy_file() {
        let dir = std::env::temp_dir();
        let h1 = "a".repeat(64);
        let legacy = dir.join("agents.json");
        let scoped = account_path(&dir, "agents", &h1);
        assert_ne!(legacy, scoped);
        // An old build only ever knows the legacy name, so it cannot target this.
        assert_ne!(scoped.file_name().unwrap(), legacy.file_name().unwrap());
    }

    // ---- hash validation: no path traversal, no email in a file name ----

    #[test]
    fn invalid_account_hashes_are_rejected() {
        assert!(!valid_account_hash(""));
        assert!(!valid_account_hash("user@example.com"));
        assert!(!valid_account_hash("../../escape"));
        assert!(!valid_account_hash("has space"));
        // Truncated / wrong-length hashes are rejected rather than silently
        // creating a second account file for the same account.
        assert!(!valid_account_hash("aaaa"));
        assert!(!valid_account_hash(&"a".repeat(63)));
        assert!(!valid_account_hash(&"a".repeat(65)));
        // Uppercase is not the canonical form, so it is refused too.
        assert!(!valid_account_hash(&"A".repeat(64)));
        assert!(valid_account_hash(&"a".repeat(64)));
        assert!(valid_account_hash(
            "47ff03cc027b9d2d104aa8e14e37fb9572f4a9aec859cee75bf440e429ca80bb"
        ));
    }

    #[test]
    fn traversal_attempt_yields_default_and_cannot_write() {
        // Read is inert, write is refused: a hostile hash cannot reach the FS.
        assert_eq!(
            load_account_agents("../evil"),
            AccountAgentsStore::default()
        );
        assert!(save_account_agents("../evil", &AccountAgentsStore::default()).is_err());
    }

    #[test]
    fn truncated_hash_is_refused_rather_than_getting_its_own_file() {
        // The failure this prevents: a short hash would otherwise create a
        // second file for one account, which reads as data loss much later.
        assert!(save_account_agents("aaaa", &AccountAgentsStore::default()).is_err());
        assert!(save_account_styles("aaaa", &AccountStylesStore::default()).is_err());
    }

    // ---- tombstones survive a round trip (gate: permanent deletes) ----

    #[test]
    fn tombstone_round_trips_and_is_retained() {
        let deleted = AccountAgent {
            deleted_at: Some(1_700_000_000_000),
            ..a("agent:1", "gone")
        };
        let store = AccountAgentsStore {
            custom_agents: vec![a("agent:1", "kept"), deleted],
        };
        let json = serde_json::to_string(&store).unwrap();
        let back: AccountAgentsStore = serde_json::from_str(&json).unwrap();
        assert_eq!(store, back);
        assert!(back.custom_agents.iter().any(|x| x.deleted_at.is_some()));
    }

    #[test]
    fn style_tombstone_round_trips() {
        let store = AccountStylesStore {
            custom_styles: vec![
                s("custom:1", "one"),
                AccountStyle {
                    deleted_at: Some(42),
                    ..s("custom:2", "two")
                },
            ],
        };
        let json = serde_json::to_string(&store).unwrap();
        let back: AccountStylesStore = serde_json::from_str(&json).unwrap();
        assert_eq!(store, back);
    }

    // ---- legacy shape must NOT be adopted: no account field appears ----

    #[test]
    fn legacy_records_carry_no_account_field() {
        // The legacy struct is unchanged, so there is nowhere to store ownership
        // and therefore nothing to infer it from.
        let legacy = crate::agents::CustomAgent {
            id: "agent:1".into(),
            name: "n".into(),
            hint: "h".into(),
        };
        let json = serde_json::to_string(&legacy).unwrap();
        assert!(!json.contains("syncAccount"));
        assert!(!json.contains("account"));
        assert!(!json.contains("deletedAt"));
    }

    #[test]
    fn account_record_omits_absent_optional_fields_on_write() {
        // Keeps files small; absent = None on read via #[serde(default)].
        let json = serde_json::to_string(&a("agent:1", "x")).unwrap();
        assert!(json.contains("syncId"));
        assert!(!json.contains("deletedAt"));
    }

    #[test]
    fn account_base_dir_is_hermetic_under_test() {
        // Reviewer-2 binding requirement: these must go through the shared
        // resolver, never raw `dirs::data_local_dir()`, or a wiring-phase test
        // with a valid hash would write the real %LOCALAPPDATA%\Fluence.
        let dir = agents_base_dir();
        assert_eq!(dir, crate::sync::stores::base_data_dir());
        assert_eq!(dir, prompts_base_dir());
        #[cfg(test)]
        {
            let temp = std::env::temp_dir();
            assert!(
                dir.starts_with(&temp),
                "under cfg(test) the account store must resolve inside the temp dir, got {}",
                dir.display()
            );
        }
    }

    // ---- gate 4: old-build round trip cannot destroy account data ----

    #[test]
    fn old_build_rewrite_of_legacy_file_cannot_delete_account_data() {
        // Simulates the exact hazard this layout exists to prevent. An "old
        // build" loads `agents.json` into the LEGACY struct, mutates it, and
        // writes it back — the same load/serialize/rename the real code does.
        // Because it only ever names the legacy path, the account file is
        // untouched. A sibling-key layout would fail this test, because serde
        // would drop the unknown key on the old build's rewrite.
        let dir = std::env::temp_dir().join(format!("fluence-p4-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let legacy_path = dir.join("agents.json");
        let account_file = dir.join(format!("agents.account-{}.json", "aaaa"));

        // Account data written first.
        let account_store = AccountAgentsStore {
            custom_agents: vec![a("agent:1", "A-owned")],
        };
        write_json(&account_file, &account_store).unwrap();

        // Legacy file, as an old build would have it: no account key at all.
        let legacy = crate::agents::AgentsStore {
            custom_agents: vec![crate::agents::CustomAgent {
                id: "agent:legacy".into(),
                name: "legacy".into(),
                hint: "h".into(),
            }],
            default_id: crate::agents::ID_BUILT_IN.to_string(),
        };
        write_json(&legacy_path, &legacy).unwrap();

        // --- old build round trip: read, mutate, write back ---
        let parsed: crate::agents::AgentsStore =
            serde_json::from_str(&std::fs::read_to_string(&legacy_path).unwrap()).unwrap();
        let mut mutated = parsed;
        mutated.custom_agents.push(crate::agents::CustomAgent {
            id: "agent:new".into(),
            name: "new".into(),
            hint: "h".into(),
        });
        write_json(&legacy_path, &mutated).unwrap();

        // Account data survived, byte-identical.
        let after: AccountAgentsStore = read_json(&account_file);
        assert_eq!(after, account_store, "old build destroyed account data");
        assert_eq!(after.custom_agents[0].name, "A-owned");

        // And the legacy file round-tripped without gaining or losing fields.
        let legacy_after: crate::agents::AgentsStore =
            serde_json::from_str(&std::fs::read_to_string(&legacy_path).unwrap()).unwrap();
        assert_eq!(legacy_after.custom_agents.len(), 2);
        assert!(!std::fs::read_to_string(&legacy_path)
            .unwrap()
            .contains("account-"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- gate 5: a corrupt account file must not damage the legacy file ----

    #[test]
    fn corrupt_account_file_is_backed_up_without_touching_legacy() {
        let dir = std::env::temp_dir().join(format!("fluence-p4c-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let legacy_path = dir.join("agents.json");
        let account_file = dir.join(format!("agents.account-{}.json", "aaaa"));

        let legacy_body = r#"{"custom_agents":[{"id":"agent:legacy","name":"l","hint":"h"}],"default_id":"builtin"}"#;
        std::fs::write(&legacy_path, legacy_body).unwrap();
        std::fs::write(&account_file, b"{ this is not json").unwrap();

        // Reading the corrupt account store falls back to default AND rotates a
        // backup beside it — the legacy file is a different path entirely.
        let loaded = read_json::<AccountAgentsStore>(&account_file);
        assert!(loaded.custom_agents.is_empty());
        // `backup_corrupt_account` RENAMES the bad file aside, so the original
        // path is intentionally gone and a `.corrupt` sibling exists instead.
        assert!(
            !account_file.exists(),
            "corrupt file should be rotated aside"
        );
        assert!(std::fs::read_dir(&dir)
            .unwrap()
            .any(|e| { e.unwrap().file_name().to_string_lossy().contains("corrupt") }));
        assert_eq!(std::fs::read_to_string(&legacy_path).unwrap(), legacy_body);

        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod legacy_claim_tests {
    use super::*;
    use std::collections::HashSet;

    // ── helpers ────────────────────────────────────────────────────────────
    //
    // Every test derives its OWN account hash from a unique label, so tests
    // running in parallel never share an account file, and each resets that file
    // first so repeated runs start from a known state.
    //
    // The legacy side is always passed in as a SLICE and never read from disk:
    // the legacy store resolves to the real %LOCALAPPDATA%\Fluence (agents_path
    // and prompts_path have NO cfg(test) redirect), so a test that touched it
    // would read — or worse, overwrite — the developer's actual data.

    fn hash_for(label: &str) -> String {
        // FNV-1a then four avalanche rounds: exactly 64 lowercase hex chars, so
        // it always satisfies valid_account_hash, and distinct per label.
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in label.as_bytes() {
            h ^= *b as u64;
            h = h.wrapping_mul(0x1000_0000_01b3);
        }
        let mut out = String::with_capacity(64);
        for _ in 0..4 {
            h ^= h >> 33;
            h = h.wrapping_mul(0xff51_afd7_ed55_8ccd);
            out.push_str(&format!("{h:016x}"));
        }
        out
    }

    fn fresh_agents(label: &str) -> String {
        let h = hash_for(label);
        save_account_agents(&h, &AccountAgentsStore::default()).unwrap();
        h
    }

    fn fresh_styles(label: &str) -> String {
        let h = hash_for(label);
        save_account_styles(&h, &AccountStylesStore::default()).unwrap();
        h
    }

    fn la(id: &str, name: &str) -> crate::agents::CustomAgent {
        crate::agents::CustomAgent {
            id: id.into(),
            name: name.into(),
            hint: format!("h-{name}"),
        }
    }

    fn ls(id: &str, name: &str) -> crate::prompts::CustomStyle {
        crate::prompts::CustomStyle {
            id: id.into(),
            name: name.into(),
            hint: format!("h-{name}"),
        }
    }

    fn ca(id: &str, name: &str) -> AccountAgent {
        AccountAgent {
            id: id.into(),
            name: name.into(),
            hint: format!("hint-{name}"),
            sync_id: None,
            updated_at: None,
            device_id: None,
            deleted_at: None,
            dirty: false,
        }
    }

    fn cs(id: &str, name: &str) -> AccountStyle {
        AccountStyle {
            id: id.into(),
            name: name.into(),
            hint: format!("hint-{name}"),
            sync_id: None,
            updated_at: None,
            device_id: None,
            deleted_at: None,
            dirty: false,
        }
    }

    fn only(ids: &[&str]) -> HashSet<String> {
        ids.iter().map(|s| s.to_string()).collect()
    }

    fn name_of(r: &VisibleRecord) -> &str {
        match r {
            VisibleRecord::Legacy { name, .. }
            | VisibleRecord::Account(AccountAgent { name, .. }) => name,
        }
    }

    /// Snapshot an account exactly as the production read path does: the union of
    /// the legacy slice with whatever the account store holds.
    fn snap_for(hash: &str, legacy: &[crate::agents::CustomAgent]) -> VisibleAgents {
        VisibleAgents::from_union_with_account(
            union_agents(legacy, &load_account_agents(hash).custom_agents),
            Some(hash),
        )
    }

    fn styles_snap_for(hash: &str, legacy: &[crate::prompts::CustomStyle]) -> VisibleStyles {
        VisibleStyles::from_union_with_account(
            union_styles(legacy, &load_account_styles(hash).custom_styles),
            Some(hash),
        )
    }

    // ── 1. no automatic ownership ──────────────────────────────────────────

    #[test]
    fn unclaimed_record_is_not_owned_runnable_or_syncable_merely_because_signed_in() {
        let h = fresh_agents("lc_no_auto");
        let legacy = vec![la("agent:1", "Legacy")];
        let snap = snap_for(&h, &legacy);
        let rec = &snap.union[0];

        assert_eq!(snap.admission_of(rec), Admission::Unassigned);
        assert!(
            !snap.is_runnable(rec),
            "an unclaimed record must never execute"
        );
        assert!(
            !snap.admission_of(rec).is_syncable(),
            "must never be uploaded"
        );
        assert!(!snap.is_known("agent:1"), "must not be known-executable");
        // Displayed, though: hiding it would read as data loss.
        assert!(snap.admission_of(rec).is_displayable());
        assert!(
            load_account_agents(&h).custom_agents.is_empty(),
            "signing in must not write ownership"
        );
    }

    #[test]
    fn claimability_follows_sign_in_state_not_record_shape() {
        let h = fresh_agents("lc_claimable_gate");
        let legacy = vec![la("agent:1", "Legacy")];

        let signed_in = snap_for(&h, &legacy);
        assert!(signed_in.is_claimable(&signed_in.union[0]));

        // Signed out the same row is DeviceLocal and already usable, so there is
        // nothing to adopt and the affordance must disappear.
        let signed_out = VisibleAgents::from_union(union_agents(&legacy, &[]));
        assert!(!signed_out.is_claimable(&signed_out.union[0]));
        assert!(signed_out.is_runnable(&signed_out.union[0]));
    }

    #[test]
    fn owned_record_is_never_claimable() {
        let h = fresh_agents("lc_owned_not_claimable");
        save_account_agents(
            &h,
            &AccountAgentsStore {
                custom_agents: vec![ca("agent:1", "Owned")],
            },
        )
        .unwrap();
        let snap = snap_for(&h, &[]);
        assert!(!snap.is_claimable(&snap.union[0]));
    }

    // ── 2. explicit claim → owned, runnable, known, syncable ───────────────

    #[test]
    fn explicit_claim_transfers_ownership_into_the_ordinary_lifecycle() {
        let h = fresh_agents("lc_claim_owned");
        let legacy = vec![la("agent:1", "Legacy")];
        let out = claim_legacy_account_agents(Some(&h), &legacy, Some(&only(&["agent:1"])));
        assert_eq!(out.claimed_ids, vec!["agent:1".to_string()]);
        assert!(out.skipped_ids.is_empty());

        let row = &load_account_agents(&h).custom_agents[0];
        assert_eq!(row.name, "Legacy");
        // Written exactly as upsert_account_agent writes: unstamped and dirty, so
        // the ORDINARY pass stamps and uploads it. No new sync path.
        assert!(row.dirty);
        assert!(row.sync_id.is_none() && row.updated_at.is_none());
        assert!(row.device_id.is_none() && row.deleted_at.is_none());
    }

    #[test]
    fn claimed_record_becomes_runnable_known_and_syncable() {
        let h = fresh_agents("lc_claim_capabilities");
        let legacy = vec![la("agent:1", "Legacy")];
        claim_legacy_account_agents(Some(&h), &legacy, Some(&only(&["agent:1"])));

        let snap = snap_for(&h, &legacy);
        let rec = &snap.union[0];
        assert!(matches!(rec, VisibleRecord::Account(_)));
        assert_eq!(snap.admission_of(rec), Admission::Owned);
        assert!(snap.is_runnable(rec));
        assert!(snap.is_known("agent:1"));
        assert!(snap.admission_of(rec).is_syncable());
        assert!(!snap.is_claimable(rec), "owned means no longer claimable");
    }

    #[test]
    fn claimed_record_resolves_a_hint_for_execution() {
        let h = fresh_agents("lc_claim_hint");
        let legacy = vec![la("agent:1", "Legacy")];
        claim_legacy_account_agents(Some(&h), &legacy, Some(&only(&["agent:1"])));
        let snap = snap_for(&h, &legacy);
        assert_eq!(
            snap.resolve(Some("agent:1")).hint.as_deref(),
            Some("h-Legacy")
        );
    }

    #[test]
    fn claimed_record_becomes_deletable_and_delete_writes_a_tombstone() {
        let h = fresh_agents("lc_claim_delete");
        let legacy = vec![la("agent:1", "Legacy")];
        claim_legacy_account_agents(Some(&h), &legacy, Some(&only(&["agent:1"])));

        delete_account_agent(&h, "agent:1");
        let row = &load_account_agents(&h).custom_agents[0];
        assert!(
            row.deleted_at.is_some(),
            "delete must tombstone, not drop the row"
        );
        assert!(row.dirty);
        // A tombstoned owned record is neither listed nor runnable…
        let snap = snap_for(&h, &legacy);
        assert!(!snap.is_runnable(&snap.union[0]));
        // …and it must NOT let the shadowed legacy row resurrect.
        let view = snap.into_view(crate::agents::ID_BUILT_IN.to_string());
        assert!(view.custom_agents.is_empty());
    }

    #[test]
    fn ownership_survives_a_restart() {
        let h = fresh_agents("lc_claim_restart");
        let legacy = vec![la("agent:1", "Legacy")];
        claim_legacy_account_agents(Some(&h), &legacy, Some(&only(&["agent:1"])));

        // A cold start re-reads from disk; the claim must still be there.
        let reloaded = load_account_agents(&h);
        assert_eq!(reloaded.custom_agents.len(), 1);
        assert_eq!(reloaded.custom_agents[0].id, "agent:1");
        assert!(reloaded.custom_agents[0].dirty);
    }

    // ── 3. account safety ──────────────────────────────────────────────────

    #[test]
    fn signed_out_claim_is_refused_and_writes_nothing() {
        let legacy = vec![la("agent:1", "Legacy")];
        assert!(claim_legacy_account_agents(None, &legacy, None).is_empty());
        assert!(claim_legacy_account_agents(Some(""), &legacy, None).is_empty());
    }

    #[test]
    fn unverifiable_identity_is_refused() {
        let legacy = vec![la("agent:1", "Legacy")];
        // A non-blank value that is not a valid hash is an UNVERIFIABLE identity,
        // not an absent one: fail closed rather than adopt.
        for bad in ["not-a-hash", "ZZZ", &"A".repeat(64), &"a".repeat(63)] {
            let out = claim_legacy_account_agents(Some(bad), &legacy, None);
            assert!(
                out.is_empty(),
                "invalid identity {bad:?} must claim nothing"
            );
        }
    }

    #[test]
    fn switching_accounts_before_claiming_transfers_nothing() {
        let a_hash = fresh_agents("lc_switch_a");
        let b_hash = fresh_agents("lc_switch_b");
        let legacy = vec![la("agent:1", "Legacy")];

        // Never claimed by anyone, then a different account looks at it.
        let b_sees = snap_for(&b_hash, &legacy);
        assert_eq!(b_sees.admission_of(&b_sees.union[0]), Admission::Unassigned);
        assert!(load_account_agents(&b_hash).custom_agents.is_empty());

        // B claims it: ownership lands in B, and nowhere else.
        claim_legacy_account_agents(Some(&b_hash), &legacy, Some(&only(&["agent:1"])));
        assert_eq!(load_account_agents(&b_hash).custom_agents.len(), 1);
        assert!(load_account_agents(&a_hash).custom_agents.is_empty());
    }

    #[test]
    fn a_record_claimed_by_one_account_is_not_visible_as_owned_by_another() {
        let a_hash = fresh_agents("lc_iso_a");
        let b_hash = fresh_agents("lc_iso_b");
        let legacy = vec![la("agent:1", "Legacy")];
        claim_legacy_account_agents(Some(&a_hash), &legacy, Some(&only(&["agent:1"])));

        let b = snap_for(&b_hash, &legacy);
        assert_eq!(b.union.len(), 1);
        assert!(
            matches!(b.union[0], VisibleRecord::Legacy { .. }),
            "the other account must still see it as unclaimed legacy, never as owned"
        );
        assert!(!b.is_runnable(&b.union[0]));
    }

    // ── 4. collision: skip, never overwrite ────────────────────────────────

    #[test]
    fn same_id_collision_is_skipped_and_never_overwritten() {
        let h = fresh_agents("lc_collision");
        // The account already owns this id with DIFFERENT content, as it would
        // after the same agent synced down from another device.
        save_account_agents(
            &h,
            &AccountAgentsStore {
                custom_agents: vec![ca("agent:1", "Account version")],
            },
        )
        .unwrap();
        let legacy = vec![la("agent:1", "Legacy version")];

        let out = claim_legacy_account_agents(Some(&h), &legacy, Some(&only(&["agent:1"])));
        assert_eq!(out.skipped_ids, vec!["agent:1".to_string()]);
        assert!(out.claimed_ids.is_empty());

        let row = &load_account_agents(&h).custom_agents[0];
        assert_eq!(
            row.name, "Account version",
            "the owned copy must win untouched"
        );
        assert_eq!(row.hint, "hint-Account version");
    }

    #[test]
    fn a_shadowed_legacy_copy_resolves_to_the_owned_record() {
        let h = fresh_agents("lc_shadow");
        save_account_agents(
            &h,
            &AccountAgentsStore {
                custom_agents: vec![ca("agent:1", "Owned")],
            },
        )
        .unwrap();
        let legacy = vec![la("agent:1", "Legacy")];

        let snap = snap_for(&h, &legacy);
        assert_eq!(snap.union.len(), 1, "one id, one row");
        assert_eq!(name_of(&snap.union[0]), "Owned", "account wins the id");

        // The legacy copy is NOT removed by a claim, so the shadow persists and the
        // owned row keeps winning the id. That redundancy is the point: if a sync
        // pass later overwrites the account row with a pre-claim snapshot, the
        // legacy row is still there to be claimed again rather than the record
        // being lost outright.
        let snap_after = snap_for(&h, &legacy);
        assert_eq!(name_of(&snap_after.union[0]), "Owned");
        assert!(
            !snap_after.is_claimable(&snap_after.union[0]),
            "an owned row must not be offered for claiming"
        );
        assert_eq!(load_account_agents(&h).custom_agents[0].name, "Owned");
    }

    /// The self-healing property that replaces the destructive cleanup: a sync
    /// pass that writes a PRE-CLAIM snapshot (omitting the claimed row) leaves the
    /// record recoverable, because the legacy copy was never deleted.
    ///
    /// This is the regression test for the residual merge race: writers such as
    /// `frozen.rs::save_merged` and `applyMergedAndClearDirty` build their payload
    /// from a snapshot taken before the Drive round trip and do not share the
    /// claim's lock. No claim-side locking can close that, so the guarantee is
    /// provided by keeping the legacy row instead.
    #[test]
    fn a_stale_merge_that_drops_the_claim_leaves_the_record_recoverable() {
        let h = fresh_agents("lc_stale_merge");
        let legacy = vec![la("agent:1", "Legacy")];
        let claim = claim_legacy_account_agents(Some(&h), &legacy, Some(&only(&["agent:1"])));
        assert_eq!(claim.claimed_ids, vec!["agent:1".to_string()]);

        // The sync pass now writes the snapshot it captured BEFORE the claim: no
        // lock is taken by such a writer, and its payload predates the claim.
        save_account_agents(&h, &AccountAgentsStore::default()).unwrap();
        assert!(
            !load_account_agents(&h)
                .custom_agents
                .iter()
                .any(|a| a.id == "agent:1"),
            "precondition: the stale write discarded the claimed row"
        );

        // The record is NOT lost: it still exists in the legacy store, is visible
        // again, and is claimable once more.
        let snap = snap_for(&h, &legacy);
        assert_eq!(snap.union.len(), 1);
        assert!(matches!(snap.union[0], VisibleRecord::Legacy { .. }));
        assert!(
            snap.is_claimable(&snap.union[0]),
            "and can be claimed again"
        );
    }

    // ── 5. idempotency and no-ops ──────────────────────────────────────────

    #[test]
    fn repeated_claim_never_duplicates_and_stays_safe() {
        let h = fresh_agents("lc_idempotent");
        let legacy = vec![la("agent:1", "Legacy")];

        let first = claim_legacy_account_agents(Some(&h), &legacy, Some(&only(&["agent:1"])));
        assert_eq!(first.claimed_ids.len(), 1);

        // The user taps again. Either the legacy row is gone (empty no-op) or it
        // is still there and now collides (skipped). Both are safe; neither may
        // create a second row or rewrite the first.
        let second = claim_legacy_account_agents(Some(&h), &legacy, Some(&only(&["agent:1"])));
        assert!(
            second.claimed_ids.is_empty(),
            "a second claim must not re-add"
        );
        assert_eq!(load_account_agents(&h).custom_agents.len(), 1);
        assert_eq!(load_account_agents(&h).custom_agents[0].name, "Legacy");
    }

    #[test]
    fn empty_and_unmatched_claims_are_no_ops() {
        let h = fresh_agents("lc_noop");
        assert!(claim_legacy_account_agents(Some(&h), &[], None).is_empty());
        let legacy = vec![la("agent:1", "Legacy")];
        let out = claim_legacy_account_agents(Some(&h), &legacy, Some(&only(&["agent:absent"])));
        assert!(out.is_empty());
        assert!(load_account_agents(&h).custom_agents.is_empty());
    }

    #[test]
    fn claiming_one_id_never_moves_the_others() {
        let h = fresh_agents("lc_single");
        let legacy = vec![
            la("agent:1", "One"),
            la("agent:2", "Two"),
            la("agent:3", "Three"),
        ];

        let out = claim_legacy_account_agents(Some(&h), &legacy, Some(&only(&["agent:2"])));
        assert_eq!(out.claimed_ids, vec!["agent:2".to_string()]);
        let owned = load_account_agents(&h).custom_agents;
        assert_eq!(owned.len(), 1);
        assert_eq!(owned[0].id, "agent:2");
    }

    #[test]
    fn claiming_several_ids_reports_them_deterministically() {
        let h = fresh_agents("lc_multi");
        let legacy = vec![la("agent:1", "One"), la("agent:2", "Two")];
        let out =
            claim_legacy_account_agents(Some(&h), &legacy, Some(&only(&["agent:1", "agent:2"])));
        let mut got = out.claimed_ids.clone();
        got.sort();
        assert_eq!(got, vec!["agent:1".to_string(), "agent:2".to_string()]);
        assert_eq!(load_account_agents(&h).custom_agents.len(), 2);
    }

    // ── 6. fail-safe: never lose the only copy ─────────────────────────────

    #[test]
    fn a_failed_ownership_write_reports_nothing_claimed() {
        let h = hash_for("lc_write_fail");
        let path = account_path(&agents_base_dir(), "agents", &h);
        let _ = std::fs::remove_file(&path);
        // A directory where the account file belongs: the write cannot land.
        std::fs::create_dir_all(&path).unwrap();

        let legacy = vec![la("agent:only", "Only")];
        let out = claim_legacy_account_agents(Some(&h), &legacy, Some(&only(&["agent:only"])));

        assert!(
            out.claimed_ids.is_empty(),
            "must not report a claim whose write never landed, or the caller would \
             delete the legacy copy and lose the record entirely"
        );
        assert!(out.skipped_ids.is_empty());
        // Prove the ownership write really did not land by re-reading the account
        // store: no row for that id exists. THAT is what forces the caller to keep
        // the legacy copy.
        //
        // Deliberately NOT asserting on the input `legacy` slice: it is an
        // immutable local that nothing can mutate, so such an assertion could
        // never fail under any implementation and would prove nothing.
        let reloaded = load_account_agents(&h);
        assert!(
            reloaded.custom_agents.iter().all(|a| a.id != "agent:only"),
            "a failed ownership write must leave no account row behind"
        );

        let _ = std::fs::remove_dir_all(&path);
    }

    // ── 6b. delete durability: the tombstone report gates the shadow ────────
    //
    // Delete-time legacy removal runs only when the tombstone provably
    // persisted. These pin the boolean that gate reads.

    #[test]
    fn delete_account_agent_reports_true_only_when_the_tombstone_persists() {
        let h = fresh_agents("lc_delete_ok");
        save_account_agents(
            &h,
            &AccountAgentsStore {
                custom_agents: vec![ca("agent:1", "Owned")],
            },
        )
        .unwrap();
        assert!(delete_account_agent(&h, "agent:1"));
        assert!(load_account_agents(&h).custom_agents[0]
            .deleted_at
            .is_some());
        // Unknown id: nothing tombstoned, nothing reported.
        assert!(!delete_account_agent(&h, "agent:absent"));
        let _ = std::fs::remove_file(account_path(&agents_base_dir(), "agents", &h));
    }

    // The fault injection below relies on Windows semantics: making a file
    // read-only blocks `std::fs::rename` from replacing it, which is exactly how
    // the durable write fails. On Unix the read-only bit only blocks writing TO
    // the file, not replacing the directory entry, so the write would succeed and
    // the assertion would fail for the wrong reason.
    #[test]
    #[cfg(windows)]
    fn delete_account_agent_reports_false_when_the_tombstone_cannot_persist() {
        let h = fresh_agents("lc_delete_fail");
        save_account_agents(
            &h,
            &AccountAgentsStore {
                custom_agents: vec![ca("agent:1", "Owned")],
            },
        )
        .unwrap();
        let path = account_path(&agents_base_dir(), "agents", &h);
        let tmp = path.with_extension("json.tmp");
        // Read-only account file: the load succeeds (the row is there) but the
        // tombstone write cannot land. Windows-only semantics: renaming onto a
        // read-only file fails, which is exactly the failure under test.
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_readonly(true);
        std::fs::set_permissions(&path, perms).unwrap();

        let ok = delete_account_agent(&h, "agent:1");
        let tombstoned = load_account_agents(&h).custom_agents[0].deleted_at;

        // Restore FIRST so no read-only state leaks even if an assert fails.
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_readonly(false);
        std::fs::set_permissions(&path, perms).unwrap();
        let _ = std::fs::remove_file(&tmp);
        let _ = std::fs::remove_file(&path);

        assert!(
            !ok,
            "a tombstone that never landed must not report success: callers \
             drop the legacy shadow on success, which would lose the record"
        );
        assert!(
            tombstoned.is_none(),
            "no tombstone may be present without a persisted write"
        );
    }

    #[test]
    fn delete_account_style_reports_true_only_when_the_tombstone_persists() {
        let h = fresh_styles("lc_style_delete_ok");
        save_account_styles(
            &h,
            &AccountStylesStore {
                custom_styles: vec![cs("style:1", "Owned")],
            },
        )
        .unwrap();
        assert!(delete_account_style(&h, "style:1"));
        assert!(load_account_styles(&h).custom_styles[0]
            .deleted_at
            .is_some());
        assert!(!delete_account_style(&h, "style:absent"));
        let _ = std::fs::remove_file(account_path(&prompts_base_dir(), "styles", &h));
    }

    // Same Windows-only fault injection as the agent counterpart.
    #[test]
    #[cfg(windows)]
    fn delete_account_style_reports_false_when_the_tombstone_cannot_persist() {
        let h = fresh_styles("lc_style_delete_fail");
        save_account_styles(
            &h,
            &AccountStylesStore {
                custom_styles: vec![cs("style:1", "Owned")],
            },
        )
        .unwrap();
        let path = account_path(&prompts_base_dir(), "styles", &h);
        let tmp = path.with_extension("json.tmp");
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_readonly(true);
        std::fs::set_permissions(&path, perms).unwrap();

        let ok = delete_account_style(&h, "style:1");
        let tombstoned = load_account_styles(&h).custom_styles[0].deleted_at;

        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_readonly(false);
        std::fs::set_permissions(&path, perms).unwrap();
        let _ = std::fs::remove_file(&tmp);
        let _ = std::fs::remove_file(&path);

        assert!(
            !ok,
            "a tombstone that never landed must not report success"
        );
        assert!(
            tombstoned.is_none(),
            "no tombstone may be present without a persisted write"
        );
    }

    // ── 7. Styles parity ───────────────────────────────────────────────────

    #[test]
    fn style_claim_grants_the_same_capabilities_as_agents() {
        let h = fresh_styles("lc_style_claim");
        let legacy = vec![ls("style:1", "Legacy")];

        let before = styles_snap_for(&h, &legacy);
        assert_eq!(before.admission_of(&before.union[0]), Admission::Unassigned);
        assert!(!before.is_runnable(&before.union[0]));
        assert!(!before.admission_of(&before.union[0]).is_syncable());

        let out = claim_legacy_account_styles(Some(&h), &legacy, Some(&only(&["style:1"])));
        assert_eq!(out.claimed_ids, vec!["style:1".to_string()]);

        let after = styles_snap_for(&h, &legacy);
        let rec = &after.union[0];
        assert_eq!(after.admission_of(rec), Admission::Owned);
        assert!(after.is_runnable(rec));
        assert!(after.is_known("style:1"));
        assert!(after.admission_of(rec).is_syncable());
        assert_eq!(after.resolve_hint("style:1").as_deref(), Some("h-Legacy"));

        let row = &load_account_styles(&h).custom_styles[0];
        assert!(row.dirty && row.sync_id.is_none() && row.updated_at.is_none());
    }

    #[test]
    fn style_claim_refuses_signed_out_and_collision_and_stays_idempotent() {
        let h = fresh_styles("lc_style_safety");
        let legacy = vec![ls("style:1", "Legacy")];

        assert!(claim_legacy_account_styles(None, &legacy, None).is_empty());

        claim_legacy_account_styles(Some(&h), &legacy, Some(&only(&["style:1"])));
        let again = claim_legacy_account_styles(Some(&h), &legacy, Some(&only(&["style:1"])));
        assert!(again.claimed_ids.is_empty());
        assert_eq!(load_account_styles(&h).custom_styles.len(), 1);

        save_account_styles(
            &h,
            &AccountStylesStore {
                custom_styles: vec![cs("style:1", "Owned")],
            },
        )
        .unwrap();
        let skip = claim_legacy_account_styles(Some(&h), &legacy, Some(&only(&["style:1"])));
        assert_eq!(skip.skipped_ids, vec!["style:1".to_string()]);
        assert_eq!(load_account_styles(&h).custom_styles[0].name, "Owned");
    }

    #[test]
    fn style_delete_after_claim_tombstones() {
        let h = fresh_styles("lc_style_delete");
        let legacy = vec![ls("style:1", "Legacy")];
        claim_legacy_account_styles(Some(&h), &legacy, Some(&only(&["style:1"])));
        delete_account_style(&h, "style:1");
        assert!(load_account_styles(&h).custom_styles[0]
            .deleted_at
            .is_some());
    }

    // ── 9. delete still deletes (Windows counterpart of the Android test) ──

    /// The claim COPIES rather than moves, so the legacy row is deliberately left
    /// behind. If the DELETE path did not then remove that shadow, deleting a
    /// claimed agent would leave a device-local row that becomes visible and
    /// RUNNABLE again on sign-out — the user deletes an agent, signs out, and it
    /// is back.
    ///
    /// Mirrors Android's `deleting_a_claimed_agent_does_not_resurrect_it_on_sign_out`.
    /// The claim itself is exercised here so the test needs no Tauri `Window`; the
    /// removal under test is the same `retain` the routed delete performs right
    /// after writing the tombstone.
    #[test]
    fn deleting_a_claimed_agent_does_not_resurrect_it_on_sign_out() {
        let h = fresh_agents("lc_delete_resurrect");
        let legacy = vec![la("agent:1", "Legacy")];
        claim_legacy_account_agents(Some(&h), &legacy, Some(&only(&["agent:1"])));

        // Tombstone in the account store (what the routed delete writes first).
        delete_account_agent(&h, "agent:1");
        assert!(
            load_account_agents(&h).custom_agents[0]
                .deleted_at
                .is_some(),
            "precondition: the tombstone must be written"
        );

        // Then the shadow removal the delete path performs.
        let mut store = crate::agents::AgentsStore::default();
        store.custom_agents = legacy.clone();
        store.custom_agents.retain(|a| a.id != "agent:1");

        // Signed out: nothing may be runnable for that id, because the only place
        // it could have survived is the legacy store — and it is gone.
        assert!(store.custom_agents.is_empty(), "the shadow must be removed");
        let signed_out = VisibleAgents::from_union(union_agents(&store.custom_agents, &[]));
        assert!(
            !signed_out.is_known("agent:1"),
            "a deleted agent must not reappear once signed out"
        );
        assert!(signed_out.resolve(Some("agent:1")).is_builtin);
    }

    // ── 10. the board projection advertises exactly what can be claimed ────

    #[test]
    fn agents_view_marks_only_signed_in_unclaimed_rows_claimable() {
        let h = fresh_agents("lc_view");
        let legacy = vec![la("agent:1", "Legacy")];
        claim_legacy_account_agents(Some(&h), &legacy, Some(&only(&["agent:1"])));
        save_account_agents(
            &h,
            &AccountAgentsStore {
                custom_agents: vec![ca("agent:2", "Owned")],
            },
        )
        .unwrap();

        let view = snap_for(&h, &legacy).into_view(crate::agents::ID_BUILT_IN.to_string());
        let flags: Vec<(&str, bool)> = view
            .custom_agents
            .iter()
            .map(|v| (v.id.as_str(), v.claimable))
            .collect();
        assert_eq!(
            flags,
            vec![("agent:2", false), ("agent:1", true)],
            "owned row not claimable; unclaimed row claimable"
        );
    }

    #[test]
    fn styles_view_lists_unclaimed_rows_so_a_claim_is_possible() {
        let h = fresh_styles("lc_style_view");
        let legacy = vec![ls("style:1", "Legacy")];
        claim_legacy_account_styles(Some(&h), &legacy, Some(&only(&["style:1"])));

        let view = styles_snap_for(&h, &legacy).into_view_list();
        assert_eq!(view.len(), 1);
        assert!(!view[0].claimable, "already claimed");

        // Unclaimed and signed in: listed AND claimable.
        let h2 = fresh_styles("lc_style_view2");
        let v2 = styles_snap_for(&h2, &legacy).into_view_list();
        assert_eq!(v2.len(), 1);
        assert!(v2[0].claimable);

        // Signed out: still listed (never looks deleted) but not claimable.
        let signed_out = VisibleStyles::from_union_with_account(union_styles(&legacy, &[]), None);
        let v3 = signed_out.into_view_list();
        assert_eq!(v3.len(), 1);
        assert!(!v3[0].claimable);
    }

    // ── 9. adversarial: the claim-vs-writer race ────────────────────────────
    //
    // The hand-rolled stand-in that lived here proved only that the claim takes
    // a lock — deleting the production stamper's lock left it green. The real
    // coverage now drives production writers and lives in `sync::stores` (which
    // alone can take `store_test_guard`):
    // `claim_survives_concurrent_real_stamp_writes`,
    // `style_claim_survives_concurrent_real_stamp_writes`,
    // `the_real_account_merge_writer_contends_for_the_claim_lock`,
    // `a_stale_real_merge_that_drops_a_claim_leaves_it_recoverable`.

    /// The invariant in one assertion: after a claim attempt, the record exists
    /// in the account store OR the legacy store — never neither.
    #[test]
    fn a_record_is_never_absent_from_both_stores() {
        let h = fresh_agents("lc_never_neither");
        let legacy = vec![la("agent:1", "Legacy")];

        // Normal claim: owned.
        let out = claim_legacy_account_agents(Some(&h), &legacy, Some(&only(&["agent:1"])));
        assert_eq!(out.claimed_ids, vec!["agent:1".to_string()]);
        assert!(load_account_agents(&h)
            .custom_agents
            .iter()
            .any(|a| a.id == "agent:1"));

        // Failed ownership write: not owned, and the caller therefore keeps the
        // legacy copy, so the record still exists.
        let h2 = fresh_agents("lc_never_neither2");
        let path = account_path(&agents_base_dir(), "agents", &h2);
        let _ = std::fs::remove_file(&path);
        std::fs::create_dir_all(&path).unwrap();
        let out2 = claim_legacy_account_agents(Some(&h2), &legacy, Some(&only(&["agent:1"])));
        assert!(out2.claimed_ids.is_empty());
        let in_account2 = load_account_agents(&h2)
            .custom_agents
            .iter()
            .any(|a| a.id == "agent:1");
        let in_legacy2 = legacy.iter().any(|l| l.id == "agent:1");
        assert!(
            in_account2 || in_legacy2,
            "record vanished from both stores"
        );
        let _ = std::fs::remove_dir_all(&path);
    }

    // ── 10. tombstone policy: DELETE wins, deterministically ──────────────

    #[test]
    fn a_tombstoned_account_record_refuses_the_claim_and_reports_it() {
        let h = fresh_agents("lc_tombstone");
        save_account_agents(
            &h,
            &AccountAgentsStore {
                custom_agents: vec![AccountAgent {
                    deleted_at: Some(1_700_000_000_000),
                    ..ca("agent:1", "Deleted")
                }],
            },
        )
        .unwrap();
        let legacy = vec![la("agent:1", "Legacy")];

        let out = claim_legacy_account_agents(Some(&h), &legacy, Some(&only(&["agent:1"])));
        assert!(
            out.claimed_ids.is_empty(),
            "a delete must never be resurrected"
        );
        assert!(
            out.refused_ids
                .iter()
                .any(|(id, why)| id == "agent:1" && *why == "tombstoned"),
            "the refusal must be explicit and reported, got {:?}",
            out.refused_ids
        );
        // Not reported as "already yours" — that would be a different lie.
        assert!(out.skipped_ids.is_empty());
        // The account row keeps its tombstone: wire semantics untouched.
        assert!(load_account_agents(&h).custom_agents[0]
            .deleted_at
            .is_some());
    }

    #[test]
    fn style_tombstone_refuses_the_claim_too() {
        let h = fresh_styles("lc_style_tombstone");
        save_account_styles(
            &h,
            &AccountStylesStore {
                custom_styles: vec![AccountStyle {
                    deleted_at: Some(1_700_000_000_000),
                    ..cs("style:1", "Deleted")
                }],
            },
        )
        .unwrap();
        let out = claim_legacy_account_styles(
            Some(&h),
            &[ls("style:1", "Legacy")],
            Some(&only(&["style:1"])),
        );
        assert!(out.claimed_ids.is_empty());
        assert!(out
            .refused_ids
            .iter()
            .any(|(id, why)| id == "style:1" && *why == "tombstoned"));
    }

    // ── 11. duplicate legacy ids: refuse, never destroy ───────────────────

    #[test]
    fn duplicate_legacy_ids_are_refused_rather_than_one_silently_destroyed() {
        let h = fresh_agents("lc_dupe");
        // Two rows, same id, DIFFERENT content — only reachable via a hand-edited
        // file. Cleanup deletes by id, so claiming would delete both while
        // reporting one adopted.
        let legacy = vec![la("agent:1", "Version A"), la("agent:1", "Version B")];

        let out = claim_legacy_account_agents(Some(&h), &legacy, Some(&only(&["agent:1"])));
        assert!(out.claimed_ids.is_empty());
        assert_eq!(
            out.refused_ids,
            vec![("agent:1".to_string(), "duplicate-legacy-id")],
            "exactly one refusal per id, even though two rows share it"
        );
        assert!(load_account_agents(&h).custom_agents.is_empty());
        // Both legacy rows are still there — nothing destroyed.
        assert_eq!(legacy.iter().filter(|l| l.id == "agent:1").count(), 2);
    }

    #[test]
    fn duplicate_legacy_ids_do_not_block_a_different_id() {
        let h = fresh_agents("lc_dupe_other");
        let legacy = vec![
            la("agent:1", "Version A"),
            la("agent:1", "Version B"),
            la("agent:2", "Fine"),
        ];
        let out = claim_legacy_account_agents(Some(&h), &legacy, Some(&only(&["agent:2"])));
        assert_eq!(out.claimed_ids, vec!["agent:2".to_string()]);
        assert!(out.refused_ids.is_empty());
    }

    // ── 12. builtin ids: refused on every path ────────────────────────────

    #[test]
    fn builtin_ids_are_refused_on_the_agent_and_style_paths() {
        let h = fresh_agents("lc_builtin_agent");
        let out = claim_legacy_account_agents(
            Some(&h),
            &[la(crate::agents::ID_BUILT_IN, "Builtin")],
            Some(&only(&[crate::agents::ID_BUILT_IN])),
        );
        assert!(
            out.claimed_ids.is_empty(),
            "a reserved id must never be ownable"
        );
        assert!(out
            .refused_ids
            .iter()
            .any(|(id, why)| id == crate::agents::ID_BUILT_IN && *why == "builtin-id"));
        assert!(load_account_agents(&h).custom_agents.is_empty());

        let hs = fresh_styles("lc_builtin_style");
        let bid = crate::prompts::ID_PROOFREAD;
        let outs =
            claim_legacy_account_styles(Some(&hs), &[ls(bid, "Proofread")], Some(&only(&[bid])));
        assert!(outs.claimed_ids.is_empty());
        assert!(outs
            .refused_ids
            .iter()
            .any(|(id, why)| id == bid && *why == "builtin-id"));
        assert!(load_account_styles(&hs).custom_styles.is_empty());
    }

    #[test]
    fn is_empty_accounts_for_every_refusal_reason() {
        // The UI relies on `is_empty` to distinguish "nothing happened" from a
        // real outcome, so a refusal must count as a non-empty result.
        let h = fresh_agents("lc_empty_reason");
        save_account_agents(
            &h,
            &AccountAgentsStore {
                custom_agents: vec![AccountAgent {
                    deleted_at: Some(1),
                    ..ca("agent:1", "Gone")
                }],
            },
        )
        .unwrap();
        let out = claim_legacy_account_agents(
            Some(&h),
            &[
                la("agent:1", "Legacy"),
                la(crate::agents::ID_BUILT_IN, "Builtin"),
            ],
            None,
        );
        assert!(!out.is_empty());
        assert!(out.claimed_ids.is_empty());
    }
}
