// Fluence Windows - Correction Extractor (Post-Injection)
// Compares injected text with the current field value to detect
// word-level corrections made by the user after pasting.
//
// The anchoring shell (`extract_edited_span`) is Fluence-specific and stays
// here. All pairing/validation DIFF logic is delegated verbatim to the
// correction decision core (`decision_core`) - plan
// plan-2-dictionary-suggestions-garbage-fix, learned from the reference
// correction-learner design (no reference source copied). The former
// tie-diagonal rule and the retired `ui_automation::is_valid_correction`
// filter are gone; the decision core's
// judge (similarity floor, corrected-length floor, case-identical skip,
// same-pair dedup) and the LCS rewrite guard now own every decision.

use std::collections::BTreeSet;

use super::extraction::Candidate;

/// Extract correction candidates by comparing injected text with
/// the user's edited field value.
///
/// `baseline_value` is the field snapshot taken right after the paste,
/// before the user edited anything. The injected span is anchored in that
/// snapshot and only its counterpart in the current value is diffed - the
/// rest of the field is never compared, so older sentences cannot
/// misalign into garbage pairs.
///
/// `known_pair_keys` are the canonical keys of pairs already in the
/// dictionary (never re-learned as new suggestions).
pub fn extract_user_corrections(
    injected_text: &str,
    baseline_value: &str,
    field_value: &str,
    known_pair_keys: &BTreeSet<String>,
) -> Vec<Candidate> {
    // Identical → no corrections
    if injected_text == field_value {
        return Vec::new();
    }

    // Anchor the injected span; unanchored or context-broken states yield
    // nothing - diffing the whole field in those cases manufactures pairs
    // across sentence boundaries (e.g. `or → Message`).
    let edited_region = match extract_edited_span(injected_text, baseline_value, field_value) {
        Some(region) => region,
        None => return Vec::new(),
    };

    // Decision core: tokenization, LCS pairing, rewrite guard/vetoes and
    // the per-pair gate chain all live in the decision core module.
    super::decision_core::extract_corrections(injected_text, &edited_region, known_pair_keys)
        .into_iter()
        .map(|(spoken, corrected)| Candidate { spoken, corrected })
        .collect()
}

/// Locate the single occurrence of the injected text in the baseline field
/// snapshot and slice the corresponding span from the current value.
///
/// Returns None when the injection cannot be anchored - absent from the
/// baseline (paste landed late or elsewhere), repeated in the baseline
/// (ambiguous which span is ours), or the surrounding context in the
/// current value no longer matches (user retyped around it). In all these
/// cases learning nothing is safer than diffing the whole field, which
/// misaligns across sentences and manufactures garbage pairs.
fn extract_edited_span(
    injected_text: &str,
    baseline_value: &str,
    field_value: &str,
) -> Option<String> {
    if injected_text.is_empty() {
        log::debug!("[AutoLearn] Span anchor miss (empty injection)");
        return None;
    }

    // All occurrences of the injection in the baseline snapshot.
    let mut occurrences = Vec::new();
    let mut search_from = 0;
    while let Some(idx) = baseline_value[search_from..].find(injected_text) {
        occurrences.push(search_from + idx);
        search_from += idx + injected_text.len();
        if occurrences.len() > 1 {
            break;
        }
    }
    if occurrences.is_empty() {
        // Injection absent from the baseline: paste landed late/elsewhere.
        log::info!(
            "[AutoLearn] Span anchor miss (not-in-baseline): baseline {} chars, field {} chars",
            baseline_value.len(),
            field_value.len()
        );
        return None;
    }
    if occurrences.len() > 1 {
        // Repeated injection: ambiguous which span is ours.
        log::info!("[AutoLearn] Span anchor miss (ambiguous repeat)");
        return None;
    }

    let pos = occurrences[0];
    let prefix = &baseline_value[..pos];
    let suffix = &baseline_value[pos + injected_text.len()..];

    // The current value must still carry the surrounding context; the span
    // between the affixes is the edited counterpart (possibly identical,
    // possibly reworded - the diff downstream decides). Head and tail are
    // checked (and logged) separately so soak logs distinguish pre-edits
    // above the injection from appends below it.
    if !field_value.starts_with(prefix) {
        log::info!(
            "[AutoLearn] Span anchor miss (head-broken): prefix {} chars",
            prefix.len()
        );
        return None;
    }
    if !field_value.ends_with(suffix) {
        log::info!(
            "[AutoLearn] Span anchor miss (tail-broken): suffix {} chars",
            suffix.len()
        );
        return None;
    }
    if field_value.len() < prefix.len() + suffix.len() {
        return None;
    }
    field_value
        .get(prefix.len()..field_value.len() - suffix.len())
        .map(|s| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    /// Test shorthand for the single-sentence case: the baseline snapshot
    /// equals what was injected (fresh empty field), so anchoring always
    /// succeeds with empty affixes. Shadows the 4-arg function for brevity.
    fn extract_user_corrections(injected_text: &str, field_value: &str) -> Vec<Candidate> {
        super::extract_user_corrections(injected_text, injected_text, field_value, &BTreeSet::new())
    }

    #[test]
    fn test_simple_correction() {
        let corrections = extract_user_corrections("I went to the shunade", "I went to the Sinead");
        assert_eq!(corrections.len(), 1);
        assert_eq!(corrections[0].spoken, "shunade");
        assert_eq!(corrections[0].corrected, "Sinead");
    }

    #[test]
    fn test_no_changes() {
        let corrections = extract_user_corrections("hello world", "hello world");
        assert!(corrections.is_empty());
    }

    #[test]
    fn test_rewrite_detection() {
        // Full rewrite (no shared coverage) - rejected by the LCS rewrite
        // guard in the ported core.
        let corrections = extract_user_corrections("the quick brown fox", "a slow red cat jumped");
        assert!(corrections.is_empty());
    }

    #[test]
    fn test_short_words_stop_surfacing() {
        // Reference parity (OQ-2, owner-confirmed): corrected words
        // shorter than 3 chars are rejected by the ported judge; the human
        // Accept path (manual Words-tab add) remains available. This
        // replaces the retired Android-parity `a→the` acceptance - the
        // original manual-add regression type (`shwande→Sinead`) is now
        // accepted to fall back to manual add too (OQ-1).
        let corrections = extract_user_corrections("I am a test", "I am the test");
        assert!(corrections.is_empty());
    }

    #[test]
    fn test_both_single_char_rejected() {
        let corrections = extract_user_corrections("I saw a dog", "I saw b dog");
        assert!(corrections.is_empty());
    }

    #[test]
    fn test_capitalization_only_skipped() {
        // Reference parity (OQ-5, owner-confirmed): lowercase-identical
        // tokens produce no pairs; case-only edits are never learned.
        let corrections = extract_user_corrections("hello world", "Hello World");
        assert!(corrections.is_empty());
    }

    #[test]
    fn test_edited_span_unique_occurrence() {
        let span = extract_edited_span(
            "I went to sined yesterday",
            "Note: I went to sined yesterday",
            "Note: I went to Sinead yesterday",
        );
        assert_eq!(span.as_deref(), Some("I went to Sinead yesterday"));
    }

    #[test]
    fn test_edited_span_single_sentence_field() {
        // Empty affixes: the whole current value is the counterpart.
        let span = extract_edited_span("hello world", "hello world", "hello Sinead");
        assert_eq!(span.as_deref(), Some("hello Sinead"));
    }

    #[test]
    fn test_edited_span_absent_injection() {
        assert!(extract_edited_span("hello world", "something else", "hello world").is_none());
    }

    #[test]
    fn test_edited_span_repeated_injection_ambiguous() {
        // Same sentence twice: impossible to know which span is ours.
        let baseline = "sined here. sined here.";
        assert!(extract_edited_span("sined here.", baseline, baseline).is_none());
    }

    #[test]
    fn test_edited_span_broken_context() {
        // Surrounding text retyped: the anchor no longer holds.
        let span = extract_edited_span(
            "sined yesterday",
            "say sined yesterday please",
            "totally different content here",
        );
        assert!(span.is_none());
    }

    #[test]
    fn test_edited_span_empty_injected() {
        assert!(extract_edited_span("", "hello", "hello").is_none());
    }

    #[test]
    fn test_multisentence_field_scopes_to_injected_span() {
        // Regression: diffing the whole multi-sentence field manufactured
        // cross-sentence pairs (`or → Message`). With anchoring, only the
        // injected span's counterpart is diffed.
        let baseline = "Old notes stay. I went to sined yesterday";
        let injected = "I went to sined yesterday";
        let current = "Old notes stay. I went to Sinead yesterday";
        let corrections =
            super::extract_user_corrections(injected, baseline, current, &BTreeSet::new());
        assert_eq!(corrections.len(), 1);
        assert_eq!(corrections[0].spoken, "sined");
        assert_eq!(corrections[0].corrected, "Sinead");
    }

    #[test]
    fn test_multisentence_rewrite_outside_span_ignored() {
        // Rewording outside the injected span must not produce candidates,
        // even though whole-field diffing would misalign on it.
        let baseline = "Old notes stay. I went to sined yesterday";
        let injected = "I went to sined yesterday";
        let current = "Completely rewritten intro here. I went to sined yesterday";
        let corrections =
            super::extract_user_corrections(injected, baseline, current, &BTreeSet::new());
        assert!(corrections.is_empty());
    }

    // ── Edge cases ───────────────────────────────────────────────

    #[test]
    fn test_empty_injected_text() {
        let corrections = extract_user_corrections("", "hello world");
        assert!(corrections.is_empty());
    }

    #[test]
    fn test_empty_field_value() {
        let corrections = extract_user_corrections("hello world", "");
        assert!(corrections.is_empty());
    }

    #[test]
    fn test_both_empty() {
        let corrections = extract_user_corrections("", "");
        assert!(corrections.is_empty());
    }

    #[test]
    fn test_whitespace_only_edit() {
        let corrections = extract_user_corrections("hello world", "hello world ");
        assert!(corrections.is_empty());
    }

    #[test]
    fn test_punctuation_only_edit() {
        let corrections = extract_user_corrections("hello world", "hello world.");
        assert!(corrections.is_empty());
    }

    #[test]
    fn test_emoji_only_field() {
        let corrections = extract_user_corrections("hello world", "hello 🌍");
        assert!(corrections.is_empty());
    }

    #[test]
    fn test_known_dictionary_pair_not_relearned() {
        // Pairs already in the dictionary (canonical keys) are skipped.
        let mut known = BTreeSet::new();
        known.insert(crate::dictionary::canonical_entry_key("shunade", "Sinead"));
        let corrections = super::extract_user_corrections(
            "I went to the shunade",
            "I went to the shunade",
            "I went to the Sinead",
            &known,
        );
        assert!(corrections.is_empty());
    }

    #[test]
    fn test_rewrite_detection_boundary_exact_50_percent() {
        // 1 sub of 5 words = under the >50% veto; `shunade → Sinead`
        // (dist 4/7 = 0.57 <= 0.65) passes the similarity floor.
        let corrections = extract_user_corrections("I went to the shunade", "I went to the Sinead");
        assert_eq!(corrections.len(), 1);
        assert_eq!(corrections[0].spoken, "shunade");
        assert_eq!(corrections[0].corrected, "Sinead");
    }

    #[test]
    fn test_rewrite_detection_over_50_percent() {
        // No shared coverage (LCS 0 of 5) - rewrite guard rejects.
        let corrections = extract_user_corrections("the big red cat sat", "a small blue dog ran");
        assert!(corrections.is_empty());
    }
}
