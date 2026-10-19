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

/// Bounded validation limit (security hardening, additive only).
/// Normal transcriptions are far below this; oversized input is rejected
/// with a clean error instead of a multi-MB GlobalAlloc.
pub const MAX_INJECT_TEXT_CHARS: usize = 100_000;

fn check_inject_text_len(text: &str) -> Result<(), String> {
    if text.chars().count() > MAX_INJECT_TEXT_CHARS {
        return Err(format!(
            "Text too long ({} chars). Maximum is {} characters.",
            text.chars().count(),
            MAX_INJECT_TEXT_CHARS
        ));
    }
    Ok(())
}

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
///
/// Allocation and fill happen BEFORE the clipboard is opened or emptied.
/// `EmptyClipboard()` is irreversible, so every fallible step must precede
/// it: previously a failed `GlobalAlloc`/`GlobalLock` left the user with an
/// empty clipboard, and the text-only snapshot cannot restore content it
/// never read.
#[cfg(target_os = "windows")]
fn set_clipboard_text(text: &str) -> Result<()> {
    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let byte_len = wide.len() * 2;

    // 1. Prepare the memory while the clipboard is still untouched.
    let hmem = unsafe { GlobalAlloc(GMEM_MOVEABLE, byte_len) }
        .map_err(|e| anyhow!("GlobalAlloc failed: {}", e))?;
    let ptr = unsafe { GlobalLock(hmem) };
    if ptr.is_null() {
        let _ = unsafe { GlobalFree(hmem) };
        return Err(anyhow!("GlobalLock failed"));
    }
    unsafe {
        std::ptr::copy_nonoverlapping(wide.as_ptr() as *const u8, ptr as *mut u8, byte_len);
        let _ = GlobalUnlock(hmem);
    }

    // 2. Only now is it safe to destroy what is there.
    unsafe {
        if let Err(e) = open_clipboard_with_retry() {
            let _ = GlobalFree(hmem);
            return Err(e);
        }
        if let Err(e) = EmptyClipboard() {
            let _ = CloseClipboard();
            let _ = GlobalFree(hmem);
            return Err(anyhow!("EmptyClipboard failed: {}", e));
        }
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

/// Clipboard formats that text-only snapshot/restore cannot bring back.
/// Injection calls `EmptyClipboard()`, so anything here is destroyed with no
/// recovery path. Text and text derivatives (HTML/RTF, which ride along
/// whenever a browser or Word copies) are deliberately not listed - warning
/// on those would fire constantly and train users to ignore the warning.
#[cfg(target_os = "windows")]
const UNPRESERVABLE_FORMATS: &[(&str, u32)] = &[
    ("bitmap", 2),
    ("DIB", 8),
    ("enhanced metafile", 14),
    ("file drop list", 15),
    ("DIBV5", 17),
];

/// True when the clipboard holds at least one format that `inject_text`
/// would silently destroy. Returns false if the clipboard cannot be read:
/// the caller then proceeds as before rather than blocking injection on a
/// failed probe.
#[cfg(target_os = "windows")]
fn clipboard_has_unpreservable_formats() -> bool {
    if open_clipboard_with_retry().is_err() {
        return false;
    }
    let found = UNPRESERVABLE_FORMATS
        .iter()
        .find(|(_, fmt)| unsafe { IsClipboardFormatAvailable(*fmt) }.is_ok())
        .map(|(name, _)| *name);
    let _ = unsafe { CloseClipboard() };
    if let Some(name) = found {
        log::warn!("clipboard holds {name} data that text injection would destroy");
    }
    found.is_some()
}

/// Linux counterpart of the Windows format scan above. arboard only
/// surfaces text and images here, so an image sitting on the clipboard is
/// the destructive case this port can detect (the screenshot-then-dictate
/// flow). Returns false when the clipboard cannot be read.
#[cfg(target_os = "linux")]
fn clipboard_has_unpreservable_formats() -> bool {
    let Ok(mut clipboard) = linux_clipboard() else {
        return false;
    };
    if clipboard.get().image().is_ok() {
        log::warn!("clipboard holds image data that text injection would destroy");
        return true;
    }
    false
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

/// Send Ctrl+V keystroke via SendInput. Returns the number of events the
/// system actually accepted so callers can detect and report partial
/// delivery instead of presenting a paste that never happened as success.
#[cfg(target_os = "windows")]
fn send_ctrl_v() -> u32 {
    let inputs = [
        make_key_input(VK_CONTROL, KEYBD_EVENT_FLAGS(0)),
        make_key_input(VK_V, KEYBD_EVENT_FLAGS(0)),
        make_key_input(VK_V, KEYEVENTF_KEYUP),
        make_key_input(VK_CONTROL, KEYEVENTF_KEYUP),
    ];
    unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) }
}

/// Send N backspace keystrokes via SendInput. Returns the number of events
/// the system actually accepted; the caller compares against `count * 2`.
#[cfg(target_os = "windows")]
pub fn send_backspaces(count: usize) -> u32 {
    use windows::Win32::UI::Input::KeyboardAndMouse::VK_BACK;
    const MAX_DELETE_CHARS: usize = 10_000;
    if count == 0 {
        return 0;
    }
    if count > MAX_DELETE_CHARS {
        log::warn!("Ignoring oversized backspace request: {}", count);
        return 0;
    }
    let mut inputs: Vec<INPUT> = Vec::with_capacity(count.saturating_mul(2));
    for _ in 0..count {
        inputs.push(make_key_input(VK_BACK, KEYBD_EVENT_FLAGS(0)));
        inputs.push(make_key_input(VK_BACK, KEYEVENTF_KEYUP));
    }
    unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) }
}

/// Send Enter key via SendInput. Returns the accepted event count (2 is a
/// full press+release pair).
#[cfg(target_os = "windows")]
pub fn send_enter() -> u32 {
    use windows::Win32::UI::Input::KeyboardAndMouse::VK_RETURN;
    let inputs = [
        make_key_input(VK_RETURN, KEYBD_EVENT_FLAGS(0)),
        make_key_input(VK_RETURN, KEYEVENTF_KEYUP),
    ];
    unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) }
}

/// Send Ctrl+A (select all) via SendInput. Returns the accepted event count
/// (4 is a full chord).
#[cfg(target_os = "windows")]
pub fn send_select_all() -> u32 {
    use windows::Win32::UI::Input::KeyboardAndMouse::VK_A;
    let inputs = [
        make_key_input(VK_CONTROL, KEYBD_EVENT_FLAGS(0)),
        make_key_input(VK_A, KEYBD_EVENT_FLAGS(0)),
        make_key_input(VK_A, KEYEVENTF_KEYUP),
        make_key_input(VK_CONTROL, KEYEVENTF_KEYUP),
    ];
    unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) }
}

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

#[cfg(target_os = "linux")]
fn send_single_key_linux(key: enigo::Key) -> Result<()> {
    send_key_clicks_linux(key, 1)
}

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
pub async fn inject_text(
    window: tauri::Window,
    text: String,
    monitor_auto_learn: Option<bool>,
) -> Result<bool, String> {
    // Text injection simulates paste into the user's applications: the
    // overlay is its only legitimate caller.
    crate::acl::require_caller(&window, &[crate::acl::OVERLAY_WINDOW])?;
    check_inject_text_len(&text)?;
    #[cfg(target_os = "windows")]
    {
        // Paste goes wherever the user's cursor is when transcription finishes.
        // No target-window check: a common flow is to read in one app while
        // dictating and then paste into another, which that check refused.
        let _transaction = CLIPBOARD_INJECTION_LOCK.lock().await;
        let start_time = std::time::Instant::now();

        // NOTE: injection has to EmptyClipboard(), so non-text clipboard
        // content (image, copied files) is replaced and cannot be restored.
        // It is detected and REPORTED, never blocked: refusing here broke
        // the exact screenshot-and-dictate workflow that needs it most (the
        // user pastes by replacing the clipboard constantly). The caller
        // learns it happened via the returned flag and can surface a notice.
        let replaced_clipboard_content = clipboard_has_unpreservable_formats();

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

        // Send Ctrl+V. SendInput can be partially or fully blocked (secure
        // desktop, focus transition, UIPI restrictions); the delivered count
        // is the only truth about whether the chord actually happened.
        let ctrlv_sent = send_ctrl_v();
        let paste_ok = ctrlv_sent == 4;

        // Restore physical modifier keys
        restore_modifiers(&released);
        let ctrlv_duration = ctrlv_start.elapsed();

        log::info!(
            "inject_text performance: total = {:?}, save_clip = {:?}, set_clip = {:?}, send_ctrl_v = {:?} ({}/4 events)",
            start_time.elapsed(),
            save_duration,
            set_duration,
            ctrlv_duration,
            ctrlv_sent
        );

        // Only the normal transcription flow opts into post-injection
        // learning. Agent-generated inserts use the same clipboard command
        // but are not transcription evidence. A paste that was never
        // delivered is not transcription evidence either.
        let should_monitor = paste_ok
            && monitor_auto_learn.unwrap_or(false)
            && crate::settings::load_settings()
                .map(|settings| settings.auto_learn_enabled)
                .unwrap_or(false);
        if should_monitor {
            crate::auto_learn::start_post_injection_monitor(text.clone());
        }

        // Restore only if Fluence still owns the clipboard. If the user or
        // another application copied something during the paste window, do
        // not overwrite that newer clipboard content. This runs even on a
        // partial paste so the user's clipboard is returned.
        sleep(Duration::from_millis(200)).await;
        if clipboard_sequence_number() == fluence_clipboard_sequence
            && get_clipboard_text().as_deref() == Some(text.as_str())
        {
            if let Some(original) = saved {
                let _ = set_clipboard_text(&original);
            }
        }

        if paste_ok {
            Ok(replaced_clipboard_content)
        } else {
            log::warn!(
                "inject_text: SendInput delivered {} of 4 key events; reporting paste failure",
                ctrlv_sent
            );
            Err(format!(
                "PASTE_FAILED: SendInput delivered {} of 4 key events",
                ctrlv_sent
            ))
        }
    }
    #[cfg(target_os = "linux")]
    {
        let _transaction = CLIPBOARD_INJECTION_LOCK.lock().await;
        let replaced_clipboard_content = clipboard_has_unpreservable_formats();
        let saved = get_clipboard_text_linux();

        set_clipboard_text_linux(&text).map_err(|e| e.to_string())?;

        send_ctrl_chord_linux('v').map_err(|e| e.to_string())?;
        log::info!("inject_text via clipboard + Ctrl+V ({} chars)", text.len());

        let should_monitor = monitor_auto_learn.unwrap_or(false)
            && crate::settings::load_settings()
                .map(|settings| settings.auto_learn_enabled)
                .unwrap_or(false);
        if should_monitor {
            crate::auto_learn::start_post_injection_monitor(text.clone());
        }

        sleep(Duration::from_millis(200)).await;
        if get_clipboard_text_linux().as_deref() == Some(text.as_str()) {
            if let Some(original) = saved {
                let _ = set_clipboard_text_linux(&original);
            }
        }

        Ok(replaced_clipboard_content)
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
pub async fn copy_text(window: tauri::Window, text: String) -> Result<(), String> {
    // Agent results are copied from the overlay; the History page reuses
    // the same command for its copy buttons (main window).
    crate::acl::require_caller(
        &window,
        &[crate::acl::OVERLAY_WINDOW, crate::acl::MAIN_WINDOW],
    )?;
    check_inject_text_len(&text)?;
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

/// Execute a keyboard action from agent mode. Delivery is reported
/// truthfully: a partially delivered SendInput is an error, and unknown
/// actions are rejected instead of silently doing nothing.
#[tauri::command]
pub async fn execute_keyboard_action(
    window: tauri::Window,
    action: String,
    char_count: Option<usize>,
) -> Result<(), String> {
    // Keyboard simulation acts on the user's focused application: the
    // overlay is its only legitimate caller.
    crate::acl::require_caller(&window, &[crate::acl::OVERLAY_WINDOW])?;
    #[cfg(target_os = "windows")]
    {
        // Like inject_text: keystrokes go to whatever holds focus now.
        #[cfg_attr(not(target_os = "windows"), allow(unused))]
        sleep(Duration::from_millis(80)).await;
        match action.as_str() {
            "delete_chars" => {
                let count =
                    char_count.ok_or_else(|| "delete_chars requires a charCount".to_string())?;
                if count > 10_000 {
                    return Err("Delete action exceeds maximum length".to_string());
                }
                let sent = send_backspaces(count);
                if sent != (count * 2) as u32 {
                    return Err(format!(
                        "KEYBOARD_ACTION_FAILED: SendInput delivered {} of {} key events",
                        sent,
                        count * 2
                    ));
                }
                Ok(())
            }
            "select_all" => {
                let sent = send_select_all();
                if sent != 4 {
                    return Err(format!(
                        "KEYBOARD_ACTION_FAILED: SendInput delivered {} of 4 key events",
                        sent
                    ));
                }
                Ok(())
            }
            "submit" => {
                let sent = send_enter();
                if sent != 2 {
                    return Err(format!(
                        "KEYBOARD_ACTION_FAILED: SendInput delivered {} of 2 key events",
                        sent
                    ));
                }
                Ok(())
            }
            other => Err(format!("Unknown keyboard action '{}'", other)),
        }
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
pub async fn grab_active_selection(window: tauri::Window) -> Result<Option<String>, String> {
    // Selection capture reads the foreground application's clipboard state
    // for the next agent turn: overlay-only.
    crate::acl::require_caller(&window, &[crate::acl::OVERLAY_WINDOW])?;
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

        let saved_text = get_clipboard_text_linux();

        let _ = set_clipboard_text_linux("");

        send_ctrl_chord_linux('c').map_err(|e| e.to_string())?;

        let mut selection: Option<String> = None;
        for _ in 0..10 {
            sleep(Duration::from_millis(30)).await;
            let current = get_clipboard_text_linux();
            if current.as_ref().map(|s| !s.is_empty()).unwrap_or(false) {
                selection = current;
                break;
            }
        }

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
