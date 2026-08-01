// Fluence Windows — UI Automation Reader
// Reads the focused text field value via Windows UI Automation (UIA).
// This is a read-only operation that does not modify focus, caret, clipboard,
// IME state, or any other aspect of the target application.
//
// Requires COM STA initialization on the calling thread.
// Must be called from the dedicated auto-learn OS thread.
//
// Focus change detection uses UIA's element comparison so edits are only
// attributed to the field that received the injection.

use windows::core::{Result as WinResult, BSTR};
use windows::Win32::Foundation::BOOL;
use windows::Win32::System::Com::{
    CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE,
};
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, IUIAutomationElement, IUIAutomationTextPattern,
    IUIAutomationValuePattern, UIA_TextPatternId, UIA_ValuePatternId,
};

use super::extraction::strip_punctuation;

/// RAII guard for COM initialization. Calls CoUninitialize on drop.
struct ComGuard;

impl ComGuard {
    fn new() -> WinResult<Self> {
        unsafe {
            CoInitializeEx(
                Some(std::ptr::null()),
                COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE,
            )
            .ok()?;
        }
        Ok(ComGuard)
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        unsafe {
            CoUninitialize();
        }
    }
}

/// Result of reading the focused text field.
pub enum ReadResult {
    /// Successfully read the text field value.
    Value(String),
    /// The focused element has no text value (e.g., a button or dropdown).
    NoValue,
    /// Could not get the focused element (no focus, or UIA failed).
    NoElement,
    /// The focused element changed since the initial read (focus moved).
    FocusChanged,
    /// The focused element is a password or secure field — skip monitoring.
    SecureField,
    /// The focused element is read-only — no edits possible.
    ReadOnly,
}

/// State of the UIA text reader for a single monitoring session.
/// Holds COM objects that must be released in order.
pub struct FocusedTextReader {
    _com: ComGuard,
    automation: IUIAutomation,
    /// The field that was focused when this monitoring session started.
    /// The monitor must never read a different focused text control.
    initial_element: IUIAutomationElement,
}

impl FocusedTextReader {
    /// Create a new reader and capture the initially focused element.
    /// Returns None if COM or UIA initialization fails, or if no text-capable
    /// element is focused.
    pub fn new() -> Option<Self> {
        let com = match ComGuard::new() {
            Ok(c) => c,
            Err(e) => {
                log::warn!("[AutoLearn] COM initialization failed: {}", e);
                return None;
            }
        };

        let automation: IUIAutomation = match unsafe {
            windows::Win32::System::Com::CoCreateInstance(
                &CUIAutomation,
                None,
                windows::Win32::System::Com::CLSCTX_INPROC_SERVER,
            )
        } {
            Ok(a) => a,
            Err(e) => {
                log::warn!("[AutoLearn] Failed to create IUIAutomation: {}", e);
                return None;
            }
        };

        // Get the focused element and verify it's a text field
        let focused = match Self::get_focused_element_static(&automation) {
            Some(e) => e,
            None => {
                log::debug!("[AutoLearn] No focused element found during init");
                return None;
            }
        };

        // Reject password/secure fields immediately — never monitor these
        let is_password = unsafe { focused.CurrentIsPassword().unwrap_or(BOOL(1)) };
        if is_password.0 != 0 {
            log::info!("[AutoLearn] Focused element is a password field — skipping");
            return None;
        }

        // Check what patterns the element supports
        let uses_value = unsafe {
            focused
                .GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)
                .is_ok()
        };
        let uses_text = unsafe {
            focused
                .GetCurrentPatternAs::<IUIAutomationTextPattern>(UIA_TextPatternId)
                .is_ok()
        };

        if !uses_value && !uses_text {
            log::debug!("[AutoLearn] Focused element doesn't support text patterns");
            return None;
        }

        // Reject read-only fields — no edits possible
        if uses_value {
            let is_readonly = unsafe {
                let pattern: IUIAutomationValuePattern = focused
                    .GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)
                    .ok()?;
                pattern.CurrentIsReadOnly().unwrap_or(BOOL(1))
            };
            if is_readonly.0 != 0 {
                log::info!("[AutoLearn] Focused element is read-only — skipping");
                return None;
            }
        }

        log::debug!(
            "[AutoLearn] UIA reader initialized (value_pattern={})",
            uses_value
        );

        Some(FocusedTextReader {
            _com: com,
            automation,
            initial_element: focused,
        })
    }

    /// Read the current value of the focused text field.
    pub fn read_current_value(&self) -> ReadResult {
        let focused = match self.get_focused_element() {
            Some(e) => e,
            None => {
                log::debug!("[AutoLearn] No focused element");
                return ReadResult::NoElement;
            }
        };

        // UIA focus can move to another text field while the monitor is
        // active. Compare the actual element identity, not just its pattern
        // support, before reading any content.
        let same_element = unsafe {
            self.automation
                .CompareElements(Some(&self.initial_element), Some(&focused))
                .map(|same| same.0 != 0)
                .unwrap_or(false)
        };
        if !same_element {
            log::debug!("[AutoLearn] Focus moved to a different text element");
            return ReadResult::FocusChanged;
        }

        // Reject password fields if user tabs into one during monitoring
        let is_password = unsafe { focused.CurrentIsPassword().unwrap_or(BOOL(1)) };
        if is_password.0 != 0 {
            log::info!("[AutoLearn] Focused element became a password field — stopping");
            return ReadResult::SecureField;
        }

        // Check if the element still supports the same pattern type.
        // If the initial element used ValuePattern but the current one doesn't,
        // focus has moved to a non-text element.
        let current_uses_value = unsafe {
            focused
                .GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)
                .is_ok()
        };
        let current_uses_text = unsafe {
            focused
                .GetCurrentPatternAs::<IUIAutomationTextPattern>(UIA_TextPatternId)
                .is_ok()
        };

        if !current_uses_value && !current_uses_text {
            log::debug!(
                "[AutoLearn] Focused element no longer supports text patterns (focus changed?)"
            );
            return ReadResult::FocusChanged;
        }

        // Reject read-only fields if user tabs into one during monitoring
        if current_uses_value {
            let is_readonly: bool = unsafe {
                let is_ro: Option<BOOL> = focused
                    .GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)
                    .ok()
                    .and_then(|p| p.CurrentIsReadOnly().ok());
                is_ro.map(|v| v.0 != 0).unwrap_or(true)
            };
            if is_readonly {
                log::info!("[AutoLearn] Focused element became read-only — stopping");
                return ReadResult::ReadOnly;
            }
        }

        // Try ValuePattern first (most common for text fields)
        if current_uses_value {
            if let Some(value) = self.try_read_value_pattern(&focused) {
                return ReadResult::Value(value);
            }
        }

        // Fallback to TextPattern (used by RichEdit, Monaco, Electron, etc.)
        if current_uses_text {
            if let Some(value) = self.try_read_text_pattern(&focused) {
                return ReadResult::Value(value);
            }
        }

        ReadResult::NoValue
    }

    /// Get the currently focused UIA element.
    fn get_focused_element(&self) -> Option<IUIAutomationElement> {
        Self::get_focused_element_static(&self.automation)
    }

    fn get_focused_element_static(automation: &IUIAutomation) -> Option<IUIAutomationElement> {
        unsafe { automation.GetFocusedElement().ok() }
    }

    /// Try to read text via UIA ValuePattern. Returns None on failure.
    fn try_read_value_pattern(&self, element: &IUIAutomationElement) -> Option<String> {
        let pattern: IUIAutomationValuePattern = unsafe {
            element
                .GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)
                .ok()?
        };

        let value: BSTR = unsafe { pattern.CurrentValue().ok()? };
        let text = value.to_string();

        if text.is_empty() {
            None
        } else {
            Some(text)
        }
    }

    /// Try to read text via UIA TextPattern. Returns None on failure.
    fn try_read_text_pattern(&self, element: &IUIAutomationElement) -> Option<String> {
        let pattern: IUIAutomationTextPattern = unsafe {
            element
                .GetCurrentPatternAs::<IUIAutomationTextPattern>(UIA_TextPatternId)
                .ok()?
        };

        let range = unsafe { pattern.DocumentRange().ok() }?;

        let value: BSTR = unsafe { range.GetText(-1).ok()? };
        let text = value.to_string();

        if text.is_empty() {
            None
        } else {
            Some(text)
        }
    }
}

/// Check if a word looks like a high-confidence correction.
/// This applies the same conservative rules as the pipeline extraction.
pub fn is_valid_correction(original_word: &str, corrected_word: &str) -> bool {
    if original_word.is_empty() || corrected_word.is_empty() {
        return false;
    }

    if original_word.len() < 3 || corrected_word.len() < 3 {
        return false;
    }

    if original_word.to_lowercase() == corrected_word.to_lowercase() {
        return false;
    }

    let orig_stripped = strip_punctuation(original_word);
    let corr_stripped = strip_punctuation(corrected_word);
    if orig_stripped == corr_stripped {
        return false;
    }

    let similarity = strsim::normalized_levenshtein(
        &original_word.to_lowercase(),
        &corrected_word.to_lowercase(),
    );

    if similarity < 0.40 {
        return false;
    }

    true
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── is_valid_correction tests ────────────────────────────────

    #[test]
    fn test_valid_correction_phonetic() {
        assert!(is_valid_correction("shunade", "Sinead"));
    }

    #[test]
    fn test_valid_correction_common_misspelling() {
        assert!(is_valid_correction("Johnatan", "Jonathan"));
    }

    #[test]
    fn test_reject_empty_original() {
        assert!(!is_valid_correction("", "hello"));
    }

    #[test]
    fn test_reject_empty_corrected() {
        assert!(!is_valid_correction("hello", ""));
    }

    #[test]
    fn test_reject_both_empty() {
        assert!(!is_valid_correction("", ""));
    }

    #[test]
    fn test_reject_short_original() {
        assert!(!is_valid_correction("ab", "abcd"));
    }

    #[test]
    fn test_reject_short_corrected() {
        assert!(!is_valid_correction("abcd", "ab"));
    }

    #[test]
    fn test_reject_both_short() {
        assert!(!is_valid_correction("ab", "cd"));
    }

    #[test]
    fn test_reject_case_only_difference() {
        assert!(!is_valid_correction("hello", "Hello"));
    }

    #[test]
    fn test_reject_case_only_difference_all_upper() {
        assert!(!is_valid_correction("HELLO", "hello"));
    }

    #[test]
    fn test_reject_punctuation_only_difference() {
        assert!(!is_valid_correction("dr.", "dr"));
    }

    #[test]
    fn test_reject_punctuation_only_difference_quotes() {
        assert!(!is_valid_correction("'hello'", "hello"));
    }

    #[test]
    fn test_reject_low_similarity() {
        // "abc" → "xyz" has very low similarity
        assert!(!is_valid_correction("abc", "xyz"));
    }

    #[test]
    fn test_accept_boundary_similarity() {
        // "abc" → "axc" has high similarity (~0.667)
        assert!(is_valid_correction("abc", "axc"));
    }

    #[test]
    fn test_valid_correction_real_world() {
        // Common voice recognition errors
        assert!(is_valid_correction("recognition", "recognision"));
        assert!(is_valid_correction("definately", "definitely"));
        assert!(is_valid_correction("accomodate", "accommodate"));
    }

    #[test]
    fn test_reject_unrelated_words() {
        assert!(!is_valid_correction("banana", "elephant"));
        assert!(!is_valid_correction("computer", "elephant"));
    }

    #[test]
    fn test_3_char_minimum_boundary() {
        // Exactly 3 chars — should pass if other conditions met
        assert!(is_valid_correction("abc", "axc"));
    }

    #[test]
    fn test_numeric_words() {
        // Numeric words should work if they meet criteria
        assert!(is_valid_correction("forteen", "fourteen"));
    }
}
