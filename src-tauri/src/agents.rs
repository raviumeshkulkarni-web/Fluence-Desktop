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
            Some(CustomAgent { id: agent_id, name: clean_name, hint: clean_hint })
        }
        None => {
            let mut store = load_store();
            let agent = CustomAgent { id: agent_id.clone(), name: clean_name, hint: clean_hint };
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
/// out. A same-id legacy row is left alone when signed in. Returns true when
/// something was deleted; also resets a deleted default to built-in.
pub fn delete_custom_agent_routed(id: &str) -> bool {
    if id == ID_BUILT_IN {
        return false;
    }
    let removed = match local_account_hash() {
        Some(hash) => {
            let before = crate::account_scope::load_account_agents(&hash).custom_agents;
            crate::account_scope::delete_account_agent(&hash, id);
            before.iter().any(|a| a.id == id)
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
    if removed {
        let mut store = load_store();
        if store.default_id == id {
            store.default_id = ID_BUILT_IN.to_string();
            let _ = save_store(&store);
        }
    }
    removed
}

// --- Tauri commands (registered in Slice 4b) ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentsView {
    pub builtin_id: String,
    pub builtin_name: String,
    pub builtin_description: String,
    pub default_id: String,
    pub custom_agents: Vec<CustomAgent>,
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
    if !delete_custom_agent_routed(&id) {
        return Err("Agent not found.".to_string());
    }
    Ok(())
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
}
