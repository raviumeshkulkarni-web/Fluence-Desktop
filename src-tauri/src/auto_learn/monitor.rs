// Fluence Windows — Auto-Learn Monitor
// Runs on a dedicated OS thread with COM STA.
// Monitors the focused text field for user edits after text injection.
// Uses adaptive polling: fast initially, then slower to reduce overhead.
//
// Lifecycle:
// 1. Receive injected text via channel
// 2. Wait 500ms for paste to settle in target app
// 3. Initialize UIA reader (captures initial element)
// 4. Poll with adaptive intervals for up to 30 seconds
// 5. On change: extract corrections, save to suggestions
// 6. Stop: timeout, focus change, element gone, or error

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use serde::Serialize;

use super::correction_extractor::extract_user_corrections;
use super::learner;
use super::ui_automation::{FocusedTextReader, ReadResult};
use super::MonitorRequest;

/// How long to wait after paste before starting to monitor.
const POST_PASTE_DELAY_MS: u64 = 500;

/// Total monitoring duration.
const MONITOR_TIMEOUT_MS: u64 = 30_000;

/// Adaptive polling intervals (in milliseconds).
const POLL_FAST_MS: u64 = 250; // 0–5 seconds
const POLL_MEDIUM_MS: u64 = 750; // 5–15 seconds
const POLL_SLOW_MS: u64 = 2000; // 15–30 seconds

/// Thresholds for adaptive polling.
const FAST_THRESHOLD_MS: u64 = 5000;
const MEDIUM_THRESHOLD_MS: u64 = 15000;

/// Maximum output characters from UIA before we consider it unreliable.
const MAX_FIELD_CHARS: usize = 50_000;

// ── Structured Session Results ────────────────────────────────────

/// Why a monitoring session ended.
#[derive(Debug, Clone, Serialize)]
pub enum ExitReason {
    /// A newer injection superseded this monitoring session.
    Superseded,
    /// Session timed out (30s reached) without detecting a change.
    Timeout,
    /// Focused element lost text patterns (user clicked away).
    FocusChanged,
    /// Focused element is a password/secure field.
    SecureFieldDetected,
    /// Focused element is read-only.
    ReadOnlyField,
    /// No focused element available.
    NoElement,
    /// Focused element has no text value.
    NoValue,
    /// Field content exceeds MAX_FIELD_CHARS.
    FieldTooLarge { chars: usize },
    /// UIA reader could not be created (COM init, no text patterns, etc.).
    ReaderInitFailed,
}

/// Structured result of a single monitoring session.
/// Returned by `run_monitoring_session` for diagnostics and logging.
#[derive(Debug, Clone, Serialize)]
pub struct SessionResult {
    /// Why the session ended.
    pub exit_reason: ExitReason,
    /// Total wall-clock duration in milliseconds (including 500ms post-paste delay).
    pub duration_ms: u64,
    /// Number of UIA poll cycles executed.
    pub poll_count: u32,
    /// Whether the field value changed at least once during monitoring.
    pub value_changed: bool,
    /// Number of correction candidates saved to the suggestion database.
    pub corrections_count: u32,
}

// ── Monitor Thread ────────────────────────────────────────────────

/// Entry point for the dedicated auto-learn OS thread.
/// Blocks on the channel receiver, processing one injection at a time.
pub fn monitoring_thread(rx: Receiver<MonitorRequest>, active_generation: &AtomicU64) {
    log::debug!("[AutoLearn] Monitor thread started");

    // This thread needs COM STA for UIA
    match super::ui_automation::FocusedTextReader::new() {
        // We just test COM init here; actual reader is created per-session
        None => {
            // COM init failed — but we can't proceed, so just loop on the channel
            // to avoid crashing. We'll re-create the reader per session anyway.
            log::warn!("[AutoLearn] Initial COM check failed, will retry per session");
        }
        Some(_) => {
            log::debug!("[AutoLearn] COM initialized successfully on monitor thread");
        }
    }

    loop {
        match rx.recv() {
            Ok(request) => {
                if request.generation != active_generation.load(Ordering::Acquire) {
                    continue;
                }

                if request.injected_text.trim().is_empty() {
                    log::debug!("[AutoLearn] Received empty text, skipping");
                    continue;
                }

                log::info!(
                    "[AutoLearn] Starting monitoring session ({} chars)",
                    request.injected_text.len()
                );

                let result = run_monitoring_session(
                    &request.injected_text,
                    request.generation,
                    active_generation,
                );

                log_session_result(&result);
            }
            Err(std::sync::mpsc::RecvError) => {
                log::debug!("[AutoLearn] Channel closed, monitor thread exiting");
                break;
            }
        }
    }
}

// ── Session Logic ─────────────────────────────────────────────────

/// Run a single monitoring session for one injection.
/// Creates a fresh UIA reader and polls until done.
/// Returns a structured `SessionResult` with exit reason and diagnostics.
fn run_monitoring_session(
    injected_text: &str,
    generation: u64,
    active_generation: &AtomicU64,
) -> SessionResult {
    let session_start = Instant::now();
    let mut poll_count: u32 = 0;
    let mut value_changed = false;
    let mut corrections_count: u32 = 0;

    if generation != active_generation.load(Ordering::Acquire) {
        return SessionResult {
            exit_reason: ExitReason::Superseded,
            duration_ms: 0,
            poll_count: 0,
            value_changed: false,
            corrections_count: 0,
        };
    }

    // Wait for paste to settle in target app
    std::thread::sleep(Duration::from_millis(POST_PASTE_DELAY_MS));

    if generation != active_generation.load(Ordering::Acquire) {
        return SessionResult {
            exit_reason: ExitReason::Superseded,
            duration_ms: session_start.elapsed().as_millis() as u64,
            poll_count: 0,
            value_changed: false,
            corrections_count: 0,
        };
    }

    // Create UIA reader for this session
    let reader = match FocusedTextReader::new() {
        Some(r) => r,
        None => {
            log::debug!("[AutoLearn] Could not create UIA reader, stopping session");
            return SessionResult {
                exit_reason: ExitReason::ReaderInitFailed,
                duration_ms: session_start.elapsed().as_millis() as u64,
                poll_count: 0,
                value_changed: false,
                corrections_count: 0,
            };
        }
    };

    // Read the initial value
    let initial_value = match reader.read_current_value() {
        ReadResult::Value(v) => {
            if v.len() > MAX_FIELD_CHARS {
                log::debug!(
                    "[AutoLearn] Initial field too large ({} chars), skipping",
                    v.len()
                );
                return SessionResult {
                    exit_reason: ExitReason::FieldTooLarge { chars: v.len() },
                    duration_ms: session_start.elapsed().as_millis() as u64,
                    poll_count: 0,
                    value_changed: false,
                    corrections_count: 0,
                };
            }
            v
        }
        ReadResult::NoValue => {
            log::debug!("[AutoLearn] Focused element has no text value, stopping");
            return SessionResult {
                exit_reason: ExitReason::NoValue,
                duration_ms: session_start.elapsed().as_millis() as u64,
                poll_count: 0,
                value_changed: false,
                corrections_count: 0,
            };
        }
        ReadResult::NoElement => {
            log::debug!("[AutoLearn] No focused element found, stopping");
            return SessionResult {
                exit_reason: ExitReason::NoElement,
                duration_ms: session_start.elapsed().as_millis() as u64,
                poll_count: 0,
                value_changed: false,
                corrections_count: 0,
            };
        }
        ReadResult::FocusChanged => {
            log::debug!("[AutoLearn] Focus changed during init, stopping");
            return SessionResult {
                exit_reason: ExitReason::FocusChanged,
                duration_ms: session_start.elapsed().as_millis() as u64,
                poll_count: 0,
                value_changed: false,
                corrections_count: 0,
            };
        }
        ReadResult::SecureField => {
            log::info!("[AutoLearn] Focused element is a password field, not monitoring");
            return SessionResult {
                exit_reason: ExitReason::SecureFieldDetected,
                duration_ms: session_start.elapsed().as_millis() as u64,
                poll_count: 0,
                value_changed: false,
                corrections_count: 0,
            };
        }
        ReadResult::ReadOnly => {
            log::debug!("[AutoLearn] Focused element is read-only, not monitoring");
            return SessionResult {
                exit_reason: ExitReason::ReadOnlyField,
                duration_ms: session_start.elapsed().as_millis() as u64,
                poll_count: 0,
                value_changed: false,
                corrections_count: 0,
            };
        }
    };

    log::debug!(
        "[AutoLearn] Initial field value captured ({} chars)",
        initial_value.len()
    );

    let mut last_value = initial_value;
    let exit_reason: ExitReason;

    // Adaptive polling loop
    loop {
        if generation != active_generation.load(Ordering::Acquire) {
            exit_reason = ExitReason::Superseded;
            break;
        }

        let elapsed = session_start.elapsed().as_millis() as u64;
        if elapsed >= MONITOR_TIMEOUT_MS {
            log::debug!("[AutoLearn] Timeout reached ({}ms), stopping", elapsed);
            exit_reason = ExitReason::Timeout;
            break;
        }

        // Determine poll interval based on elapsed time
        let poll_ms = if elapsed < FAST_THRESHOLD_MS {
            POLL_FAST_MS
        } else if elapsed < MEDIUM_THRESHOLD_MS {
            POLL_MEDIUM_MS
        } else {
            POLL_SLOW_MS
        };

        std::thread::sleep(Duration::from_millis(poll_ms));
        poll_count += 1;

        // Read current value
        match reader.read_current_value() {
            ReadResult::Value(current_value) => {
                if current_value.len() > MAX_FIELD_CHARS {
                    log::debug!(
                        "[AutoLearn] Field value too large ({} chars), stopping",
                        current_value.len()
                    );
                    exit_reason = ExitReason::FieldTooLarge {
                        chars: current_value.len(),
                    };
                    break;
                }

                if current_value != last_value {
                    log::debug!(
                        "[AutoLearn] Field value changed ({} → {} chars)",
                        last_value.len(),
                        current_value.len()
                    );

                    value_changed = true;

                    // Extract corrections from the diff
                    let corrections = extract_user_corrections(injected_text, &current_value);

                    if !corrections.is_empty() {
                        match learner::save_corrections(corrections) {
                            Ok(count) => {
                                log::info!(
                                    "[AutoLearn] Saved {} corrections after {}ms",
                                    count,
                                    session_start.elapsed().as_millis()
                                );
                                corrections_count += count as u32;
                            }
                            Err(e) => {
                                log::warn!("[AutoLearn] Failed to save corrections: {}", e);
                            }
                        }
                    }

                    last_value = current_value;
                }
            }
            ReadResult::NoValue => {
                log::debug!("[AutoLearn] Field has no value, stopping");
                exit_reason = ExitReason::NoValue;
                break;
            }
            ReadResult::NoElement => {
                log::debug!("[AutoLearn] No focused element, stopping");
                exit_reason = ExitReason::NoElement;
                break;
            }
            ReadResult::FocusChanged => {
                log::debug!(
                    "[AutoLearn] Focus changed after {}ms, stopping",
                    session_start.elapsed().as_millis()
                );
                exit_reason = ExitReason::FocusChanged;
                break;
            }
            ReadResult::SecureField => {
                log::info!("[AutoLearn] User tabbed into password field, stopping");
                exit_reason = ExitReason::SecureFieldDetected;
                break;
            }
            ReadResult::ReadOnly => {
                log::info!("[AutoLearn] User tabbed into read-only field, stopping");
                exit_reason = ExitReason::ReadOnlyField;
                break;
            }
        }
    }

    if corrections_count > 0 {
        log::info!(
            "[AutoLearn] Session complete: {} corrections learned in {}ms",
            corrections_count,
            session_start.elapsed().as_millis()
        );
    } else {
        log::debug!(
            "[AutoLearn] Session complete: no corrections detected in {}ms",
            session_start.elapsed().as_millis()
        );
    }

    SessionResult {
        exit_reason,
        duration_ms: session_start.elapsed().as_millis() as u64,
        poll_count,
        value_changed,
        corrections_count,
    }
}

// ── Diagnostics ───────────────────────────────────────────────────

/// Log session result. Structured diagnostics gated behind debug assertions.
fn log_session_result(result: &SessionResult) {
    #[cfg(debug_assertions)]
    log::debug!(
        "[AutoLearn] Session diagnostic: {}",
        serde_json::to_string(result).unwrap_or_else(|_| format!("{:?}", result))
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_exit_reason_serialization() {
        let reasons = vec![
            ExitReason::Superseded,
            ExitReason::Timeout,
            ExitReason::FocusChanged,
            ExitReason::SecureFieldDetected,
            ExitReason::ReadOnlyField,
            ExitReason::NoElement,
            ExitReason::NoValue,
            ExitReason::FieldTooLarge { chars: 99999 },
            ExitReason::ReaderInitFailed,
        ];
        for reason in &reasons {
            let json = serde_json::to_string(reason).unwrap();
            assert!(!json.is_empty(), "Failed to serialize {:?}", reason);
        }
    }

    #[test]
    fn test_session_result_serialization() {
        let result = SessionResult {
            exit_reason: ExitReason::Timeout,
            duration_ms: 30000,
            poll_count: 120,
            value_changed: true,
            corrections_count: 2,
        };
        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains("\"exit_reason\""));
        assert!(json.contains("\"duration_ms\":30000"));
        assert!(json.contains("\"poll_count\":120"));
        assert!(json.contains("\"value_changed\":true"));
        assert!(json.contains("\"corrections_count\":2"));
    }

    #[test]
    fn test_session_result_default_no_corrections() {
        let result = SessionResult {
            exit_reason: ExitReason::NoElement,
            duration_ms: 600,
            poll_count: 0,
            value_changed: false,
            corrections_count: 0,
        };
        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains("\"corrections_count\":0"));
        assert!(json.contains("\"value_changed\":false"));
    }

    #[test]
    fn test_exit_reason_field_too_large_serializes_chars() {
        let reason = ExitReason::FieldTooLarge { chars: 55000 };
        let json = serde_json::to_string(&reason).unwrap();
        assert!(json.contains("55000"));
    }

    #[test]
    fn test_all_exit_reasons_are_distinct() {
        let reasons = vec![
            ExitReason::Superseded,
            ExitReason::Timeout,
            ExitReason::FocusChanged,
            ExitReason::SecureFieldDetected,
            ExitReason::ReadOnlyField,
            ExitReason::NoElement,
            ExitReason::NoValue,
            ExitReason::FieldTooLarge { chars: 0 },
            ExitReason::ReaderInitFailed,
        ];
        let jsons: Vec<String> = reasons
            .iter()
            .map(|r| serde_json::to_string(r).unwrap())
            .collect();
        let unique: std::collections::HashSet<&str> = jsons.iter().map(|s| s.as_str()).collect();
        assert_eq!(
            unique.len(),
            reasons.len(),
            "Some ExitReason variants serialize identically"
        );
    }
}
