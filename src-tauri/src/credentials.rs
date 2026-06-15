// Fluence Windows — Windows Credential Manager integration
// Securely stores API keys using the Windows Credential Manager API.

use anyhow::{anyhow, Result};

#[cfg(target_os = "windows")]
use windows::{
    core::{PCWSTR, PWSTR},
    Win32::Security::Credentials::{
        CredDeleteW, CredFree, CredReadW, CredWriteW, CREDENTIALW, CRED_FLAGS, CRED_PERSIST_LOCAL_MACHINE,
        CRED_TYPE_GENERIC,
    },
};

fn to_wide(s: &str) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    std::ffi::OsStr::new(s)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// Store an API key in Windows Credential Manager
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
        Persist: CRED_PERSIST_LOCAL_MACHINE,
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
        let blob = std::slice::from_raw_parts(cred.CredentialBlob, cred.CredentialBlobSize as usize);
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

/// Generate a provider-specific target for STT keys
pub fn get_stt_target(preset: &str) -> String {
    format!("{}/{}", STT_API_KEY_TARGET, preset.to_lowercase().replace(' ', "_"))
}

/// Generate a provider-specific target for LLM keys
pub fn get_llm_target(preset: &str) -> String {
    format!("{}/{}", LLM_API_KEY_TARGET, preset.to_lowercase().replace(' ', "_"))
}

// Tauri commands
#[tauri::command]
pub fn save_api_key(target: String, key: String) -> Result<(), String> {
    store_credential(&target, "fluence", &key).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn get_api_key(target: String) -> Result<String, String> {
    // 1. Try the specific target requested
    if let Ok(key) = read_credential(&target) {
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
                log::info!("Found legacy key in global slot, migrating to: {}", target);
                // Optional: Migrate the key to the new specific slot automatically
                let _ = store_credential(&target, "fluence", &legacy_key);
                return Ok(legacy_key);
            }
        }
    }

    // 3. Final attempt at the raw target (or return the read error)
    read_credential(&target).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn delete_api_key(target: String) -> Result<(), String> {
    delete_credential(&target).map_err(|e| e.to_string())
}
