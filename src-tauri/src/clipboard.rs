// Fluence Windows - Clipboard + text injection module
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
            CloseClipboard, EmptyClipboard, GetClipboardData, GetClipboardSequenceNumber,
            IsClipboardFormatAvailable, OpenClipboard, SetClipboardData,
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

// ── Linux clipboard + text injection (arboard + enigo) ─────────────────
// Works fully on X11. On Wayland, synthetic key events require compositor
// cooperation and clipboard access is brokered by the compositor, so either
// step can fail; failures surface as actionable errors instead of silent
// no-ops (the transcribed text stays in the clipboard as a fallback).
#[cfg(target_os = "linux")]
fn linux_clipboard() -> Result<arboard::Clipboard> {
    arboard::Clipboard::new().map_err(|e| anyhow!("Failed to open clipboard: {e}"))
}

#[cfg(target_os = "linux")]
fn get_clipboard_text_linux() -> Option<String> {
    linux_clipboard().ok()?.get_text().ok()
}

#[cfg(target_os = "linux")]
fn set_clipboard_text_linux(text: &str) -> Result<()> {
    linux_clipboard()?
        .set_text(text)
        .map_err(|e| anyhow!("Failed to set clipboard: {e}"))
}

#[cfg(target_os = "linux")]
fn linux_key_sender() -> Result<enigo::Enigo> {
    use enigo::Settings;
    enigo::Enigo::new(&Settings::default()).map_err(|e| {
        anyhow!(
            "Keyboard simulation is unavailable in this session ({e}). \
             Wayland compositors restrict synthetic input: the text has been \
             copied to the clipboard, paste it with Ctrl+V."
        )
    })
}

/// Send a Control+<key> chord (paste, copy, select-all) via enigo.
#[cfg(target_os = "linux")]
fn send_ctrl_chord_linux(key: char) -> Result<()> {
    use enigo::{Direction, Key, Keyboard};
    let mut enigo = linux_key_sender()?;
    enigo
        .key(Key::Control, Direction::Press)
        .map_err(|e| anyhow!("Failed to press Control: {e}"))?;
    let chord = enigo.key(Key::Unicode(key), Direction::Click);
    let _ = enigo.key(Key::Control, Direction::Release);
    chord.map_err(|e| {
        anyhow!(
            "Failed to send Ctrl+{key} ({e}). The text is in the clipboard; paste it with Ctrl+V."
        )
    })
}

/// Send a single non-text key (Backspace, Return) via enigo.
#[cfg(target_os = "linux")]
fn send_single_key_linux(key: enigo::Key) -> Result<()> {
    send_key_clicks_linux(key, 1)
}

/// Send `count` clicks of a non-text key through a single enigo session
/// (one connection setup, not one per keystroke).
#[cfg(target_os = "linux")]
fn send_key_clicks_linux(key: enigo::Key, count: usize) -> Result<()> {
    use enigo::{Direction, Keyboard};
    let mut enigo = linux_key_sender()?;
    for _ in 0..count {
        enigo
            .key(key, Direction::Click)
            .map_err(|e| anyhow!("Failed to send key: {e}"))?;
    }
    Ok(())
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
    #[cfg(target_os = "linux")]
    {
        let _transaction = CLIPBOARD_INJECTION_LOCK.lock().await;
        // Save current clipboard (best effort: failure just means there is
        // nothing to restore afterwards).
        let saved = get_clipboard_text_linux();

        // Set clipboard to our transcribed text.
        set_clipboard_text_linux(&text).map_err(|e| e.to_string())?;

        // Paste via synthetic Ctrl+V. On failure the text is already in the
        // clipboard, so the error tells the user to paste manually.
        send_ctrl_chord_linux('v').map_err(|e| e.to_string())?;
        log::info!("inject_text via clipboard + Ctrl+V ({} chars)", text.len());

        // Auto-learn monitoring is a no-op on Linux (see auto_learn/mod.rs),
        // but keep the call site identical so a future Linux monitor lights
        // up without touching this flow.
        let should_monitor = monitor_auto_learn.unwrap_or(false)
            && crate::settings::load_settings()
                .map(|settings| settings.auto_learn_enabled)
                .unwrap_or(false);
        if should_monitor {
            crate::auto_learn::start_post_injection_monitor(text.clone());
        }

        // Restore the original clipboard only if Fluence still owns it.
        sleep(Duration::from_millis(200)).await;
        if get_clipboard_text_linux().as_deref() == Some(text.as_str()) {
            if let Some(original) = saved {
                let _ = set_clipboard_text_linux(&original);
            }
        }

        Ok(())
    }
    #[cfg(not(any(target_os = "windows", target_os = "linux")))]
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
    #[cfg(target_os = "linux")]
    {
        let _transaction = CLIPBOARD_INJECTION_LOCK.lock().await;
        set_clipboard_text_linux(&text).map_err(|e| e.to_string())
    }
    #[cfg(not(any(target_os = "windows", target_os = "linux")))]
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
    #[cfg(target_os = "linux")]
    {
        use enigo::Key;
        sleep(Duration::from_millis(80)).await;
        match action.as_str() {
            "delete_chars" => {
                if let Some(n) = char_count {
                    if n > 10_000 {
                        return Err("Delete action exceeds maximum length".to_string());
                    }
                    send_key_clicks_linux(Key::Backspace, n).map_err(|e| e.to_string())?;
                }
            }
            "select_all" => send_ctrl_chord_linux('a').map_err(|e| e.to_string())?,
            "submit" => send_single_key_linux(Key::Return).map_err(|e| e.to_string())?,
            _ => {}
        }
        Ok(())
    }
    #[cfg(not(any(target_os = "windows", target_os = "linux")))]
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
/// BUG-03: bounded polling (not 120ms fixed) to avoid truncating slow apps (Gemini/Electron)
/// BUG-04: hardened modifier handling preserves user's clipboard and avoids stuck modifiers
#[tauri::command]
pub async fn grab_active_selection() -> Result<Option<String>, String> {
    #[cfg(target_os = "windows")]
    {
        let _transaction = CLIPBOARD_INJECTION_LOCK.lock().await;

        // 1. Save original clipboard text and sequence
        let saved_text = get_clipboard_text();
        let saved_seq = clipboard_sequence_number();
        // Capture sequence after save to detect if another app changed clipboard concurrently
        let _ = saved_seq;

        // 2. Clear clipboard so we can detect if Ctrl+C successfully writes new text
        unsafe {
            if open_clipboard_with_retry().is_ok() {
                let _ = EmptyClipboard();
                let _ = CloseClipboard();
            }
        }
        let clear_seq = clipboard_sequence_number();

        // 3. Release modifier keys (avoid dropped chord when Ctrl is held for hold_to_record)
        let released = release_held_modifiers();

        // 4. Send Ctrl+C
        send_ctrl_c();

        // 5. Restore modifiers immediately after the synthetic chord
        restore_modifiers(&released);

        // 6. Bounded polling: wait for target app to write to clipboard
        // Immediate success path stays ~30ms; slow apps (Gemini/WebView) get up to 300ms.
        let mut selection: Option<String> = None;
        for _ in 0..10 {
            sleep(Duration::from_millis(30)).await;
            let cur_seq = clipboard_sequence_number();
            if cur_seq != clear_seq {
                selection = get_clipboard_text();
                if selection
                    .as_ref()
                    .map(|s| !s.trim().is_empty())
                    .unwrap_or(false)
                {
                    break;
                }
                // Non-empty check failed but sequence changed -> treat as selection even if empty (collapsed selection)
                if selection.is_some() {
                    break;
                }
            }
        }
        // Final attempt if polling never saw a sequence change (timing edge)
        if selection.is_none() {
            selection = get_clipboard_text();
            // If we still see the cleared state (sequence == clear_seq and empty), treat as no selection
            if clipboard_sequence_number() == clear_seq
                && selection.as_ref().map(|s| s.is_empty()).unwrap_or(true)
            {
                selection = None;
            }
        }

        // 7. Snapshot ownership before restoration
        let selection_clipboard_sequence = clipboard_sequence_number();
        let final_selection = selection.clone().or_else(|| {
            // Fallback: if we cleared but app wrote synchronously before our first poll, capture again
            let s = get_clipboard_text();
            if s.as_ref().map(|t| !t.is_empty()).unwrap_or(false)
                && clipboard_sequence_number() != clear_seq
            {
                s
            } else {
                selection
            }
        });

        // 8. Restore only if nothing else changed the clipboard after the selection copy
        if clipboard_sequence_number() == selection_clipboard_sequence {
            if let Some(original) = saved_text {
                // Only restore if we actually changed the clipboard (sequence != clear_seq or we captured something)
                if clipboard_sequence_number() != clear_seq || final_selection.is_some() {
                    let _ = set_clipboard_text(&original);
                } else {
                    // No selection captured and clipboard still cleared -> restore original immediately
                    let _ = set_clipboard_text(&original);
                }
            } else if final_selection.is_none() {
                // No original and no selection -> leave clipboard empty (was empty before)
                // Ensure cleared state is visible: already empty, no restore needed
            }
        }

        Ok(final_selection)
    }
    #[cfg(target_os = "linux")]
    {
        let _transaction = CLIPBOARD_INJECTION_LOCK.lock().await;

        // 1. Save original clipboard (best effort).
        let saved_text = get_clipboard_text_linux();

        // 2. Clear clipboard so a subsequent change proves Ctrl+C landed.
        let _ = set_clipboard_text_linux("");

        // 3. Send Ctrl+C.
        send_ctrl_chord_linux('c').map_err(|e| e.to_string())?;

        // 4. Bounded polling: wait for the target app to write to clipboard
        // (up to ~300ms for slow Electron/WebView apps).
        let mut selection: Option<String> = None;
        for _ in 0..10 {
            sleep(Duration::from_millis(30)).await;
            let current = get_clipboard_text_linux();
            if current.as_ref().map(|s| !s.is_empty()).unwrap_or(false) {
                selection = current;
                break;
            }
        }

        // 5. Restore the original clipboard if we still own it.
        if get_clipboard_text_linux() == selection {
            if let Some(original) = saved_text {
                let _ = set_clipboard_text_linux(&original);
            }
        }

        Ok(selection)
    }
    #[cfg(not(any(target_os = "windows", target_os = "linux")))]
    Err("Active selection grabbing not supported on this platform".to_string())
}
