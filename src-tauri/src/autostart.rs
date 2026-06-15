// Fluence Windows — Windows Auto-start module
// Adds/removes a registry Run entry so Fluence launches with Windows.

use anyhow::{anyhow, Result};

#[cfg(target_os = "windows")]
use windows::{
    core::PCWSTR,
    Win32::System::Registry::{
        RegCloseKey, RegDeleteValueW, RegSetValueExW,
        HKEY_CURRENT_USER, KEY_SET_VALUE, REG_SZ,
    },
};

const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
const APP_NAME: &str = "Fluence";

#[cfg(target_os = "windows")]
fn to_wide(s: &str) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    std::ffi::OsStr::new(s)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// Enable auto-start: write registry Run entry
#[cfg(target_os = "windows")]
pub fn enable_autostart() -> Result<()> {
    let exe_path = std::env::current_exe()?.to_string_lossy().to_string();
    let run_key_wide = to_wide(RUN_KEY);
    let app_name_wide = to_wide(APP_NAME);
    let value_wide: Vec<u16> = exe_path.encode_utf16().chain(std::iter::once(0)).collect();
    let value_bytes = unsafe {
        std::slice::from_raw_parts(
            value_wide.as_ptr() as *const u8,
            value_wide.len() * 2,
        )
    };

    unsafe {
        let mut hkey = windows::Win32::System::Registry::HKEY::default();
        windows::Win32::System::Registry::RegOpenKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(run_key_wide.as_ptr()),
            0,
            KEY_SET_VALUE,
            &mut hkey,
        )
        .ok()
        .map_err(|e| anyhow!("RegOpenKeyExW failed: {}", e))?;

        RegSetValueExW(
            hkey,
            PCWSTR(app_name_wide.as_ptr()),
            0,
            REG_SZ,
            Some(value_bytes),
        )
        .ok()
        .map_err(|e| anyhow!("RegSetValueExW failed: {}", e))?;

        let _ = RegCloseKey(hkey);
    }
    Ok(())
}

/// Disable auto-start: remove registry Run entry
#[cfg(target_os = "windows")]
pub fn disable_autostart() -> Result<()> {
    let run_key_wide = to_wide(RUN_KEY);
    let app_name_wide = to_wide(APP_NAME);

    unsafe {
        let mut hkey = windows::Win32::System::Registry::HKEY::default();
        let result = windows::Win32::System::Registry::RegOpenKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(run_key_wide.as_ptr()),
            0,
            KEY_SET_VALUE,
            &mut hkey,
        );
        if result.ok().is_err() {
            return Ok(()); // Key doesn't exist, nothing to do
        }
        let _ = RegDeleteValueW(hkey, PCWSTR(app_name_wide.as_ptr()));
        let _ = RegCloseKey(hkey);
    }
    Ok(())
}

#[cfg(not(target_os = "windows"))]
pub fn enable_autostart() -> Result<()> {
    Err(anyhow!("Autostart not supported on this platform"))
}

#[cfg(not(target_os = "windows"))]
pub fn disable_autostart() -> Result<()> {
    Ok(())
}

// Tauri commands

#[tauri::command]
pub fn set_autostart(enabled: bool) -> Result<(), String> {
    if enabled {
        enable_autostart().map_err(|e| e.to_string())
    } else {
        disable_autostart().map_err(|e| e.to_string())
    }
}
