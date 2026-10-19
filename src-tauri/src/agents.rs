// Fluence Windows - Custom agents store (Slice 4b)
// Mirrors Android AgentPreferences: fixed built-in multipurpose agent plus
// named custom hints that run through the same JSON action contract in
// agent.rs. Additive, fail-closed: unknown/deleted ids resolve to built-in,
// storage is a standalone agents.json file, audio/STT untouched.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

pub const ID_BUILT_IN: &str = "builtin";
pub const NAME_BUILT_IN: &str = "Fluence Agent";
pub const MAX_AGENT_NAME_LENGTH: usize = 30;
pub const MAX_AGENT_HINT_LENGTH: usize = 1000;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CustomAgent {
    pub id: String,
    pub name: String,
    pub hint: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentsStore {
    #[serde(default)]
    pub custom_agents: Vec<CustomAgent>,
    #[serde(default = "default_builtin")]
    pub default_id: String,
}

fn default_builtin() -> String {
    ID_BUILT_IN.to_string()
}

impl Default for AgentsStore {
    fn default() -> Self {
        Self {
            custom_agents: Vec::new(),
            default_id: ID_BUILT_IN.to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedAgent {
    pub id: String,
    pub is_builtin: bool,
    pub hint: Option<String>,
}

pub fn agents_path() -> PathBuf {
    // Honour `FLUENCE_DATA_DIR` exactly like `sync::stores::base_data_dir()`.
    //
    // Without this the legacy store resolved to the REAL
    // `%LOCALAPPDATA%\agents.json` — note NO `Fluence` leaf: this production
    // branch starts from `dirs::data_local_dir()` and pushes only
    // `agents.json`. Only callers of `sync::stores::base_data_dir()` append the
    // `Fluence` component, which is what the test branch below does. That store
    // is shared by every agent test and survives between runs, so the harness
    // runs in parallel had tests writing over each other and each run reading
    // the previous one's state — which made any durability test over this store
    // flaky for reasons unrelated to the code under test.
    //
    // Read ONLY outside tests, matching `base_data_dir()`. Under `cfg(test)` an
    // exported variable would collapse every `cargo test` run on the machine
    // into one fixed directory across processes and time — the recycled-pid /
    // shared-root hazard the per-process temp key exists to prevent. No test
    // sets it.
    #[cfg(not(test))]
    let mut path = std::env::var("FLUENCE_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| dirs::data_local_dir().unwrap_or_else(|| PathBuf::from(".")));
    #[cfg(test)]
    let mut path = crate::sync::stores::base_data_dir().join("Fluence");

    path.push("agents.json");
    path
}

pub fn load_store() -> AgentsStore {
    let path = agents_path();
    if !path.exists() {
        return AgentsStore::default();
    }
    match fs::read_to_string(&path) {
        Ok(data) => match serde_json::from_str::<AgentsStore>(&data) {
            Ok(mut store) => {
                // Fail-closed: unknown default ids reset to built-in.
                if !is_known_id(&store, &store.default_id.clone()) {
                    store.default_id = ID_BUILT_IN.to_string();
                }
                store
            }
            Err(e) => {
                log::warn!("Corrupt agents file, backing up and using defaults: {e:?}");
                backup_corrupt(&path);
                AgentsStore::default()
            }
        },
        Err(e) => {
            log::warn!("Failed to read agents file, using defaults: {e}");
            AgentsStore::default()
        }
    }
}

fn backup_corrupt(path: &PathBuf) {
    let mut target = path.with_extension("json.corrupt.json");
    let mut counter = 1;
    while target.exists() {
        target = path.with_extension(format!("json.corrupt.{counter}.json"));
        counter += 1;
    }
    if fs::rename(path, &target).is_err() && fs::copy(path, &target).is_ok() {
        let _ = fs::remove_file(path);
    }
}

pub fn save_store(store: &AgentsStore) -> Result<(), String> {
    let path = agents_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let data = serde_json::to_string_pretty(store).map_err(|e| e.to_string())?;
    // Unique tmp + real flush + rename, matching `settings::save_settings` and
    // `sync::stores::atomic_write`. Two defects lived here:
    //  - a FIXED `json.tmp` name, so two writers both wrote the same tmp and
    //    both renamed it — the loser renamed a tmp the winner had consumed, or
    //    renamed a PARTIALLY written tmp onto the destination, truncating a
    //    USER-DATA store;
    //  - `fs::File::open` is O_RDONLY, and on Windows `sync_all` maps to
    //    `FlushFileBuffers`, which needs `GENERIC_WRITE` — so the flush silently
    //    did nothing despite the durability intent.
    let tmp_path = {
        static TMP_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let seq = TMP_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut name = path.as_os_str().to_os_string();
        name.push(format!(".tmp.{}.{}", std::process::id(), seq));
        PathBuf::from(name)
    };
    let result = (|| -> Result<(), String> {
        fs::write(&tmp_path, &data).map_err(|e| e.to_string())?;
        if let Ok(f) = fs::OpenOptions::new().write(true).open(&tmp_path) {
            let _ = f.sync_all();
        }
        fs::rename(&tmp_path, &path).map_err(|e| e.to_string())
    })();
    if result.is_err() {
        // Its name is unpredictable, so nothing else can clean it up.
        let _ = fs::remove_file(&tmp_path);
    }
    result
}

pub fn sanitize_hint(hint: &str) -> String {
    let trimmed = hint.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    trimmed.chars().take(MAX_AGENT_HINT_LENGTH).collect()
}

pub fn sanitize_name(name: &str) -> String {
    name.replace('\r', "")
        .replace('\n', " ")
        .trim()
        .chars()
        .take(MAX_AGENT_NAME_LENGTH)
        .collect::<String>()
        .trim()
        .to_string()
}

pub fn is_known_id(store: &AgentsStore, id: &str) -> bool {
    id == ID_BUILT_IN || store.custom_agents.iter().any(|a| a.id == id)
}

/// Validate a custom agent name. Returns the error string, or None when OK.
/// Mirrors Android validateAgentName: non-blank, `builtin` reserved,
// case-insensitive duplicate check (excluding the agent being edited).
pub fn validate_agent_name(
    store: &AgentsStore,
    name: &str,
    excluding_id: Option<&str>,
) -> Option<String> {
    let clean = sanitize_name(name);
    if clean.is_empty() {
        return Some("Agent name is empty.".to_string());
    }
    if clean.to_lowercase() == ID_BUILT_IN {
        return Some("That name is reserved.".to_string());
    }
    let dup = store.custom_agents.iter().any(|a| {
        a.name.to_lowercase() == clean.to_lowercase() && Some(a.id.as_str()) != excluding_id
    });
    if dup {
        return Some("An agent with that name already exists.".to_string());
    }
    None
}

pub fn save_custom_agent(
    store: &mut AgentsStore,
    name: &str,
    hint: &str,
    id: Option<&str>,
) -> Option<CustomAgent> {
    let clean_name = sanitize_name(name);
    let clean_hint = sanitize_hint(hint);
    if clean_name.is_empty() || clean_hint.is_empty() {
        return None;
    }
    if validate_agent_name(store, &clean_name, id).is_some() {
        return None;
    }
    let agent_id = match id {
        Some(existing) if existing.starts_with("agent:") => existing.to_string(),
        _ => format!("agent:{}", uuid::Uuid::new_v4()),
    };
    let agent = CustomAgent {
        id: agent_id.clone(),
        name: clean_name,
        hint: clean_hint,
    };
    if let Some(pos) = store.custom_agents.iter().position(|a| a.id == agent_id) {
        store.custom_agents[pos] = agent.clone();
    } else {
        store.custom_agents.push(agent.clone());
    }
    agent.into()
}

/// Delete a custom agent. Built-in cannot be deleted. When the deleted
/// agent was the default, the default resets to built-in.
pub fn delete_custom_agent(store: &mut AgentsStore, id: &str) -> bool {
    if id == ID_BUILT_IN {
        return false;
    }
    let before = store.custom_agents.len();
    store.custom_agents.retain(|a| a.id != id);
    let removed = store.custom_agents.len() != before;
    if removed && store.default_id == id {
        store.default_id = ID_BUILT_IN.to_string();
    }
    removed
}

pub fn set_default_agent_id(store: &mut AgentsStore, id: &str) -> bool {
    if !is_known_id(store, id) {
        return false;
    }
    store.default_id = id.to_string();
    true
}

/// Resolve an agent id to its runtime form. Unknown/deleted ids fall back
/// to built-in with a None hint (never stuck, Android parity).
pub fn resolve_active_agent(store: &AgentsStore, id: Option<&str>) -> ResolvedAgent {
    let requested = id.unwrap_or(&store.default_id).to_string();
    if requested == ID_BUILT_IN {
        return ResolvedAgent {
            id: ID_BUILT_IN.to_string(),
            is_builtin: true,
            hint: None,
        };
    }
    if let Some(custom) = store.custom_agents.iter().find(|a| a.id == requested) {
        let hint = sanitize_hint(&custom.hint);
        return ResolvedAgent {
            id: custom.id.clone(),
            is_builtin: false,
            hint: if hint.is_empty() { None } else { Some(hint) },
        };
    }
    ResolvedAgent {
        id: ID_BUILT_IN.to_string(),
        is_builtin: true,
        hint: None,
    }
}

/// Hint lookup for the agent execution path (id -> sanitized hint).
pub fn resolve_hint(store: &AgentsStore, id: Option<&str>) -> Option<String> {
    resolve_active_agent(store, id).hint
}

// ── Phase 6: account-routed read/write paths ────────────────────────────────
//
// These mirror Android `AgentPreferences`. The legacy-store functions above
// stay (they serve the signed-out path and their unit tests); every production
// consumer — the Tauri commands below and `agent.rs::resolve_agent_hint` —
// goes through the routed versions, so no consumer can bypass admission.

/// The account whose namespace local reads, writes and deletes use: the
/// durable persisted sign-in identity (NOT a volatile in-memory flag — Windows
/// has no `tokenVerified` equivalent, and `active_account_hash` derives from
/// settings refreshed on sign-in/out, so cold starts and sync-off users route
/// correctly with no pass required). Uploads still demand live token
/// verification independently in the sync engine.
pub fn local_account_hash() -> Option<String> {
    crate::account_scope::active_account_hash()
}

/// Read the union of the active account's agents and the legacy device-local
/// ones, returning only what runtime consumers may execute. Tombstones are
/// excluded (a deleted agent must be neither listed nor resolvable); sync
/// never reads this function.
pub fn load_custom_agents() -> Vec<CustomAgent> {
    let hash = local_account_hash();
    let snapshot = crate::account_scope::VisibleAgents::load(hash.as_deref());
    snapshot
        .admitted()
        .into_iter()
        .filter(|r| match r {
            crate::account_scope::VisibleRecord::Legacy { .. } => true,
            crate::account_scope::VisibleRecord::Account(a) => a.deleted_at.is_none(),
        })
        .map(|r| match r {
            crate::account_scope::VisibleRecord::Legacy { id, name, hint } => {
                CustomAgent { id, name, hint }
            }
            crate::account_scope::VisibleRecord::Account(a) => CustomAgent {
                id: a.id,
                name: a.name,
                hint: a.hint,
            },
        })
        .collect()
}

/// True for the builtin id and admitted custom agents. An unassigned legacy
/// record is displayable but not known-executable.
pub fn is_known_agent(agent_id: &str) -> bool {
    if agent_id == ID_BUILT_IN {
        return true;
    }
    load_custom_agents().iter().any(|a| a.id == agent_id)
}

/// Resolve an agent id for execution through the admitted snapshot. Unknown,
/// deleted and unadmitted ids fall back to built-in (never stuck).
pub fn resolve_active_agent_routed(id: Option<&str>) -> ResolvedAgent {
    let hash = local_account_hash();
    let snapshot = crate::account_scope::VisibleAgents::load(hash.as_deref());
    snapshot.resolve(id)
}

/// Create or update an agent: account store when signed in (so it is Owned,
/// runnable and syncable immediately), legacy store when signed out.
///
/// `None` means the agent was NOT created or updated. Two ways to get there:
/// the inputs failed validation (empty after sanitize, duplicate name), or the
/// write to the account store failed. They are deliberately NOT split: the
/// signed-out branch below has always collapsed a failed `save_store` into
/// `None` the same way, and the command layer turns `None` into one error the
/// user sees instead of a phantom row.
///
/// The account branch MUST report an undurable write as `None`. It used to
/// return the new agent unconditionally, so an unwritable store (full volume,
/// `%LOCALAPPDATA%` ACL, an indexer holding the file — routine on Windows)
/// produced an agent the UI rendered and the user saw, which was then gone on
/// the next launch: silent data loss with nothing on disk and nothing logged.
pub fn save_custom_agent_routed(name: &str, hint: &str, id: Option<&str>) -> Option<CustomAgent> {
    let clean_name = sanitize_name(name);
    let clean_hint = sanitize_hint(hint);
    if clean_name.is_empty() || clean_hint.is_empty() {
        return None;
    }
    if validate_agent_name_routed(&clean_name, id).is_some() {
        return None;
    }
    let agent_id = match id {
        Some(existing) if existing.starts_with("agent:") => existing.to_string(),
        _ => format!("agent:{}", uuid::Uuid::new_v4()),
    };
    match local_account_hash() {
        Some(hash) => {
            // Durability gate: no `Some(agent)` unless the row reached the disk.
            if !crate::account_scope::upsert_account_agent(
                &hash,
                &agent_id,
                &clean_name,
                &clean_hint,
            ) {
                return None;
            }
            Some(CustomAgent {
                id: agent_id,
                name: clean_name,
                hint: clean_hint,
            })
        }
        None => {
            let mut store = load_store();
            let agent = CustomAgent {
                id: agent_id.clone(),
                name: clean_name,
                hint: clean_hint,
            };
            if let Some(pos) = store.custom_agents.iter().position(|a| a.id == agent_id) {
                store.custom_agents[pos] = agent.clone();
            } else {
                store.custom_agents.push(agent.clone());
            }
            save_store(&store).ok()?;
            Some(agent)
        }
    }
}

/// Validate a name against the ADMITTED set (not just the legacy file), so
/// duplicates are caught across both stores.
pub fn validate_agent_name_routed(name: &str, excluding_id: Option<&str>) -> Option<String> {
    let clean = sanitize_name(name);
    if clean.is_empty() {
        return Some("Agent name is empty.".to_string());
    }
    if clean.to_lowercase() == ID_BUILT_IN {
        return Some("That name is reserved.".to_string());
    }
    let dup = load_custom_agents().iter().any(|a| {
        a.name.to_lowercase() == clean.to_lowercase() && Some(a.id.as_str()) != excluding_id
    });
    if dup {
        return Some("An agent with that name already exists.".to_string());
    }
    None
}

/// Delete an agent: tombstone in the account store when signed in (so the
/// delete propagates instead of resurrecting), legacy removal when signed
/// out. Once the tombstone is durable, the same-id legacy shadow is removed
/// so a delete still deletes after sign-out.
///
/// Returns whether the delete became DURABLE — true only when the account
/// tombstone was actually persisted (or, signed out, the legacy row was
/// actually removed). Existence alone is deliberately NOT success: a delete
/// that could not be written has not happened, and reporting otherwise tells
/// the user their agent is gone while it is still there after a restart.
/// Also resets a deleted default to built-in, on success only.
pub fn delete_custom_agent_routed(id: &str) -> bool {
    if id == ID_BUILT_IN {
        return false;
    }
    let durable = match local_account_hash() {
        Some(hash) => {
            let before = crate::account_scope::load_account_agents(&hash).custom_agents;
            let existed = before.iter().any(|a| a.id == id);
            // The tombstone MUST be durable before the legacy shadow is
            // touched: on a failed write the legacy copy is the only copy
            // left, and removing it would lose the record entirely.
            let persisted = crate::account_scope::delete_account_agent(&hash, id);
            if existed && persisted {
                // Drop the same-id legacy shadow, mirroring Android.
                //
                // The claim COPIES rather than moves, so it leaves the legacy row in
                // place on purpose. Without this, deleting a claimed agent would
                // leave that row as a device-local record — visible and RUNNABLE
                // again the moment the user signed out. Deleting must still delete.
                //
                // Only safe here: the tombstone was just written, so the account store
                // provably held the record and any same-id legacy row is a shadow the
                // claim created. Idempotent, and it runs only AFTER the tombstone.
                let mut legacy = load_store();
                let before_legacy = legacy.custom_agents.len();
                legacy.custom_agents.retain(|a| a.id != id);
                if legacy.custom_agents.len() != before_legacy {
                    if let Err(e) = save_store(&legacy) {
                        // Tombstone IS durable, so the account copy survives and
                        // shadows this row: recoverable, not data loss. It only
                        // means the shadow can resurface as a device-local row
                        // after sign-out. Logged rather than silently dropped.
                        log::warn!(
                            "Deleted agent {} but could not drop its legacy shadow: {e}",
                            id
                        );
                    }
                }
            }
            // `existed && persisted`, not `persisted` alone: a delete aimed at an id
            // that was not there is not a durable delete either.
            existed && persisted
        }
        None => {
            let mut store = load_store();
            let before = store.custom_agents.len();
            store.custom_agents.retain(|a| a.id != id);
            let removed = store.custom_agents.len() != before;
            let mut persisted = false;
            if removed {
                // Durability, not just intent: dropping the row from memory is not
                // a delete. Returning `removed` unconditionally made a signed-out
                // delete report success while the row silently resurrected on the
                // next launch whenever the write failed (full volume, %LOCALAPPDATA%
                // ACL, an indexer holding the file). Mirrors the signed-in branch
                // above: `existed && persisted`.
                match save_store(&store) {
                    Ok(()) => persisted = true,
                    Err(e) => log::error!("Failed to persist agent deletion for {id}: {e}"),
                }
            }
            // `removed && persisted`, matching the account branch: a delete aimed
            // at an id that was not there is not a durable delete either.
            removed && persisted
        }
    };
    if durable {
        let mut store = load_store();
        if store.default_id == id {
            store.default_id = ID_BUILT_IN.to_string();
            let _ = save_store(&store);
        }
    }
    durable
}

// --- Tauri commands (registered in Slice 4b) ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentsView {
    pub builtin_id: String,
    pub builtin_name: String,
    pub builtin_description: String,
    pub default_id: String,
    pub custom_agents: Vec<CustomAgentView>,
}

/// One row of the agents board.
///
/// Deliberately a VIEW type, not an extra field on [`CustomAgent`]: `CustomAgent`
/// IS the on-disk legacy shape, so a claimability flag there would persist a
/// policy decision into `agents.json` and let a hand-edited legacy file assert
/// its own eligibility. `account_scope` computes the flag from the admission gate
/// and hands it in, so the frontend can never influence it.
///
/// Field names match `CustomAgent`, so existing consumers keep reading
/// `id`/`name`/`hint` unchanged; `claimable` is purely additive.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CustomAgentView {
    pub id: String,
    pub name: String,
    pub hint: String,
    /// True only for an unclaimed pre-account record while an account is signed
    /// in. False for signed-out rows (already usable, nothing to adopt) and for
    /// records already owned.
    #[serde(default)]
    pub claimable: bool,
}

#[tauri::command]
pub fn get_agents(window: tauri::Window) -> Result<AgentsView, String> {
    crate::acl::require_caller(
        &window,
        &[
            crate::acl::MAIN_WINDOW,
            crate::acl::OVERLAY_WINDOW,
            crate::acl::WIZARD_WINDOW,
        ],
    )?;
    let store = load_store();
    // Read the union ONCE, then thread that same snapshot through the view.
    // `default_id` stays device-local (D1c) and is passed through untouched.
    let snapshot = crate::account_scope::VisibleAgents::load(
        crate::account_scope::active_account_hash().as_deref(),
    );
    Ok(snapshot.into_view(store.default_id.clone()))
}

#[tauri::command]
pub fn save_agent(
    window: tauri::Window,
    name: String,
    hint: String,
    id: Option<String>,
) -> Result<CustomAgent, String> {
    crate::acl::require_caller(&window, &[crate::acl::MAIN_WINDOW])?;
    // Routed: account store when signed in (Owned, runnable, syncable), legacy
    // when signed out. Writing legacy while signed in would create an
    // immediately-unrunnable, never-synced record.
    match save_custom_agent_routed(&name, &hint, id.as_deref()) {
        Some(agent) => Ok(agent),
        None => Err("Could not save. Try a shorter name and hint.".to_string()),
    }
}

#[tauri::command]
pub fn delete_agent(window: tauri::Window, id: String) -> Result<(), String> {
    crate::acl::require_caller(&window, &[crate::acl::MAIN_WINDOW])?;
    // Routed: tombstone in the account store when signed in (propagates),
    // legacy removal when signed out.
    //
    // Existence is checked FIRST so the two failures stay distinguishable: a
    // missing id is "not found", while an id that exists but whose tombstone
    // could not be written is a real failure the user must be told about —
    // the record is untouched, and claiming success would leave it silently
    // reappearing after a restart.
    if id == ID_BUILT_IN || !is_known_agent(&id) {
        return Err("Agent not found.".to_string());
    }
    if !delete_custom_agent_routed(&id) {
        return Err("Could not save the delete. Nothing was lost — please try again.".to_string());
    }
    Ok(())
}

/// Adopt ONE unclaimed pre-account agent into the account signed in right now.
///
/// The destination account is resolved HERE from the durable session — it is
/// deliberately NOT a parameter. No caller and no frontend payload can name the
/// account to claim into, so the only reachable outcome is adoption into the
/// user's own current account, which is the only legitimate one. `only_ids` is
/// pinned to this single id: a per-row action must never move records the user
/// did not choose.
///
/// Fail-safe ordering, mirroring Android:
///  1. ownership is written to the account store, and reported only if that write
///     succeeded;
///  2. only then is the legacy copy dropped.
///
/// If step 1 fails, nothing is reported claimed and the legacy row is left
/// untouched — the record therefore always exists in at least one store, and a
/// failed cleanup instead leaves a shadow the union already dedupes away in
/// favour of the owned copy.
#[tauri::command]
pub fn claim_legacy_agent(
    window: tauri::Window,
    id: String,
) -> Result<crate::account_scope::ClaimOutcome, String> {
    crate::acl::require_caller(&window, &[crate::acl::MAIN_WINDOW])?;
    if id == ID_BUILT_IN {
        return Err("That agent is built in.".to_string());
    }
    let Some(hash) = local_account_hash() else {
        // Refuse rather than guess. Defence in depth: the core also claims nothing
        // without a valid hash, so there is no path that adopts while signed out.
        return Err("Sign in to add this agent to your account.".to_string());
    };
    let legacy = load_store().custom_agents;
    let only = std::collections::HashSet::from([id.clone()]);
    // NO legacy cleanup, by decision. The claim COPIES the record into the account;
    // it never MOVES it, so the pre-account row stays on disk.
    //
    // The account store is a whole-document read-modify-write and not every writer
    // shares the claim's `io_lock` — the sync pass notably loads the document,
    // performs a Drive round trip, and only then writes a `merged` payload built
    // from that pre-network snapshot (see `frozen.rs`). A claim landing in that
    // window is discarded by the pass. Had the claim also DELETED the legacy copy,
    // the record would then exist in NEITHER store: permanent loss. Leaving it
    // makes that outcome degrade instead — the row resurfaces as unassigned and
    // can simply be claimed again.
    //
    // Consistent with existing policy: a `skipped` (already owned) or `refused` id
    // already keeps its legacy row. Accepted cost, stated plainly: signing out
    // re-exposes the row as device-local and runnable, which is the user's own
    // pre-existing data and is already true today for skipped and refused rows.
    //
    // S6: the claim returns `Err` when the durable save fails, so a failed
    // claim surfaces as an error toast. It never returns an empty Ok that the
    // UI would render as "Nothing to add".
    crate::account_scope::claim_legacy_account_agents(
        Some(&hash),
        &legacy,
        Some(&only),
    )
}

#[tauri::command]
pub fn set_default_agent(window: tauri::Window, id: String) -> Result<(), String> {
    crate::acl::require_caller(&window, &[crate::acl::MAIN_WINDOW])?;
    // Gated on the admitted set: a default must never point at an
    // inadmissible (unassigned/tombstoned) record. The id itself stays
    // device-local (D1c).
    if !is_known_agent(&id) {
        return Err("Unknown agent.".to_string());
    }
    let mut store = load_store();
    store.default_id = id;
    save_store(&store)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_builtin() {
        let _guard = crate::sync::stores::store_test_guard();
        let store = AgentsStore::default();
        assert_eq!(store.default_id, ID_BUILT_IN);
        let resolved = resolve_active_agent(&store, None);
        assert!(resolved.is_builtin);
        assert!(resolved.hint.is_none());
    }

    #[test]
    fn save_and_resolve_custom() {
        let _guard = crate::sync::stores::store_test_guard();
        let mut store = AgentsStore::default();
        let agent =
            save_custom_agent(&mut store, "Translator", "always reply in Hindi", None).unwrap();
        assert!(agent.id.starts_with("agent:"));
        let resolved = resolve_active_agent(&store, Some(&agent.id));
        assert!(!resolved.is_builtin);
        assert_eq!(resolved.hint.as_deref(), Some("always reply in Hindi"));
    }

    #[test]
    fn unknown_id_falls_back_to_builtin() {
        let _guard = crate::sync::stores::store_test_guard();
        let store = AgentsStore::default();
        let resolved = resolve_active_agent(&store, Some("agent:missing"));
        assert!(resolved.is_builtin);
        assert_eq!(resolved.id, ID_BUILT_IN);
    }

    #[test]
    fn delete_resets_default_to_builtin() {
        let _guard = crate::sync::stores::store_test_guard();
        let mut store = AgentsStore::default();
        let agent = save_custom_agent(&mut store, "T", "hint text", None).unwrap();
        assert!(set_default_agent_id(&mut store, &agent.id));
        assert!(delete_custom_agent(&mut store, &agent.id));
        assert_eq!(store.default_id, ID_BUILT_IN);
    }

    #[test]
    fn builtin_cannot_be_deleted() {
        let _guard = crate::sync::stores::store_test_guard();
        let mut store = AgentsStore::default();
        assert!(!delete_custom_agent(&mut store, ID_BUILT_IN));
    }

    #[test]
    fn name_validation() {
        let mut store = AgentsStore::default();
        save_custom_agent(&mut store, "Translator", "hint text", None).unwrap();
        assert!(validate_agent_name(&store, "translator", None).is_some());
        assert!(validate_agent_name(&store, "builtin", None).is_some());
        assert!(validate_agent_name(&store, "  ", None).is_some());
        assert!(validate_agent_name(&store, "New Name", None).is_none());
    }

    #[test]
    fn sanitize_caps() {
        assert_eq!(sanitize_hint("  hi  "), "hi");
        let long = "a".repeat(MAX_AGENT_HINT_LENGTH + 10);
        assert_eq!(sanitize_hint(&long).chars().count(), MAX_AGENT_HINT_LENGTH);
        assert_eq!(sanitize_name("  Hello\nWorld  "), "Hello World");
    }

    // ---- one-tap legacy claim: legacy-store cleanup ----

    // ---- create durability: the routed path must not report a lost save ----
    //
    // `save_custom_agent_routed` is what `save_agent` hands to the frontend.
    // Its signed-in branch used to ignore the account-store result and return
    // the new agent unconditionally, so a failed disk write produced an agent
    // the UI rendered and the user saw — gone on the next launch, with nothing
    // on disk and nothing logged. `save_agent` already turns `None` into an
    // error for the frontend, so returning `None` here is what surfaces it.

    /// Sign in as `email` by writing the durable session key, and hand back a
    /// restore closure. The guard is held by the CALLER for the whole test:
    /// `sync_account_key` lives in the shared per-process settings file, so a
    /// parallel `save_settings` between the sign-in and the routed call would
    /// flip the branch back to legacy and the test would pass for the wrong
    /// reason.
    fn signed_in_as(email: &str) -> (String, Box<dyn FnOnce()>) {
        let mut settings = crate::settings::load_settings().unwrap_or_default();
        let previous = settings.sync_account_key.clone();
        settings.sync_account_key = Some(email.to_string());
        crate::settings::save_settings(&settings).unwrap();
        assert_eq!(
            local_account_hash().as_deref(),
            Some(crate::sync::metadata::account_hash_from_email(email).as_str()),
            "precondition: the routed path must take the ACCOUNT branch"
        );
        let restore: Box<dyn FnOnce()> = Box::new(move || {
            let mut s = crate::settings::load_settings().unwrap_or_default();
            s.sync_account_key = previous;
            let _ = crate::settings::save_settings(&s);
        });
        let hash = crate::sync::metadata::account_hash_from_email(email);
        (hash, restore)
    }

    /// Move `hash`'s account store aside and put a DIRECTORY in its place, so the
    /// atomic rename cannot land. The bytes are preserved, never deleted.
    fn block_store_aside(hash: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let target = crate::account_scope::account_path_for_test(
            &crate::account_scope::agents_base_dir_for_test(),
            "agents",
            hash,
        );
        let saved = target.with_extension("json.set-aside-by-test");
        let _ = std::fs::remove_file(&saved);
        std::fs::rename(&target, &saved).expect("must be able to set the store aside");
        std::fs::create_dir_all(&target).expect("must be able to block the target");
        (target, saved)
    }

    fn unblock_store(target: &std::path::Path, saved: &std::path::Path) {
        let _ = std::fs::remove_dir_all(target);
        std::fs::rename(saved, target).expect("must be able to restore the store");
    }

    #[test]
    fn a_routed_create_whose_account_write_fails_returns_none() {
        let _guard = crate::sync::stores::store_test_guard();
        let (hash, restore) = signed_in_as("agents-create-gate@example.com");

        // Healthy first, so the assertion below cannot pass vacuously.
        assert!(
            save_custom_agent_routed("Durable", "hint text", None).is_some(),
            "precondition: a writable store returns the created agent"
        );

        // Block exactly this account's file with a directory, so the atomic
        // rename cannot land. Every other account's store stays writable.
        // The store is set ASIDE, never deleted: it is this test's own
        // precondition and deleting it would hide a regression in it.
        let (target, saved) = block_store_aside(&hash);
        let result = save_custom_agent_routed("Lost", "hint text", None);
        unblock_store(&target, &saved);
        restore();

        assert!(
            result.is_none(),
            "a create that never reached the disk must not be reported as saved: {:?}",
            result.map(|a| a.name)
        );
    }

    /// A RENAME is the same code path as a create and must fail the same way:
    /// the user editing an existing agent's hint and being shown "saved" is the
    /// exact silent-loss scenario.
    #[test]
    fn a_routed_rename_whose_account_write_fails_returns_none() {
        let _guard = crate::sync::stores::store_test_guard();
        let (hash, restore) = signed_in_as("agents-rename-gate@example.com");

        let created =
            save_custom_agent_routed("Editable", "first hint", None).expect("must create");
        let (target, saved) = block_store_aside(&hash);

        let result = save_custom_agent_routed("Editable", "second hint", Some(&created.id));
        unblock_store(&target, &saved);
        restore();

        assert!(
            result.is_none(),
            "a rename that never reached the disk must not be reported as saved"
        );
    }

    /// The signed-OUT branch already gated on `save_store(&store).ok()?`; that
    /// behaviour is unchanged and must stay that way. Pinned so the signed-in
    /// gate is not later "simplified" by removing both.
    #[test]
    fn the_signed_out_create_still_reports_its_validation_failures() {
        let _guard = crate::sync::stores::store_test_guard();
        assert!(
            save_custom_agent_routed("", "hint", None).is_none(),
            "an empty name is a refusal"
        );
        assert!(
            save_custom_agent_routed("Valid Name", "", None).is_none(),
            "an empty hint is a refusal"
        );
        assert!(
            save_custom_agent_routed(ID_BUILT_IN, "hint", None).is_none(),
            "the builtin name is reserved"
        );
    }

    /// Sign OUT (clear the durable session key) and hand back a restore closure.
    /// Paired with [`signed_in_as`]: the routed delete/create branches on
    /// `local_account_hash()`, so exercising the signed-out path needs this.
    fn signed_out() -> Box<dyn FnOnce()> {
        let mut settings = crate::settings::load_settings().unwrap_or_default();
        let previous = settings.sync_account_key.clone();
        settings.sync_account_key = None;
        crate::settings::save_settings(&settings).unwrap();
        assert!(
            local_account_hash().is_none(),
            "precondition: the routed path must take the LEGACY branch"
        );
        Box::new(move || {
            let mut s = crate::settings::load_settings().unwrap_or_default();
            s.sync_account_key = previous;
            let _ = crate::settings::save_settings(&s);
        })
    }

    /// Make the legacy `save_store` FAIL while leaving the store READABLE.
    ///
    /// Block the write by making the DESTINATION read-only, NOT by occupying a
    /// tmp name. `save_store` now names its tmp per writer
    /// (`agents.json.tmp.<pid>.<seq>`), so occupying the old fixed
    /// `agents.json.tmp` blocks nothing at all — the helper silently becomes a
    /// no-op, the delete reports success, and this test passes for the wrong
    /// reason, which is exactly the failure mode its own doc warns about. (It
    /// really did: the test passed in the full suite only because a concurrent
    /// test happened to block the same shared store, and failed in isolation.)
    ///
    /// A read-only destination makes `fs::rename(tmp, path)` fail on Windows
    /// while leaving the file fully readable, so `load_store` still sees the row
    /// — the property the assertions below depend on.
    ///
    /// Moving `agents.json` aside instead would NOT work: `load_store` would
    /// then return an EMPTY store, `retain` would remove nothing, and `removed`
    /// would be false for the wrong reason.
    ///
    /// Returns the store path; the caller MUST restore write permission.
    fn block_legacy_store_aside() -> std::path::PathBuf {
        let path = agents_path();
        #[cfg(target_os = "windows")]
        {
            let mut perms = std::fs::metadata(&path)
                .expect("precondition: the legacy store must exist")
                .permissions();
            perms.set_readonly(true);
            std::fs::set_permissions(&path, perms).expect("must be able to set read-only");
            assert!(
                path.exists(),
                "the store must still be READABLE, or the delete has nothing to remove"
            );
            path
        }
        #[cfg(not(target_os = "windows"))]
        {
            let dir = path
                .parent()
                .expect("store path must have a parent dir")
                .to_path_buf();
            let mut perms = std::fs::metadata(&dir)
                .expect("precondition: the store dir must exist")
                .permissions();
            perms.set_readonly(true);
            std::fs::set_permissions(&dir, perms).expect("must be able to set read-only");
            assert!(
                path.exists(),
                "the store must still be READABLE, or the delete has nothing to remove"
            );
            dir
        }
    }

    /// A name unique per run.
    ///
    /// These tests deliberately leave state behind: the blocked-delete test
    /// creates a row and then cannot persist its deletion, and the legacy store
    /// lives in the shared test data directory and SURVIVES between runs. A
    /// fixed name would therefore collide with the previous run's orphan on the
    /// next run and be refused by `validate_agent_name_routed` as a duplicate.
    fn unique_name(prefix: &str) -> String {
        format!("{prefix}{}", uuid::Uuid::new_v4().simple())
    }

    /// A signed-out DELETE must report failure when the write fails.
    ///
    /// The signed-out branch dropped the row from memory and returned `removed`
    /// without checking the write, so the UI showed a successful delete while
    /// the row silently resurrected on the next launch. Mirrors the signed-in
    /// branch's `existed && persisted` gate.
    #[test]
    fn a_signed_out_delete_whose_legacy_write_fails_is_not_reported_as_deleted() {
        let _guard = crate::sync::stores::store_test_guard();
        let restore = signed_out();

        let victim = save_custom_agent_routed(&unique_name("SignedOutVictim"), "hint text", None)
            .expect("precondition: a signed-out create must succeed");
        // The row really is on disk and readable, so `removed` below is true and
        // the ONLY thing that can falsify the delete is the failed write.
        assert!(
            agents_path().exists(),
            "precondition: the legacy store must exist and be readable"
        );

        let blocked = block_legacy_store_aside();
        let durable = delete_custom_agent_routed(&victim.id);
        // Restore write permission FIRST, so no read-only state leaks even if an
        // assert below fails.
        if let Ok(meta) = std::fs::metadata(&blocked) {
            let mut perms = meta.permissions();
            perms.set_readonly(false);
            let _ = std::fs::set_permissions(&blocked, perms);
        }
        restore();

        assert!(
            !durable,
            "a delete that never reached the disk must not be reported as done"
        );
    }

    /// The same delete against a HEALTHY store must still report success, so
    /// the guard above cannot be satisfied by simply refusing every delete.
    #[test]
    fn a_signed_out_delete_against_a_writable_store_still_reports_success() {
        let _guard = crate::sync::stores::store_test_guard();
        let restore = signed_out();

        let victim = save_custom_agent_routed(&unique_name("SignedOutHealthy"), "hint text", None)
            .expect("precondition: a signed-out create must succeed");
        let durable = delete_custom_agent_routed(&victim.id);
        restore();

        assert!(durable, "a writable signed-out delete must report success");
    }
}
