// Fluence Windows - Correction Extractor (Post-Injection)
// Compares injected text with the current field value to detect
// word-level corrections made by the user after pasting.
//
// Uses word-level LCS to find substitution pairs, then applies
// conservative filters to avoid learning accidental edits.

use super::extraction::Candidate;

/// Extract correction candidates by comparing injected text with
/// the user's edited field value.
///
/// `baseline_value` is the field snapshot taken right after the paste,
/// before the user edited anything. The injected span is anchored in that
/// snapshot and only its counterpart in the current value is diffed - the
/// rest of the field is never compared, so older sentences cannot
/// misalign into garbage pairs.
pub fn extract_user_corrections(
    injected_text: &str,
    baseline_value: &str,
    field_value: &str,
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

    let injected_words = tokenize(injected_text);
    let edited_words = tokenize(&edited_region);

    if injected_words.is_empty() || edited_words.is_empty() {
        return Vec::new();
    }

    // Find substitution pairs via word-level LCS
    let subs = find_substitutions(&injected_words, &edited_words);

    // If more than 50% of words changed, this is a rewrite - not corrections
    if subs.len() > injected_words.len() / 2 {
        log::debug!(
            "[AutoLearn] Skipping: {} of {} words changed (>50%)",
            subs.len(),
            injected_words.len()
        );
        return Vec::new();
    }

    // Filter to high-confidence corrections only
    let mut candidates = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for (original_word, corrected_word) in &subs {
        let normalized = (original_word.to_lowercase(), corrected_word.to_lowercase());

        // Skip duplicates
        if !seen.insert(normalized.clone()) {
            continue;
        }

        // Apply conservative filters (same rules as pipeline extraction)
        if !super::ui_automation::is_valid_correction(original_word, corrected_word) {
            log::debug!(
                "[AutoLearn] Skipping correction ({} chars → {} chars): failed validation",
                original_word.chars().count(),
                corrected_word.chars().count()
            );
            continue;
        }

        candidates.push(Candidate {
            spoken: original_word.clone(),
            corrected: corrected_word.clone(),
        });
    }

    if !candidates.is_empty() {
        log::info!(
            "[AutoLearn] Extracted {} corrections from user edit",
            candidates.len()
        );
    }

    candidates
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

/// Tokenize text into words, stripping punctuation from edges.
fn tokenize(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(|w| {
            w.trim_matches(|c: char| !c.is_alphanumeric() && c != '_')
                .to_string()
        })
        .filter(|w| !w.is_empty())
        .collect()
}

/// Word-level LCS to find [originalWord, editedWord] substitution pairs.
fn find_substitutions(orig_words: &[String], edited_words: &[String]) -> Vec<(String, String)> {
    let m = orig_words.len();
    let n = edited_words.len();

    // Build LCS DP table
    let mut dp = vec![vec![0u32; n + 1]; m + 1];
    for i in 1..=m {
        for j in 1..=n {
            if orig_words[i - 1].to_lowercase() == edited_words[j - 1].to_lowercase() {
                dp[i][j] = dp[i - 1][j - 1] + 1;
            } else {
                dp[i][j] = dp[i - 1][j].max(dp[i][j - 1]);
            }
        }
    }

    // Trace back to find aligned pairs. On a tie (either path preserves the
    // LCS length) emit a direct substitution pair - Android parity
    // (WordLcsExtractor emits a pair on ties). This matters because the
    // >50% rewrite veto downstream, not the traceback, decides whether a
    // heavily rewritten field is learnable: without tie pairs, a total
    // rewrite collapses to one spurious boundary pair that slips under the
    // veto. As a bonus, nearby substitutions pair up correctly instead of
    // being misattributed across positions.
    let mut aligned: Vec<(Option<String>, Option<String>)> = Vec::new();
    let mut i = m;
    let mut j = n;

    while i > 0 || j > 0 {
        if i > 0
            && j > 0
            && (orig_words[i - 1].to_lowercase() == edited_words[j - 1].to_lowercase()
                || dp[i - 1][j] == dp[i][j - 1])
        {
            aligned.push((
                Some(orig_words[i - 1].clone()),
                Some(edited_words[j - 1].clone()),
            ));
            i -= 1;
            j -= 1;
        } else if j > 0 && (i == 0 || dp[i][j - 1] >= dp[i - 1][j]) {
            aligned.push((None, Some(edited_words[j - 1].clone())));
            j -= 1;
        } else {
            aligned.push((Some(orig_words[i - 1].clone()), None));
            i -= 1;
        }
    }

    aligned.reverse();

    // Extract substitution pairs: direct diagonal pairs whose words differ,
    // plus consecutive [orig, null] + [null, edited] gaps from strictly
    // one-sided traceback steps. Equal-word diagonals are alignment, not
    // substitutions, and are skipped.
    let mut subs = Vec::new();
    for k in 0..aligned.len() {
        let (ref orig_w, ref edit_w) = aligned[k];

        if let (Some(orig), Some(edit)) = (orig_w, edit_w) {
            if orig.to_lowercase() != edit.to_lowercase() {
                subs.push((orig.clone(), edit.clone()));
            }
            continue;
        }

        if k + 1 >= aligned.len() {
            continue;
        }
        let (ref next_orig, ref next_edit) = aligned[k + 1];

        if orig_w.is_some() && edit_w.is_none() && next_orig.is_none() && next_edit.is_some() {
            if let (Some(orig), Some(edit)) = (orig_w, next_edit) {
                subs.push((orig.clone(), edit.clone()));
            }
        }
    }

    subs
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test shorthand for the single-sentence case: the baseline snapshot
    /// equals what was injected (fresh empty field), so anchoring always
    /// succeeds with empty affixes. Shadows the 3-arg function for brevity.
    fn extract_user_corrections(injected_text: &str, field_value: &str) -> Vec<Candidate> {
        super::extract_user_corrections(injected_text, injected_text, field_value)
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
        // More than 50% changed - treated as rewrite
        let corrections = extract_user_corrections("the quick brown fox", "a slow red cat jumped");
        assert!(corrections.is_empty());
    }

    #[test]
    fn test_short_words_surface_for_human_review() {
        // Android parity: only BOTH sides < 2 chars is rejected, so
        // "a" → "the" now surfaces; the human Accept step stays the gate.
        // (Single-word utterances remain guarded by the >50% rewrite veto.)
        let corrections = extract_user_corrections("I am a test", "I am the test");
        assert_eq!(corrections.len(), 1);
        assert_eq!(corrections[0].spoken, "a");
        assert_eq!(corrections[0].corrected, "the");
    }

    #[test]
    fn test_both_single_char_rejected() {
        let corrections = extract_user_corrections("I saw a dog", "I saw b dog");
        assert!(corrections.is_empty());
    }

    #[test]
    fn test_capitalization_only_skipped() {
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
        let corrections = super::extract_user_corrections(injected, baseline, current);
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
        let corrections = super::extract_user_corrections(injected, baseline, current);
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
    fn test_tokenize_basic() {
        let tokens = tokenize("hello world test");
        assert_eq!(tokens, vec!["hello", "world", "test"]);
    }

    #[test]
    fn test_tokenize_strips_punctuation() {
        let tokens = tokenize("hello, world! test.");
        assert_eq!(tokens, vec!["hello", "world", "test"]);
    }

    #[test]
    fn test_tokenize_empty() {
        let tokens = tokenize("");
        assert!(tokens.is_empty());
    }

    #[test]
    fn test_tokenize_whitespace_only() {
        let tokens = tokenize("   ");
        assert!(tokens.is_empty());
    }

    #[test]
    fn test_tokenize_single_word() {
        let tokens = tokenize("hello");
        assert_eq!(tokens, vec!["hello"]);
    }

    #[test]
    fn test_find_substitutions_identical() {
        let orig = vec!["hello".to_string(), "world".to_string()];
        let edited = vec!["hello".to_string(), "world".to_string()];
        let subs = find_substitutions(&orig, &edited);
        assert!(subs.is_empty());
    }

    #[test]
    fn test_find_substitutions_single() {
        let orig = vec!["hello".to_string(), "shunade".to_string()];
        let edited = vec!["hello".to_string(), "Sinead".to_string()];
        let subs = find_substitutions(&orig, &edited);
        assert_eq!(subs.len(), 1);
        assert_eq!(subs[0].0, "shunade");
        assert_eq!(subs[0].1, "Sinead");
    }

    #[test]
    fn test_find_substitutions_insertion_only() {
        let orig = vec!["hello".to_string()];
        let edited = vec!["hello".to_string(), "world".to_string()];
        let subs = find_substitutions(&orig, &edited);
        assert!(subs.is_empty());
    }

    #[test]
    fn test_find_substitutions_deletion_only() {
        let orig = vec!["hello".to_string(), "world".to_string()];
        let edited = vec!["hello".to_string()];
        let subs = find_substitutions(&orig, &edited);
        assert!(subs.is_empty());
    }

    #[test]
    fn test_duplicate_corrections_deduped() {
        let corrections = extract_user_corrections("the shunade shunade", "the shunade Sinead");
        let unique: std::collections::HashSet<_> =
            corrections.iter().map(|c| c.corrected.clone()).collect();
        assert_eq!(unique.len(), corrections.len());
    }

    #[test]
    fn test_rewrite_detection_boundary_exact_50_percent() {
        // 5 words, 1 sub = 2 changed / 5 total = 40% (NOT > 50%)
        // "shunade" → "Sinead" has similarity ~0.46
        let corrections = extract_user_corrections("I went to the shunade", "I went to the Sinead");
        assert_eq!(corrections.len(), 1);
        assert_eq!(corrections[0].spoken, "shunade");
        assert_eq!(corrections[0].corrected, "Sinead");
    }

    #[test]
    fn test_rewrite_detection_over_50_percent() {
        let corrections = extract_user_corrections("the big red cat sat", "a small blue dog ran");
        assert!(corrections.is_empty());
    }
}
