// Fluence Windows - automatic pre-upload silence gate (production).
//
// The frozen online pipeline (audio.rs capture/DSP, transcribe.rs sender)
// is untouched: this module only ADDS the read-only energy measurement
// and gate decision used by the audio stop path. The gate runs
// unconditionally (no flag, no UI): clearly silent takes return empty
// before peak normalization can amplify mic noise to full scale.

// ---------------------------------------------------------------------------
//
// Rationale (evidence 2026-09-04, stt_exp_diag.jsonl): three silent takes
// arrived at Groq peak-normalized to exactly 0.90 (Fluence's own norm
// target firing on mic noise above the 0.005 gate) and came back as
// confident phantoms with no_speech_prob = 0.0. No response-side
// threshold can catch those, so the take must be rejected BEFORE
// normalization/encoding/upload, on PRE-normalization energy.
//
// Calibration: post-norm silence RMS measured 0.037-0.14 at peak 0.9
// implies pre-norm silence RMS of roughly 2e-4..3e-3 (dividing by the
// norm gain 0.9/peak_pre with peak_pre in 0.005..0.02 for room noise).
// Genuine speech raw RMS is typically an order of magnitude hotter.
// Thresholds below sit above the inferred silence band with margin,
// below conversational speech, and mirror TypeWhisper's pre-gain energy
// gates (peak 0.003/0.006 discard, RMS energy gate 0.01):
//
// REVISION 2 (2026-09-04, from live gate evidence): whole-take PEAK is
// useless in a transient-prone room - silent takes showed peaks of
// 0.033-0.055 (clicks/bumps) while a 9 s SPOKEN sentence peaked at only
// 0.037 with whole-take rms 0.0045. Peak cannot separate them.
// REVISION 3 (authorized recalibration): the hottest-window rule ate soft
// single words, so the gate counts SUSTAINED warmth instead - speech
// spreads energy across windows, clicks/rumbles concentrate it.
// A 100 ms window at/above this RMS counts as warm (sits above stationary
// silence at <=0.008 with margin, below the softest observed passing
// speech).
pub const GATE_WARM_WINDOW_RMS: f32 = 0.012;
// Minimum warm windows required to pass. A single click warms at most one
// or two adjacent windows; a soft 300 ms word warms three or more.
pub const GATE_REQUIRED_WARM_WINDOWS: usize = 2;

/// Outcome of the silence-gate evaluation for one take.
#[derive(Debug, Clone)]
pub struct SilenceGateDecision {
    /// RMS of the raw native-rate interleaved f32 buffer (whole take).
    pub rms: f32,
    /// Peak of the raw native-rate interleaved f32 buffer (whole take).
    pub peak: f32,
    /// Hottest 100 ms window RMS (diagnostic context).
    pub max_window_rms: f32,
    /// Count of windows at/above GATE_WARM_WINDOW_RMS (the gate signal).
    /// Read by tests; retained on the decision record for future calibration.
    #[allow(dead_code)]
    pub warm_windows: usize,
    /// Take duration in ms (for logging / future calibration).
    pub duration_ms: u64,
    /// True when the take is clearly silent and must NOT be uploaded.
    pub rejected: bool,
}

/// Read-only audio-level statistics for the processed f32 take.
/// Pure computation: borrows the samples, mutates nothing.
pub(crate) fn audio_level_stats(samples: &[f32]) -> (f32, f32) {
    if samples.is_empty() {
        return (0.0, 0.0);
    }
    let peak = samples.iter().map(|&s| s.abs()).fold(0.0f32, f32::max);
    let sum_sq: f32 = samples.iter().map(|&s| s * s).sum();
    let rms = (sum_sq / samples.len() as f32).sqrt();
    (rms, peak)
}

/// Measure raw energy of the native-rate interleaved f32 capture buffer.
/// Pre-normalization by construction: callers pass the buffer BEFORE any
/// DSP. Pure function: borrows, mutates nothing.
pub fn measure_raw_energy(samples: &[f32]) -> (f32, f32) {
    audio_level_stats(samples)
}

/// Evaluate the gate (revision 3: sustained warmth). Splits the raw buffer
/// into ~100 ms windows at the given sample rate. Rejects iff fewer than
/// GATE_REQUIRED_WARM_WINDOWS windows reach GATE_WARM_WINDOW_RMS -
/// i.e. the take contains no sustained speech energy anywhere. A click
/// warms at most one or two adjacent windows; a soft word warms several.
/// An empty buffer yields zero warm windows and is rejected (unreachable
/// in practice: the stop path errors on empty buffers first).
pub fn evaluate_silence_gate(
    samples: &[f32],
    sample_rate_hz: u32,
    duration_ms: u64,
) -> SilenceGateDecision {
    let (rms, peak) = measure_raw_energy(samples);
    let window_len = ((sample_rate_hz.max(1) as usize) / 10).max(1);
    let mut max_window_rms = 0.0f32;
    let mut warm_windows: usize = 0;
    for window in samples.chunks(window_len) {
        if window.is_empty() {
            continue;
        }
        let sum_sq: f32 = window.iter().map(|&s| s * s).sum();
        let window_rms = (sum_sq / window.len() as f32).sqrt();
        if window_rms > max_window_rms {
            max_window_rms = window_rms;
        }
        if window_rms >= GATE_WARM_WINDOW_RMS {
            warm_windows += 1;
        }
    }
    let rejected = warm_windows < GATE_REQUIRED_WARM_WINDOWS;
    SilenceGateDecision {
        rms,
        peak,
        max_window_rms,
        warm_windows,
        duration_ms,
        rejected,
    }
}

/// Marker for the most recent gate rejection (see take_gate_rejected).
/// NOTE: the silence gate itself is automatic and unconditional (see the
/// audio stop path) - there is intentionally no flag for it.
static LAST_GATE_REJECTED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Record that the most recent take was rejected by the silence gate.
/// Called from the gate blocks in audio.rs (both stop variants).
pub(crate) fn note_gate_rejected() {
    LAST_GATE_REJECTED.store(true, std::sync::atomic::Ordering::SeqCst);
}

/// Consume the rejection marker (self-clearing): true iff a gate
/// rejection happened since the last call. Lets workflow.rs label the
/// flow result so the overlay can vanish instantly on gate rejects
/// instead of showing the generic no-speech notice.
pub(crate) fn take_gate_rejected() -> bool {
    LAST_GATE_REJECTED.swap(false, std::sync::atomic::Ordering::SeqCst)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gate_rejection_marker_is_self_clearing() {
        assert!(!take_gate_rejected());
        note_gate_rejected();
        assert!(take_gate_rejected());
        assert!(!take_gate_rejected());
    }

    // --- Silence-gate tests (sustained-warmth rule) ---

    #[test]
    fn gate_rejects_digital_silence() {
        let silent = vec![0.0f32; 48000];
        let d = evaluate_silence_gate(&silent, 16000, 3000);
        assert!(d.rejected);
        assert_eq!(d.rms, 0.0);
        assert_eq!(d.peak, 0.0);
        assert_eq!(d.max_window_rms, 0.0);
        assert_eq!(d.warm_windows, 0);
    }

    #[test]
    fn gate_rejects_room_noise_floor() {
        // Simulated mic noise: alternating +-0.004 in every window ->
        // zero warm windows -> rejected.
        let noise: Vec<f32> = (0..48000)
            .map(|i| if i % 2 == 0 { 0.004 } else { -0.004 })
            .collect();
        let d = evaluate_silence_gate(&noise, 16000, 3000);
        assert!(d.rejected, "warm_windows={}", d.warm_windows);
        assert_eq!(d.warm_windows, 0);
    }

    #[test]
    fn gate_rejects_click_inside_silence() {
        // Live evidence 2026-09-04: silent takes with peak spikes up to
        // 0.055 still hallucinated. A single loud sample warms at most
        // one window - below the required two - so the take stays
        // rejected even though its peak looks loud.
        let mut take = vec![0.001f32; 48000];
        take[24000] = 0.5;
        let d = evaluate_silence_gate(&take, 16000, 3000);
        assert!(
            d.rejected,
            "peak={} warm_windows={}",
            d.peak, d.warm_windows
        );
        assert!(d.peak > 0.4, "test setup must contain the spike");
        assert!(d.warm_windows < GATE_REQUIRED_WARM_WINDOWS);
    }

    #[test]
    fn gate_passes_conversational_speech() {
        // 440 Hz tone at 0.3 amplitude: every window rms ~0.21.
        let speech: Vec<f32> = (0..48000)
            .map(|i| (i as f32 / 48000.0 * 440.0 * 2.0 * std::f32::consts::PI).sin() * 0.3)
            .collect();
        let d = evaluate_silence_gate(&speech, 48000, 1000);
        assert!(!d.rejected, "warm_windows={}", d.warm_windows);
        assert!(d.warm_windows >= GATE_REQUIRED_WARM_WINDOWS);
    }

    #[test]
    fn gate_passes_soft_burst_in_quiet_take() {
        // 200 ms soft burst (0.05 tone) inside 3 s of near-silence:
        // exactly the two windows it spans go warm -> passes. Models a
        // soft single-word utterance.
        let mut take = vec![0.001f32; 48000];
        for (k, s) in take.iter_mut().enumerate().take(3200) {
            *s = (*s + (k as f32 / 16000.0 * 220.0 * 2.0 * std::f32::consts::PI).sin() * 0.05)
                .clamp(-1.0, 1.0);
        }
        let d = evaluate_silence_gate(&take, 16000, 3000);
        assert!(!d.rejected, "warm_windows={}", d.warm_windows);
        assert!(d.warm_windows >= GATE_REQUIRED_WARM_WINDOWS);
    }

    #[test]
    fn gate_passes_sustained_soft_tone_v2_would_reject() {
        // Regression test for eaten single words: a steady soft tone at
        // 0.013 RMS never reaches the old 0.02 hottest-window bar, but
        // warms every window - sustained speech the v3 rule must pass.
        let soft = vec![0.013f32; 48000];
        let d = evaluate_silence_gate(&soft, 16000, 3000);
        assert!(d.max_window_rms < 0.02, "setup must stay below the old bar");
        assert!(!d.rejected, "warm_windows={}", d.warm_windows);
    }

    #[test]
    fn gate_passes_short_speech_blip() {
        // Half-second "I'm"-style blip at conversational level.
        let blip: Vec<f32> = (0..8000)
            .map(|i| (i as f32 / 8000.0 * 300.0 * 2.0 * std::f32::consts::PI).sin() * 0.2)
            .collect();
        let d = evaluate_silence_gate(&blip, 16000, 500);
        assert!(!d.rejected, "warm_windows={}", d.warm_windows);
    }

    #[test]
    fn gate_thresholds_match_documented_constants() {
        // Pin the calibrated values so they can only change deliberately.
        assert_eq!(GATE_WARM_WINDOW_RMS, 0.012);
        assert_eq!(GATE_REQUIRED_WARM_WINDOWS, 2);
    }

    #[test]
    fn measure_raw_energy_is_read_only() {
        let samples = vec![0.1f32, -0.2, 0.05];
        let snapshot = samples.clone();
        let (rms, peak) = measure_raw_energy(&samples);
        assert_eq!(samples, snapshot);
        assert!((peak - 0.2).abs() < 1e-6);
        let expected_rms = ((0.01f32 + 0.04 + 0.0025) / 3.0).sqrt();
        assert!((rms - expected_rms).abs() < 1e-6);
    }
}
