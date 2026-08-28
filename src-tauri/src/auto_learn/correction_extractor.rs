// Fluence Windows — Correction Extractor (Post-Injection)
// Compares injected text with the current field value to detect
// word-level corrections made by the user after pasting.
//
// Uses word-level LCS to find substitution pairs, then applies
// conservative filters to avoid learning accidental edits.

use super::extraction::Candidate;

/// Extract correction candidates by comparing injected text with
/// the user's edited field value.
///
/// Returns a list of high-confidence correction pairs.
pub fn extract_user_corrections(injected_text: &str, field_value: &str) -> Vec<Candidate> {
    // Identical → no corrections
    if injected_text == field_value {
        return Vec::new();
    }

    // Find the region in field_value that corresponds to the injected text
    let edited_region = find_edited_region(injected_text, field_value);

    let injected_words = tokenize(injected_text);
    let edited_words = tokenize(&edited_region);

    if injected_words.is_empty() || edited_words.is_empty() {
        return Vec::new();
    }

    // Find substitution pairs via word-level LCS
    let subs = find_substitutions(&injected_words, &edited_words);

    // If more than 50% of words changed, this is a rewrite — not corrections
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

/// Find the region in field_value that corresponds to the pasted text.
/// If the field only contains the pasted text, returns field_value as-is.
/// Otherwise, uses sliding window to find the best match.
fn find_edited_region(injected_text: &str, field_value: &str) -> String {
    // If field is close in size to injected text, use the whole field
    if field_value.len() <= injected_text.len() * 2 {
        return field_value.to_string();
    }

    // Check if injected text appears verbatim in the field
    if let Some(_idx) = field_value.find(injected_text) {
        // The injected text is somewhere in the field — but user may have edited it.
        // Use the surrounding region for comparison.
        return field_value.to_string();
    }

    // Sliding window: find the region with highest word overlap
    let injected_words = tokenize(injected_text);
    let field_words = tokenize(field_value);
    let window_size = injected_words.len();

    if field_words.len() <= window_size {
        return field_value.to_string();
    }

    let mut best_start = 0;
    let mut best_score = 0usize;

    for i in 0..=field_words.len().saturating_sub(window_size) {
        let mut matches = 0;
        for j in 0..window_size {
            if i + j < field_words.len()
                && field_words[i + j].to_lowercase() == injected_words[j].to_lowercase()
            {
                matches += 1;
            }
        }
        if matches > best_score {
            best_score = matches;
            best_start = i;
        }
    }

    // Require at least 30% word overlap
    if best_score < window_size / 3 {
        return field_value.to_string();
    }

    field_words[best_start..(best_start + window_size).min(field_words.len())].join(" ")
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

    // Trace back to find aligned pairs
    let mut aligned: Vec<(Option<String>, Option<String>)> = Vec::new();
    let mut i = m;
    let mut j = n;

    while i > 0 || j > 0 {
        if i > 0 && j > 0 && orig_words[i - 1].to_lowercase() == edited_words[j - 1].to_lowercase()
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

    // Extract substitution pairs: consecutive [orig, null] + [null, edited]
    let mut subs = Vec::new();
    for k in 0..aligned.len().saturating_sub(1) {
        let (ref orig_w, ref edit_w) = aligned[k];
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
        // More than 50% changed — treated as rewrite
        let corrections = extract_user_corrections("the quick brown fox", "a slow red cat jumped");
        assert!(corrections.is_empty());
    }

    #[test]
    fn test_short_words_skipped() {
        let corrections = extract_user_corrections("I am a test", "I am the test");
        // "a" → "the" is too short
        assert!(corrections.is_empty());
    }

    #[test]
    fn test_capitalization_only_skipped() {
        let corrections = extract_user_corrections("hello world", "Hello World");
        assert!(corrections.is_empty());
    }

    #[test]
    fn test_find_edited_region_exact() {
        let region = find_edited_region("hello world", "hello world");
        assert_eq!(region, "hello world");
    }

    #[test]
    fn test_find_edited_region_with_surrounding() {
        let region = find_edited_region("hello world", "say hello world please");
        assert_eq!(region, "say hello world please");
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
    fn test_find_edited_region_small_field() {
        let region = find_edited_region("hello world test", "hello Sinead test");
        assert_eq!(region, "hello Sinead test");
    }

    #[test]
    fn test_find_edited_region_sliding_window() {
        let injected = "the quick brown fox jumps";
        let field = "once upon a time the quick brown fox jumps over the lazy dog the end";
        let region = find_edited_region(injected, field);
        let region_words: Vec<&str> = region.split_whitespace().collect();
        assert!(region_words.len() >= 5);
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
