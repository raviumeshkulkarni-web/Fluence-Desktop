// Fluence Windows - Foreground context (read-only, fail-closed)
// Resolves the exe behind the current foreground window so per-app AI
// styles can be matched. No categories, no built-in app lists: apps are
// never auto-categorized; the user assigns styles explicitly.
// No audio/STT/LLM touch.

/// Lowercased exe file name + raw window title of the foreground app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForegroundContext {
    /// Lowercased exe file name, e.g. "chrome.exe".
    pub exe: String,
    /// Raw window title (may be empty).
    pub title: String,
}

/// Read the current foreground app. Fail-closed: None on any error,
/// when there is no foreground window, or when it belongs to Fluence.
#[cfg(windows)]
pub fn get_foreground_context() -> Option<ForegroundContext> {
    use windows::Win32::System::Threading::GetCurrentProcessId;
    use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};

    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.is_invalid() {
            return None;
        }
        let mut pid: u32 = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == 0 {
            return None;
        }
        if pid == GetCurrentProcessId() {
            return None;
        }
        // Exe file name, lowercased for override matching.
        let exe = match query_exe_name(pid) {
            Some(e) => e.to_lowercase(),
            None => return None,
        };
        // Window title: best-effort, empty on failure.
        let title = query_window_title(hwnd).unwrap_or_default();
        if exe.is_empty() {
            return None;
        }
        Some(ForegroundContext { exe, title })
    }
}

#[cfg(windows)]
fn query_exe_name(pid: u32) -> Option<String> {
    use std::path::Path;
    use windows::core::PWSTR;
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_FORMAT,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };

    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buffer = vec![0u16; 32768];
        let mut len = buffer.len() as u32;
        let ok = QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_FORMAT(0),
            PWSTR(buffer.as_mut_ptr()),
            &mut len,
        );
        let _ = CloseHandle(handle);
        ok.ok()?;
        buffer.truncate(len as usize);
        let full = String::from_utf16_lossy(&buffer);
        Path::new(&full)
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
    }
}

#[cfg(windows)]
fn query_window_title(hwnd: windows::Win32::Foundation::HWND) -> Option<String> {
    use windows::Win32::UI::WindowsAndMessaging::GetWindowTextW;

    unsafe {
        let mut buffer = vec![0u16; 1024];
        let len = GetWindowTextW(hwnd, &mut buffer);
        if len <= 0 {
            return Some(String::new());
        }
        let len = len as usize;
        if len > buffer.len() {
            return Some(String::new());
        }
        Some(String::from_utf16_lossy(&buffer[..len]))
    }
}

#[cfg(not(windows))]
pub fn get_foreground_context() -> Option<ForegroundContext> {
    None
}
