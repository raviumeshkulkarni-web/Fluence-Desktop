// Fluence Windows — Windows Auto-start module
// Adds/removes a registry Run entry so Fluence launches with Windows.

use anyhow::{anyhow, Result};

#[cfg(target_os = "windows")]
use windows::{
    core::PCWSTR,
    Win32::System::Registry::{
        RegCloseKey, RegDeleteValueW, RegSetValueExW, HKEY_CURRENT_USER, KEY_SET_VALUE, REG_SZ,
    },
};

const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
// Task Manager / Settings "Startup apps" stores enable/disable state here; a stale
// "disabled" flag silently overrides the Run entry until cleared.
const STARTUP_APPROVED_KEY: &str =
    "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\StartupApproved\\Run";
const APP_NAME: &str = "Fluence";

/// Run values are parsed with CreateProcess semantics: an unquoted path
/// containing spaces is ambiguous, so always quote.
fn run_command_value(exe_path: &str) -> String {
    format!("\"{}\"", exe_path)
}

#[cfg(target_os = "windows")]
fn to_wide(s: &str) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    std::ffi::OsStr::new(s)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// Delete a value from a HKCU subkey, ignoring missing key/value.
#[cfg(target_os = "windows")]
fn delete_hkcu_value(key_path: &str, value_name: &str) {
    let key_wide = to_wide(key_path);
    let name_wide = to_wide(value_name);
    unsafe {
        let mut hkey = windows::Win32::System::Registry::HKEY::default();
        let result = windows::Win32::System::Registry::RegOpenKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(key_wide.as_ptr()),
            0,
            KEY_SET_VALUE,
            &mut hkey,
        );
        if result.ok().is_ok() {
            let _ = RegDeleteValueW(hkey, PCWSTR(name_wide.as_ptr()));
            let _ = RegCloseKey(hkey);
        }
    }
}

/// Enable auto-start: write registry Run entry
#[cfg(target_os = "windows")]
pub fn enable_autostart() -> Result<()> {
    let exe_path = run_command_value(&std::env::current_exe()?.to_string_lossy());
    let run_key_wide = to_wide(RUN_KEY);
    let app_name_wide = to_wide(APP_NAME);
    let value_wide: Vec<u16> = exe_path.encode_utf16().chain(std::iter::once(0)).collect();
    let value_bytes = unsafe {
        std::slice::from_raw_parts(value_wide.as_ptr() as *const u8, value_wide.len() * 2)
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

        let result = RegSetValueExW(
            hkey,
            PCWSTR(app_name_wide.as_ptr()),
            0,
            REG_SZ,
            Some(value_bytes),
        );

        let _ = RegCloseKey(hkey);

        result
            .ok()
            .map_err(|e| anyhow!("RegSetValueExW failed: {}", e))?;
    }

    // Clear any "disabled" flag left by Task Manager, otherwise the Run entry
    // is silently ignored even though it exists.
    delete_hkcu_value(STARTUP_APPROVED_KEY, APP_NAME);

    Ok(())
}

/// Disable auto-start: remove registry Run entry
#[cfg(target_os = "windows")]
pub fn disable_autostart() -> Result<()> {
    delete_hkcu_value(RUN_KEY, APP_NAME);
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
        enable_autostart().map_err(|e| e.to_string())?;
    } else {
        disable_autostart().map_err(|e| e.to_string())?;
    }

    // Persist immediately: sync_autostart_on_launch() needs this to re-assert the
    // Run entry after updates, and the frontend's Save button must not be a
    // prerequisite for autostart to survive.
    let mut settings = crate::settings::load_settings().map_err(|e| e.to_string())?;
    settings.auto_start = enabled;
    crate::settings::save_settings(&settings).map_err(|e| e.to_string())
}

/// Re-assert the Run entry at app launch. NSIS updates delete it and the exe
/// path can change between versions, so writing it only when the settings
/// toggle changes means autostart silently dies after every update.
pub fn sync_autostart_on_launch(settings: &crate::settings::AppSettings) {
    if settings.auto_start {
        if let Err(e) = enable_autostart() {
            log::warn!("Failed to re-assert autostart Run entry: {}", e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_run_command_value_quotes_path() {
        assert_eq!(
            run_command_value(r"C:\Program Files\Fluence\fluence-windows.exe"),
            r#""C:\Program Files\Fluence\fluence-windows.exe""#
        );
    }
}
