// Fluence sync — pure reconciliation engine (spec §7–§19).
//
// Platform-neutral: no Drive HTTP, no OAuth, no scheduler, no UI. All local
// persistence goes through `LocalStore`, all remote I/O through `DriveStore`,
// token liveness through `TokenProvider`. The engine never sees tokens or
// transcript-adjacent logging.
//
// Stage order per spec §7: PREFLIGHT → LIST → VALIDATE → GROUP → RECONCILE →
// PUSH → APPLY → FINALIZE. Absence per §10 (one fresh re-list, confirmed
// absence → re-upload current state), tombstone-wins per §11, quarantine latch
// per §12, account namespace per §13, hard-delete of never-uploaded deleted
// rows per §14, `sync_state` only via the §6 table with a debug assertion that
// persisted equals derived after every pass.

use std::collections::{BTreeMap, BTreeSet};

use crate::sync::wire::{self, InvalidReason, RecordContent, RecordType, WireRecord};

pub const SYNC_STATE_LOCAL: &str = "local";
pub const SYNC_STATE_CLEAN: &str = "clean";
pub const SYNC_STATE_DIRTY: &str = "dirty";
pub const SYNC_STATE_QUARANTINED: &str = "quarantined";

/// A local row as seen by the sync engine (spec §5, Windows columns; §30
/// adds the record kind and its content fields).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalRow {
    pub uuid: String,
    pub timestamp_ms: i64,
    pub text: String,
    pub mode: String,
    pub duration_ms: i64,
    pub provider: String,
    pub model: Option<String>,
    pub language: Option<String>,
    // §30 kind fields.
    pub rtype: RecordType,
    pub spoken: Option<String>,
    pub corrected: Option<String>,
    pub kind: Option<String>,
    pub trigger: Option<String>,
    pub expansion: Option<String>,
    pub settings_key: Option<String>,
    pub settings_value: Option<String>,
    pub deleted_at: Option<i64>,
    pub server_file_id: Option<String>,
    pub sync_account: Option<String>,
    pub sync_state: String,
    pub quarantine_reason: Option<String>,
}

impl LocalRow {
    pub fn content(&self) -> RecordContent {
        match self.rtype {
            RecordType::History => RecordContent::History(wire::ContentTuple {
                created_at: self.timestamp_ms,
                text: self.text.clone(),
                mode: self.mode.clone(),
                duration_ms: self.duration_ms,
                provider: self.provider.clone(),
                model: self.model.clone(),
                language: self.language.clone(),
            }),
            RecordType::Dictionary => RecordContent::Dictionary(wire::DictionaryTuple {
                created_at: self.timestamp_ms,
                spoken: self.spoken.clone().unwrap_or_default(),
                corrected: self.corrected.clone().unwrap_or_default(),
                kind: self.kind.clone().unwrap_or_default(),
            }),
            RecordType::Snippet => RecordContent::Snippet(wire::SnippetTuple {
                created_at: self.timestamp_ms,
                trigger: self.trigger.clone().unwrap_or_default(),
                expansion: self.expansion.clone().unwrap_or_default(),
            }),
            RecordType::Settings => RecordContent::Settings(wire::SettingsTuple {
                created_at: self.timestamp_ms,
                key: self.settings_key.clone().unwrap_or_default(),
                value: self.settings_value.clone().unwrap_or_default(),
            }),
        }
    }

    pub fn is_tombstoned(&self) -> bool {
        self.deleted_at.is_some()
    }

    pub fn is_latched(&self) -> bool {
        self.quarantine_reason.is_some()
    }

    pub fn to_wire(&self) -> WireRecord {
        WireRecord {
            v: 1,
            id: self.uuid.clone(),
            created_at: self.timestamp_ms,
            deleted_at: self.deleted_at,
            rtype: self.rtype,
            text: self.text.clone(),
            mode: self.mode.clone(),
            duration_ms: self.duration_ms,
            provider: self.provider.clone(),
            model: self.model.clone(),
            language: self.language.clone(),
            spoken: self.spoken.clone(),
            corrected: self.corrected.clone(),
            kind: self.kind.clone(),
            trigger: self.trigger.clone(),
            expansion: self.expansion.clone(),
            settings_key: self.settings_key.clone(),
            settings_value: self.settings_value.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileMeta {
    pub file_id: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupedFile {
    pub file_id: String,
    pub name: String,
    pub record: WireRecord,
}

#[derive(Debug, Clone, Default)]
struct Group {
    files: Vec<GroupedFile>,
    invalid: Vec<(String, InvalidReason)>,
}

/// Per-UUID group verdict (spec §9).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupVerdict {
    Absent,
    HealthyLive,
    HealthyDeleted,
    Divergent,
}

/// Why a group is latched (spec §12). Persisted via `as_str()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuarantineReason {
    ContentDeviation,
    CorruptFile,
    UnknownSchemaVersion,
    IdNameMismatch,
    UnknownType,
    Collision,
}

impl QuarantineReason {
    pub fn as_str(self) -> &'static str {
        match self {
            QuarantineReason::ContentDeviation => "content_deviation",
            QuarantineReason::CorruptFile => "corrupt_file",
            QuarantineReason::UnknownSchemaVersion => "unknown_schema_version",
            QuarantineReason::IdNameMismatch => "id_name_mismatch",
            QuarantineReason::UnknownType => "unknown_type",
            QuarantineReason::Collision => "collision",
        }
    }
}

/// A per-row unit of work computed in RECONCILE and consumed in APPLY/PUSH.
#[derive(Debug, Clone)]
pub enum SyncAction {
    ImportLive {
        row: LocalRow,
    },
    ImportTombstone {
        row: LocalRow,
    },
    ImportQuarantined {
        row: LocalRow,
    },
    MarkTombstoned {
        uuid: String,
        deleted_at: i64,
    },
    Quarantine {
        uuid: String,
        reason: QuarantineReason,
    },
    HardDelete {
        uuid: String,
    },
    Create {
        uuid: String,
        record: WireRecord,
    },
    Reupload {
        uuid: String,
        record: WireRecord,
    },
    PatchTombstone {
        file_id: String,
        record: WireRecord,
    },
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SyncOutcome {
    pub imported: usize,
    pub created: usize,
    pub reuploaded: usize,
    pub patches: usize,
    pub tombstoned_local: usize,
    pub quarantined: usize,
    pub hard_deleted: usize,
    pub retryable_failures: usize,
}

#[derive(Debug)]
pub enum SyncError {
    Retryable(String),
    Fatal(String),
    /// 401 from the token holder — the caller must reauthenticate (§23).
    AuthRequired,
    /// 403 under the `drive.file` scope — file/folder not ours; skip, never
    /// retry-bomb (§23).
    NotOurs,
}

impl std::fmt::Display for SyncError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SyncError::Retryable(m) | SyncError::Fatal(m) => write!(f, "{m}"),
            SyncError::AuthRequired => write!(f, "authentication required"),
            SyncError::NotOurs => write!(f, "file not owned by this app (403 drive.file)"),
        }
    }
}

impl std::error::Error for SyncError {}

/// Local persistence seam (spec §7). Every method that touches more than one
/// column commits in one transaction in the real store.
pub trait LocalStore {
    /// Rows stamped `null` or `account` (spec §13) — the store filters.
    fn list_rows(&self, account: Option<&str>) -> Vec<LocalRow>;
    /// Unfiltered lookup by UUID; the engine checks stamps itself before import.
    fn find_row(&self, uuid: &str) -> Option<LocalRow>;
    /// Upsert by UUID (spec §7 `import`).
    fn import(&mut self, row: LocalRow) -> Result<(), SyncError>;
    /// Set `deleted_at` + `sync_state='dirty'` (one tx).
    fn mark_tombstoned(&mut self, uuid: &str, deleted_at: i64) -> Result<(), SyncError>;
    fn set_server_file_id(&mut self, uuid: &str, file_id: &str) -> Result<(), SyncError>;
    fn set_sync_state(&mut self, uuid: &str, state: &str) -> Result<(), SyncError>;
    /// Set `quarantine_reason` + `sync_state='quarantined'` (one tx).
    fn quarantine(&mut self, uuid: &str, reason: QuarantineReason) -> Result<(), SyncError>;
    /// User resolution only — never called by the engine (spec §12).
    fn clear_quarantine(&mut self, uuid: &str) -> Result<(), SyncError>;
    fn hard_delete(&mut self, uuid: &str) -> Result<(), SyncError>;
}

/// Remote Drive seam (spec §7, §23). `update_content` is tombstone-media only.
pub trait DriveStore {
    fn find_or_create_folder(&mut self) -> Result<(), SyncError>;
    /// Full listing, `trashed=false`, paginated to exhaustion by the store.
    fn list_files(&mut self) -> Result<Vec<FileMeta>, SyncError>;
    /// `Ok(None)` = fetch-404 (file vanished after list) → dropped from group.
    fn get_content(&mut self, file_id: &str) -> Result<Option<Vec<u8>>, SyncError>;
    /// Create `<uuid>.json` with the exact record; returns the new file id.
    fn create_file(&mut self, name: &str, record: &WireRecord) -> Result<String, SyncError>;
    fn update_content(&mut self, file_id: &str, record: &WireRecord) -> Result<(), SyncError>;
}

/// Token liveness only; the engine never sees tokens (spec §24).
pub trait TokenProvider {
    fn has_valid_token(&mut self) -> bool;
}

/// Run one full sync pass for `account` and one record `kind` (§30.1). Rows
/// of other kinds and groups of other kinds are inert for the pass. Pure
/// reconciliation — all side effects go through the traits. Stage failures
/// before PUSH abort the pass with nothing mutated; per-op PUSH failures are
/// counted in `outcome.retryable_failures` and the row is left unchanged for
/// next pass.
pub fn run(
    kind: RecordType,
    account: Option<&str>,
    local: &mut impl LocalStore,
    drive: &mut impl DriveStore,
    token: &mut impl TokenProvider,
) -> Result<SyncOutcome, SyncError> {
    // PREFLIGHT — no token → skip the whole pass, nothing mutated.
    if !token.has_valid_token() {
        return Err(SyncError::AuthRequired);
    }
    let mut outcome = SyncOutcome::default();

    // LIST — folder creation is the only side effect; failures abort the pass.
    drive.find_or_create_folder()?;
    let listing = drive.list_files()?;
    let listed_names: BTreeMap<String, String> = listing
        .iter()
        .map(|f| (f.file_id.clone(), f.name.clone()))
        .collect();

    // VALIDATE — fetch content; fetch-404 drops the file from its group this
    // pass (spec §7); non-UUID names are inert and never fetched (spec §15).
    let mut groups: BTreeMap<String, Group> = BTreeMap::new();
    for file in listing {
        let Some(uuid) = wire::uuid_basename(&file.name) else {
            continue;
        };
        match drive.get_content(&file.file_id)? {
            None => {}
            Some(bytes) => match wire::parse(&bytes, uuid) {
                Ok(record) => groups
                    .entry(uuid.to_string())
                    .or_default()
                    .files
                    .push(GroupedFile {
                        file_id: file.file_id,
                        name: file.name,
                        record,
                    }),
                Err(reason) => groups
                    .entry(uuid.to_string())
                    .or_default()
                    .invalid
                    .push((file.file_id, reason)),
            },
        }
    }
    for group in groups.values_mut() {
        group.files.sort_by(|a, b| a.file_id.cmp(&b.file_id));
        group.invalid.sort_by(|a, b| a.0.cmp(&b.0));
    }

    // RECONCILE — local rows, deterministic order, one tx per row later.
    let mut rows = local.list_rows(account);
    rows.sort_by(|a, b| a.uuid.cmp(&b.uuid));
    let known: BTreeSet<&str> = rows.iter().map(|r| r.uuid.as_str()).collect();

    let mut local_actions: Vec<SyncAction> = Vec::new();
    let mut push_actions: Vec<SyncAction> = Vec::new();

    for row in &rows {
        if row.rtype != kind {
            continue; // §30: other kinds are inert for this pass
        }
        if row.is_latched() {
            continue; // §12: latched groups are skipped entirely
        }
        match groups.get(&row.uuid) {
            None => {
                // Absent group (§10). Absence never deletes or tombstones.
                if row.is_tombstoned() {
                    if row.server_file_id.is_none() {
                        // §14: never-uploaded deleted row, provably safe.
                        local_actions.push(SyncAction::HardDelete {
                            uuid: row.uuid.clone(),
                        });
                    }
                } else if row.server_file_id.is_none() && row.sync_account.is_none() {
                    // Fresh local record: first upload (exact T).
                    push_actions.push(SyncAction::Create {
                        uuid: row.uuid.clone(),
                        record: row.to_wire(),
                    });
                }
                // Imported live rows (stamp set, no sfi) and rows whose file
                // vanished with a stale sfi are handled by the absence stage.
            }
            Some(g) => match classify(g, Some(row)) {
                GroupVerdict::HealthyLive => {
                    if row.is_tombstoned() {
                        // Propagation: tombstone every listed live file (§11).
                        push_actions.extend(patch_live_files(g, &row.to_wire()));
                    } else if row.server_file_id.is_none() && row.sync_account.is_none() {
                        // Retry after an uncommitted create → duplicate-identical
                        // is harmless (§17) — coexist, one row.
                        push_actions.push(SyncAction::Create {
                            uuid: row.uuid.clone(),
                            record: row.to_wire(),
                        });
                    }
                }
                GroupVerdict::HealthyDeleted => {
                    let deleted_at = group_deleted_at(g);
                    if row.is_tombstoned() {
                        push_actions.extend(patch_live_files(g, &row.to_wire()));
                    } else {
                        // Tombstone-wins: row becomes a tombstone in RECONCILE,
                        // before PUSH; never uploaded live afterward (§11).
                        let tombstone = wire::tombstone(&row.to_wire(), deleted_at);
                        local_actions.push(SyncAction::MarkTombstoned {
                            uuid: row.uuid.clone(),
                            deleted_at,
                        });
                        push_actions.extend(patch_live_files(g, &tombstone));
                    }
                }
                GroupVerdict::Divergent => {
                    local_actions.push(SyncAction::Quarantine {
                        uuid: row.uuid.clone(),
                        reason: divergent_reason(g, true),
                    });
                }
                GroupVerdict::Absent => {}
            },
        }
    }

    // RECONCILE — unknown-UUID groups (imports). Foreign stamps are checked
    // against the unfiltered `find_row` so they stay byte-untouched (§13).
    for (uuid, group) in groups.iter() {
        if known.contains(uuid.as_str()) {
            continue;
        }
        if local.find_row(uuid).is_some() {
            continue; // existing row invisible to this pass: byte-untouched
        }
        if group.files.first().is_some_and(|f| f.record.rtype != kind) {
            continue; // §30: groups of other kinds are inert for this pass
        }
        match classify(group, None) {
            GroupVerdict::HealthyLive => {
                let record = &group.files[0].record;
                local_actions.push(SyncAction::ImportLive {
                    row: import_live_row(record, account),
                });
            }
            GroupVerdict::HealthyDeleted => {
                let record = &group.files[0].record;
                let row = import_tombstone_row(record, group_deleted_at(group), account);
                // The imported holder propagates the tombstone same pass (§11).
                push_actions.extend(patch_live_files(group, &row.to_wire()));
                local_actions.push(SyncAction::ImportTombstone { row });
            }
            GroupVerdict::Divergent => {
                let reason = divergent_reason(group, false);
                local_actions.push(SyncAction::ImportQuarantined {
                    row: import_placeholder_row(uuid, kind, account, reason),
                });
            }
            GroupVerdict::Absent => {}
        }
    }

    // APPLY — local commits, one tx per row in the real store.
    for action in local_actions {
        match action {
            SyncAction::ImportLive { row }
            | SyncAction::ImportTombstone { row }
            | SyncAction::ImportQuarantined { row } => {
                local.import(row)?;
                outcome.imported += 1;
            }
            SyncAction::MarkTombstoned { uuid, deleted_at } => {
                local.mark_tombstoned(&uuid, deleted_at)?;
                outcome.tombstoned_local += 1;
            }
            SyncAction::Quarantine { uuid, reason } => {
                local.quarantine(&uuid, reason)?;
                outcome.quarantined += 1;
            }
            SyncAction::HardDelete { uuid } => {
                local.hard_delete(&uuid)?;
                outcome.hard_deleted += 1;
            }
            _ => unreachable!("push actions are not applied in the local phase"),
        }
    }

    // ABSENCE — rows whose server_file_id is not listed under the canonical
    // `<uuid>.json` name become candidates; ONE fresh re-list confirms (§10).
    // Renamed and trashed files count as absent (§15). Never deletes, never
    // tombstones; re-upload reproduces the exact current state.
    let mut reuploads: Vec<SyncAction> = Vec::new();
    if let Some(candidates) = absence_candidates(kind, &rows, &listed_names) {
        let relist = drive.list_files()?;
        let relisted: BTreeMap<String, String> = relist
            .iter()
            .map(|f| (f.file_id.clone(), f.name.clone()))
            .collect();
        for row in candidates {
            if is_absent(&row, &relisted) {
                reuploads.push(SyncAction::Reupload {
                    uuid: row.uuid.clone(),
                    record: row.to_wire(),
                });
            }
        }
    }

    // PUSH — Drive writes, each independent; on failure the row is left
    // unchanged, the failure is counted, and the pass continues (§7).
    let mut reuploaded_uuids: BTreeSet<String> = BTreeSet::new();
    let mut patched_file_ids: BTreeSet<String> = BTreeSet::new();
    for action in push_actions.into_iter().chain(reuploads) {
        match action {
            SyncAction::Create { uuid, record } => {
                match drive.create_file(&file_name(&uuid), &record) {
                    Ok(file_id) => {
                        local.set_server_file_id(&uuid, &file_id)?;
                        local.set_sync_state(&uuid, SYNC_STATE_CLEAN)?;
                        outcome.created += 1;
                    }
                    Err(e) => match e {
                        // Retryable: row unchanged, counted, pass continues (§7).
                        SyncError::Retryable(_) => outcome.retryable_failures += 1,
                        // Fatal/AuthRequired/NotOurs: abort the pass, surface (§23).
                        other => return Err(other),
                    },
                }
            }
            SyncAction::Reupload { uuid, record } => {
                match drive.create_file(&file_name(&uuid), &record) {
                    Ok(file_id) => {
                        local.set_server_file_id(&uuid, &file_id)?;
                        local.set_sync_state(&uuid, SYNC_STATE_CLEAN)?;
                        reuploaded_uuids.insert(uuid);
                        outcome.reuploaded += 1;
                    }
                    Err(e) => match e {
                        // Retryable: row unchanged, counted, pass continues (§7).
                        SyncError::Retryable(_) => outcome.retryable_failures += 1,
                        // Fatal/AuthRequired/NotOurs: abort the pass, surface (§23).
                        other => return Err(other),
                    },
                }
            }
            SyncAction::PatchTombstone { file_id, record } => {
                match drive.update_content(&file_id, &record) {
                    Ok(()) => {
                        patched_file_ids.insert(file_id);
                        outcome.patches += 1;
                    }
                    Err(e) => match e {
                        // Retryable: row unchanged, counted, pass continues (§7).
                        SyncError::Retryable(_) => outcome.retryable_failures += 1,
                        // Fatal/AuthRequired/NotOurs: abort the pass, surface (§23).
                        other => return Err(other),
                    },
                }
            }
            _ => unreachable!("local actions never reach PUSH"),
        }
    }

    // FINALIZE — `sync_state` only via the §6 table. The pass's own transitions
    // (e.g. tombstone-wins dirty → clean once fully propagated) are committed
    // here; only then is the end-of-pass invariant asserted in debug builds.
    let mut fixups: Vec<(String, &'static str)> = Vec::new();
    for row in local.list_rows(account) {
        let group = groups.get(&row.uuid);
        let verdict = match group {
            Some(g) => classify(g, Some(&row)),
            None => GroupVerdict::Absent,
        };
        let derived = derived_sync_state(
            &row,
            verdict,
            group_fully_tombstoned(&row, group, &reuploaded_uuids, &patched_file_ids),
        );
        if row.sync_state != derived {
            fixups.push((row.uuid.clone(), derived));
        }
    }
    for (uuid, state) in &fixups {
        local.set_sync_state(uuid, state)?;
    }
    if cfg!(debug_assertions) {
        for row in local.list_rows(account) {
            let group = groups.get(&row.uuid);
            let verdict = match group {
                Some(g) => classify(g, Some(&row)),
                None => GroupVerdict::Absent,
            };
            let derived = derived_sync_state(
                &row,
                verdict,
                group_fully_tombstoned(&row, group, &reuploaded_uuids, &patched_file_ids),
            );
            debug_assert_eq!(
                row.sync_state, derived,
                "sync_state invariant violated for row {}",
                row.uuid
            );
        }
    }

    Ok(outcome)
}

fn absence_candidates(
    kind: RecordType,
    rows: &[LocalRow],
    listed_names: &BTreeMap<String, String>,
) -> Option<Vec<LocalRow>> {
    let candidates: Vec<LocalRow> = rows
        .iter()
        .filter(|r| r.rtype == kind)
        .filter(|r| !r.is_latched())
        .filter(|r| is_absent(r, listed_names))
        .cloned()
        .collect();
    if candidates.is_empty() {
        None
    } else {
        Some(candidates)
    }
}

/// A row's file is absent when its `server_file_id` is not in the listing
/// under the canonical `<uuid>.json` name (spec §10, §15 — renamed and
/// trashed files count as absent). Files with non-UUID names never anchor
/// presence.
fn is_absent(row: &LocalRow, listed_names: &BTreeMap<String, String>) -> bool {
    match row.server_file_id.as_deref() {
        None => false,
        Some(sfi) => match listed_names.get(sfi) {
            None => true,
            Some(name) => name != &file_name(&row.uuid),
        },
    }
}

fn classify(group: &Group, row: Option<&LocalRow>) -> GroupVerdict {
    if !group.invalid.is_empty() {
        return GroupVerdict::Divergent;
    }
    let t0 = group.files[0].record.content();
    if group
        .files
        .iter()
        .skip(1)
        .any(|f| !wire::tuples_equal(&f.record.content(), &t0))
    {
        return GroupVerdict::Divergent;
    }
    if let Some(r) = row {
        if !wire::tuples_equal(&r.content(), &t0) {
            return GroupVerdict::Divergent;
        }
    }
    if group.files.iter().any(|f| f.record.deleted_at.is_some()) {
        GroupVerdict::HealthyDeleted
    } else {
        GroupVerdict::HealthyLive
    }
}

fn divergent_reason(group: &Group, has_row: bool) -> QuarantineReason {
    if let Some((_, reason)) = group.invalid.first() {
        return quarantine_reason_of(*reason);
    }
    let t0 = group.files[0].record.content();
    let files_disagree = group
        .files
        .iter()
        .skip(1)
        .any(|f| !wire::tuples_equal(&f.record.content(), &t0));
    if files_disagree {
        QuarantineReason::Collision
    } else if has_row {
        QuarantineReason::ContentDeviation
    } else {
        QuarantineReason::Collision
    }
}

fn quarantine_reason_of(reason: InvalidReason) -> QuarantineReason {
    match reason {
        InvalidReason::MalformedJson
        | InvalidReason::BadTimestamp
        | InvalidReason::BadMode
        | InvalidReason::NonIntegral
        | InvalidReason::MissingTypeField
        | InvalidReason::BadKind => QuarantineReason::CorruptFile,
        InvalidReason::UnknownSchemaVersion => QuarantineReason::UnknownSchemaVersion,
        InvalidReason::IdNameMismatch => QuarantineReason::IdNameMismatch,
        InvalidReason::UnknownType => QuarantineReason::UnknownType,
    }
}

fn group_deleted_at(group: &Group) -> i64 {
    group
        .files
        .iter()
        .find_map(|f| f.record.deleted_at)
        .expect("HealthyDeleted verdict implies a tombstoned file")
}

fn patch_live_files(group: &Group, record: &WireRecord) -> Vec<SyncAction> {
    group
        .files
        .iter()
        .filter(|f| f.record.deleted_at.is_none())
        .map(|f| SyncAction::PatchTombstone {
            file_id: f.file_id.clone(),
            record: record.clone(),
        })
        .collect()
}

fn file_name(uuid: &str) -> String {
    format!("{uuid}.json")
}

fn import_live_row(record: &WireRecord, account: Option<&str>) -> LocalRow {
    LocalRow {
        uuid: record.id.clone(),
        timestamp_ms: record.created_at,
        text: record.text.clone(),
        mode: record.mode.clone(),
        duration_ms: record.duration_ms,
        provider: record.provider.clone(),
        model: record.model.clone(),
        language: record.language.clone(),
        rtype: record.rtype,
        spoken: record.spoken.clone(),
        corrected: record.corrected.clone(),
        kind: record.kind.clone(),
        trigger: record.trigger.clone(),
        expansion: record.expansion.clone(),
        settings_key: record.settings_key.clone(),
        settings_value: record.settings_value.clone(),
        deleted_at: None,
        server_file_id: None,
        sync_account: account.map(str::to_string),
        sync_state: SYNC_STATE_LOCAL.to_string(),
        quarantine_reason: None,
    }
}

fn import_tombstone_row(record: &WireRecord, deleted_at: i64, account: Option<&str>) -> LocalRow {
    let mut row = import_live_row(record, account);
    row.deleted_at = Some(deleted_at);
    row.sync_state = SYNC_STATE_CLEAN.to_string();
    row
}

/// Placeholder for a DIVERGENT group with no local row (spec §12): no user
/// content, stamped active, latched with the group's reason. Overwritten by a
/// later import once the user resolves the group.
fn import_placeholder_row(
    uuid: &str,
    kind: RecordType,
    account: Option<&str>,
    reason: QuarantineReason,
) -> LocalRow {
    LocalRow {
        uuid: uuid.to_string(),
        timestamp_ms: 0,
        text: String::new(),
        mode: String::new(),
        duration_ms: 0,
        provider: String::new(),
        model: None,
        language: None,
        rtype: kind,
        spoken: None,
        corrected: None,
        kind: None,
        trigger: None,
        expansion: None,
        settings_key: None,
        settings_value: None,
        deleted_at: None,
        server_file_id: None,
        sync_account: account.map(str::to_string),
        sync_state: SYNC_STATE_QUARANTINED.to_string(),
        quarantine_reason: Some(reason.as_str().to_string()),
    }
}

/// The `sync_state` derived from `(deleted_at, server_file_id,
/// quarantine_reason, group verdict)` — the only transitions the §6 table
/// allows. Asserted equal to the persisted value at FINALIZE.
pub fn derived_sync_state(
    row: &LocalRow,
    verdict: GroupVerdict,
    group_fully_tombstoned: bool,
) -> &'static str {
    if row.is_latched() {
        return SYNC_STATE_QUARANTINED;
    }
    if row.is_tombstoned() {
        return if group_fully_tombstoned {
            SYNC_STATE_CLEAN
        } else {
            SYNC_STATE_DIRTY
        };
    }
    match verdict {
        GroupVerdict::Divergent => SYNC_STATE_QUARANTINED,
        GroupVerdict::HealthyDeleted => SYNC_STATE_DIRTY,
        GroupVerdict::Absent | GroupVerdict::HealthyLive => {
            if row.server_file_id.is_some() {
                SYNC_STATE_CLEAN
            } else {
                SYNC_STATE_LOCAL
            }
        }
    }
}

fn group_fully_tombstoned(
    row: &LocalRow,
    group: Option<&Group>,
    reuploaded_uuids: &BTreeSet<String>,
    patched_file_ids: &BTreeSet<String>,
) -> bool {
    match group {
        Some(g) => g
            .files
            .iter()
            .all(|f| f.record.deleted_at.is_some() || patched_file_ids.contains(&f.file_id)),
        None => reuploaded_uuids.contains(&row.uuid),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ACCOUNT: &str = "account-a";
    const UUID_A: &str = "00000000-0000-4000-8000-000000000001";
    const UUID_B: &str = "00000000-0000-4000-8000-0000000000bb";
    const UUID_C: &str = "00000000-0000-4000-8000-0000000000cc";
    const UUID_D: &str = "00000000-0000-4000-8000-0000000000dd";
    const UUID_E: &str = "00000000-0000-4000-8000-0000000000ee";
    const CREATED_AT: i64 = 1713456000123;
    const DELETED_AT: i64 = 1713462000456;
    const TEXT: &str = "Meeting notes: rename the module before the demo.";

    const FIXTURE_A: &[u8] =
        include_bytes!("../../../examples/sync/00000000-0000-4000-8000-000000000001.json");

    fn wire_a() -> WireRecord {
        wire::parse(FIXTURE_A, UUID_A).expect("fixture must parse")
    }

    fn live_row(uuid: &str) -> LocalRow {
        LocalRow {
            uuid: uuid.to_string(),
            timestamp_ms: CREATED_AT,
            text: TEXT.to_string(),
            mode: "transcription".to_string(),
            duration_ms: 8400,
            provider: "groq".to_string(),
            model: Some("whisper-large-v3".to_string()),
            language: Some("en".to_string()),
            rtype: RecordType::History,
            spoken: None,
            corrected: None,
            kind: None,
            trigger: None,
            expansion: None,
            settings_key: None,
            settings_value: None,
            deleted_at: None,
            server_file_id: None,
            sync_account: None,
            sync_state: SYNC_STATE_LOCAL.to_string(),
            quarantine_reason: None,
        }
    }

    fn snippet_wire(
        id: &str,
        trigger: &str,
        expansion: &str,
        deleted_at: Option<i64>,
    ) -> WireRecord {
        WireRecord {
            v: 1,
            id: id.to_string(),
            created_at: 1713468000123,
            deleted_at,
            rtype: RecordType::Snippet,
            text: String::new(),
            mode: String::new(),
            duration_ms: 0,
            provider: String::new(),
            model: None,
            language: None,
            spoken: None,
            corrected: None,
            kind: None,
            trigger: Some(trigger.to_string()),
            expansion: Some(expansion.to_string()),
            settings_key: None,
            settings_value: None,
        }
    }

    fn snippet_row(uuid: &str, trigger: &str, expansion: &str) -> LocalRow {
        LocalRow {
            uuid: uuid.to_string(),
            timestamp_ms: 1713468000123,
            text: String::new(),
            mode: String::new(),
            duration_ms: 0,
            provider: String::new(),
            model: None,
            language: None,
            rtype: RecordType::Snippet,
            spoken: None,
            corrected: None,
            kind: None,
            trigger: Some(trigger.to_string()),
            expansion: Some(expansion.to_string()),
            settings_key: None,
            settings_value: None,
            deleted_at: None,
            server_file_id: None,
            sync_account: None,
            sync_state: SYNC_STATE_LOCAL.to_string(),
            quarantine_reason: None,
        }
    }

    fn snippet_row_clean(uuid: &str, trigger: &str, expansion: &str, sfi: &str) -> LocalRow {
        let mut row = snippet_row(uuid, trigger, expansion);
        row.server_file_id = Some(sfi.to_string());
        row.sync_state = SYNC_STATE_CLEAN.to_string();
        row
    }

    fn settings_wire(id: &str, key: &str, value: &str) -> WireRecord {
        WireRecord {
            v: 1,
            id: id.to_string(),
            created_at: 1713471000123,
            deleted_at: None,
            rtype: RecordType::Settings,
            text: String::new(),
            mode: String::new(),
            duration_ms: 0,
            provider: String::new(),
            model: None,
            language: None,
            spoken: None,
            corrected: None,
            kind: None,
            trigger: None,
            expansion: None,
            settings_key: Some(key.to_string()),
            settings_value: Some(value.to_string()),
        }
    }

    fn settings_row(uuid: &str, key: &str, value: &str) -> LocalRow {
        LocalRow {
            uuid: uuid.to_string(),
            timestamp_ms: 1713471000123,
            text: String::new(),
            mode: String::new(),
            duration_ms: 0,
            provider: String::new(),
            model: None,
            language: None,
            rtype: RecordType::Settings,
            spoken: None,
            corrected: None,
            kind: None,
            trigger: None,
            expansion: None,
            settings_key: Some(key.to_string()),
            settings_value: Some(value.to_string()),
            deleted_at: None,
            server_file_id: None,
            sync_account: None,
            sync_state: SYNC_STATE_LOCAL.to_string(),
            quarantine_reason: None,
        }
    }

    fn settings_row_clean(uuid: &str, key: &str, value: &str, sfi: &str) -> LocalRow {
        LocalRow {
            uuid: uuid.to_string(),
            timestamp_ms: 1713471000123,
            text: String::new(),
            mode: String::new(),
            duration_ms: 0,
            provider: String::new(),
            model: None,
            language: None,
            rtype: RecordType::Settings,
            spoken: None,
            corrected: None,
            kind: None,
            trigger: None,
            expansion: None,
            settings_key: Some(key.to_string()),
            settings_value: Some(value.to_string()),
            deleted_at: None,
            server_file_id: Some(sfi.to_string()),
            sync_account: None,
            sync_state: SYNC_STATE_CLEAN.to_string(),
            quarantine_reason: None,
        }
    }

    fn live_row_clean(uuid: &str, sfi: &str) -> LocalRow {
        let mut row = live_row(uuid);
        row.server_file_id = Some(sfi.to_string());
        row.sync_state = SYNC_STATE_CLEAN.to_string();
        row
    }

    fn tombstone_row(uuid: &str) -> LocalRow {
        let mut row = live_row(uuid);
        row.deleted_at = Some(DELETED_AT);
        row.sync_state = SYNC_STATE_DIRTY.to_string();
        row
    }

    fn tombstone_row_clean(uuid: &str, sfi: &str) -> LocalRow {
        let mut row = tombstone_row(uuid);
        row.server_file_id = Some(sfi.to_string());
        row.sync_state = SYNC_STATE_CLEAN.to_string();
        row
    }

    #[derive(Debug, Clone, Default)]
    struct FakeLocalStore {
        rows: Vec<LocalRow>,
        ops: Vec<String>,
    }

    impl FakeLocalStore {
        fn row(&self, uuid: &str) -> Option<LocalRow> {
            self.rows.iter().find(|r| r.uuid == uuid).cloned()
        }
    }

    impl LocalStore for FakeLocalStore {
        fn list_rows(&self, account: Option<&str>) -> Vec<LocalRow> {
            let mut out: Vec<LocalRow> = self
                .rows
                .iter()
                .filter(|r| match account {
                    None => r.sync_account.is_none(),
                    Some(a) => r.sync_account.as_deref().map_or(true, |s| s == a),
                })
                .cloned()
                .collect();
            out.sort_by(|a, b| a.uuid.cmp(&b.uuid));
            out
        }

        fn find_row(&self, uuid: &str) -> Option<LocalRow> {
            self.row(uuid)
        }

        fn import(&mut self, row: LocalRow) -> Result<(), SyncError> {
            self.ops.push(format!("import:{}", row.uuid));
            if let Some(existing) = self.rows.iter_mut().find(|r| r.uuid == row.uuid) {
                *existing = row;
            } else {
                self.rows.push(row);
            }
            Ok(())
        }

        fn mark_tombstoned(&mut self, uuid: &str, deleted_at: i64) -> Result<(), SyncError> {
            self.ops.push(format!("tombstone:{uuid}"));
            if let Some(r) = self.rows.iter_mut().find(|r| r.uuid == uuid) {
                r.deleted_at = Some(deleted_at);
                r.sync_state = SYNC_STATE_DIRTY.to_string();
            }
            Ok(())
        }

        fn set_server_file_id(&mut self, uuid: &str, file_id: &str) -> Result<(), SyncError> {
            self.ops.push(format!("sfi:{uuid}:{file_id}"));
            if let Some(r) = self.rows.iter_mut().find(|r| r.uuid == uuid) {
                r.server_file_id = Some(file_id.to_string());
            }
            Ok(())
        }

        fn set_sync_state(&mut self, uuid: &str, state: &str) -> Result<(), SyncError> {
            self.ops.push(format!("state:{uuid}:{state}"));
            if let Some(r) = self.rows.iter_mut().find(|r| r.uuid == uuid) {
                r.sync_state = state.to_string();
            }
            Ok(())
        }

        fn quarantine(&mut self, uuid: &str, reason: QuarantineReason) -> Result<(), SyncError> {
            self.ops
                .push(format!("quarantine:{uuid}:{}", reason.as_str()));
            if let Some(r) = self.rows.iter_mut().find(|r| r.uuid == uuid) {
                r.quarantine_reason = Some(reason.as_str().to_string());
                r.sync_state = SYNC_STATE_QUARANTINED.to_string();
            }
            Ok(())
        }

        fn clear_quarantine(&mut self, _uuid: &str) -> Result<(), SyncError> {
            Ok(())
        }

        fn hard_delete(&mut self, uuid: &str) -> Result<(), SyncError> {
            self.ops.push(format!("hard_delete:{uuid}"));
            self.rows.retain(|r| r.uuid != uuid);
            Ok(())
        }
    }

    #[derive(Debug, Clone)]
    struct FakeFile {
        file_id: String,
        name: String,
        bytes: String,
        trashed: bool,
    }

    #[derive(Debug)]
    struct FakeDrive {
        files: Vec<FakeFile>,
        folder_created: bool,
        next_file_id: usize,
        list_calls: usize,
        create_fail: bool,
        patch_fault_after: Option<usize>,
        patch_ok: usize,
        hide_once: Option<String>,
        missing_content: Option<String>,
        ops: Vec<String>,
    }

    impl Default for FakeDrive {
        fn default() -> Self {
            Self {
                files: vec![],
                folder_created: false,
                next_file_id: 1,
                list_calls: 0,
                create_fail: false,
                patch_fault_after: None,
                patch_ok: 0,
                hide_once: None,
                missing_content: None,
                ops: vec![],
            }
        }
    }

    impl FakeDrive {
        fn add_file(&mut self, name: &str, record: &WireRecord) -> String {
            let file_id = format!("F{}", self.next_file_id);
            self.next_file_id += 1;
            self.files.push(FakeFile {
                file_id: file_id.clone(),
                name: name.to_string(),
                bytes: record.to_json(),
                trashed: false,
            });
            file_id
        }

        fn add_identical(&mut self, name: &str, record: &WireRecord, count: usize) -> Vec<String> {
            (0..count).map(|_| self.add_file(name, record)).collect()
        }

        fn add_raw(&mut self, file_id: &str, name: &str, bytes: &str) {
            self.files.push(FakeFile {
                file_id: file_id.to_string(),
                name: name.to_string(),
                bytes: bytes.to_string(),
                trashed: false,
            });
        }

        fn file(&self, file_id: &str) -> FakeFile {
            self.files
                .iter()
                .find(|f| f.file_id == file_id)
                .unwrap_or_else(|| panic!("no file {file_id}"))
                .clone()
        }

        fn rename(&mut self, file_id: &str, new_name: &str) {
            if let Some(f) = self.files.iter_mut().find(|f| f.file_id == file_id) {
                f.name = new_name.to_string();
            }
        }

        fn trash(&mut self, file_id: &str) {
            if let Some(f) = self.files.iter_mut().find(|f| f.file_id == file_id) {
                f.trashed = true;
            }
        }

        fn restore(&mut self, file_id: &str) {
            if let Some(f) = self.files.iter_mut().find(|f| f.file_id == file_id) {
                f.trashed = false;
            }
        }

        fn remove(&mut self, file_id: &str) {
            self.files.retain(|f| f.file_id != file_id);
        }

        fn parsed(&self, file_id: &str) -> WireRecord {
            wire::parse(self.file(file_id).bytes.as_bytes(), UUID_A).expect("fake file parses")
        }

        fn parsed_named(&self, file_id: &str) -> WireRecord {
            let f = self.file(file_id);
            let uuid = wire::uuid_basename(&f.name).expect("file has uuid name");
            wire::parse(f.bytes.as_bytes(), uuid).expect("fake file parses")
        }
    }

    impl DriveStore for FakeDrive {
        fn find_or_create_folder(&mut self) -> Result<(), SyncError> {
            self.folder_created = true;
            self.ops.push("folder".to_string());
            Ok(())
        }

        fn list_files(&mut self) -> Result<Vec<FileMeta>, SyncError> {
            self.list_calls += 1;
            let hidden = self.hide_once.take();
            let mut out: Vec<FileMeta> = self
                .files
                .iter()
                .filter(|f| !f.trashed && Some(&f.file_id) != hidden.as_ref())
                .map(|f| FileMeta {
                    file_id: f.file_id.clone(),
                    name: f.name.clone(),
                })
                .collect();
            out.sort_by(|a, b| a.file_id.cmp(&b.file_id));
            self.ops.push(format!("list#{}", self.list_calls));
            Ok(out)
        }

        fn get_content(&mut self, file_id: &str) -> Result<Option<Vec<u8>>, SyncError> {
            self.ops.push(format!("get:{file_id}"));
            if self.missing_content.as_deref() == Some(file_id) {
                return Ok(None);
            }
            Ok(self
                .files
                .iter()
                .find(|f| f.file_id == file_id)
                .map(|f| f.bytes.clone().into_bytes()))
        }

        fn create_file(&mut self, name: &str, record: &WireRecord) -> Result<String, SyncError> {
            let file_id = self.add_file(name, record);
            self.ops.push(format!("create:{name}:{file_id}"));
            if self.create_fail {
                return Err(SyncError::Retryable("injected create failure".to_string()));
            }
            Ok(file_id)
        }

        fn update_content(&mut self, file_id: &str, record: &WireRecord) -> Result<(), SyncError> {
            self.ops.push(format!("patch:{file_id}"));
            if let Some(n) = self.patch_fault_after {
                if self.patch_ok >= n {
                    return Err(SyncError::Retryable("injected patch failure".to_string()));
                }
            }
            if let Some(f) = self.files.iter_mut().find(|f| f.file_id == file_id) {
                f.bytes = record.to_json();
            }
            self.patch_ok += 1;
            Ok(())
        }
    }

    #[derive(Debug, Clone, Copy)]
    struct FakeToken {
        valid: bool,
    }

    impl TokenProvider for FakeToken {
        fn has_valid_token(&mut self) -> bool {
            self.valid
        }
    }

    fn run_pass(local: &mut FakeLocalStore, drive: &mut FakeDrive) -> SyncOutcome {
        let mut token = FakeToken { valid: true };
        run(RecordType::History, Some(ACCOUNT), local, drive, &mut token)
            .expect("pass must succeed")
    }

    fn run_pass_kind(
        kind: RecordType,
        local: &mut FakeLocalStore,
        drive: &mut FakeDrive,
    ) -> SyncOutcome {
        let mut token = FakeToken { valid: true };
        run(kind, Some(ACCOUNT), local, drive, &mut token).expect("pass must succeed")
    }

    // ---------------------------------------------------------------------
    // Layer 2 — pure engine
    // ---------------------------------------------------------------------

    #[test]
    fn import_healthy_group() {
        let mut drive = FakeDrive::default();
        let f1 = drive.add_file(&format!("{UUID_A}.json"), &wire_a());
        let mut local = FakeLocalStore::default();

        let o = run_pass(&mut local, &mut drive);
        assert_eq!(o.imported, 1);
        let rows = local.list_rows(Some(ACCOUNT));
        assert_eq!(rows.len(), 1);
        let r = &rows[0];
        assert_eq!(r.uuid, UUID_A);
        assert_eq!(r.sync_account.as_deref(), Some(ACCOUNT));
        assert_eq!(r.sync_state, SYNC_STATE_LOCAL);
        assert!(r.server_file_id.is_none());
        assert!(!r.is_tombstoned());
        assert_eq!(r.text, TEXT);
        assert_eq!(r.timestamp_ms, CREATED_AT);

        // Idempotent: a second pass changes nothing.
        let o2 = run_pass(&mut local, &mut drive);
        assert_eq!(o2, SyncOutcome::default());
        assert_eq!(local.rows.len(), 1);
        assert!(local.ops.iter().any(|op| op == &format!("import:{UUID_A}")));
        assert_eq!(drive.file(&f1).name, format!("{UUID_A}.json"));
    }

    #[test]
    fn duplicate_identical_files_import_once() {
        let mut drive = FakeDrive::default();
        drive.add_identical(&format!("{UUID_A}.json"), &wire_a(), 2);
        let mut local = FakeLocalStore::default();

        let o = run_pass(&mut local, &mut drive);
        assert_eq!(o.imported, 1);
        assert_eq!(local.rows.len(), 1);
        assert_eq!(run_pass(&mut local, &mut drive), SyncOutcome::default());
        assert_eq!(local.rows.len(), 1);
    }

    #[test]
    fn duplicate_divergent_files_quarantine_whole_group() {
        let mut drive = FakeDrive::default();
        drive.add_file(&format!("{UUID_A}.json"), &wire_a());
        let mut diverged = wire_a();
        diverged.text = "Different text.".to_string();
        drive.add_file(&format!("{UUID_A}.json"), &diverged);
        let mut local = FakeLocalStore::default();

        let o = run_pass(&mut local, &mut drive);
        assert_eq!(o.imported, 1);
        let r = local.row(UUID_A).expect("placeholder row imported");
        assert_eq!(r.sync_state, SYNC_STATE_QUARANTINED);
        assert_eq!(r.quarantine_reason.as_deref(), Some("collision"));
        assert_eq!(r.text, "");
        assert_eq!(r.timestamp_ms, 0);
        assert_eq!(r.sync_account.as_deref(), Some(ACCOUNT));

        // Latched: nothing happens on the next pass; offending files untouched.
        let o2 = run_pass(&mut local, &mut drive);
        assert_eq!(o2, SyncOutcome::default());
        assert_eq!(drive.files.len(), 2);
        assert!(
            drive
                .ops
                .iter()
                .filter(|op| op.starts_with("patch:"))
                .count()
                == 0
        );
    }

    #[test]
    fn tombstone_plus_live_duplicate_resolves_deleted() {
        let mut drive = FakeDrive::default();
        let f1 = drive.add_file(&format!("{UUID_A}.json"), &wire_a());
        let f2 = drive.add_file(
            &format!("{UUID_A}.json"),
            &wire::tombstone(&wire_a(), DELETED_AT),
        );
        let mut local = FakeLocalStore::default();
        local.import(live_row_clean(UUID_A, &f1));

        let o = run_pass(&mut local, &mut drive);
        assert_eq!(o.tombstoned_local, 1);
        assert_eq!(o.patches, 1);
        let r = local.row(UUID_A).expect("row survives");
        assert!(r.is_tombstoned());
        assert_eq!(r.deleted_at, Some(DELETED_AT));
        assert_eq!(r.sync_state, SYNC_STATE_CLEAN);
        // The live duplicate was converted in the same pass.
        assert_eq!(drive.parsed(&f1).deleted_at, Some(DELETED_AT));
        assert_eq!(drive.parsed(&f2).deleted_at, Some(DELETED_AT));

        let o2 = run_pass(&mut local, &mut drive);
        assert_eq!(o2, SyncOutcome::default());
    }

    #[test]
    fn live_local_row_group_deleted_converts_to_tombstone() {
        let mut drive = FakeDrive::default();
        let f1 = drive.add_file(
            &format!("{UUID_A}.json"),
            &wire::tombstone(&wire_a(), DELETED_AT),
        );
        let mut local = FakeLocalStore::default();
        local.import(live_row_clean(UUID_A, &f1));

        let o = run_pass(&mut local, &mut drive);
        assert_eq!(o.tombstoned_local, 1);
        assert_eq!(o.imported, 0);
        let r = local.row(UUID_A).expect("row survives");
        assert!(r.is_tombstoned());
        assert_eq!(r.deleted_at, Some(DELETED_AT));
        assert_eq!(r.sync_state, SYNC_STATE_CLEAN);
        assert_eq!(
            drive.file(&f1).bytes,
            wire::tombstone(&wire_a(), DELETED_AT).to_json()
        );

        assert_eq!(run_pass(&mut local, &mut drive), SyncOutcome::default());
    }

    #[test]
    fn never_uploaded_deleted_is_hard_deleted() {
        let mut drive = FakeDrive::default();
        let mut local = FakeLocalStore::default();
        local.import(tombstone_row(UUID_A));

        let o = run_pass(&mut local, &mut drive);
        assert_eq!(o.hard_deleted, 1);
        assert!(local.rows.is_empty());
        assert!(local
            .ops
            .iter()
            .any(|op| op == &format!("hard_delete:{UUID_A}")));

        assert_eq!(run_pass(&mut local, &mut drive), SyncOutcome::default());
    }

    #[test]
    fn account_scope_excludes_foreign_rows() {
        let mut drive = FakeDrive::default();
        let foreign = live_row_clean(UUID_B, "F1");
        let mut foreign = foreign;
        foreign.text = "Foreign text.".to_string();
        foreign.sync_account = Some("account-b".to_string());
        drive.add_file(&format!("{UUID_B}.json"), &foreign.to_wire());
        let mut local = FakeLocalStore::default();
        local.import(foreign.clone());
        local.import(live_row(UUID_A));
        local.ops.clear(); // seeding is not part of the pass

        let o = run_pass(&mut local, &mut drive);
        assert_eq!(o.created, 1); // only the local row A is uploaded
        assert_eq!(local.row(UUID_B).unwrap(), foreign); // byte-untouched
        assert!(
            !local.ops.iter().any(|op| op.contains(UUID_B)),
            "no ops may touch the foreign row: {:#?}",
            local.ops
        );
        assert_eq!(drive.files.len(), 2);
        let f_foreign = drive
            .files
            .iter()
            .find(|f| f.name == format!("{UUID_B}.json"))
            .unwrap();
        assert_eq!(f_foreign.bytes, foreign.to_wire().to_json());
    }

    #[test]
    fn stale_offline_device_reconnects_no_resurrection() {
        let mut drive = FakeDrive::default();
        let f1 = drive.add_file(
            &format!("{UUID_A}.json"),
            &wire::tombstone(&wire_a(), DELETED_AT),
        );
        let mut local = FakeLocalStore::default();
        local.import(live_row_clean(UUID_A, &f1));

        let o = run_pass(&mut local, &mut drive);
        assert_eq!(o.imported, 0);
        assert_eq!(o.created, 0);
        assert_eq!(o.tombstoned_local, 1);
        let rows = local.list_rows(Some(ACCOUNT));
        assert_eq!(rows.len(), 1);
        assert!(rows[0].is_tombstoned());
        assert_eq!(rows[0].sync_state, SYNC_STATE_CLEAN);
    }

    #[test]
    fn repeated_sync_reaches_fixed_point() {
        let mut drive = FakeDrive::default();
        let mut local = FakeLocalStore::default();

        let f1 = drive.add_file(&format!("{UUID_A}.json"), &wire_a());
        local.import(live_row_clean(UUID_A, &f1));

        let b_wire = tombstone_row_clean(UUID_B, "").to_wire();
        let f2 = drive.add_file(&format!("{UUID_B}.json"), &b_wire);
        local.import(tombstone_row_clean(UUID_B, &f2));

        local.import(live_row(UUID_C)); // fresh local record, nothing remote

        let mut latched = live_row(UUID_D);
        latched.quarantine_reason = Some("corrupt_file".to_string());
        latched.sync_state = SYNC_STATE_QUARANTINED.to_string();
        local.import(latched);

        let first = run_pass(&mut local, &mut drive);
        assert_eq!(first.created, 1);
        assert_eq!(first.imported, 0);

        for _ in 0..4 {
            assert_eq!(run_pass(&mut local, &mut drive), SyncOutcome::default());
        }
        let c = local.row(UUID_C).unwrap();
        assert!(c.server_file_id.is_some());
        assert_eq!(c.sync_state, SYNC_STATE_CLEAN);
        assert_eq!(
            local.row(UUID_D).unwrap().sync_state,
            SYNC_STATE_QUARANTINED
        );
    }

    #[test]
    fn absence_confirmed_by_listing_only() {
        let mut drive = FakeDrive::default();
        let f1 = drive.add_file(&format!("{UUID_A}.json"), &wire_a());
        drive.hide_once = Some(f1.clone());
        let mut local = FakeLocalStore::default();
        local.import(live_row_clean(UUID_A, &f1));

        let o = run_pass(&mut local, &mut drive);
        assert_eq!(o, SyncOutcome::default());
        assert_eq!(drive.list_calls, 2); // initial list + one confirmation re-list
        assert_eq!(drive.files.len(), 1);
        assert_eq!(
            local.row(UUID_A).unwrap().server_file_id.as_deref(),
            Some(f1.as_str())
        );
        assert_eq!(local.row(UUID_A).unwrap().sync_state, SYNC_STATE_CLEAN);

        assert_eq!(run_pass(&mut local, &mut drive), SyncOutcome::default());
    }

    #[test]
    fn trashed_file_counts_as_absent_reupload() {
        let mut drive = FakeDrive::default();
        let f1 = drive.add_file(&format!("{UUID_A}.json"), &wire_a());
        drive.trash(&f1);
        let mut local = FakeLocalStore::default();
        local.import(live_row_clean(UUID_A, &f1));

        let o = run_pass(&mut local, &mut drive);
        assert_eq!(o.reuploaded, 1);
        assert_eq!(drive.list_calls, 2);
        let r = local.row(UUID_A).unwrap();
        let f2 = r.server_file_id.clone().expect("new file id");
        assert_ne!(f2, f1);
        assert_eq!(r.sync_state, SYNC_STATE_CLEAN);
        assert_eq!(drive.file(&f2).name, format!("{UUID_A}.json"));
        assert_eq!(drive.file(&f2).bytes, wire_a().to_json());
        // The trashed original is untouched.
        assert!(drive.file(&f1).trashed);
        assert_eq!(drive.file(&f1).bytes, wire_a().to_json());

        assert_eq!(run_pass(&mut local, &mut drive), SyncOutcome::default());
    }

    #[test]
    fn renamed_file_counts_as_absent_reupload() {
        let mut drive = FakeDrive::default();
        let f1 = drive.add_file(&format!("{UUID_A}.json"), &wire_a());
        drive.rename(&f1, "X-copy.json");
        let mut local = FakeLocalStore::default();
        local.import(live_row_clean(UUID_A, &f1));

        let o = run_pass(&mut local, &mut drive);
        assert_eq!(o.reuploaded, 1);
        let r = local.row(UUID_A).unwrap();
        let f2 = r.server_file_id.clone().expect("new file id");
        assert_eq!(drive.file(&f2).name, format!("{UUID_A}.json"));
        assert_eq!(drive.file(&f2).bytes, wire_a().to_json());
        assert_eq!(r.sync_state, SYNC_STATE_CLEAN);
    }

    #[test]
    fn no_token_returns_auth_required() {
        let mut drive = FakeDrive::default();
        let mut local = FakeLocalStore::default();
        let mut token = FakeToken { valid: false };
        let err = run(
            RecordType::History,
            Some(ACCOUNT),
            &mut local,
            &mut drive,
            &mut token,
        )
        .unwrap_err();
        assert!(matches!(err, SyncError::AuthRequired));
        assert!(!drive.folder_created, "nothing may happen without a token");
        assert!(local.rows.is_empty());
    }

    #[test]
    fn fetch_404_drops_file_this_pass() {
        let mut drive = FakeDrive::default();
        let f1 = drive.add_file(&format!("{UUID_A}.json"), &wire_a());
        drive.missing_content = Some(f1.clone());
        let mut local = FakeLocalStore::default();
        local.import(live_row_clean(UUID_A, &f1));

        let o = run_pass(&mut local, &mut drive);
        assert_eq!(o, SyncOutcome::default());
        // Absence is listing-based only (spec §7, §10): the file IS listed, so
        // no candidate, no re-list, no re-upload.
        assert_eq!(drive.list_calls, 1);
        assert_eq!(
            local.row(UUID_A).unwrap().server_file_id.as_deref(),
            Some(f1.as_str())
        );
    }

    // ---------------------------------------------------------------------
    // Layer 5 — Fake Drive integration
    // ---------------------------------------------------------------------

    #[test]
    fn post_timeout_creates_duplicate_identical_file() {
        let mut drive = FakeDrive::default();
        drive.create_fail = true;
        let mut local = FakeLocalStore::default();
        local.import(live_row(UUID_A));

        // Pass 1: the create POST landed but the commit never happened.
        let o1 = run_pass(&mut local, &mut drive);
        assert_eq!(o1.retryable_failures, 1);
        assert_eq!(o1.created, 0);
        assert_eq!(local.row(UUID_A).unwrap().sync_state, SYNC_STATE_LOCAL);
        assert!(local.row(UUID_A).unwrap().server_file_id.is_none());
        assert_eq!(drive.files.len(), 1);

        // Pass 2: the orphaned file is duplicate-identical → harmless retry.
        drive.create_fail = false;
        let o2 = run_pass(&mut local, &mut drive);
        assert_eq!(o2.created, 1);
        assert_eq!(o2.retryable_failures, 0);
        let r = local.row(UUID_A).unwrap();
        assert_eq!(r.sync_state, SYNC_STATE_CLEAN);
        assert_eq!(drive.files.len(), 2);
        let all_same = drive.files.iter().all(|f| f.bytes == wire_a().to_json());
        assert!(all_same, "duplicate files must be byte-identical");

        assert_eq!(run_pass(&mut local, &mut drive), SyncOutcome::default());
    }

    #[test]
    fn crash_halfway_through_100_duplicate_tombstones_completes_next_pass() {
        let mut drive = FakeDrive::default();
        let ids = drive.add_identical(&format!("{UUID_A}.json"), &wire_a(), 100);
        let f1 = ids[0].clone();
        let mut local = FakeLocalStore::default();
        local.import(tombstone_row(UUID_A).with_sfi(&f1));

        // Pass 1: propagation crashes halfway (50 patched, 50 failed).
        drive.patch_fault_after = Some(50);
        let o1 = run_pass(&mut local, &mut drive);
        assert_eq!(o1.patches, 50);
        assert_eq!(o1.retryable_failures, 50);
        assert_eq!(local.row(UUID_A).unwrap().sync_state, SYNC_STATE_DIRTY);
        let tombstoned_count = drive
            .files
            .iter()
            .filter(|f| drive_parsed_tombstoned(f))
            .count();
        assert_eq!(tombstoned_count, 50);

        // Pass 2: the remaining 50 complete; the row converges to clean.
        drive.patch_fault_after = None;
        let o2 = run_pass(&mut local, &mut drive);
        assert_eq!(o2.patches, 50);
        assert_eq!(o2.retryable_failures, 0);
        assert_eq!(local.row(UUID_A).unwrap().sync_state, SYNC_STATE_CLEAN);
        assert!(
            drive.files.iter().all(drive_parsed_tombstoned),
            "every duplicate must carry deleted_at"
        );
    }

    #[test]
    fn remote_tombstone_deleted_reuploaded_by_tombstone_holder() {
        let mut drive = FakeDrive::default();
        let f1 = drive.add_file(
            &format!("{UUID_A}.json"),
            &wire::tombstone(&wire_a(), DELETED_AT),
        );
        drive.remove(&f1);
        let mut local = FakeLocalStore::default();
        local.import(tombstone_row_clean(UUID_A, &f1));

        let o = run_pass(&mut local, &mut drive);
        assert_eq!(o.reuploaded, 1);
        let r = local.row(UUID_A).unwrap();
        let f2 = r.server_file_id.clone().expect("new tombstone file");
        assert_eq!(r.sync_state, SYNC_STATE_CLEAN);
        let rep = drive.parsed(&f2);
        assert_eq!(rep.deleted_at, Some(DELETED_AT));
        assert_eq!(rep.content(), wire_a().content());

        assert_eq!(run_pass(&mut local, &mut drive), SyncOutcome::default());
    }

    #[test]
    fn local_tombstone_destroyed_boundary_documented() {
        // §18: a deleted record permanently resurrects iff EVERY copy of the
        // tombstone is destroyed — Drive file AND local row. That is outside
        // the protocol's authority; the engine simply re-imports the live file.
        let mut drive = FakeDrive::default();
        let f1 = drive.add_file(
            &format!("{UUID_A}.json"),
            &wire::tombstone(&wire_a(), DELETED_AT),
        );
        let mut local = FakeLocalStore::default();
        local.import(tombstone_row_clean(UUID_A, &f1));

        // External destruction (neither the app nor the engine did this).
        drive.remove(&f1);
        local.hard_delete(UUID_A).unwrap();

        assert_eq!(run_pass(&mut local, &mut drive), SyncOutcome::default());

        // Another device uploads a live file → the record resurrects.
        drive.add_file(&format!("{UUID_A}.json"), &wire_a());
        let o = run_pass(&mut local, &mut drive);
        assert_eq!(o.imported, 1);
        let r = local.row(UUID_A).unwrap();
        assert!(!r.is_tombstoned());
        assert_eq!(r.sync_state, SYNC_STATE_LOCAL);
    }

    #[test]
    fn entire_folder_deleted_all_records_restored() {
        let mut drive = FakeDrive::default();
        let mut local = FakeLocalStore::default();

        let f1 = drive.add_file(&format!("{UUID_A}.json"), &wire_a());
        local.import(live_row_clean(UUID_A, &f1));
        let b_wire = tombstone_row_clean(UUID_B, "").to_wire();
        let f2 = drive.add_file(&format!("{UUID_B}.json"), &b_wire);
        local.import(tombstone_row_clean(UUID_B, &f2));

        drive.remove(&f1);
        drive.remove(&f2);

        let o = run_pass(&mut local, &mut drive);
        assert_eq!(o.reuploaded, 2);
        assert!(drive.folder_created);
        let a = local.row(UUID_A).unwrap();
        let b = local.row(UUID_B).unwrap();
        assert_eq!(a.sync_state, SYNC_STATE_CLEAN);
        assert_eq!(b.sync_state, SYNC_STATE_CLEAN);
        let fa = drive.file(&a.server_file_id.clone().unwrap());
        assert_eq!(fa.name, format!("{UUID_A}.json"));
        assert!(!drive.parsed_named(&fa.file_id).deleted_at.is_some());
        let fb = drive.file(&b.server_file_id.clone().unwrap());
        assert_eq!(fb.name, format!("{UUID_B}.json"));
        assert_eq!(drive.parsed_named(&fb.file_id).deleted_at, Some(DELETED_AT));

        assert_eq!(run_pass(&mut local, &mut drive), SyncOutcome::default());
    }

    #[test]
    fn drive_list_lag_absorbed_by_duplicate_identical() {
        let mut drive = FakeDrive::default();
        let f1 = drive.add_file(&format!("{UUID_A}.json"), &wire_a());
        drive.hide_once = Some(f1.clone());
        let mut local = FakeLocalStore::default();
        local.import(live_row_clean(UUID_A, &f1));

        // Pass 1: lag hides the file from the first list; the confirmation
        // re-list sees it → not confirmed → no re-upload at all.
        let o1 = run_pass(&mut local, &mut drive);
        assert_eq!(o1, SyncOutcome::default());
        assert_eq!(drive.list_calls, 2);
        assert_eq!(drive.files.len(), 1);

        // Pass 2: everything visible; healthy group, nothing to write.
        let o2 = run_pass(&mut local, &mut drive);
        assert_eq!(o2, SyncOutcome::default());
        assert_eq!(drive.files.len(), 1, "no duplicate may be created");
        assert_eq!(
            local.row(UUID_A).unwrap().server_file_id.as_deref(),
            Some(f1.as_str())
        );
    }

    #[test]
    fn folder_recreation_restores_records() {
        let mut drive = FakeDrive::default();
        let mut local = FakeLocalStore::default();
        local.import(live_row_clean(UUID_A, "F1")); // folder gone; file gone too

        let o = run_pass(&mut local, &mut drive);
        assert!(drive.folder_created, "folder must be recreated");
        assert_eq!(o.reuploaded, 1);
        let r = local.row(UUID_A).unwrap();
        assert_eq!(r.sync_state, SYNC_STATE_CLEAN);
        assert_eq!(
            drive.file(&r.server_file_id.unwrap()).name,
            format!("{UUID_A}.json")
        );
    }

    #[test]
    fn renamed_file_counts_as_absent_reupload_left_untouched() {
        // R3, spec §15 rename: misnamed files are inert; the row's own file is
        // re-uploaded under the correct name; the misnamed file is untouched.
        let mut drive = FakeDrive::default();
        let f1 = drive.add_file(&format!("{UUID_A}.json"), &wire_a());
        let f1_before = drive.file(&f1).clone();
        let mut local = FakeLocalStore::default();
        local.import(live_row_clean(UUID_A, &f1));

        drive.rename(&f1, "X-copy.json");

        let o = run_pass(&mut local, &mut drive);
        assert_eq!(o.reuploaded, 1);
        assert_eq!(o.hard_deleted, 0);
        assert_eq!(o.quarantined, 0);

        // The row is intact, clean, pointing at the new file.
        let r = local.row(UUID_A).unwrap();
        let f2 = r.server_file_id.clone().expect("new file id");
        assert_eq!(r.sync_state, SYNC_STATE_CLEAN);
        assert!(!r.is_tombstoned());
        assert!(r.quarantine_reason.is_none());
        assert_eq!(local.rows.len(), 1);
        assert!(!local.ops.iter().any(|op| op.starts_with("hard_delete")));

        // F2 is the correctly-named exact re-upload.
        assert_eq!(drive.file(&f2).name, format!("{UUID_A}.json"));
        assert_eq!(drive.file(&f2).bytes, wire_a().to_json());

        // F1 keeps its misnamed existence byte-for-byte and is never fetched.
        let f1_after = drive.file(&f1);
        assert_eq!(f1_after.name, "X-copy.json");
        assert_eq!(f1_after.bytes, f1_before.bytes);
        assert!(!drive.ops.iter().any(|op| op == "get:F1"));
    }

    #[test]
    fn restored_trash_file_joins_healthy_group() {
        let mut drive = FakeDrive::default();
        let f1 = drive.add_file(&format!("{UUID_A}.json"), &wire_a());
        let mut local = FakeLocalStore::default();
        local.import(live_row_clean(UUID_A, &f1));

        drive.trash(&f1);
        let o1 = run_pass(&mut local, &mut drive);
        assert_eq!(o1.reuploaded, 1);
        let r1 = local.row(UUID_A).unwrap();
        let f2 = r1.server_file_id.clone().expect("new file id");

        // Restore: the original reappears → HEALTHY duplicate-identical.
        drive.restore(&f1);
        let o2 = run_pass(&mut local, &mut drive);
        assert_eq!(o2, SyncOutcome::default());
        let r2 = local.row(UUID_A).unwrap();
        assert_eq!(r2.server_file_id.as_deref(), Some(f2.as_str()));
        assert_eq!(r2.sync_state, SYNC_STATE_CLEAN);
        assert_eq!(drive.files.len(), 2, "duplicates coexist, no winner");
        assert!(
            drive.files.iter().all(|f| f.bytes == wire_a().to_json()),
            "both copies byte-identical"
        );
    }

    #[test]
    fn sync_state_equals_derived() {
        // Invariant (spec §6): after a pass, every row's persisted sync_state
        // equals the value derived from (deleted_at, server_file_id,
        // quarantine_reason, group verdict).
        let mut drive = FakeDrive::default();
        let mut local = FakeLocalStore::default();

        let f1 = drive.add_file(&format!("{UUID_A}.json"), &wire_a());
        local.import(live_row_clean(UUID_A, &f1));

        let b_wire = tombstone_row_clean(UUID_B, "").to_wire();
        let f2 = drive.add_file(&format!("{UUID_B}.json"), &b_wire);
        local.import(tombstone_row_clean(UUID_B, &f2));

        local.import(live_row(UUID_C)); // fresh → will be created this pass

        let mut latched = live_row(UUID_D);
        latched.quarantine_reason = Some("corrupt_file".to_string());
        latched.sync_state = SYNC_STATE_QUARANTINED.to_string();
        local.import(latched);

        run_pass(&mut local, &mut drive);

        for row in local.list_rows(Some(ACCOUNT)) {
            assert_eq!(
                row.sync_state,
                derived_from_facts(&row, &drive),
                "row {} must match its derived state",
                row.uuid
            );
        }
        assert_eq!(local.row(UUID_A).unwrap().sync_state, SYNC_STATE_CLEAN);
        assert_eq!(local.row(UUID_B).unwrap().sync_state, SYNC_STATE_CLEAN);
        assert_eq!(local.row(UUID_C).unwrap().sync_state, SYNC_STATE_CLEAN);
        assert_eq!(
            local.row(UUID_D).unwrap().sync_state,
            SYNC_STATE_QUARANTINED
        );
    }

    fn drive_parsed_tombstoned(file: &FakeFile) -> bool {
        let Some(uuid) = wire::uuid_basename(&file.name) else {
            return false;
        };
        wire::parse(file.bytes.as_bytes(), uuid)
            .map(|r| r.deleted_at.is_some())
            .unwrap_or(false)
    }

    /// Independent recomputation of the §6 derived state from final facts.
    fn derived_from_facts(row: &LocalRow, drive: &FakeDrive) -> &'static str {
        if row.is_latched() {
            return SYNC_STATE_QUARANTINED;
        }
        let group: Vec<&FakeFile> = drive
            .files
            .iter()
            .filter(|f| !f.trashed && f.name == format!("{}.json", row.uuid))
            .collect();
        let all_tombstoned = !group.is_empty()
            && group.iter().all(|f| {
                wire::parse(f.bytes.as_bytes(), row.uuid.as_str())
                    .map(|r| r.deleted_at.is_some())
                    .unwrap_or(false)
            });
        if row.is_tombstoned() {
            return if all_tombstoned {
                SYNC_STATE_CLEAN
            } else {
                SYNC_STATE_DIRTY
            };
        }
        if group.is_empty() {
            return if row.server_file_id.is_some() {
                SYNC_STATE_CLEAN
            } else {
                SYNC_STATE_LOCAL
            };
        }
        if group.iter().any(|f| {
            wire::parse(f.bytes.as_bytes(), row.uuid.as_str())
                .map(|r| r.deleted_at.is_some())
                .unwrap_or(false)
        }) {
            return SYNC_STATE_DIRTY;
        }
        if row.server_file_id.is_some() {
            SYNC_STATE_CLEAN
        } else {
            SYNC_STATE_LOCAL
        }
    }

    // ---------------------------------------------------------------------
    // Layer 2 — §30.5 record kinds (pure engine)
    // ---------------------------------------------------------------------

    #[test]
    fn other_kinds_are_inert_for_this_pass() {
        let mut drive = FakeDrive::default();
        let f_s = drive.add_file(
            &format!("{UUID_B}.json"),
            &snippet_wire(UUID_B, "addr", "456 Oak Ave", None),
        );
        drive.trash(&f_s); // absent from listing; only a snippet pass may restore it
        let mut local = FakeLocalStore::default();
        local.import(snippet_row_clean(UUID_B, "addr", "456 Oak Ave", &f_s));

        // History pass: the snippet row is inert — no re-upload, no import.
        let o = run_pass(&mut local, &mut drive);
        assert_eq!(o, SyncOutcome::default());
        assert_eq!(
            drive.list_calls, 1,
            "no confirmation re-list for other kinds"
        );
        assert_eq!(local.row(UUID_B).unwrap().sync_state, SYNC_STATE_CLEAN);

        // Snippet pass: the row is restored.
        let o2 = run_pass_kind(RecordType::Snippet, &mut local, &mut drive);
        assert_eq!(o2.reuploaded, 1);
    }

    #[test]
    fn unknown_group_of_other_kind_not_imported_by_this_pass() {
        let mut drive = FakeDrive::default();
        drive.add_file(
            &format!("{UUID_B}.json"),
            &snippet_wire(UUID_B, "addr", "456 Oak Ave", None),
        );
        let mut local = FakeLocalStore::default();

        let o = run_pass(&mut local, &mut drive);
        assert_eq!(o, SyncOutcome::default(), "history pass imports nothing");
        assert!(local.rows.is_empty());

        let o2 = run_pass_kind(RecordType::Snippet, &mut local, &mut drive);
        assert_eq!(o2.imported, 1);
        assert_eq!(local.row(UUID_B).unwrap().trigger.as_deref(), Some("addr"));
    }

    #[test]
    fn unknown_type_file_quarantines_with_unknown_type_reason() {
        let mut drive = FakeDrive::default();
        drive.add_raw(
            "F1",
            &format!("{UUID_A}.json"),
            r#"{"v":1,"id":"00000000-0000-4000-8000-000000000001","created_at":1713456000123,"deleted_at":null,"type":"note","text":"x","mode":"transcription","duration_ms":1,"provider":"groq"}"#,
        );
        let mut local = FakeLocalStore::default();

        let o = run_pass(&mut local, &mut drive);
        assert_eq!(o.imported, 1);
        let r = local.row(UUID_A).expect("placeholder row imported");
        assert_eq!(r.sync_state, SYNC_STATE_QUARANTINED);
        assert_eq!(r.quarantine_reason.as_deref(), Some("unknown_type"));
        assert_eq!(r.text, "");
        assert_eq!(r.timestamp_ms, 0);
    }

    #[test]
    fn edit_creates_new_uuid_tombstones_old() {
        // §30.2: an edit = tombstone of the old row + a new UUID row. The
        // snippet pass propagates both: patch the old file, create the new one.
        let mut drive = FakeDrive::default();
        let f_old = drive.add_file(
            &format!("{UUID_B}.json"),
            &snippet_wire(UUID_B, "addr", "123 Main St", None),
        );
        let mut local = FakeLocalStore::default();
        let mut old = snippet_row_clean(UUID_B, "addr", "123 Main St", &f_old);
        old.deleted_at = Some(DELETED_AT);
        old.sync_state = SYNC_STATE_DIRTY.to_string();
        local.import(old);
        local.import(snippet_row(UUID_C, "addr", "456 Oak Ave"));

        let o = run_pass_kind(RecordType::Snippet, &mut local, &mut drive);
        assert_eq!(o.patches, 1);
        assert_eq!(o.created, 1);
        assert_eq!(o.quarantined, 0);

        // Old file tombstoned, content preserved (§4 tombstone keeps T).
        let p = drive.parsed_named(&f_old);
        assert_eq!(p.deleted_at, Some(DELETED_AT));
        assert_eq!(p.trigger.as_deref(), Some("addr"));
        assert_eq!(p.expansion.as_deref(), Some("123 Main St"));
        // New file carries the edited content under its own UUID.
        let r = local.row(UUID_C).unwrap();
        let f_new = r.server_file_id.clone().expect("new file id");
        let pn = drive.parsed_named(&f_new);
        assert_eq!(pn.rtype, RecordType::Snippet);
        assert_eq!(pn.trigger.as_deref(), Some("addr"));
        assert_eq!(pn.expansion.as_deref(), Some("456 Oak Ave"));
        assert_eq!(local.row(UUID_B).unwrap().sync_state, SYNC_STATE_CLEAN);
        assert_eq!(local.row(UUID_C).unwrap().sync_state, SYNC_STATE_CLEAN);

        assert_eq!(
            run_pass_kind(RecordType::Snippet, &mut local, &mut drive),
            SyncOutcome::default()
        );
    }

    #[test]
    fn edit_propagates_to_other_device() {
        // Device A edited (tombstone B + live C); device B starts empty and
        // must receive both rows with correct states.
        let mut drive = FakeDrive::default();
        drive.add_file(
            &format!("{UUID_B}.json"),
            &snippet_wire(UUID_B, "addr", "123 Main St", Some(DELETED_AT)),
        );
        drive.add_file(
            &format!("{UUID_C}.json"),
            &snippet_wire(UUID_C, "addr", "456 Oak Ave", None),
        );
        let mut local = FakeLocalStore::default();

        let o = run_pass_kind(RecordType::Snippet, &mut local, &mut drive);
        assert_eq!(o.imported, 2);
        let b = local.row(UUID_B).unwrap();
        assert!(b.is_tombstoned());
        assert_eq!(b.sync_state, SYNC_STATE_CLEAN);
        assert_eq!(b.trigger.as_deref(), Some("addr"));
        let c = local.row(UUID_C).unwrap();
        assert!(!c.is_tombstoned());
        assert_eq!(c.sync_state, SYNC_STATE_LOCAL);
        assert_eq!(c.expansion.as_deref(), Some("456 Oak Ave"));

        assert_eq!(
            run_pass_kind(RecordType::Snippet, &mut local, &mut drive),
            SyncOutcome::default()
        );
    }

    #[test]
    fn concurrent_edits_both_survive_no_winner() {
        // Two devices edit the same snippet independently → two new UUIDs;
        // both edits survive, nothing is quarantined.
        let mut drive = FakeDrive::default();
        let f_a = drive.add_file(
            &format!("{UUID_B}.json"),
            &snippet_wire(UUID_B, "addr", "123 Main St", None),
        );
        let f_b = drive.add_file(
            &format!("{UUID_B}.json"),
            &snippet_wire(UUID_B, "addr", "123 Main St", None),
        );

        // Device A: tombstone B (own file f_a), new live row C.
        let mut local_a = FakeLocalStore::default();
        let mut old_a = snippet_row_clean(UUID_B, "addr", "123 Main St", &f_a);
        old_a.deleted_at = Some(DELETED_AT);
        old_a.sync_state = SYNC_STATE_DIRTY.to_string();
        local_a.import(old_a);
        local_a.import(snippet_row(UUID_C, "addr", "456 Oak Ave"));
        let oa = run_pass_kind(RecordType::Snippet, &mut local_a, &mut drive);
        assert_eq!(
            oa.patches, 2,
            "tombstone-wins patches every listed live copy"
        );
        assert_eq!(oa.created, 1);

        // Device B: tombstone B (own file f_b), new live row E. A's pass
        // already tombstoned every live copy of the old record, so B has
        // nothing to patch — it only uploads its own edit and imports A's.
        let mut local_b = FakeLocalStore::default();
        let mut old_b = snippet_row_clean(UUID_B, "addr", "123 Main St", &f_b);
        old_b.deleted_at = Some(DELETED_AT);
        old_b.sync_state = SYNC_STATE_DIRTY.to_string();
        local_b.import(old_b);
        local_b.import(snippet_row(UUID_E, "addr", "789 Elm St"));
        let ob = run_pass_kind(RecordType::Snippet, &mut local_b, &mut drive);
        assert_eq!(ob.patches, 0);
        assert_eq!(ob.created, 1, "B's own new row is uploaded");
        assert_eq!(ob.imported, 1, "B imports A's new row");

        // Both rows survive on B, in protocol-valid states, nothing latched.
        let c = local_b.row(UUID_C).unwrap();
        assert!(!c.is_tombstoned());
        assert_eq!(c.expansion.as_deref(), Some("456 Oak Ave"));
        assert_eq!(c.sync_state, SYNC_STATE_LOCAL);
        let e = local_b.row(UUID_E).unwrap();
        assert!(!e.is_tombstoned());
        assert_eq!(e.expansion.as_deref(), Some("789 Elm St"));
        assert_eq!(e.sync_state, SYNC_STATE_CLEAN);
        assert_eq!(local_b.row(UUID_B).unwrap().sync_state, SYNC_STATE_CLEAN);
        assert!(local_b.rows.iter().all(|r| r.quarantine_reason.is_none()));

        // Fixed point on both devices (A picks up B's new row on this pass).
        let oa2 = run_pass_kind(RecordType::Snippet, &mut local_a, &mut drive);
        assert_eq!(oa2.imported, 1, "A imports B's new row on its next pass");
        let e_a = local_a.row(UUID_E).unwrap();
        assert!(!e_a.is_tombstoned());
        assert_eq!(e_a.expansion.as_deref(), Some("789 Elm St"));
        assert_eq!(e_a.sync_state, SYNC_STATE_LOCAL);
        assert_eq!(
            run_pass_kind(RecordType::Snippet, &mut local_b, &mut drive),
            SyncOutcome::default()
        );
    }

    // ---------------------------------------------------------------------
    // Layer 5 — §30.5 record kinds (Fake Drive integration)
    // ---------------------------------------------------------------------

    #[test]
    fn edited_snippet_reuploaded_as_new_uuid() {
        // The old file vanished from Drive entirely (trashed/deleted by hand);
        // the tombstoned old row is re-uploaded as a tombstone and the edited
        // row uploads under its new UUID.
        let mut drive = FakeDrive::default();
        let f_old = drive.add_file(
            &format!("{UUID_B}.json"),
            &snippet_wire(UUID_B, "addr", "123 Main St", None),
        );
        drive.remove(&f_old);
        let mut local = FakeLocalStore::default();
        let mut old = snippet_row_clean(UUID_B, "addr", "123 Main St", &f_old);
        old.deleted_at = Some(DELETED_AT);
        old.sync_state = SYNC_STATE_DIRTY.to_string();
        local.import(old);
        local.import(snippet_row(UUID_C, "addr", "456 Oak Ave"));

        let o = run_pass_kind(RecordType::Snippet, &mut local, &mut drive);
        assert_eq!(o.reuploaded, 1);
        assert_eq!(o.created, 1);
        assert_eq!(o.patches, 0);
        assert_eq!(drive.list_calls, 2);

        let b = local.row(UUID_B).unwrap();
        assert!(b.is_tombstoned());
        assert_eq!(b.sync_state, SYNC_STATE_CLEAN);
        let rb = drive.parsed_named(&b.server_file_id.clone().unwrap());
        assert_eq!(rb.deleted_at, Some(DELETED_AT));
        assert_eq!(rb.trigger.as_deref(), Some("addr"));
        assert_eq!(rb.expansion.as_deref(), Some("123 Main St"));

        let c = local.row(UUID_C).unwrap();
        assert_eq!(c.sync_state, SYNC_STATE_CLEAN);
        let rc = drive.parsed_named(&c.server_file_id.clone().unwrap());
        assert_eq!(rc.rtype, RecordType::Snippet);
        assert_eq!(rc.expansion.as_deref(), Some("456 Oak Ave"));

        assert_eq!(
            run_pass_kind(RecordType::Snippet, &mut local, &mut drive),
            SyncOutcome::default()
        );
    }

    #[test]
    fn toggled_enabled_propagates() {
        // §30.3: `snippets_enabled` is a settings record; toggling tombstones
        // the old record and creates a new UUID with the new value.
        let mut drive = FakeDrive::default();
        let f1 = drive.add_file(
            &format!("{UUID_B}.json"),
            &settings_wire(UUID_B, "snippets_enabled", "true"),
        );
        let mut local = FakeLocalStore::default();
        local.import(settings_row_clean(UUID_B, "snippets_enabled", "true", &f1));
        let mut old = settings_row_clean(UUID_B, "snippets_enabled", "true", &f1);
        old.deleted_at = Some(DELETED_AT);
        old.sync_state = SYNC_STATE_DIRTY.to_string();
        local.import(old);
        local.import(settings_row(UUID_C, "snippets_enabled", "false"));

        let o = run_pass_kind(RecordType::Settings, &mut local, &mut drive);
        assert_eq!(o.patches, 1);
        assert_eq!(o.created, 1);

        let p = drive.parsed_named(&f1);
        assert_eq!(p.deleted_at, Some(DELETED_AT));
        assert_eq!(p.settings_key.as_deref(), Some("snippets_enabled"));
        assert_eq!(p.settings_value.as_deref(), Some("true"));
        let r = local.row(UUID_C).unwrap();
        assert_eq!(r.sync_state, SYNC_STATE_CLEAN);
        let p2 = drive.parsed_named(&r.server_file_id.clone().unwrap());
        assert_eq!(p2.settings_key.as_deref(), Some("snippets_enabled"));
        assert_eq!(p2.settings_value.as_deref(), Some("false"));
        assert_eq!(local.row(UUID_B).unwrap().sync_state, SYNC_STATE_CLEAN);

        // A second device sees exactly the toggled state.
        let mut other = FakeLocalStore::default();
        let o2 = run_pass_kind(RecordType::Settings, &mut other, &mut drive);
        assert_eq!(o2.imported, 2);
        let b2 = other.row(UUID_B).unwrap();
        assert!(b2.is_tombstoned());
        assert_eq!(b2.settings_value.as_deref(), Some("true"));
        let c2 = other.row(UUID_C).unwrap();
        assert!(!c2.is_tombstoned());
        assert_eq!(c2.settings_value.as_deref(), Some("false"));

        assert_eq!(
            run_pass_kind(RecordType::Settings, &mut local, &mut drive),
            SyncOutcome::default()
        );
    }

    // Test-only row helpers -------------------------------------------------

    impl LocalRow {
        fn with_sfi(mut self, sfi: &str) -> Self {
            self.server_file_id = Some(sfi.to_string());
            self
        }
    }
}
