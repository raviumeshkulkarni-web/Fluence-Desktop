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

#[cfg(target_os = "windows")]
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

#[cfg(target_os = "linux")]
pub fn store_credential(target: &str, _username: &str, secret: &str) -> Result<()> {
    let entry =
        keyring::Entry::new("Fluence", target).map_err(|e| anyhow!("keyring open failed: {e}"))?;
    entry
        .set_password(secret)
        .map_err(|e| anyhow!("Secret Service store failed: {e}"))
}

#[cfg(target_os = "linux")]
pub fn read_credential(target: &str) -> Result<String> {
    let entry =
        keyring::Entry::new("Fluence", target).map_err(|e| anyhow!("keyring open failed: {e}"))?;
    entry
        .get_password()
        .map_err(|e| anyhow!("Secret Service read failed for {target}: {e}"))
}

#[cfg(target_os = "linux")]
pub fn delete_credential(target: &str) -> Result<()> {
    let entry =
        keyring::Entry::new("Fluence", target).map_err(|e| anyhow!("keyring open failed: {e}"))?;
    match entry.delete_credential() {
        Ok(()) => Ok(()),
        Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(anyhow!("Secret Service delete failed for {target}: {e}")),
    }
}

// Non-Windows stubs (for compilation on other platforms)
#[cfg(not(any(target_os = "windows", target_os = "linux")))]
pub fn store_credential(_target: &str, _username: &str, _secret: &str) -> Result<()> {
    Err(anyhow!("Credential Manager not supported on this platform"))
}

#[cfg(not(any(target_os = "windows", target_os = "linux")))]
pub fn read_credential(_target: &str) -> Result<String> {
    Err(anyhow!("Credential Manager not supported on this platform"))
}

#[cfg(not(any(target_os = "windows", target_os = "linux")))]
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

/// Canonical preset slug for credential targets (FIX-02 contract).
/// Rules, in order: Unicode-lowercase, replace every ASCII space with `_`,
/// then map any char that is not `[a-z0-9_]` to `_`.
/// The frontend (`keyTarget`/`canonicalPresetSlug` in `web/src/ipc/providers.ts`
/// and `src/js/settings.js`) implements these exact rules, so both sides name
/// the same slot for save, read, delete, fallback resolution, and the secure
/// Agent Mode lookup (`get_llm/stt_target` below).
/// Collision policy: slugs that differ only by mapped characters share one
/// slot (e.g. `my-provider` and `my_provider` both resolve to `my_provider`;
/// last write wins). This is accepted because preset ids are product-controlled
/// (`groq`, `openai`, `mistral`, `custom`, `Local Offline` — all collision-free)
/// and the alternative (rejecting on write but reading a remapped slot) would
/// reintroduce save/read asymmetry.
/// Security: the output alphabet keeps every generated target inside
/// `validate_credential_target` (no `/`, `.`, or `..` can survive), and IPC
/// callers are still validated strictly — a non-canonical target sent over IPC
/// is rejected, never silently remapped.
fn sanitize_preset(preset: &str) -> String {
    let lowered = preset.to_lowercase().replace(' ', "_");
    lowered
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
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

    #[cfg(target_os = "linux")]
    #[test]
    fn secret_service_store_read_delete_roundtrip() {
        let target = get_llm_target(&format!("test_rt_{}", std::process::id()));
        validate_credential_target(&target).unwrap();

        let open = || {
            keyring::Entry::new("Fluence", &target).map_err(|e| format!("open: {e}"))
        };
        let Ok(entry) = open() else {
            eprintln!("SKIP: no Secret Service daemon reachable");
            return;
        };
        let probe = entry.set_password("probe");
        if probe.is_err() {
            eprintln!("SKIP: Secret Service not writable here ({})", probe.unwrap_err());
            return;
        }

        let secret = "fluence-linux-test-secret-äöü";
        struct Cleanup<'a> {
            target: &'a str,
        }
        impl Drop for Cleanup<'_> {
            fn drop(&mut self) {
                let _ = delete_credential(self.target);
            }
        }
        let _cleanup = Cleanup { target: &target };
        store_credential(&target, "fluence", secret).expect("store must succeed");
        let back = read_credential(&target).expect("read must succeed");
        assert_eq!(back, secret);
        delete_credential(&target).expect("delete must succeed");
        assert!(read_credential(&target).is_err());
    }
    // FIX-02: canonical slug matrix. Every generated target must pass the
    // strict validator (save/read/delete symmetry), and the frontend
    // `canonicalPresetSlug` must produce the identical slug (see
    // tests/credential-target-parity.test.mjs for the cross-side check).
    #[test]
    fn canonical_slug_builtin_presets_unchanged() {
        for preset in ["groq", "openai", "mistral", "custom"] {
            assert_eq!(
                get_llm_target(preset),
                format!("Fluence/LLM_ApiKey/{preset}")
            );
            assert_eq!(
                get_stt_target(preset),
                format!("Fluence/STT_ApiKey/{preset}")
            );
        }
        // "Local Offline" keeps its historical slot.
        assert_eq!(
            get_stt_target("Local Offline"),
            "Fluence/STT_ApiKey/local_offline"
        );
    }

    #[test]
    fn canonical_slug_hyphen_space_case_folded() {
        assert_eq!(
            get_llm_target("deep-infra"),
            "Fluence/LLM_ApiKey/deep_infra"
        );
        assert_eq!(
            get_llm_target("deep_infra"),
            "Fluence/LLM_ApiKey/deep_infra"
        );
        assert_eq!(
            get_llm_target("Deep Infra"),
            "Fluence/LLM_ApiKey/deep_infra"
        );
        assert_eq!(get_llm_target("GROQ"), "Fluence/LLM_ApiKey/groq");
    }

    #[test]
    fn canonical_slug_strips_traversal_characters() {
        for preset in ["a/b", "../x", "..\\..\\secret", "C:\\keys", "/abs", "a:b"] {
            let slug = sanitize_preset(preset);
            assert!(
                !slug.contains('/')
                    && !slug.contains('\\')
                    && !slug.contains(':')
                    && !slug.contains('.'),
                "slug for {preset:?} must not carry path characters, got {slug:?}"
            );
            assert!(
                validate_credential_target(&get_llm_target(preset)).is_ok(),
                "generated target for {preset:?} must validate"
            );
        }
    }

    #[test]
    fn canonical_slug_collision_is_documented_last_write_wins() {
        // `my-provider` and `my_provider` intentionally share one slot.
        assert_eq!(get_llm_target("my-provider"), get_llm_target("my_provider"));
        assert_eq!(
            get_llm_target("my-provider"),
            "Fluence/LLM_ApiKey/my_provider"
        );
    }

    #[test]
    fn canonical_slug_empty_preset_fails_closed() {
        // An empty slug must never become a valid target: IPC callers sending
        // it are rejected instead of being silently remapped.
        assert!(validate_credential_target(&get_llm_target("")).is_err());
        // Whitespace-only input folds to underscores — still strictly inside
        // the namespace, and the frontend folds it identically (`___`).
        assert_eq!(sanitize_preset("   "), "___");
        assert!(validate_credential_target(&get_stt_target("   ")).is_ok());
    }

    // FIX-01: the secure Agent Mode path resolves through
    // `get_llm_api_key_or_err`, so a missing key must surface the stable
    // actionable message — never a raw Credential Manager / OS error.
    #[test]
    fn missing_llm_key_error_is_actionable_not_raw() {
        let preset = "test_nonexistent_preset_xyz_abc";
        match get_llm_api_key_or_err(preset) {
            Err(e) => {
                assert!(e.contains("Missing API key"), "got: {e}");
                assert!(e.contains(preset), "preset context missing: {e}");
                for raw in [
                    "CredReadW",
                    "CredWriteW",
                    "os error",
                    "Credential Manager",
                    "Access denied",
                ] {
                    assert!(!e.contains(raw), "raw backend detail leaked: {e}");
                }
            }
            Ok(_) => println!("SKIP: unexpected key present for {preset}"),
        }
    }

    #[test]
    fn missing_stt_key_error_is_actionable_not_raw() {
        let preset = "test_nonexistent_preset_xyz_abc";
        match get_stt_api_key_or_err(preset) {
            Err(e) => {
                assert!(e.contains("Missing API key"), "got: {e}");
                assert!(e.contains(preset), "preset context missing: {e}");
            }
            Ok(_) => println!("SKIP: unexpected key present for {preset}"),
        }
    }

    // Windows runtime proof for FIX-02 (hyphenated preset lifecycle) and the
    // FIX-01 missing-key message, against the REAL Credential Manager.
    // Opt-in only (`cargo test -- --ignored`): it writes and deletes a real
    // credential, so it must never run as part of the normal suite.
    #[test]
    #[ignore]
    #[cfg(target_os = "windows")]
    fn windows_credential_manager_canonical_roundtrip() {
        // Hyphenated preset exercises the canonical fold end to end.
        let preset = "test-rt-deep-infra";
        let target = get_llm_target(preset);
        assert_eq!(target, "Fluence/LLM_ApiKey/test_rt_deep_infra");
        // Start clean and always leave clean (best effort).
        let _ = delete_credential(&target);
        let secret = "fluence-test-secret-value";
        store_credential(&target, "fluence", secret).expect("test cred must store");
        // Read back through the same server-side path the secure Agent Mode
        // uses (exact slot + global/groq fallbacks).
        let read_back = read_api_key_target(&target).expect("test cred must read");
        assert_eq!(read_back, secret);
        // Delete is exact-slot: the per-preset slot itself must be gone…
        delete_credential(&target).expect("test cred must delete");
        assert!(
            read_credential(&target).is_err(),
            "per-preset slot must be gone after delete"
        );
        // …after which resolution either reports a friendly missing-key error
        // (no global key on this machine) or serves the documented global
        // fallback (a real global key exists) — never a raw OS error, and
        // never the deleted per-preset value.
        match get_llm_api_key_or_err(preset) {
            Err(e) => {
                assert!(e.contains("Missing API key"), "got: {e}");
                assert!(e.contains(preset), "preset context missing: {e}");
                for raw in ["CredReadW", "os error", "Credential Manager"] {
                    assert!(!e.contains(raw), "raw backend detail leaked: {e}");
                }
            }
            Ok(fallback_key) => {
                let global_key = read_credential(LLM_API_KEY_TARGET)
                    .expect("fallback key must come from the global slot");
                assert_eq!(fallback_key, global_key);
                assert_ne!(
                    fallback_key, secret,
                    "deleted per-preset value must not resurface"
                );
                println!(
                    "NOTE: machine holds a real global LLM key; fallback served as documented"
                );
            }
        }
        let _ = delete_credential(&target);
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
