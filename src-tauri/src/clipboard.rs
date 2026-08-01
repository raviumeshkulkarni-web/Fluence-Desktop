// Fluence Windows — Clipboard + text injection module
// Saves clipboard, sets new text, simulates Ctrl+V, then restores original clipboard.

use anyhow::{anyhow, Result};
use once_cell::sync::Lazy;
use std::time::Duration;
use tokio::time::sleep;

// Serializes save → set → paste → restore so a delayed restore from one
// injection cannot overwrite the clipboard while another injection is pasting.
static CLIPBOARD_INJECTION_LOCK: Lazy<tokio::sync::Mutex<()>> =
    Lazy::new(|| tokio::sync::Mutex::new(()));

#[cfg(target_os = "windows")]
use windows::Win32::{
    Foundation::HWND,
    System::{
        DataExchange::{
            CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable,
            GetClipboardSequenceNumber, OpenClipboard, SetClipboardData,
        },
        Memory::{GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE},
    },
    UI::Input::KeyboardAndMouse::{
        GetAsyncKeyState, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS,
        KEYEVENTF_KEYUP, VIRTUAL_KEY, VK_CONTROL, VK_LCONTROL, VK_LMENU, VK_LSHIFT, VK_LWIN,
        VK_RCONTROL, VK_RMENU, VK_RSHIFT, VK_RWIN, VK_V,
    },
};

#[cfg(target_os = "windows")]
fn make_key_input(vk: VIRTUAL_KEY, flags: KEYBD_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

#[cfg(target_os = "windows")]
const CF_UNICODETEXT: u32 = 13;

#[cfg(target_os = "windows")]
#[link(name = "kernel32")]
extern "system" {
    fn GlobalFree(hmem: windows::Win32::Foundation::HGLOBAL)
        -> windows::Win32::Foundation::HGLOBAL;
}

#[cfg(target_os = "windows")]
fn open_clipboard_with_retry() -> Result<()> {
    let mut last_error = None;
    for _ in 0..12 {
        match unsafe { OpenClipboard(HWND::default()) } {
            Ok(()) => return Ok(()),
            Err(e) => {
                last_error = Some(e);
                std::thread::sleep(Duration::from_millis(15));
            }
        }
    }
    Err(anyhow!(
        "OpenClipboard failed after retries: {}",
        last_error
            .map(|e| e.to_string())
            .unwrap_or_else(|| "unknown error".to_string())
    ))
}

/// Get current clipboard text (Windows)
#[cfg(target_os = "windows")]
fn get_clipboard_text() -> Option<String> {
    unsafe {
        if IsClipboardFormatAvailable(CF_UNICODETEXT).is_err() {
            return None;
        }
        if open_clipboard_with_retry().is_err() {
            return None;
        }
        let h = GetClipboardData(CF_UNICODETEXT);
        let result = h.ok().and_then(|handle| {
            let total_size = GlobalSize(windows::Win32::Foundation::HGLOBAL(handle.0));
            if total_size == 0 {
                return None;
            }
            let max_words = total_size / 2;
            let ptr = GlobalLock(windows::Win32::Foundation::HGLOBAL(handle.0));
            if ptr.is_null() {
                return None;
            }
            // Read null-terminated UTF-16 string
            let mut len = 0usize;
            let wptr = ptr as *const u16;
            while len < max_words && *wptr.add(len) != 0 {
                len += 1;
            }
            let slice = std::slice::from_raw_parts(wptr, len);
            let text = String::from_utf16_lossy(slice);
            GlobalUnlock(windows::Win32::Foundation::HGLOBAL(handle.0)).ok();
            Some(text)
        });
        let _ = CloseClipboard();
        result
    }
}

/// Set clipboard to a UTF-16 string (Windows)
#[cfg(target_os = "windows")]
fn set_clipboard_text(text: &str) -> Result<()> {
    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let byte_len = wide.len() * 2;

    unsafe {
        open_clipboard_with_retry()?;
        if let Err(e) = EmptyClipboard() {
            let _ = CloseClipboard();
            return Err(anyhow!("EmptyClipboard failed: {}", e));
        }

        let hmem = match GlobalAlloc(GMEM_MOVEABLE, byte_len) {
            Ok(hmem) => hmem,
            Err(e) => {
                let _ = CloseClipboard();
                return Err(anyhow!("GlobalAlloc failed: {}", e));
            }
        };
        let ptr = GlobalLock(hmem);
        if ptr.is_null() {
            let _ = CloseClipboard();
            let _ = GlobalFree(hmem);
            return Err(anyhow!("GlobalLock failed"));
        }
        std::ptr::copy_nonoverlapping(wide.as_ptr() as *const u8, ptr as *mut u8, byte_len);
        GlobalUnlock(hmem).ok();

        if let Err(e) = SetClipboardData(CF_UNICODETEXT, windows::Win32::Foundation::HANDLE(hmem.0))
        {
            let _ = CloseClipboard();
            let _ = GlobalFree(hmem);
            return Err(anyhow!("SetClipboardData failed: {}", e));
        }

        let _ = CloseClipboard();
    }
    Ok(())
}

#[cfg(target_os = "windows")]
const MODIFIER_VKS: &[VIRTUAL_KEY] = &[
    VK_LCONTROL,
    VK_RCONTROL,
    VK_LSHIFT,
    VK_RSHIFT,
    VK_LMENU,
    VK_RMENU, // Alt keys
    VK_LWIN,
    VK_RWIN,
];

#[cfg(target_os = "windows")]
fn release_held_modifiers() -> Vec<VIRTUAL_KEY> {
    let mut released = Vec::new();
    for &vk in MODIFIER_VKS {
        unsafe {
            if (GetAsyncKeyState(vk.0 as i32) as u16 & 0x8000) != 0 {
                released.push(vk);
            }
        }
    }
    if !released.is_empty() {
        let mut inputs: Vec<INPUT> = Vec::with_capacity(released.len());
        for &vk in &released {
            inputs.push(make_key_input(vk, KEYEVENTF_KEYUP));
        }
        unsafe {
            let sent = SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
            if sent != inputs.len() as u32 {
                log::warn!(
                    "SendInput (release modifiers): sent {} of {} events",
                    sent,
                    inputs.len()
                );
            }
        }
    }
    released
}

#[cfg(target_os = "windows")]
fn restore_modifiers(released: &[VIRTUAL_KEY]) {
    if released.is_empty() {
        return;
    }
    let mut inputs: Vec<INPUT> = Vec::with_capacity(released.len());
    for &vk in released {
        inputs.push(make_key_input(vk, KEYBD_EVENT_FLAGS(0)));
    }
    unsafe {
        let sent = SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
        if sent != inputs.len() as u32 {
            log::warn!(
                "SendInput (restore modifiers): sent {} of {} events",
                sent,
                inputs.len()
            );
        }
    }
}

/// Send Ctrl+V keystroke via SendInput
#[cfg(target_os = "windows")]
fn send_ctrl_v() {
    let inputs = [
        make_key_input(VK_CONTROL, KEYBD_EVENT_FLAGS(0)),
        make_key_input(VK_V, KEYBD_EVENT_FLAGS(0)),
        make_key_input(VK_V, KEYEVENTF_KEYUP),
        make_key_input(VK_CONTROL, KEYEVENTF_KEYUP),
    ];
    unsafe {
        let sent = SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
        if sent != inputs.len() as u32 {
            log::warn!(
                "SendInput (ctrl_v): sent {} of {} events",
                sent,
                inputs.len()
            );
        }
    }
}

/// Send N backspace keystrokes via SendInput
#[cfg(target_os = "windows")]
pub fn send_backspaces(count: usize) {
    use windows::Win32::UI::Input::KeyboardAndMouse::VK_BACK;
    const MAX_DELETE_CHARS: usize = 10_000;
    if count == 0 {
        return;
    }
    if count > MAX_DELETE_CHARS {
        log::warn!("Ignoring oversized backspace request: {}", count);
        return;
    }
    let mut inputs: Vec<INPUT> = Vec::with_capacity(count.saturating_mul(2));
    for _ in 0..count {
        inputs.push(make_key_input(VK_BACK, KEYBD_EVENT_FLAGS(0)));
        inputs.push(make_key_input(VK_BACK, KEYEVENTF_KEYUP));
    }
    unsafe {
        let sent = SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
        if sent != inputs.len() as u32 {
            log::warn!(
                "SendInput (backspaces): sent {} of {} events",
                sent,
                inputs.len()
            );
        }
    }
}

/// Send Enter key via SendInput
#[cfg(target_os = "windows")]
pub fn send_enter() {
    use windows::Win32::UI::Input::KeyboardAndMouse::VK_RETURN;
    let inputs = [
        make_key_input(VK_RETURN, KEYBD_EVENT_FLAGS(0)),
        make_key_input(VK_RETURN, KEYEVENTF_KEYUP),
    ];
    unsafe {
        let sent = SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
        if sent != inputs.len() as u32 {
            log::warn!(
                "SendInput (enter): sent {} of {} events",
                sent,
                inputs.len()
            );
        }
    }
}

/// Send Ctrl+A (select all) via SendInput
#[cfg(target_os = "windows")]
pub fn send_select_all() {
    use windows::Win32::UI::Input::KeyboardAndMouse::VK_A;
    let inputs = [
        make_key_input(VK_CONTROL, KEYBD_EVENT_FLAGS(0)),
        make_key_input(VK_A, KEYBD_EVENT_FLAGS(0)),
        make_key_input(VK_A, KEYEVENTF_KEYUP),
        make_key_input(VK_CONTROL, KEYEVENTF_KEYUP),
    ];
    unsafe {
        let sent = SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
        if sent != inputs.len() as u32 {
            log::warn!(
                "SendInput (select_all): sent {} of {} events",
                sent,
                inputs.len()
            );
        }
    }
}

/// Main Tauri command: inject text into the focused application
/// Saves clipboard → sets text → Ctrl+V → restores clipboard after delay
#[tauri::command]
pub async fn inject_text(text: String, monitor_auto_learn: Option<bool>) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        let _transaction = CLIPBOARD_INJECTION_LOCK.lock().await;
        let start_time = std::time::Instant::now();
        // Save current clipboard
        let saved = get_clipboard_text();
        let save_duration = start_time.elapsed();

        let set_start = std::time::Instant::now();
        // Set clipboard to our transcribed text
        set_clipboard_text(&text).map_err(|e| e.to_string())?;
        let fluence_clipboard_sequence = clipboard_sequence_number();
        let set_duration = set_start.elapsed();

        let ctrlv_start = std::time::Instant::now();
        // Release physical modifier keys held down by the user
        let released = release_held_modifiers();

        // Send Ctrl+V
        send_ctrl_v();

        // Restore physical modifier keys
        restore_modifiers(&released);
        let ctrlv_duration = ctrlv_start.elapsed();

        log::info!(
            "inject_text performance: total = {:?}, save_clip = {:?}, set_clip = {:?}, send_ctrl_v = {:?}",
            start_time.elapsed(),
            save_duration,
            set_duration,
            ctrlv_duration
        );

        // Only the normal transcription flow opts into post-injection
        // learning. Agent-generated inserts use the same clipboard command
        // but are not transcription evidence.
        let should_monitor = monitor_auto_learn.unwrap_or(false)
            && crate::settings::load_settings()
                .map(|settings| settings.auto_learn_enabled)
                .unwrap_or(false);
        if should_monitor {
            crate::auto_learn::start_post_injection_monitor(text.clone());
        }

        // Restore only if Fluence still owns the clipboard. If the user or
        // another application copied something during the paste window, do
        // not overwrite that newer clipboard content.
        sleep(Duration::from_millis(200)).await;
        if clipboard_sequence_number() == fluence_clipboard_sequence
            && get_clipboard_text().as_deref() == Some(text.as_str())
        {
            if let Some(original) = saved {
                let _ = set_clipboard_text(&original);
            }
        }

        Ok(())
    }
    #[cfg(not(target_os = "windows"))]
    Err("Text injection not supported on this platform".to_string())
}

#[cfg(target_os = "windows")]
fn clipboard_sequence_number() -> u32 {
    unsafe { GetClipboardSequenceNumber() }
}

/// Copy text while participating in the same clipboard transaction lock as
/// text injection and active-selection capture.
#[tauri::command]
pub async fn copy_text(text: String) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        let _transaction = CLIPBOARD_INJECTION_LOCK.lock().await;
        set_clipboard_text(&text).map_err(|e| e.to_string())
    }
    #[cfg(not(target_os = "windows"))]
    Err("Clipboard operations are not supported on this platform".to_string())
}

/// Execute a keyboard action from agent mode
#[tauri::command]
pub async fn execute_keyboard_action(
    action: String,
    char_count: Option<usize>,
) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        sleep(Duration::from_millis(80)).await;
        match action.as_str() {
            "delete_chars" => {
                if let Some(n) = char_count {
                    if n > 10_000 {
                        return Err("Delete action exceeds maximum length".to_string());
                    }
                    send_backspaces(n);
                }
            }
            "select_all" => send_select_all(),
            "submit" => send_enter(),
            _ => {}
        }
        Ok(())
    }
    #[cfg(not(target_os = "windows"))]
    Err("Keyboard actions not supported on this platform".to_string())
}

/// Send Ctrl+C keystroke via SendInput
#[cfg(target_os = "windows")]
fn send_ctrl_c() {
    use windows::Win32::UI::Input::KeyboardAndMouse::VK_C;
    let inputs = [
        make_key_input(VK_CONTROL, KEYBD_EVENT_FLAGS(0)),
        make_key_input(VK_C, KEYBD_EVENT_FLAGS(0)),
        make_key_input(VK_C, KEYEVENTF_KEYUP),
        make_key_input(VK_CONTROL, KEYEVENTF_KEYUP),
    ];
    unsafe {
        let sent = SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
        if sent != inputs.len() as u32 {
            log::warn!(
                "SendInput (ctrl_c): sent {} of {} events",
                sent,
                inputs.len()
            );
        }
    }
}

/// Grab selected text from the active application by simulating Ctrl+C
#[tauri::command]
pub async fn grab_active_selection() -> Result<Option<String>, String> {
    #[cfg(target_os = "windows")]
    {
        let _transaction = CLIPBOARD_INJECTION_LOCK.lock().await;

        // 1. Save original clipboard text
        let saved_text = get_clipboard_text();

        // 2. Clear clipboard so we can detect if Ctrl+C successfully writes new text
        unsafe {
            if open_clipboard_with_retry().is_ok() {
                let _ = EmptyClipboard();
                let _ = CloseClipboard();
            }
        }

        // 3. Release modifier keys
        let released = release_held_modifiers();

        // 4. Send Ctrl+C
        send_ctrl_c();

        // 5. Restore modifiers
        restore_modifiers(&released);

        // 6. Wait for target app to write to clipboard
        sleep(Duration::from_millis(120)).await;

        // 7. Snapshot ownership before reading the copied selection. If an
        // external clipboard writer changes it during the read, restoration
        // must not overwrite that newer content.
        let selection_clipboard_sequence = clipboard_sequence_number();
        let selection = get_clipboard_text();

        // 8. Restore only if nothing else changed the clipboard after the
        // selection copy. Do not overwrite a newer user/application copy.
        if clipboard_sequence_number() == selection_clipboard_sequence {
            if let Some(original) = saved_text {
                let _ = set_clipboard_text(&original);
            }
        }

        Ok(selection)
    }
    #[cfg(not(target_os = "windows"))]
    Err("Active selection grabbing not supported on this platform".to_string())
}
