// Fluence Windows - correction decision core
//
// Rust implementation of the correction-learning design learned from an
// open-source reference (its correction-learner module; reference study
// 2026-10-20). Learned knowledge, not copied code - all logic below is
// Fluence-written Rust. Task: plan-2-dictionary-suggestions-garbage-fix.
//
// Reference design anchor map (reference ranges -> this file):
//   ref L7-25    edit_distance
//   ref L28-33   tokenize
//   ref L83-127  find_substitutions
//   ref L137-177 extract_corrections
//   ref L158-174 is_valid_pair (the per-pair skip chain, exported as a judge)
//
// Differences from the reference design (documented, non-algorithmic):
//   - the reference finds its own edited region; Fluence's anchored-span
//     shell (`correction_extractor::extract_edited_span`) already provides a
//     strictly tighter edited counterpart; this module receives it
//     ready-made.
//   - the reference returns corrected WORDS (a flat word list app): this
//     module returns
//     SPOKEN->CORRECTED pairs (Fluence's store is pair-keyed, see D4).
//   - the reference's corrected-word dict-skip/dedup becomes pair-key
//     skip/dedup: in
//     Fluence a learned pair is an active rewrite rule, not an inert word.
//   - Strings compare by `char`s (JS counts UTF-16 units; identical for the
//     BMP text this app processes).
//
// Fluence safety extra beyond reference parity (RC-5): the LCS-coverage
// rewrite guard in `extract_corrections`. In the reference design a
// mis-learned WORD
// is inert; in Fluence a mis-learned PAIR rewrites future transcripts
// (dictionary::apply_corrections), so a full rewrite must yield no
// candidates even when one boundary pair passes the per-pair gates.
//
// This module is deliberately OUTSIDE the `#[cfg(target_os = "windows")]`
// group in `auto_learn/mod.rs`: the suggestion store imports it and is not
// windows-gated, so a gated placement (like `ui_automation`) would break
// non-Windows builds.

use std::collections::BTreeSet;

use crate::dictionary::canonical_entry_key;

/// Levenshtein edit distance between two strings (JS L7-25).
pub(crate) fn edit_distance(a: &str, b: &str) -> usize {
    let m = a.chars().count();
    let n = b.chars().count();

    // JS builds a full (m+1)x(n+1) table; mirror it for transliteration
    // fidelity (inputs here are single words, so size is never a factor).
    let mut dp = vec![vec![0usize; n + 1]; m + 1];
    for (i, row) in dp.iter_mut().enumerate() {
        row[0] = i;
    }
    for j in 0..=n {
        dp[0][j] = j;
    }

    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    for i in 1..=m {
        for j in 1..=n {
            dp[i][j] = if a[i - 1] == b[j - 1] {
                dp[i - 1][j - 1]
            } else {
                1 + dp[i - 1][j].min(dp[i][j - 1]).min(dp[i - 1][j - 1])
            };
        }
    }

    dp[m][n]
}

/// Tokenize text into words, stripping punctuation from edges (JS L28-33).
/// JS strips `^[^\p{L}\p{N}_]+|[^\p{L}\p{N}_]+$` via Unicode categories.
/// Rust's `is_alphanumeric` covers the Alphabetic ∪ Number properties -
/// a SUPERSET of `\p{L}`+`\p{N}` (it additionally accepts Other_Alphabetic
/// marks), NOT an exact translation (audit Round-4 F5). Behaviorally
/// identical for BMP Latin/Cyrillic word text.
pub(crate) fn tokenize(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(|w| {
            w.trim_matches(|c: char| !c.is_alphanumeric() && c != '_')
                .to_string()
        })
        .filter(|w| !w.is_empty())
        .collect()
}

/// Word-level LCS length between two token lists (the head of JS L83-96,
/// exposed separately for the rewrite guard).
pub(crate) fn lcs_length(orig_words: &[String], edited_words: &[String]) -> usize {
    let m = orig_words.len();
    let n = edited_words.len();

    let mut dp = vec![vec![0usize; n + 1]; m + 1];
    for i in 1..=m {
        for j in 1..=n {
            dp[i][j] = if orig_words[i - 1].to_lowercase() == edited_words[j - 1].to_lowercase() {
                dp[i - 1][j - 1] + 1
            } else {
                dp[i - 1][j].max(dp[i][j - 1])
            };
        }
    }

    dp[m][n]
}

/// Word-level LCS traceback to find [originalWord, editedWord] substitution
/// pairs (JS L83-127, verbatim logic).
///
/// Traceback details that MUST stay exactly as in JS:
///   - the diagonal is taken ONLY when the words are equal (case-insensitive)
///     - unlike the retired Fluence tie rule, a raw dp tie never pairs two
///       differing words;
///   - ties between the insert and delete branches prefer insertion
///     (`dp[i][j-1] >= dp[i-1][j]` -> insertion side);
///   - pairs are formed ONLY from consecutive `[orig, null] + [null, edited]`
///     windows.
pub(crate) fn find_substitutions(
    orig_words: &[String],
    edited_words: &[String],
) -> Vec<(String, String)> {
    let m = orig_words.len();
    let n = edited_words.len();

    let mut dp = vec![vec![0usize; n + 1]; m + 1];
    for i in 1..=m {
        for j in 1..=n {
            dp[i][j] = if orig_words[i - 1].to_lowercase() == edited_words[j - 1].to_lowercase() {
                dp[i - 1][j - 1] + 1
            } else {
                dp[i - 1][j].max(dp[i][j - 1])
            };
        }
    }

    // JS uses `aligned.unshift`; pushing + a final reverse is equivalent.
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

    // Consecutive [orig, null] + [null, edited] windows = substitution (JS
    // L115-124). Equal-word diagonals are alignment, not substitutions.
    let mut subs = Vec::new();
    for k in 0..aligned.len().saturating_sub(1) {
        let (orig_w, edit_w) = &aligned[k];
        let (next_orig_w, next_edit_w) = &aligned[k + 1];

        if orig_w.is_some() && edit_w.is_none() && next_orig_w.is_none() && next_edit_w.is_some() {
            subs.push((orig_w.clone().unwrap(), next_edit_w.clone().unwrap()));
        }
    }

    subs
}

/// The single decision core: does a spoken->corrected pair qualify as a
/// correction worth learning? (JS L161-170 skip chain, exported as judge.)
///
/// Skips, in JS order:
///   1. pair already known (in JS: corrected word in the dictionary; in
///      Fluence: pair key known - the store is pair-keyed, see D4),
///   2. identical after lowercasing (case-only pairs are skipped - the
///      reference behavior, owner-confirmed OQ-5),
///   3. corrected word shorter than 3 chars,
///   4. `edit_distance / max_length > 0.65` (similarity floor 0.35; the 0.65
///      cutoff passes phonetic corrections like "shunade" -> "Sinead"
///      (JS comment: dist 4/7 = 0.57) while filtering unrelated words).
pub(crate) fn is_valid_pair(
    spoken: &str,
    corrected: &str,
    known_pair_keys: &BTreeSet<String>,
) -> bool {
    invalid_pair_reason(spoken, corrected, known_pair_keys).is_none()
}

/// Log-facing mirror of [`is_valid_pair`] (audit Round-4 F2): walks the
/// EXACT same gates in the same order and returns the first failure's
/// reason, or `None` for a pair that passes every gate. Decisions keep
/// going through `is_valid_pair`; this only powers soak diagnostics
/// (purge demote logs, skip logs) so operators see WHY a row was demoted.
pub(crate) fn invalid_pair_reason(
    spoken: &str,
    corrected: &str,
    known_pair_keys: &BTreeSet<String>,
) -> Option<&'static str> {
    // JS L161 - known (pair-level here)
    if known_pair_keys.contains(&canonical_entry_key(spoken, corrected)) {
        return Some("already in dictionary");
    }

    // JS L163 - identical after lowercasing
    if spoken.to_lowercase() == corrected.to_lowercase() {
        return Some("case-only difference");
    }

    // JS L164 - corrected word must be at least 3 chars
    if corrected.chars().count() < 3 {
        return Some("corrected shorter than 3 chars");
    }

    // JS L168-170 - similarity floor
    let orig_lc = spoken.to_lowercase();
    let corr_lc = corrected.to_lowercase();
    let dist = edit_distance(&orig_lc, &corr_lc);
    let max_len = orig_lc.chars().count().max(corr_lc.chars().count());
    if max_len == 0 {
        // Unreachable after the case gate (both blank lowercases equal),
        // kept for structural parity with the boolean mirror.
        return Some("empty pair");
    }
    // JS keeps a pair when `dist / maxLen <= 0.65`.
    if (dist as f64) / (max_len as f64) <= 0.65 {
        return None;
    }
    Some("below similarity floor")
}

/// Extract spoken->corrected pair candidates from the injected text and the
/// user's edited counterpart region (JS L137-177).
///
/// The edited counterpart is provided by the caller (Fluence's anchored-span
/// shell, replacing JS `findEditedRegion` - deviation documented in the
/// module header). Returns pairs in observation order.
pub(crate) fn extract_corrections(
    original_text: &str,
    edited_region: &str,
    known_pair_keys: &BTreeSet<String>,
) -> Vec<(String, String)> {
    // JS L138-139 early-outs
    if original_text.is_empty() || edited_region.is_empty() {
        return Vec::new();
    }
    if original_text == edited_region {
        return Vec::new();
    }

    let orig_words = tokenize(original_text);
    let edited_words = tokenize(edited_region);

    // JS L147
    if orig_words.is_empty() || edited_words.is_empty() {
        return Vec::new();
    }

    // Fluence safety extra (RC-5): when the shared word coverage collapses
    // (LCS < half the injected tokens), the edit is a rewrite, not a set of
    // corrections. Without this, a full rewrite collapses to one boundary
    // pair that can slip under the per-pair gates
    // (`we should release` -> `replace the plan` leaks `release->replace`,
    // sim 0.571). Verified: RC-1 (LCS 5, m 9) is NOT a rewrite and passes.
    if lcs_length(&orig_words, &edited_words) * 2 < orig_words.len() {
        log::debug!(
            "[AutoLearn] Skipping: rewrite (LCS {} of {} words)",
            lcs_length(&orig_words, &edited_words),
            orig_words.len()
        );
        return Vec::new();
    }

    let subs = find_substitutions(&orig_words, &edited_words);

    // JS L149-151: if more than 50% of words changed, this is a rewrite,
    // not corrections.
    if subs.len() * 2 > orig_words.len() {
        log::debug!(
            "[AutoLearn] Skipping: {} of {} words changed (>50%)",
            subs.len(),
            orig_words.len()
        );
        return Vec::new();
    }

    // JS L153-174, with pair-level known-skip and pair-level dedup (D4: the
    // JS corrected-word sets are word-list semantics; Fluence is pair-keyed).
    let mut seen_pairs = BTreeSet::new();
    let mut results = Vec::new();

    for (orig_word, corrected_word) in &subs {
        if !is_valid_pair(orig_word, corrected_word, known_pair_keys) {
            continue;
        }

        let pair_key = canonical_entry_key(orig_word, corrected_word);
        if !seen_pairs.insert(pair_key.clone()) {
            continue;
        }

        results.push((orig_word.clone(), corrected_word.clone()));
    }

    if !results.is_empty() {
        log::info!(
            "[AutoLearn] Extracted {} corrections from user edit",
            results.len()
        );
    }

    results
}

#[cfg(test)]
mod tests {
    use super::*;

    fn known_from(pairs: &[(&str, &str)]) -> BTreeSet<String> {
        pairs
            .iter()
            .map(|(s, c)| canonical_entry_key(s, c))
            .collect()
    }

    fn no_known() -> BTreeSet<String> {
        BTreeSet::new()
    }

    // ── tokenize ────────────────────────────────────────────────

    #[test]
    fn test_tokenize_basic() {
        assert_eq!(tokenize("hello world test"), vec!["hello", "world", "test"]);
    }

    #[test]
    fn test_tokenize_strips_punctuation() {
        assert_eq!(
            tokenize("hello, world! test."),
            vec!["hello", "world", "test"]
        );
    }

    #[test]
    fn test_tokenize_keeps_inner_punctuation_and_numbers() {
        // Edge punctuation is stripped; inner content including letters AND
        // numbers survives (JS \p{L}\p{N}_ class).
        assert_eq!(tokenize("CS50, it's ok"), vec!["CS50", "it's", "ok"]);
    }

    #[test]
    fn test_tokenize_unicode_letters_and_numbers() {
        assert_eq!(
            tokenize("извините, пожалуйста"),
            vec!["извините", "пожалуйста"]
        );
    }

    #[test]
    fn test_tokenize_empty_and_whitespace_only() {
        assert!(tokenize("").is_empty());
        assert!(tokenize("   \t  ").is_empty());
    }

    // ── edit_distance ───────────────────────────────────────────

    #[test]
    fn test_edit_distance_basic() {
        assert_eq!(edit_distance("cool", "good"), 2);
        assert_eq!(edit_distance("abc", "axc"), 1);
        assert_eq!(edit_distance("abcdef", "abcdef"), 0);
        assert_eq!(edit_distance("", "ab"), 2);
    }

    // ── find_substitutions (JS traceback, insertion-prefer ties) ─

    #[test]
    fn test_find_substitutions_identical() {
        let orig: Vec<String> = vec!["hello".into(), "world".into()];
        assert!(find_substitutions(&orig, &orig).is_empty());
    }

    #[test]
    fn test_find_substitutions_single() {
        let orig: Vec<String> = vec!["hello".into(), "shunade".into()];
        let edited: Vec<String> = vec!["hello".into(), "Sinead".into()];
        let subs = find_substitutions(&orig, &edited);
        assert_eq!(
            subs,
            vec![(String::from("shunade"), String::from("Sinead"))]
        );
    }

    #[test]
    fn test_find_substitutions_insertion_only_no_pairs() {
        let orig: Vec<String> = vec!["hello".into()];
        let edited: Vec<String> = vec!["hello".into(), "world".into()];
        assert!(find_substitutions(&orig, &edited).is_empty());
    }

    #[test]
    fn test_find_substitutions_deletion_only_no_pairs() {
        let orig: Vec<String> = vec!["hello".into(), "world".into()];
        let edited: Vec<String> = vec!["hello".into()];
        assert!(find_substitutions(&orig, &edited).is_empty());
    }

    #[test]
    fn test_find_substitutions_rc1_shape_yields_one_boundary_pair() {
        // RC-1 (plan §1): the OLD Fluence tie rule manufactured
        // `cool->stuff`, `was->agent`, `it->Good`. The JS-faithful
        // traceback produces exactly ONE adjacency pair: [cool, Good].
        let orig: Vec<String> = [
            "I", "went", "to", "the", "shunade", "and", "it", "was", "cool",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let edited: Vec<String> = [
            "I", "went", "to", "the", "shunade", "Good", "agent", "stuff",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let subs = find_substitutions(&orig, &edited);
        assert_eq!(subs.len(), 1);
        assert_eq!(subs[0].0, "cool");
        assert_eq!(subs[0].1, "Good");
    }

    // ── is_valid_pair (the gate table; plan §5, floor 0.35) ─────

    #[test]
    fn test_valid_pairs_pass_gates() {
        assert!(is_valid_pair("shunade", "Sinead", &no_known())); // 0.57
        assert!(is_valid_pair("Johnatan", "Jonathan", &no_known())); // 0.25
        assert!(is_valid_pair("recognision", "recognition", &no_known()));
        assert!(is_valid_pair("definately", "definitely", &no_known())); // 0.9
        assert!(is_valid_pair("accomodate", "accommodate", &no_known()));
        assert!(is_valid_pair("abc", "axc", &no_known())); // 0.333
        assert!(is_valid_pair("'hello'", "hello", &no_known())); // 0.286
        assert!(is_valid_pair("forteen", "fourteen", &no_known()));
        assert!(is_valid_pair("ab", "abcd", &no_known())); // 0.5
    }

    #[test]
    fn test_dissimilar_pairs_rejected() {
        // Screenshot garbage (plan §5): all below the 0.35 floor.
        assert!(!is_valid_pair("stuck", "agent", &no_known()));
        assert!(!is_valid_pair("CS50", "tabs", &no_known()));
        assert!(!is_valid_pair("have", "Type", &no_known()));
        assert!(!is_valid_pair("release", "agent", &no_known()));
        assert!(!is_valid_pair("in", "add", &no_known()));
        assert!(!is_valid_pair("do", "add", &no_known())); // 0.333, tightest
        assert!(!is_valid_pair("check", "add", &no_known()));
        assert!(!is_valid_pair("testing", "ChatGPT", &no_known())); // dist 6/7
                                                                    // Plan §5 "exact value asserted in test" (audit Round-4 F6): pinned
                                                                    // empirically at cargo-test time: both words length 7, dist 6 →
                                                                    // ratio 6/7 ≈ 0.857 > 0.65.
        assert_eq!(edit_distance("testing", "chatgpt"), 6);
        assert!(!is_valid_pair("in", "to", &no_known())); // length rule
        assert!(!is_valid_pair("Let", "et", &no_known())); // length rule ONLY
    }

    #[test]
    fn test_case_only_pairs_skipped() {
        // OQ-5 (owner-confirmed): reference parity - lowercase-identical
        // pairs are skipped, hit the human Add path in the Words tab.
        assert!(!is_valid_pair("hello", "Hello", &no_known()));
        assert!(!is_valid_pair("HELLO", "hello", &no_known()));
        assert!(!is_valid_pair("hello", "hello", &no_known()));
    }

    #[test]
    fn test_short_corrected_rejected() {
        // OQ-2 (owner-confirmed): corrected words < 3 chars do not surface.
        // Let->et survives the ratio (1/3 = 0.333) - the length rule is the
        // sole killer (plan §5, load-bearing).
        assert!(!is_valid_pair("abcd", "ab", &no_known()));
        assert!(!is_valid_pair("ab", "cd", &no_known())); // sim 0 + len
        assert!(!is_valid_pair("a", "b", &no_known()));
    }

    #[test]
    fn test_trivially_short_original_but_usable_corrected_allowed() {
        // Only the CORRECTED side has a minimum; ab -> abcd passes (0.5).
        assert!(is_valid_pair("ab", "abcd", &no_known()));
    }

    #[test]
    fn test_numeric_pairs_rejected_by_gates() {
        // OQ-6 (owner-confirmed): exact parity - NO numeric-mix rule is
        // ported; the gates reject these on their own.
        assert!(!is_valid_pair("123", "456", &no_known())); // sim 0.0
        assert!(!is_valid_pair("123", "abc", &no_known())); // ratio 1.0
        assert!(!is_valid_pair("abc", "123", &no_known()));
    }

    #[test]
    fn test_dissimilar_word_pairs_rejected() {
        assert!(!is_valid_pair("abc", "xyz", &no_known()));
        assert!(!is_valid_pair("banana", "elephant", &no_known()));
        assert!(!is_valid_pair("computer", "elephant", &no_known()));
    }

    #[test]
    fn test_reported_case_shwande_no_longer_surfaces() {
        // OQ-1 (owner-accepted): shwande->Sinead (sim 0.286) sits below the
        // floor; no floor value admits it without re-admitting do->add.
        assert!(!is_valid_pair("shwande", "Sinead", &no_known()));
    }

    #[test]
    fn test_residual_boundary_pairs_still_pass() {
        // Documented residuals (plan §5): owner-accepted class; one Dismiss
        // silences each forever.
        assert!(is_valid_pair("cool", "Good", &no_known())); // 0.50 (RC-1)
        assert!(is_valid_pair("again", "agent", &no_known())); // 0.400
        assert!(is_valid_pair("shunade", "Sinead", &no_known())); // 0.43 expected case
    }

    #[test]
    fn test_known_pair_skip() {
        let known = known_from(&[("shunade", "Sinead")]);
        assert!(!is_valid_pair("shunade", "Sinead", &known));
        assert!(!is_valid_pair("SHUNADE", "sinead", &known)); // canonical
        assert!(is_valid_pair("shunade", "Shanade", &known)); // different pair
    }

    #[test]
    fn test_empty_strings_rejected() {
        assert!(!is_valid_pair("", "hello", &no_known()));
        assert!(!is_valid_pair("hello", "", &no_known()));
        assert!(!is_valid_pair("", "", &no_known()));
    }

    // ── extract_corrections (JS L137-177 shape) ─────────────────

    #[test]
    fn test_extract_simple_correction() {
        let pairs =
            extract_corrections("I went to the shunade", "I went to the Sinead", &no_known());
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].0, "shunade");
        assert_eq!(pairs[0].1, "Sinead");
    }

    #[test]
    fn test_extract_identical_returns_empty() {
        assert!(extract_corrections("hello world", "hello world", &no_known()).is_empty());
    }

    #[test]
    fn test_extract_empty_inputs() {
        assert!(extract_corrections("", "hello world", &no_known()).is_empty());
        assert!(extract_corrections("hello world", "", &no_known()).is_empty());
        assert!(extract_corrections("", "", &no_known()).is_empty());
    }

    #[test]
    fn test_extract_full_rewrite_through_guard() {
        // RC-5 leak fixture: `release->replace` (sim 0.571) would pass the
        // per-pair gates; the LCS guard vetoes the whole rewrite.
        let pairs = extract_corrections("we should release", "replace the plan", &no_known());
        assert!(pairs.is_empty());
    }

    #[test]
    fn test_extract_quick_brown_fox_rewrite_rejected() {
        let pairs =
            extract_corrections("the quick brown fox", "a slow red cat jumped", &no_known());
        assert!(pairs.is_empty());
    }

    #[test]
    fn test_extract_partial_correction_survives_vetoes() {
        // 1 sub of 4 words (=25%, under the veto) and LCS 2 of 4 (guard
        // passes): one legitimate pair survives the vetoes.
        // great->grand: dist 3/5 = 0.6 <= 0.65 -> valid.
        let pairs = extract_corrections("this city is great", "this town is grand", &no_known());
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].0, "great");
        assert_eq!(pairs[0].1, "grand");
    }

    #[test]
    fn test_extract_rc1_shape_single_residual_candidate() {
        // RC-1 (plan §1, v4): exactly ONE candidate `cool->Good` survives;
        // the old tie-pairs (`cool->stuff`, `was->agent`, `it->Good`) must
        // not reappear. Boundary-word residual, owner-accepted class.
        let pairs = extract_corrections(
            "I went to the shunade and it was cool",
            "I went to the shunade Good agent stuff",
            &no_known(),
        );
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].0, "cool");
        assert_eq!(pairs[0].1, "Good");
    }

    #[test]
    fn test_extract_known_pairs_alone_filter() {
        let known = known_from(&[("shunade", "Sinead")]);
        let pairs = extract_corrections("I went to the shunade", "I went to the Sinead", &known);
        assert!(pairs.is_empty());
    }

    #[test]
    fn test_extract_quoted_tokens_form_no_pair() {
        // Round-4 F4 (pipeline truth): the tokenizer strips edge quotes on
        // BOTH sides, so the tokens are identical and no substitution
        // window forms. The judge accepts the RAW strings (its own test
        // stays), but in production no candidate ever reaches it.
        assert!(extract_corrections("hello", "'hello'", &no_known()).is_empty());
        assert!(extract_corrections("'hello'", "hello", &no_known()).is_empty());
    }

    #[test]
    fn test_extract_duplicate_pair_deduped() {
        // Pair-level dedup within one call (D4): two INDEPENDENT
        // shunade→Sinead regions, separated by shared words so each gets
        // its own substitution window. Both survive `is_valid_pair`, so the
        // second is dropped solely by the seen_pairs guard (Round-4 F1:
        // the previous fixture produced zero pairs and never reached it).
        let pairs = extract_corrections(
            "the shunade is fine shunade",
            "the Sinead is fine Sinead",
            &no_known(),
        );
        assert_eq!(
            pairs.len(),
            1,
            "exactly one (shunade, Sinead) survives: {pairs:?}"
        );
        assert_eq!(pairs[0], ("shunade".to_string(), "Sinead".to_string()));
    }

    #[test]
    fn test_invalid_pair_reason_covers_skip_chain() {
        // Round-4 F2: the reason mirror must walk the gates in the SAME
        // order as `is_valid_pair`, with `None` exactly for accepted pairs.
        let known = known_from(&[("shunade", "Sinead")]);
        assert_eq!(
            invalid_pair_reason("shunade", "Sinead", &known),
            Some("already in dictionary")
        );
        assert_eq!(
            invalid_pair_reason("HELLO", "hello", &no_known()),
            Some("case-only difference")
        );
        // corrected < 3 chars ("dr" is 2); all earlier gates miss.
        assert_eq!(
            invalid_pair_reason("shunade", "dr", &no_known()),
            Some("corrected shorter than 3 chars")
        );
        assert_eq!(
            invalid_pair_reason("stuck", "agent", &no_known()),
            Some("below similarity floor")
        );
        // 4/7 ≈ 0.571 ≤ 0.65, len ok, not case-equivalent, unknown → valid.
        assert_eq!(invalid_pair_reason("shunade", "Sinead", &no_known()), None);
        // Gate ORDER: do/add is known AND dissimilar (dist 2/3 > 0.65), so
        // reporting "already in dictionary" proves the known gate wins.
        assert_eq!(
            invalid_pair_reason("do", "add", &known_from(&[("do", "add")])),
            Some("already in dictionary")
        );
    }

    #[test]
    fn test_extract_multisentence_punctuation_only_unchanged() {
        // Tokenizer strips edge punctuation: `hello world.` tokenizes to
        // `hello world` - no candidates.
        assert!(extract_corrections("hello world", "hello world.", &no_known()).is_empty());
        // Emoji-only token vanishes at tokenization (no pairing possible).
        assert!(extract_corrections("hello world", "hello 🌍", &no_known()).is_empty());
    }

    #[test]
    fn test_extract_remaining_word_case_only_skipped() {
        // Case-only edit inside the anchored span: lowercase-identical
        // gates reject the pair -> nothing to learn (OQ-5).
        assert!(extract_corrections("hello world", "Hello World", &no_known()).is_empty());
    }
}
