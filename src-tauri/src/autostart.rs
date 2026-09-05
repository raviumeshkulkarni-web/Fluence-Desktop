// Fluence Windows - Windows Auto-start module
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
const APP_NAME: &str = "Fluence";

/// Format the executable path for the HKCU\...\Run value.
///
/// Registry Run values are parsed like command lines at logon. A bare path
/// containing spaces (e.g. "C:\Program Files\Fluence\fluence.exe") is split
/// into arguments and the app silently fails to launch, so the path must be
/// wrapped in double quotes. Normalizes an already-quoted input.
fn format_run_value(exe_path: &str) -> String {
    let trimmed = exe_path.trim();
    if trimmed.starts_with('"') && trimmed.ends_with('"') && trimmed.len() >= 2 {
        trimmed.to_string()
    } else {
        format!("\"{trimmed}\"")
    }
}

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
    let value_wide: Vec<u16> = format_run_value(&exe_path)
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
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

#[cfg(test)]
mod tests {
    use super::format_run_value;

    #[test]
    fn quotes_plain_path() {
        assert_eq!(
            format_run_value(r"C:\Program Files\Fluence\fluence.exe"),
            "\"C:\\Program Files\\Fluence\\fluence.exe\""
        );
    }

    #[test]
    fn does_not_double_quote() {
        assert_eq!(
            format_run_value(r#""C:\Program Files\Fluence\fluence.exe""#),
            r#""C:\Program Files\Fluence\fluence.exe""#
        );
    }

    #[test]
    fn trims_surrounding_whitespace() {
        assert_eq!(
            format_run_value("  C:\\Fluence\\fluence.exe  "),
            "\"C:\\Fluence\\fluence.exe\""
        );
    }

    #[test]
    fn quirkless_single_component_path_is_quoted_too() {
        // Even space-free paths get quoted: harmless and keeps the format uniform.
        assert_eq!(
            format_run_value(r"C:\Fluence\fluence.exe"),
            "\"C:\\Fluence\\fluence.exe\""
        );
    }
}
