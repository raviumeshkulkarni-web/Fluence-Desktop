// Fluence Windows - UI Automation Reader
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

/// Maximum attempts when reading a fallible UIA boolean property.
const PROP_READ_ATTEMPTS: u32 = 3;
/// Delay between property-read attempts.
const PROP_READ_RETRY_DELAY_MS: u64 = 100;

/// Read a fallible boolean UIA property, retrying transient failures.
/// UIA property reads commonly fail once right after a paste or focus move;
/// a single failed read must not end the monitoring session. Returns None
/// when the property could not be read after all attempts - the caller
/// decides the safe default (fail-safe for password state, fail-open for
/// read-only state where a misread is harmless).
fn read_bool_prop(read: impl Fn() -> WinResult<BOOL>) -> Option<bool> {
    for attempt in 0..PROP_READ_ATTEMPTS {
        match read() {
            Ok(value) => return Some(value.0 != 0),
            Err(e) => {
                if attempt + 1 < PROP_READ_ATTEMPTS {
                    log::debug!(
                        "[AutoLearn] UIA property read failed (attempt {}): {}",
                        attempt + 1,
                        e
                    );
                    std::thread::sleep(std::time::Duration::from_millis(PROP_READ_RETRY_DELAY_MS));
                } else {
                    log::debug!(
                        "[AutoLearn] UIA property read failed after {} attempts: {}",
                        PROP_READ_ATTEMPTS,
                        e
                    );
                }
            }
        }
    }
    None
}

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
    /// The focused element is a password or secure field - skip monitoring.
    SecureField,
    /// The focused element is read-only - no edits possible.
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

        // Reject password/secure fields immediately - never monitor these.
        // The read is retried so a transient failure is not mistaken for a
        // secure field; a persistent failure still aborts (fail-safe).
        match read_bool_prop(|| unsafe { focused.CurrentIsPassword() }) {
            Some(true) => {
                log::info!("[AutoLearn] Focused element is a password field - skipping");
                return None;
            }
            Some(false) => {}
            None => {
                log::warn!("[AutoLearn] Could not determine password state - skipping");
                return None;
            }
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

        // Reject read-only fields - no edits possible. Unreadable state
        // fails open: a truly read-only field simply never changes, so the
        // session times out harmlessly (and the monitor never writes).
        if uses_value
            && read_bool_prop(|| unsafe {
                focused
                    .GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)
                    .and_then(|pattern| pattern.CurrentIsReadOnly())
            }) == Some(true)
        {
            log::info!("[AutoLearn] Focused element is read-only - skipping");
            return None;
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

        // Reject password fields if user tabs into one during monitoring.
        // Fail-safe: an unreadable state still stops the session.
        match read_bool_prop(|| unsafe { focused.CurrentIsPassword() }) {
            Some(true) => {
                log::info!("[AutoLearn] Focused element became a password field - stopping");
                return ReadResult::SecureField;
            }
            Some(false) => {}
            None => {
                log::warn!("[AutoLearn] Could not determine password state - stopping");
                return ReadResult::SecureField;
            }
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

        // Reject read-only fields if user tabs into one during monitoring.
        // An unreadable state fails open (a read-only field never changes,
        // so at worst the session times out without learning anything).
        if current_uses_value
            && read_bool_prop(|| unsafe {
                focused
                    .GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)
                    .and_then(|pattern| pattern.CurrentIsReadOnly())
            }) == Some(true)
        {
            log::info!("[AutoLearn] Focused element became read-only - stopping");
            return ReadResult::ReadOnly;
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

/// RETIRED (plan plan-2, Task 2): `is_valid_correction`.
///
/// The old Android-parity filter (`see WordLcsExtractor`) accepted every
/// pair except case-sensitive-identical / both-<2-chars / digit-mixed -
/// no similarity floor, no corrected-length floor - which let the
/// screenshot garbage pairs (`stuck→agent`, `do→add`, `CS50→tabs`, ...)
/// through at 4-8x frequency. The Android contract is void: Android's own
/// pipeline shows ZERO suggestions after days of use.
///
/// All U-path pair validation now goes through the correction decision
/// core (`super::decision_core`), learned from the reference
/// correction-learner design (similarity skip chain + the
/// LCS rewrite guard).
///
/// The tests below are the original filter's suite, flipped to the single
/// decision core per the plan's test-flip ledger (owner-confirmed
/// OQ-1/2/5/6 defaults).
#[cfg(test)]
mod tests {
    use super::super::decision_core::is_valid_pair;
    use std::collections::BTreeSet;

    // The retired filter's original suite, flipped to the decision core's judge.
    // Assertion changes are annotated with the plan decision they encode.
    fn is_valid_correction(original_word: &str, corrected_word: &str) -> bool {
        is_valid_pair(original_word, corrected_word, &BTreeSet::new())
    }

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
    fn test_reject_identical_pair() {
        assert!(!is_valid_correction("hello", "hello"));
    }

    #[test]
    fn test_accept_short_original() {
        // Android parity: only BOTH sides < 2 chars is rejected.
        assert!(is_valid_correction("ab", "abcd"));
    }

    #[test]
    fn test_accept_short_corrected() {
        // FLIP (OQ-2, owner-confirmed): corrected words < 3 chars no
        // longer surface; manual Words-tab add is the fallback.
        assert!(!is_valid_correction("abcd", "ab"));
    }

    #[test]
    fn test_accept_both_short_but_not_trivial() {
        // FLIP: ab→cd has similarity 0.0 - below the 0.35 floor.
        assert!(!is_valid_correction("ab", "cd"));
    }

    #[test]
    fn test_reject_both_single_char() {
        assert!(!is_valid_correction("a", "b"));
    }

    #[test]
    fn test_allow_case_only_difference() {
        // FLIP (OQ-5, owner-confirmed): case-only pairs are skipped -
        // reference parity; the Words tab's manual Add is the path.
        assert!(!is_valid_correction("hello", "Hello"));
    }

    #[test]
    fn test_allow_case_only_difference_all_upper() {
        // FLIP (OQ-5).
        assert!(!is_valid_correction("HELLO", "hello"));
    }

    #[test]
    fn test_allow_punctuation_only_difference() {
        // FLIP: dr→dr is a 2-char corrected word - rejected by the
        // corrected-length rule ('hello'→hello stays allowed below).
        assert!(!is_valid_correction("dr.", "dr"));
    }

    #[test]
    fn test_allow_punctuation_only_difference_quotes() {
        // Judge level only (Round-4 F4): dist 2/7 (0.286) passes the gates.
        // The full pipeline can never form this candidate - the extractor
        // tokenizes edges on both sides, stripping quotes ('hello'/hello →
        // tokens hello/hello), so no such candidate reaches this judge
        // (asserted in decision_core::test_extract_quoted_tokens_form_no_pair).
        assert!(is_valid_correction("'hello'", "hello"));
    }

    #[test]
    fn test_accept_dissimilar_words() {
        // FLIP: abc→xyz similarity 0.0 - below the floor (similarity gate
        // by design in the ported reference).
        assert!(!is_valid_correction("abc", "xyz"));
    }

    #[test]
    fn test_accept_reported_case() {
        // FLIP (OQ-1, owner-accepted): shwande→Sinead (sim 0.286) sits
        // below the 0.35 floor; no floor admits it without re-admitting
        // do→add (0.333). Manual Words-tab add is the fallback.
        assert!(!is_valid_correction("shwande", "Sinead"));
    }

    #[test]
    fn test_accept_boundary_similarity() {
        // "abc" → "axc" (dist 1/3 = 0.333) passes the 0.35 floor.
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
    fn test_accept_unrelated_words() {
        // FLIP: unrelated words are rejected by the similarity gate; the
        // >50% rewrite vetoes in the ported extractor handle rewrites.
        assert!(!is_valid_correction("banana", "elephant"));
        assert!(!is_valid_correction("computer", "elephant"));
    }

    #[test]
    fn test_3_char_minimum_boundary() {
        // Exactly 3 chars - passes if the gates allow (abc→axc = 0.333).
        assert!(is_valid_correction("abc", "axc"));
    }

    #[test]
    fn test_numeric_words() {
        assert!(is_valid_correction("forteen", "fourteen"));
    }

    #[test]
    fn test_accept_number_to_number() {
        // FLIP (OQ-6, owner-confirmed): exact reference parity - NO
        // numeric-mix or numeric exemption rule is ported; 123→456 is
        // rejected by the similarity gate itself (sim 0.0, dist 3/3).
        assert!(!is_valid_correction("123", "456"));
    }

    #[test]
    fn test_reject_mixed_number_and_word() {
        // Stays rejected WITHOUT any digit rule - the gates do it
        // (ratio 1.0 > 0.65): exact parity behavior, test stays green.
        assert!(!is_valid_correction("123", "abc"));
        assert!(!is_valid_correction("abc", "123"));
    }
}
