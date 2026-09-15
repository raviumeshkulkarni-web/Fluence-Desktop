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

#[cfg(target_os = "windows")]
const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
#[cfg(target_os = "windows")]
const APP_NAME: &str = "Fluence";

/// Format the executable path for the HKCU\...\Run value.
///
/// Registry Run values are parsed like command lines at logon. A bare path
/// containing spaces (e.g. "C:\Program Files\Fluence\fluence.exe") is split
/// into arguments and the app silently fails to launch, so the path must be
/// wrapped in double quotes. Normalizes an already-quoted input.
#[cfg(target_os = "windows")]
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

#[cfg(target_os = "linux")]
fn autostart_desktop_path() -> Result<std::path::PathBuf> {
    let mut dir = dirs::config_dir()
        .ok_or_else(|| anyhow!("Could not resolve user config directory"))?;
    dir.push("autostart");
    std::fs::create_dir_all(&dir)?;
    dir.push("fluence.desktop");
    Ok(dir)
}

/// Render the XDG desktop entry body for the given executable path.
/// Pure function so the format is unit-testable without touching the fs.
#[cfg(target_os = "linux")]
fn desktop_entry_contents(exe_path: &str) -> String {
    format!(
        "[Desktop Entry]\nType=Application\nName=Fluence\nComment=AI voice typing\nExec={exe_path}\nTerminal=false\nCategories=Utility;\nX-GNOME-Autostart-enabled=true\n"
    )
}
/// Enable auto-start on Linux: write an XDG autostart desktop entry
/// (`~/.config/autostart/fluence.desktop`) so desktop environments that
/// follow the freedesktop autostart spec (GNOME, KDE, Xfce, ...) launch
/// Fluence on login.
#[cfg(target_os = "linux")]
pub fn enable_autostart() -> Result<()> {
    let exe_path = std::env::current_exe()?;
    // No extra flags: the app starts into the tray on its own whenever it
    // is not a first run (see main.rs setup), so a plain Exec line is enough.
    let entry = desktop_entry_contents(&exe_path.to_string_lossy());
    std::fs::write(autostart_desktop_path()?, entry)?;
    Ok(())
}

/// Disable auto-start on Linux: remove the XDG autostart desktop entry.
/// Missing file is not an error (nothing to do).
#[cfg(target_os = "linux")]
pub fn disable_autostart() -> Result<()> {
    match std::fs::remove_file(autostart_desktop_path()?) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(anyhow!("Failed to remove autostart entry: {e}")),
    }
}

#[cfg(not(any(target_os = "windows", target_os = "linux")))]
pub fn enable_autostart() -> Result<()> {
    Err(anyhow!("Autostart not supported on this platform"))
}

#[cfg(not(any(target_os = "windows", target_os = "linux")))]
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
    #[cfg(target_os = "windows")]
    use super::format_run_value;
    #[cfg(target_os = "linux")]
    use super::desktop_entry_contents;

    #[cfg(target_os = "windows")]
    #[test]
    fn quotes_plain_path() {
        assert_eq!(
            format_run_value(r"C:\Program Files\Fluence\fluence.exe"),
            "\"C:\\Program Files\\Fluence\\fluence.exe\""
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn does_not_double_quote() {
        assert_eq!(
            format_run_value(r#""C:\Program Files\Fluence\fluence.exe""#),
            r#""C:\Program Files\Fluence\fluence.exe""#
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn trims_surrounding_whitespace() {
        assert_eq!(
            format_run_value("  C:\\Fluence\\fluence.exe  "),
            "\"C:\\Fluence\\fluence.exe\""
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn quirkless_single_component_path_is_quoted_too() {
        // Even space-free paths get quoted: harmless and keeps the format uniform.
        assert_eq!(
            format_run_value(r"C:\Fluence\fluence.exe"),
            "\"C:\\Fluence\\fluence.exe\""
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn desktop_entry_is_valid_xdg_autostart() {
        let entry = desktop_entry_contents("/usr/bin/fluence");
        assert!(entry.starts_with("[Desktop Entry]\n"));
        assert!(entry.contains("\nType=Application\n"));
        assert!(entry.contains("\nExec=/usr/bin/fluence\n"));
        assert!(entry.contains("\nTerminal=false\n"));
        assert!(entry.contains("X-GNOME-Autostart-enabled=true"));
    }

    /// Full enable → disable roundtrip against an isolated XDG_CONFIG_HOME.
    /// Safe to commit: autostart is the only module using `config_dir`
    /// (everything else uses `data_local_dir`), so overriding
    /// XDG_CONFIG_HOME cannot disturb parallel tests. Restores the env.
    #[cfg(target_os = "linux")]
    #[test]
    fn autostart_enable_disable_roundtrip() {
        let tmp = std::env::temp_dir().join(format!(
            "fluence-autostart-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        let prev = std::env::var_os("XDG_CONFIG_HOME");
        std::env::set_var("XDG_CONFIG_HOME", &tmp);

        let result = (|| -> anyhow::Result<()> {
            super::enable_autostart()?;
            let entry_path = tmp.join("autostart/fluence.desktop");
            let body = std::fs::read_to_string(&entry_path)?;
            assert!(body.starts_with("[Desktop Entry]\n"));
            assert!(body.contains("\nType=Application\n"));
            super::disable_autostart()?;
            assert!(!entry_path.exists());
            // Disabling twice is not an error (idempotent).
            super::disable_autostart()?;
            Ok(())
        })();

        match prev {
            Some(v) => std::env::set_var("XDG_CONFIG_HOME", v),
            None => std::env::remove_var("XDG_CONFIG_HOME"),
        }
        let _ = std::fs::remove_dir_all(&tmp);
        result.unwrap();
    }
}
