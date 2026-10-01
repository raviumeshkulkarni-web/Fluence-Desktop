// Fluence Windows - Installed apps enumeration (Text Formatting picker)
// Lists the apps actually installed on THIS pc from three rule-based
// sources (no hard-coded names): Start Menu shortcuts (per-user +
// all-users), Desktop shortcuts (per-user + public), and the registry
// Uninstall entries (HKCU + HKLM + HKLM Wow6432Node) that cover apps
// installed to any directory without shipping a shortcut. Each candidate
// resolves to a real on-disk .exe and gets the real icon via the same
// SHGetFileInfoW path the overlay pill uses.
//
// Contracts:
// - Nothing is hard-coded: no app names, no exe lists, no categories.
//   If Outlook is not installed, it simply does not appear.
// - Fail-closed: unreadable shortcuts/keys are skipped, never errors.
// - Audio/STT/LLM untouched. Read-only: no process, file, or window harmed.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

/// One installed app: exe file name (override key), display name from the
/// shortcut, and the real icon when one could be extracted.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InstalledApp {
    pub exe: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon_data_url: Option<String>,
}

const MAX_ENTRIES: usize = 600;
const MAX_WALK_DEPTH: u8 = 6;

/// Tauri command - list installed apps with icons for the picker.
/// Always succeeds (possibly empty); per-entry failures are skipped.
#[tauri::command]
pub fn list_installed_apps(window: tauri::Window) -> Result<Vec<InstalledApp>, String> {
    crate::acl::require_caller(&window, &[crate::acl::MAIN_WINDOW])?;
    let mut dirs = start_menu_dirs();
    dirs.extend(desktop_dirs());
    Ok(collect_installed_apps(&dirs))
}

fn start_menu_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    for key in ["APPDATA", "PROGRAMDATA"] {
        if let Ok(base) = std::env::var(key) {
            let mut p = PathBuf::from(base);
            p.push("Microsoft");
            p.push("Windows");
            p.push("Start Menu");
            p.push("Programs");
            if p.is_dir() {
                dirs.push(p);
            }
        }
    }
    dirs
}

/// Desktop shortcuts (per-user + public). Catches apps whose installer
/// drops a desktop icon but no Start Menu entry.
fn desktop_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(home) = std::env::var("USERPROFILE") {
        let p = PathBuf::from(home).join("Desktop");
        if p.is_dir() {
            dirs.push(p);
        }
    }
    if let Ok(public) = std::env::var("PUBLIC") {
        let p = PathBuf::from(public).join("Desktop");
        if p.is_dir() {
            dirs.push(p);
        }
    }
    dirs
}

fn collect_installed_apps(dirs: &[PathBuf]) -> Vec<InstalledApp> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut apps: Vec<InstalledApp> = Vec::new();
    let own_stem = std::env::current_exe()
        .ok()
        .and_then(|p| p.file_stem().map(|s| s.to_string_lossy().to_lowercase()))
        .unwrap_or_default();

    // Shared admit-one gate for every source: the target must be a real
    // on-disk .exe, first name wins (so Start Menu shortcut names beat
    // raw registry DisplayNames), uninstallers and our own binary never
    // listed, icons best-effort.
    let mut push_candidate = |name: String, target: String| {
        if apps.len() >= MAX_ENTRIES {
            return;
        }
        if !is_exe_target(&target) {
            return;
        }
        let exe = file_name_lower(&target);
        if exe.is_empty() || !seen.insert(exe.clone()) {
            return;
        }
        if is_uninstaller(&name, &target) {
            return;
        }
        if !own_stem.is_empty() {
            let stem = exe.strip_suffix(".exe").unwrap_or(&exe);
            if stem == own_stem {
                return;
            }
        }
        #[cfg(target_os = "windows")]
        let icon_data_url = crate::app_icon::load_exe_icon(&target)
            .ok()
            .and_then(|icon| crate::app_icon::icon_to_data_url(icon).ok());
        #[cfg(not(target_os = "windows"))]
        let icon_data_url: Option<String> = None;
        apps.push(InstalledApp {
            exe,
            name,
            icon_data_url,
        });
    };

    for dir in dirs {
        collect_lnk_files(dir, 0, &mut |path| {
            let bytes = match fs::read(path) {
                Ok(b) => b,
                Err(_) => return,
            };
            let target = match resolve_lnk_target(&bytes) {
                Some(t) => t,
                None => return,
            };
            push_candidate(lnk_display_name(path), target);
        });
    }

    #[cfg(target_os = "windows")]
    collect_registry_apps(&mut push_candidate);

    apps.sort_by_key(|a| a.name.to_lowercase());
    apps
}

fn collect_lnk_files(dir: &Path, depth: u8, visit: &mut impl FnMut(&Path)) {
    if depth > MAX_WALK_DEPTH {
        return;
    }
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_lnk_files(&path, depth + 1, visit);
        } else if path
            .extension()
            .map(|e| e.eq_ignore_ascii_case("lnk"))
            .unwrap_or(false)
        {
            visit(&path);
        }
    }
}

fn lnk_display_name(lnk_path: &Path) -> String {
    let normalized = lnk_path.to_string_lossy().replace('\\', "/");
    Path::new(&normalized)
        .file_stem()
        .map(|s| s.to_string_lossy().trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "App".to_string())
}

fn is_exe_target(target: &str) -> bool {
    target.to_lowercase().ends_with(".exe") && Path::new(target).is_file()
}

fn file_name_lower(target: &str) -> String {
    Path::new(target)
        .file_name()
        .map(|s| s.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

fn is_uninstaller(name: &str, target: &str) -> bool {
    let n = name.to_lowercase();
    let t = target.to_lowercase();
    n.contains("uninstall")
        || n.contains("unins")
        || t.contains("uninstall")
        || t.contains("unins000")
}

// --- Registry Uninstall source -------------------------------------------
// Apps installed to any directory register here even when they ship no
// Start Menu or Desktop shortcut, which is exactly the gap users hit.
// Same fail-closed contract: any unreadable key/value is skipped.
// Add/Remove-Programs parity filters, all rule-based (no app names):
// SystemComponent=1, ParentKeyName set (patches/children), or a
// non-empty ReleaseType (updates/hotfixes) never list.

const UNINSTALL_SUBKEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall";
const UNINSTALL_SUBKEY_WOW64: &str =
    "Software\\Wow6432Node\\Microsoft\\Windows\\CurrentVersion\\Uninstall";

/// Parse a registry DisplayIcon value into an exe path.
/// Handles `"C:\...\app.exe"`, `"C:\...\app.exe",0` (icon index), and
/// bare paths. Rejects .ico/.dll targets (no launchable exe), %env%
/// values (unexpanded), and anything that is not an .exe path.
fn parse_display_icon_target(raw: &str) -> Option<String> {
    let mut s = raw.trim();
    if s.is_empty() || s.contains('%') {
        return None;
    }
    if s.starts_with('"') {
        match s[1..].find('"') {
            Some(end) => s = s[1..1 + end].trim(),
            None => return None,
        }
    }
    if let Some((head, tail)) = s.rsplit_once(',') {
        if tail.trim().parse::<i32>().is_ok() {
            s = head.trim();
        }
    }
    let s = s.trim().trim_matches('"').trim();
    if s.is_empty() || !s.to_lowercase().ends_with(".exe") {
        return None;
    }
    Some(s.to_string())
}

#[cfg(target_os = "windows")]
fn to_wide_null(s: &str) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    std::ffi::OsStr::new(s)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

#[cfg(target_os = "windows")]
fn reg_open(
    hive: windows::Win32::System::Registry::HKEY,
    subkey: &str,
) -> Option<windows::Win32::System::Registry::HKEY> {
    use windows::Win32::System::Registry::KEY_READ;
    use windows::{core::PCWSTR, Win32::System::Registry::RegOpenKeyExW};
    unsafe {
        let sub = to_wide_null(subkey);
        let mut handle = windows::Win32::System::Registry::HKEY::default();
        RegOpenKeyExW(hive, PCWSTR(sub.as_ptr()), 0, KEY_READ, &mut handle)
            .ok()
            .ok()?;
        Some(handle)
    }
}

#[cfg(target_os = "windows")]
fn reg_subkey_names(hkey: windows::Win32::System::Registry::HKEY) -> Vec<String> {
    use windows::{core::PWSTR, Win32::System::Registry::RegEnumKeyExW};
    let mut names = Vec::new();
    unsafe {
        let mut index: u32 = 0;
        loop {
            let mut buf = vec![0u16; 256];
            let mut len = buf.len() as u32;
            let status = RegEnumKeyExW(
                hkey,
                index,
                PWSTR(buf.as_mut_ptr()),
                &mut len,
                None,
                PWSTR::null(),
                None,
                None,
            );
            if status.0 != 0 {
                break;
            }
            names.push(String::from_utf16_lossy(&buf[..len as usize]));
            index = index.saturating_add(1);
        }
    }
    names
}

#[cfg(target_os = "windows")]
#[allow(clippy::type_complexity)]
fn reg_query_raw(
    hkey: windows::Win32::System::Registry::HKEY,
    name: &str,
) -> Option<(windows::Win32::System::Registry::REG_VALUE_TYPE, Vec<u8>)> {
    use windows::Win32::System::Registry::REG_NONE;
    use windows::{core::PCWSTR, Win32::System::Registry::RegQueryValueExW};
    unsafe {
        let wname = to_wide_null(name);
        let mut value_type = REG_NONE;
        let mut size: u32 = 0;
        let status = RegQueryValueExW(
            hkey,
            PCWSTR(wname.as_ptr()),
            None,
            Some(&mut value_type),
            None,
            Some(&mut size),
        );
        if status.0 != 0 || size == 0 || size > (1 << 20) {
            return None;
        }
        let mut buf = vec![0u8; size as usize];
        let mut size_back = size;
        let status = RegQueryValueExW(
            hkey,
            PCWSTR(wname.as_ptr()),
            None,
            Some(&mut value_type),
            Some(buf.as_mut_ptr()),
            Some(&mut size_back),
        );
        if status.0 != 0 {
            return None;
        }
        buf.truncate(size_back as usize);
        Some((value_type, buf))
    }
}

#[cfg(target_os = "windows")]
fn reg_get_sz(hkey: windows::Win32::System::Registry::HKEY, name: &str) -> Option<String> {
    use windows::Win32::System::Registry::{REG_EXPAND_SZ, REG_SZ};
    let (value_type, buf) = reg_query_raw(hkey, name)?;
    if value_type != REG_SZ && value_type != REG_EXPAND_SZ {
        return None;
    }
    // chunks_exact drops a truncated odd tail; the NUL trim below drops
    // the terminator. Either way the result is a clean Rust string.
    let units: Vec<u16> = buf
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    let text = String::from_utf16_lossy(&units);
    Some(text.trim_matches('\0').trim().to_string())
}

#[cfg(target_os = "windows")]
fn reg_get_dword(hkey: windows::Win32::System::Registry::HKEY, name: &str) -> Option<u32> {
    use windows::Win32::System::Registry::REG_DWORD;
    let (value_type, buf) = reg_query_raw(hkey, name)?;
    if value_type != REG_DWORD || buf.len() < 4 {
        return None;
    }
    buf[0..4].try_into().map(u32::from_le_bytes).ok()
}

#[cfg(target_os = "windows")]
fn collect_registry_apps(visit: &mut impl FnMut(String, String)) {
    use windows::Win32::System::Registry::{RegCloseKey, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
    let roots = [
        (HKEY_CURRENT_USER, UNINSTALL_SUBKEY),
        (HKEY_LOCAL_MACHINE, UNINSTALL_SUBKEY),
        (HKEY_LOCAL_MACHINE, UNINSTALL_SUBKEY_WOW64),
    ];
    for (hive, subkey) in roots {
        let Some(uninstall) = reg_open(hive, subkey) else {
            continue;
        };
        for entry_name in reg_subkey_names(uninstall) {
            let full = format!("{subkey}\\{entry_name}");
            let Some(entry) = reg_open(hive, &full) else {
                continue;
            };
            let system_component = reg_get_dword(entry, "SystemComponent").unwrap_or(0) == 1;
            let has_parent = reg_get_sz(entry, "ParentKeyName")
                .map(|v| !v.is_empty())
                .unwrap_or(false);
            let is_release = reg_get_sz(entry, "ReleaseType")
                .map(|v| !v.is_empty())
                .unwrap_or(false);
            let display_name = reg_get_sz(entry, "DisplayName").unwrap_or_default();
            let display_icon = reg_get_sz(entry, "DisplayIcon").unwrap_or_default();
            unsafe {
                let _ = RegCloseKey(entry);
            }
            if system_component || has_parent || is_release {
                continue;
            }
            if display_name.trim().is_empty() {
                continue;
            }
            let Some(target) = parse_display_icon_target(&display_icon) else {
                continue;
            };
            visit(display_name.trim().to_string(), target);
        }
        unsafe {
            let _ = RegCloseKey(uninstall);
        }
    }
}

/// Resolve a .lnk file to its local target path.
/// Pure parser for the MS-SHLLINK format: header magic, optional ID list,
/// then LinkInfo with a local base path (Unicode preferred, ANSI fallback).
/// Returns None for anything else (network-only, directory, corrupt).
pub fn resolve_lnk_target(bytes: &[u8]) -> Option<String> {
    const HEADER_LEN: usize = 76;
    const HAS_ID_LIST: u32 = 0x01;
    const HAS_LINK_INFO: u32 = 0x02;
    const WANT_LOCAL: u32 = 0x01;

    if bytes.len() < HEADER_LEN {
        return None;
    }
    if u32::from_le_bytes(bytes[0..4].try_into().ok()?) != 0x0000_004C {
        return None;
    }
    let flags = u32::from_le_bytes(bytes[0x14..0x18].try_into().ok()?);
    let mut cursor = HEADER_LEN;

    if flags & HAS_ID_LIST != 0 {
        if bytes.len() < cursor + 2 {
            return None;
        }
        let id_size = u16::from_le_bytes(bytes[cursor..cursor + 2].try_into().ok()?) as usize;
        cursor += 2 + id_size;
    }
    if flags & HAS_LINK_INFO == 0 {
        return None;
    }
    if bytes.len() < cursor + 0x1C {
        return None;
    }
    let info_size = u32::from_le_bytes(bytes[cursor..cursor + 4].try_into().ok()?) as usize;
    let header_size = u32::from_le_bytes(bytes[cursor + 4..cursor + 8].try_into().ok()?) as usize;
    let info_flags = u32::from_le_bytes(bytes[cursor + 8..cursor + 12].try_into().ok()?);
    if info_flags & WANT_LOCAL == 0 {
        return None;
    }
    if bytes.len() < cursor + info_size {
        return None;
    }
    let read_c_string = |off: usize| -> Option<String> {
        let abs = cursor + off;
        if abs >= bytes.len() {
            return None;
        }
        let end = bytes[abs..].iter().position(|&c| c == 0)?;
        Some(
            String::from_utf8_lossy(&bytes[abs..abs + end])
                .trim()
                .to_string(),
        )
    };
    let read_c_wstring = |off: usize| -> Option<String> {
        let abs = cursor + off;
        if abs + 2 > bytes.len() {
            return None;
        }
        let units = bytes[abs..]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .take_while(|&c| c != 0)
            .collect::<Vec<_>>();
        let end_abs = abs + units.len() * 2;
        if end_abs > bytes.len() {
            return None;
        }
        let s = String::from_utf16_lossy(&units).trim().to_string();
        if s.is_empty() {
            return None;
        }
        Some(s)
    };

    // Prefer the Unicode base path when the header carries it.
    if header_size >= 0x24 {
        let uni_off =
            u32::from_le_bytes(bytes[cursor + 0x1C..cursor + 0x20].try_into().ok()?) as usize;
        if uni_off != 0 {
            if let Some(p) = read_c_wstring(uni_off) {
                return Some(p);
            }
        }
    }
    let ansi_off =
        u32::from_le_bytes(bytes[cursor + 0x10..cursor + 0x14].try_into().ok()?) as usize;
    if ansi_off == 0 {
        return None;
    }
    read_c_string(ansi_off).filter(|p| !p.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal synthetic .lnk: header + LinkInfo with a Unicode + ANSI
    /// local base path, no ID list. Mirrors MS-SHLLINK layout.
    fn sample_lnk(target: &str) -> Vec<u8> {
        let mut bytes = vec![0u8; 76];
        bytes[0] = 0x4C;
        // LinkFlags: HasLinkInfo.
        bytes[0x14] = 0x02;
        let wide: Vec<u16> = target.encode_utf16().chain(std::iter::once(0)).collect();
        let mut wide_bytes = Vec::new();
        for u in &wide {
            wide_bytes.extend_from_slice(&u.to_le_bytes());
        }
        let ansi = format!("{target}\0");
        // LinkInfo: header size 0x24, flags local path, offsets.
        let header_size: usize = 0x24;
        let ansi_off = header_size;
        let uni_off = header_size + ansi.len();
        let info_size = uni_off + wide_bytes.len();
        let mut info = vec![0u8; info_size];
        info[0..4].copy_from_slice(&(info_size as u32).to_le_bytes());
        info[4..8].copy_from_slice(&(header_size as u32).to_le_bytes());
        info[8..12].copy_from_slice(&1u32.to_le_bytes());
        info[0x10..0x14].copy_from_slice(&(ansi_off as u32).to_le_bytes());
        info[0x1C..0x20].copy_from_slice(&(uni_off as u32).to_le_bytes());
        info[ansi_off..ansi_off + ansi.len()].copy_from_slice(ansi.as_bytes());
        info[uni_off..uni_off + wide_bytes.len()].copy_from_slice(&wide_bytes);
        bytes.extend_from_slice(&info);
        bytes
    }

    #[test]
    fn resolves_unicode_target() {
        let out = resolve_lnk_target(&sample_lnk("C:\\Apps\\Demo\\app.exe"));
        assert_eq!(out.as_deref(), Some("C:\\Apps\\Demo\\app.exe"));
    }

    #[test]
    fn rejects_bad_magic_and_short_input() {
        assert!(resolve_lnk_target(&[]).is_none());
        assert!(resolve_lnk_target(&vec![0u8; 100]).is_none());
    }

    #[test]
    fn rejects_link_without_local_path() {
        // Header only, no HasLinkInfo flag.
        let mut bytes = vec![0u8; 76];
        bytes[0] = 0x4C;
        assert!(resolve_lnk_target(&bytes).is_none());
    }

    #[test]
    fn uninstallers_are_filtered() {
        assert!(is_uninstaller("Uninstall VLC", "C:\\VLC\\uninstall.exe"));
        assert!(is_uninstaller("App", "C:\\App\\unins000.exe"));
        assert!(!is_uninstaller("VLC media player", "C:\\VLC\\vlc.exe"));
    }

    #[test]
    fn display_icon_paths_resolve_to_exe() {
        assert_eq!(
            parse_display_icon_target("\"C:\\Program Files\\App\\app.exe\",0").as_deref(),
            Some("C:\\Program Files\\App\\app.exe")
        );
        assert_eq!(
            parse_display_icon_target("C:\\Tools\\run.exe").as_deref(),
            Some("C:\\Tools\\run.exe")
        );
        // Comma inside the directory name is not an icon index.
        assert_eq!(
            parse_display_icon_target("\"C:\\Foo, Inc\\app.exe\"").as_deref(),
            Some("C:\\Foo, Inc\\app.exe")
        );
        // Negative icon resource ids are valid suffixes too.
        assert_eq!(
            parse_display_icon_target("C:\\App\\app.exe,-32512").as_deref(),
            Some("C:\\App\\app.exe")
        );
    }

    #[test]
    fn display_icon_non_exe_targets_rejected() {
        assert!(parse_display_icon_target("").is_none());
        assert!(parse_display_icon_target("C:\\App\\icon.ico").is_none());
        assert!(parse_display_icon_target("C:\\App\\lib.dll,0").is_none());
        assert!(parse_display_icon_target("%ProgramFiles%\\App\\app.exe").is_none());
        assert!(parse_display_icon_target("\"C:\\App\\app.exe").is_none());
        assert!(parse_display_icon_target("C:\\App\\app.txt").is_none());
    }

    #[test]
    fn display_name_falls_back() {
        assert_eq!(
            lnk_display_name(Path::new("C:\\x\\Google Chrome.lnk")),
            "Google Chrome"
        );
    }
}
