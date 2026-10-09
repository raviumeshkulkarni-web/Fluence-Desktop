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
    let mut path = dirs::data_local_dir().unwrap_or_else(|| PathBuf::from("."));
    path.push("Fluence");
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
    let tmp_path = path.with_extension("json.tmp");
    fs::write(&tmp_path, &data).map_err(|e| e.to_string())?;
    if let Ok(f) = fs::File::open(&tmp_path) {
        let _ = f.sync_all();
    }
    fs::rename(&tmp_path, &path).map_err(|e| e.to_string())?;
    Ok(())
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
            crate::account_scope::upsert_account_agent(&hash, &agent_id, &clean_name, &clean_hint);
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
            if removed {
                let _ = save_store(&store);
            }
            removed
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
    Ok(crate::account_scope::claim_legacy_account_agents(
        Some(&hash),
        &legacy,
        Some(&only),
    ))
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
        let store = AgentsStore::default();
        assert_eq!(store.default_id, ID_BUILT_IN);
        let resolved = resolve_active_agent(&store, None);
        assert!(resolved.is_builtin);
        assert!(resolved.hint.is_none());
    }

    #[test]
    fn save_and_resolve_custom() {
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
        let store = AgentsStore::default();
        let resolved = resolve_active_agent(&store, Some("agent:missing"));
        assert!(resolved.is_builtin);
        assert_eq!(resolved.id, ID_BUILT_IN);
    }

    #[test]
    fn delete_resets_default_to_builtin() {
        let mut store = AgentsStore::default();
        let agent = save_custom_agent(&mut store, "T", "hint text", None).unwrap();
        assert!(set_default_agent_id(&mut store, &agent.id));
        assert!(delete_custom_agent(&mut store, &agent.id));
        assert_eq!(store.default_id, ID_BUILT_IN);
    }

    #[test]
    fn builtin_cannot_be_deleted() {
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
}
