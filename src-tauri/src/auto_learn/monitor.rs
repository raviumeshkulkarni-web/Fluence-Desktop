// Fluence Windows - Auto-Learn Monitor
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

/// Attempts (and delay between them) when establishing the initial field
/// value. A single read right after paste commonly misses: the paste may
/// still be landing, focus settling, or the UIA tree churning.
const INITIAL_READ_ATTEMPTS: u32 = 4;
const INITIAL_READ_RETRY_DELAY_MS: u64 = 250;

/// Consecutive transient read failures tolerated mid-session before giving
/// up. Semantic exits (focus change, secure/read-only field) still stop
/// the session immediately; only NoElement/NoValue are retried.
const MAX_TRANSIENT_FAILURES: u32 = 3;

/// Consecutive identical reads required before a changed value is treated
/// as settled and diffed. While the user is typing, every poll sees a new
/// intermediate value; extracting from those would turn each keystroke into
/// its own candidate. Waiting for two identical reads collapses the whole
/// keystroke burst into one diff of the final text.
const SETTLED_POLLS: u32 = 2;

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
    /// The observed field never contained the injected text (wrong field
    /// locked on, paste landed elsewhere, or the app rewrote content).
    InjectedTextNotObserved,
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
            // COM init failed - but we can't proceed, so just loop on the channel
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

    // Establish the baseline field value, retrying transient failures.
    // Also verify the field actually contains the injected text: without
    // this check the monitor can diff against wrong-field content and either
    // learn garbage or (more often) silently learn nothing.
    let initial_value: String = {
        let mut attempts: u32 = 0;
        loop {
            attempts += 1;
            match reader.read_current_value() {
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
                    if v.contains(injected_text) {
                        break v;
                    }
                    log::debug!(
                        "[AutoLearn] Injected text not yet observed (attempt {})",
                        attempts
                    );
                    if attempts >= INITIAL_READ_ATTEMPTS {
                        log::info!("[AutoLearn] Field never contained injected text, stopping");
                        return SessionResult {
                            exit_reason: ExitReason::InjectedTextNotObserved,
                            duration_ms: session_start.elapsed().as_millis() as u64,
                            poll_count: 0,
                            value_changed: false,
                            corrections_count: 0,
                        };
                    }
                }
                ReadResult::NoValue => {
                    if attempts >= INITIAL_READ_ATTEMPTS {
                        log::debug!("[AutoLearn] Focused element has no text value, stopping");
                        return SessionResult {
                            exit_reason: ExitReason::NoValue,
                            duration_ms: session_start.elapsed().as_millis() as u64,
                            poll_count: 0,
                            value_changed: false,
                            corrections_count: 0,
                        };
                    }
                    log::debug!(
                        "[AutoLearn] No text value yet, retrying (attempt {})",
                        attempts
                    );
                }
                ReadResult::NoElement => {
                    if attempts >= INITIAL_READ_ATTEMPTS {
                        log::debug!("[AutoLearn] No focused element found, stopping");
                        return SessionResult {
                            exit_reason: ExitReason::NoElement,
                            duration_ms: session_start.elapsed().as_millis() as u64,
                            poll_count: 0,
                            value_changed: false,
                            corrections_count: 0,
                        };
                    }
                    log::debug!(
                        "[AutoLearn] No focused element yet, retrying (attempt {})",
                        attempts
                    );
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
            }
            std::thread::sleep(Duration::from_millis(INITIAL_READ_RETRY_DELAY_MS));
        }
    };

    log::debug!(
        "[AutoLearn] Initial field value captured ({} chars)",
        initial_value.len()
    );

    let mut last_value = initial_value.clone();
    let mut consecutive_transient_failures: u32 = 0;
    // Settle tracking: only a value observed unchanged across SETTLED_POLLS
    // consecutive reads is diffed. `last_extracted_value` guards the final
    // end-of-session diff against re-saving an already extracted state.
    let mut pending_value: Option<String> = None;
    let mut stable_polls: u32 = 0;
    let mut last_extracted_value = initial_value.clone();
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
                consecutive_transient_failures = 0;
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
                    last_value = current_value;
                }

                // Settle gate: track the latest value, but only diff it once
                // it has been observed unchanged across SETTLED_POLLS polls.
                // Intermediate keystrokes keep resetting the counter, so a
                // burst of typing yields one diff of the final text instead
                // of one candidate per keystroke. No per-session cap: a
                // session with several genuine corrections captures all of
                // them, one settled diff at a time.
                if last_value == last_extracted_value {
                    pending_value = None;
                    stable_polls = 0;
                } else if pending_value.as_deref() == Some(last_value.as_str()) {
                    stable_polls += 1;
                } else {
                    pending_value = Some(last_value.clone());
                    stable_polls = 1;
                }

                if stable_polls >= SETTLED_POLLS {
                    if let Some(target) = pending_value.take() {
                        stable_polls = 0;
                        let corrections =
                            extract_user_corrections(injected_text, &initial_value, &target);

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

                        last_extracted_value = target;
                    }
                }
            }
            ReadResult::NoValue => {
                consecutive_transient_failures += 1;
                if consecutive_transient_failures < MAX_TRANSIENT_FAILURES {
                    log::debug!(
                        "[AutoLearn] Field has no value (transient {}/{}), retrying",
                        consecutive_transient_failures,
                        MAX_TRANSIENT_FAILURES
                    );
                    continue;
                }
                log::debug!("[AutoLearn] Field has no value, stopping");
                exit_reason = ExitReason::NoValue;
                break;
            }
            ReadResult::NoElement => {
                consecutive_transient_failures += 1;
                if consecutive_transient_failures < MAX_TRANSIENT_FAILURES {
                    log::debug!(
                        "[AutoLearn] No focused element (transient {}/{}), retrying",
                        consecutive_transient_failures,
                        MAX_TRANSIENT_FAILURES
                    );
                    continue;
                }
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

    // Final diff: the last observed state may never have settled (user was
    // still typing at timeout, or clicked away mid-edit). One diff of the
    // final-vs-injected text captures the net correction without reviving
    // the per-keystroke noise above. Only for exits where the last read is
    // trustworthy: Timeout (field kept changing) and FocusChanged (last
    // read passed the same-element check). Superseded sessions are skipped
    // - the user is actively dictating again and the new session owns the
    // field - as are error exits where the field is gone or unreadable.
    if matches!(exit_reason, ExitReason::Timeout | ExitReason::FocusChanged)
        && last_value != last_extracted_value
    {
        let corrections = extract_user_corrections(injected_text, &initial_value, &last_value);
        if !corrections.is_empty() {
            match learner::save_corrections(corrections) {
                Ok(count) => {
                    log::info!(
                        "[AutoLearn] Saved {} corrections from final state after {}ms",
                        count,
                        session_start.elapsed().as_millis()
                    );
                    corrections_count += count as u32;
                }
                Err(e) => {
                    log::warn!(
                        "[AutoLearn] Failed to save corrections from final state: {}",
                        e
                    );
                }
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

/// Log session result. Always compiled in: the exit reason is the primary
/// diagnostic for "why did Auto Learn not learn anything" reports.
/// Run with RUST_LOG=info to see these lines in production builds.
fn log_session_result(result: &SessionResult) {
    log::info!(
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
            ExitReason::InjectedTextNotObserved,
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
            ExitReason::InjectedTextNotObserved,
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
