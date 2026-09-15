// Fluence Windows - Windows Credential Manager integration
// Securely stores API keys using the Windows Credential Manager API.

use anyhow::{anyhow, Result};

#[cfg(target_os = "windows")]
use windows::{
    core::{PCWSTR, PWSTR},
    Win32::Security::Credentials::{
        CredDeleteW, CredFree, CredReadW, CredWriteW, CREDENTIALW, CRED_FLAGS,
        CRED_PERSIST_ENTERPRISE, CRED_TYPE_GENERIC,
    },
};

const CREDENTIAL_NAMESPACE: &str = "Fluence/";

/// Validate that a credential target is one the renderer may name:
/// the two legacy global slots or a per-preset subpath with a strict
/// `[a-z0-9_]+` suffix. Anything else — including `Fluence/Sync/*` and
/// arbitrary subpaths — is rejected, so one window cannot probe or
/// squat unrelated credential slots through the generic IPC namespace.
fn validate_credential_target(target: &str) -> Result<()> {
    if !target.starts_with(CREDENTIAL_NAMESPACE) {
        return Err(anyhow!(
            "Access denied: only Fluence credentials can be accessed"
        ));
    }
    if target.contains("..") {
        return Err(anyhow!("Invalid credential target"));
    }
    if target == STT_API_KEY_TARGET || target == LLM_API_KEY_TARGET {
        return Ok(());
    }
    for base in [STT_API_KEY_TARGET, LLM_API_KEY_TARGET] {
        if let Some(suffix) = target.strip_prefix(&format!("{base}/")) {
            if !suffix.is_empty()
                && suffix
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
            {
                return Ok(());
            }
        }
    }
    Err(anyhow!("Access denied: unknown credential target"))
}

fn to_wide(s: &str) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    std::ffi::OsStr::new(s)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// Store an API key in Windows Credential Manager
/// Note: existing credentials written with CRED_PERSIST_LOCAL_MACHINE remain
/// machine-visible until the next successful `store_credential` for that target,
/// which rewrites them as CRED_PERSIST_ENTERPRISE (per-user/per-enterprise).
/// No automatic migration is performed - the window closes on first re-save/re-auth.
#[cfg(target_os = "windows")]
pub fn store_credential(target: &str, username: &str, secret: &str) -> Result<()> {
    let target_wide = to_wide(target);
    let username_wide = to_wide(username);
    let secret_bytes = secret.as_bytes();

    let credential = CREDENTIALW {
        Flags: CRED_FLAGS(0),
        Type: CRED_TYPE_GENERIC,
        TargetName: PWSTR(target_wide.as_ptr() as *mut u16),
        Comment: PWSTR::null(),
        LastWritten: windows::Win32::Foundation::FILETIME::default(),
        CredentialBlobSize: secret_bytes.len() as u32,
        CredentialBlob: secret_bytes.as_ptr() as *mut u8,
        Persist: CRED_PERSIST_ENTERPRISE,
        AttributeCount: 0,
        Attributes: std::ptr::null_mut(),
        TargetAlias: PWSTR::null(),
        UserName: PWSTR(username_wide.as_ptr() as *mut u16),
    };

    unsafe {
        CredWriteW(&credential, 0).map_err(|e| anyhow!("CredWriteW failed: {}", e))?;
    }
    Ok(())
}

/// Read an API key from Windows Credential Manager
#[cfg(target_os = "windows")]
pub fn read_credential(target: &str) -> Result<String> {
    let target_wide = to_wide(target);
    let mut pcred: *mut CREDENTIALW = std::ptr::null_mut();

    unsafe {
        CredReadW(
            PCWSTR(target_wide.as_ptr()),
            CRED_TYPE_GENERIC,
            0,
            &mut pcred,
        )
        .map_err(|e| anyhow!("CredReadW failed: {}", e))?;

        if pcred.is_null() {
            return Err(anyhow!("Credential not found: {}", target));
        }

        let cred = &*pcred;
        let blob =
            std::slice::from_raw_parts(cred.CredentialBlob, cred.CredentialBlobSize as usize);
        let secret = String::from_utf8_lossy(blob).to_string();
        CredFree(pcred as *mut _);
        Ok(secret)
    }
}

/// Delete a credential from Windows Credential Manager
#[cfg(target_os = "windows")]
pub fn delete_credential(target: &str) -> Result<()> {
    let target_wide = to_wide(target);
    unsafe {
        CredDeleteW(PCWSTR(target_wide.as_ptr()), CRED_TYPE_GENERIC, 0)
            .map_err(|e| anyhow!("CredDeleteW failed: {}", e))?;
    }
    Ok(())
}

// Non-Windows stubs (for compilation on other platforms)
#[cfg(not(target_os = "windows"))]
pub fn store_credential(_target: &str, _username: &str, _secret: &str) -> Result<()> {
    Err(anyhow!("Credential Manager not supported on this platform"))
}

#[cfg(not(target_os = "windows"))]
pub fn read_credential(_target: &str) -> Result<String> {
    Err(anyhow!("Credential Manager not supported on this platform"))
}

#[cfg(not(target_os = "windows"))]
pub fn delete_credential(_target: &str) -> Result<()> {
    Err(anyhow!("Credential Manager not supported on this platform"))
}

// Credential target name constants
pub const STT_API_KEY_TARGET: &str = "Fluence/STT_ApiKey";
pub const LLM_API_KEY_TARGET: &str = "Fluence/LLM_ApiKey";
/// OAuth refresh token for sync (spec §24) - stored in Credential Manager,
/// never in a file; the access token stays in memory only.
pub const SYNC_REFRESH_TOKEN_TARGET: &str = "Fluence/Sync/RefreshToken";

/// Persist the sync refresh token (overwrites the previous one, if any).
pub fn store_sync_refresh_token(token: &str) -> Result<()> {
    store_credential(SYNC_REFRESH_TOKEN_TARGET, "fluence", token)
}

/// Read the sync refresh token. `Err` means the user must sign in again.
pub fn read_sync_refresh_token() -> Result<String> {
    read_credential(SYNC_REFRESH_TOKEN_TARGET)
}

/// Forget the sync refresh token (sign-out).
pub fn delete_sync_refresh_token() -> Result<()> {
    delete_credential(SYNC_REFRESH_TOKEN_TARGET)
}

fn sanitize_preset(preset: &str) -> String {
    // Whitelist: only a-z0-9_ after lowercasing and space→underscore.
    // Prevents `../` or `a/b/c` style preset injecting extra path segments
    // that would still pass validate_credential_target (which only checks `..`).
    let lowered = preset.to_lowercase().replace(' ', "_");
    lowered
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' })
        .collect()
}

/// Generate a provider-specific target for STT keys
pub fn get_stt_target(preset: &str) -> String {
    format!("{}/{}", STT_API_KEY_TARGET, sanitize_preset(preset))
}

/// Generate a provider-specific target for LLM keys
pub fn get_llm_target(preset: &str) -> String {
    format!("{}/{}", LLM_API_KEY_TARGET, sanitize_preset(preset))
}

// Tauri commands (caller-gated per src-tauri/src/acl.rs inventory)
#[tauri::command]
pub fn save_api_key(window: tauri::Window, target: String, key: String) -> Result<(), String> {
    crate::acl::require_caller(
        &window,
        &[crate::acl::MAIN_WINDOW, crate::acl::WIZARD_WINDOW],
    )?;
    if crate::acl::is_sync_credential_target(&target) {
        return Err("Sync credentials are managed by the sync scheduler, not IPC".to_string());
    }
    validate_credential_target(&target).map_err(|e| e.to_string())?;
    store_credential(&target, "fluence", &key).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_stt_target() {
        assert!(validate_credential_target("Fluence/STT_ApiKey").is_ok());
    }

    #[test]
    fn valid_llm_target() {
        assert!(validate_credential_target("Fluence/LLM_ApiKey").is_ok());
    }

    #[test]
    fn valid_provider_specific() {
        assert!(validate_credential_target("Fluence/STT_ApiKey/groq").is_ok());
        assert!(validate_credential_target("Fluence/LLM_ApiKey/openai").is_ok());
        assert!(validate_credential_target("Fluence/LLM_ApiKey/my_provider").is_ok());
    }

    #[test]
    fn reject_empty_target() {
        assert!(validate_credential_target("").is_err());
    }

    #[test]
    fn reject_non_fluence_namespace() {
        assert!(validate_credential_target("OtherApp/Apikey").is_err());
        assert!(validate_credential_target("Mozilla/").is_err());
        assert!(validate_credential_target("Google/Chrome/Login").is_err());
    }

    #[test]
    fn reject_path_traversal() {
        assert!(validate_credential_target("Fluence/../etc/passwd").is_err());
        assert!(validate_credential_target("Fluence/STT_ApiKey/../../../secret").is_err());
    }

    #[test]
    fn reject_no_namespace_prefix() {
        assert!(validate_credential_target("STT_ApiKey").is_err());
        assert!(validate_credential_target("api_key").is_err());
    }

    #[test]
    fn reject_prefix_spoof() {
        assert!(validate_credential_target("FluenceX/Apikey").is_err());
        assert!(validate_credential_target("fluence/Apikey").is_err());
    }

    #[test]
    fn reject_arbitrary_subpath() {
        // The generic Fluence/* namespace is closed: only the known
        // STT/LLM slots (plus strict per-preset suffixes) are nameable.
        assert!(validate_credential_target("Fluence/any/sub/path").is_err());
        assert!(validate_credential_target("Fluence/Sync/RefreshToken").is_err());
        assert!(validate_credential_target("Fluence/STT_ApiKey/groq/extra").is_err());
        assert!(validate_credential_target("Fluence/LLM_ApiKey/GROQ").is_err());
        assert!(validate_credential_target("Fluence/LLM_ApiKey/groq-key").is_err());
        assert!(validate_credential_target("Fluence/STT_ApiKey/").is_err());
    }

    #[test]
    fn valid_per_preset_targets() {
        assert!(validate_credential_target("Fluence/STT_ApiKey/groq").is_ok());
        assert!(validate_credential_target("Fluence/LLM_ApiKey/local_offline").is_ok());
        assert!(validate_credential_target("Fluence/LLM_ApiKey/custom").is_ok());
    }

    #[test]
    fn get_stt_target_format() {
        let t = get_stt_target("groq");
        assert_eq!(t, "Fluence/STT_ApiKey/groq");
    }

    #[test]
    fn get_llm_target_format() {
        let t = get_llm_target("openai");
        assert_eq!(t, "Fluence/LLM_ApiKey/openai");
    }

    #[test]
    fn get_stt_target_spaces_to_underscores() {
        let t = get_stt_target("My Provider");
        assert_eq!(t, "Fluence/STT_ApiKey/my_provider");
    }
}

#[tauri::command]
pub fn get_api_key(window: tauri::Window, target: String) -> Result<String, String> {
    if crate::acl::is_sync_credential_target(&target) {
        return Err("Sync credentials are managed by the sync scheduler, not IPC".to_string());
    }
    // Task 4 moved overlay agent mode server-side: no renderer window
    // besides main/wizard may read credentials anymore.
    crate::acl::require_caller(
        &window,
        &[crate::acl::MAIN_WINDOW, crate::acl::WIZARD_WINDOW],
    )?;
    read_api_key_target(&target)
}

/// Direct credential read without migration fallbacks (exact slot only).
fn read_exact_slot(target: &str) -> Option<String> {
    read_credential(target)
        .ok()
        .filter(|key| !key.trim().is_empty())
}

/// Server-side credential read (backend use only — bypasses window gates).
/// Used by `get_llm/stt_api_key_or_err` so workflows never depend on IPC.
/// Includes the legacy migration fallbacks (global slot, groq cross-slot)
/// so existing installs keep working after the per-preset migration.
pub(crate) fn read_api_key_target(target: &str) -> Result<String, String> {
    validate_credential_target(target).map_err(|e| e.to_string())?;

    // 1. Try the specific target requested
    if let Some(key) = read_exact_slot(target) {
        return Ok(key);
    }

    // 2. Fallback: If it's a provider-specific target, check the legacy global slot
    if target.contains('/') {
        let base = if target.starts_with(STT_API_KEY_TARGET) {
            Some(STT_API_KEY_TARGET)
        } else if target.starts_with(LLM_API_KEY_TARGET) {
            Some(LLM_API_KEY_TARGET)
        } else {
            None
        };

        if let Some(base_target) = base {
            if let Ok(legacy_key) = read_credential(base_target) {
                if !legacy_key.trim().is_empty() {
                    log::info!(
                        "Found legacy key in global slot, using for: {} (legacy fallback, not auto-migrating)",
                        target
                    );
                    // Do NOT auto-migrate to per-preset slot: prevents cross-preset contamination
                    // (e.g., global openai key being persisted as groq). User should re-save per-preset explicitly.
                    return Ok(legacy_key);
                }
            }
        }

        // 2b. Android-compatible Groq fallback: LLM and STT groq share a single user key.
        // If LLM_ApiKey/groq is missing, try STT_ApiKey/groq and vice versa.
        // This is safe only for the canonical "groq" preset - custom presets remain isolated.
        let groq_llm = format!("{}/groq", LLM_API_KEY_TARGET);
        let groq_stt = format!("{}/groq", STT_API_KEY_TARGET);
        if target == groq_llm {
            if let Ok(k) = read_credential(&groq_stt) {
                if !k.trim().is_empty() {
                    log::info!("Groq LLM key missing, using STT groq key as fallback");
                    return Ok(k);
                }
            }
            if let Ok(k) = read_credential(LLM_API_KEY_TARGET) {
                if !k.trim().is_empty() {
                    return Ok(k);
                }
            }
        } else if target == groq_stt {
            if let Ok(k) = read_credential(&groq_llm) {
                if !k.trim().is_empty() {
                    log::info!("Groq STT key missing, using LLM groq key as fallback");
                    return Ok(k);
                }
            }
        }
    }

    // 3. Final attempt at the raw target (or return the read error)
    read_credential(&target).map_err(|e| e.to_string())
}

/// Helper for Agent/LLM paths: returns a user-facing error for missing credentials
pub fn get_llm_api_key_or_err(preset: &str) -> Result<String, String> {
    let target = get_llm_target(preset);
    match read_api_key_target(&target) {
        Ok(k) if !k.trim().is_empty() => Ok(k),
        _ => Err(format!(
            "Missing API key for LLM provider '{}'. Open Settings → Providers → LLM → Save key.",
            preset
        )),
    }
}

pub fn get_stt_api_key_or_err(preset: &str) -> Result<String, String> {
    let target = get_stt_target(preset);
    match read_api_key_target(&target) {
        Ok(k) if !k.trim().is_empty() => Ok(k),
        _ => Err(format!(
            "Missing API key for STT provider '{}'. Open Settings → Providers → STT → Save key.",
            preset
        )),
    }
}

#[tauri::command]
pub fn delete_api_key(window: tauri::Window, target: String) -> Result<(), String> {
    crate::acl::require_caller(&window, &[crate::acl::MAIN_WINDOW])?;
    if crate::acl::is_sync_credential_target(&target) {
        return Err("Sync credentials are managed by the sync scheduler, not IPC".to_string());
    }
    validate_credential_target(&target).map_err(|e| e.to_string())?;
    delete_credential(&target).map_err(|e| e.to_string())
}
