// Fluence Windows — Auto-Learn Candidate Extraction
// Pure functions that compare raw STT output with final text
// to detect word-level corrections. No side effects.
//
// Evidence → Hypothesis → Human Decision → Persistent Knowledge

use serde::{Deserialize, Serialize};
use similar::TextDiff;

/// Classification of how the text was transformed.
/// Determines whether candidate extraction is safe.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum TransformationType {
    /// Deterministic cleanup: filler word removal, grammar fix.
    /// Safe for candidate extraction.
    Cleanup,
    /// Semantic rewrite: translation, bullet points, professional rewrite.
    /// NOT safe for candidate extraction.
    Rewrite,
}

impl TransformationType {
    /// Classify an AI polish style into a transformation category.
    /// Unknown styles default to Rewrite (safe behavior).
    pub fn from_ai_polish_style(style: &str) -> Self {
        match style {
            // "none" means no AI polish ran; the text was only
            // deterministically cleaned (filler removal, grammar).
            // Safe for candidate extraction.
            "none" => TransformationType::Cleanup,
            // Every other style applies an LLM transform (cleanup,
            // translation, bullet points, ...). The output is a
            // semantic rewrite, NOT a deterministic cleanup.
            _ => TransformationType::Rewrite,
        }
    }
}

/// Context for candidate extraction.
/// Contains all information needed to detect corrections.
#[derive(Debug, Clone)]
pub struct ExtractionContext {
    /// STT output before any corrections (raw transcription)
    pub original: String,
    /// Final text after dictionary corrections and AI polish
    pub transformed: String,
    /// How the text was transformed (Cleanup or Rewrite)
    pub transformation_type: TransformationType,
    /// Language code (e.g., "en", "es")
    pub language: String,
    /// STT provider name (e.g., "groq", "openai", "Local Offline")
    pub provider: String,
}

/// A candidate correction extracted from the diff.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    /// What the STT heard (original text, not normalized)
    pub spoken: String,
    /// What it should be (final text, not normalized)
    pub corrected: String,
}

/// Extract candidate corrections from raw vs final text.
///
/// Returns a list of `Candidate` entries that represent
/// potential transcription corrections. Each pair is validated
/// against extraction rules before being returned.
pub fn extract_candidates(ctx: &ExtractionContext) -> Vec<Candidate> {
    // Rule 1: Skip extraction for semantic rewrite modes
    if ctx.transformation_type == TransformationType::Rewrite {
        log::info!(
            "Skipping candidate extraction for {:?} transformation",
            ctx.transformation_type
        );
        return Vec::new();
    }

    // Rule 2: Skip if texts are identical (no corrections happened)
    if ctx.original == ctx.transformed {
        return Vec::new();
    }

    // Rule 3: Skip if texts are empty
    let original_trimmed = ctx.original.trim();
    let transformed_trimmed = ctx.transformed.trim();
    if original_trimmed.is_empty() || transformed_trimmed.is_empty() {
        return Vec::new();
    }

    // Rule 4: Word-level diff using the similar crate
    let diff = TextDiff::from_words(original_trimmed, transformed_trimmed);

    // Rule 5: Check if too many words changed (>50% = rewrite, not correction)
    let total_words = count_words(original_trimmed).max(count_words(transformed_trimmed));
    let changed_words = count_changed_words(&diff);
    if total_words > 0 && (changed_words as f64 / total_words as f64) > 0.5 {
        log::info!(
            "Skipping candidate extraction: {} of {} words changed (>50%)",
            changed_words,
            total_words
        );
        return Vec::new();
    }

    // Rule 6: Extract substitution pairs from diff changes
    let mut candidates = Vec::new();
    let mut pending_deletes = Vec::new();
    let mut pending_inserts = Vec::new();

    for change in diff.iter_all_changes() {
        match change.tag() {
            similar::ChangeTag::Delete => {
                let word = change.value().trim();
                if !word.is_empty() {
                    // If we have pending inserts, process the previous substitution first
                    if !pending_inserts.is_empty() {
                        process_substitution(&pending_deletes, &pending_inserts, &mut candidates);
                        pending_deletes.clear();
                        pending_inserts.clear();
                    }
                    pending_deletes.push(word.to_string());
                }
            }
            similar::ChangeTag::Insert => {
                let word = change.value().trim();
                if !word.is_empty() {
                    pending_inserts.push(word.to_string());
                }
            }
            similar::ChangeTag::Equal => {
                // Process any accumulated substitution
                if !pending_deletes.is_empty() || !pending_inserts.is_empty() {
                    process_substitution(&pending_deletes, &pending_inserts, &mut candidates);
                    pending_deletes.clear();
                    pending_inserts.clear();
                }
            }
        }
    }

    // Process any remaining substitution at end
    process_substitution(&pending_deletes, &pending_inserts, &mut candidates);

    candidates
}

/// Process a substitution pair (deletes → inserts) and add valid candidates.
fn process_substitution(deletes: &[String], inserts: &[String], candidates: &mut Vec<Candidate>) {
    // Rule: Only one-word → one-word substitutions
    if deletes.len() != 1 || inserts.len() != 1 {
        return;
    }

    let spoken = &deletes[0];
    let corrected = &inserts[0];

    // Rule: Both must be at least 3 characters
    if spoken.len() < 3 || corrected.len() < 3 {
        return;
    }

    // Rule: Ignore capitalization-only differences
    if spoken.to_lowercase() == corrected.to_lowercase() {
        return;
    }

    // Rule: Ignore punctuation-only differences
    let spoken_stripped = strip_punctuation(spoken);
    let corrected_stripped = strip_punctuation(corrected);
    if spoken_stripped == corrected_stripped {
        return;
    }

    // Rule: Levenshtein similarity must be >= 0.40
    // Compare lowercase to avoid case sensitivity inflating the distance.
    // Threshold tuned for real-world corrections: catches phonetic mishearings
    // while filtering completely unrelated words.
    let similarity =
        strsim::normalized_levenshtein(&spoken.to_lowercase(), &corrected.to_lowercase());
    if similarity < 0.40 {
        log::debug!(
            "Skipping candidate ({} chars → {} chars): similarity {:.2} < 0.40",
            spoken.chars().count(),
            corrected.chars().count(),
            similarity
        );
        return;
    }

    candidates.push(Candidate {
        spoken: spoken.clone(),
        corrected: corrected.clone(),
    });
}

/// Count words in a string.
fn count_words(text: &str) -> usize {
    text.split_whitespace().count()
}

/// Count words that changed in a diff.
fn count_changed_words(diff: &TextDiff<str>) -> usize {
    let mut count = 0;
    for change in diff.iter_all_changes() {
        match change.tag() {
            similar::ChangeTag::Delete | similar::ChangeTag::Insert => {
                let word = change.value().trim();
                if !word.is_empty() {
                    count += 1;
                }
            }
            similar::ChangeTag::Equal => {}
        }
    }
    count
}

/// Strip punctuation from the edges of a word.
pub(crate) fn strip_punctuation(word: &str) -> String {
    word.trim_matches(|c: char| !c.is_alphanumeric())
        .to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_ctx(original: &str, transformed: &str) -> ExtractionContext {
        ExtractionContext {
            original: original.to_string(),
            transformed: transformed.to_string(),
            transformation_type: TransformationType::Cleanup,
            language: "en".to_string(),
            provider: "groq".to_string(),
        }
    }

    #[test]
    fn test_extract_simple_correction() {
        let ctx = make_ctx("I went to the shunade", "I went to the Sinead");
        let candidates = extract_candidates(&ctx);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].spoken, "shunade");
        assert_eq!(candidates[0].corrected, "Sinead");
    }

    #[test]
    fn test_extract_no_correction_identical() {
        let ctx = make_ctx("hello world", "hello world");
        let candidates = extract_candidates(&ctx);
        assert!(candidates.is_empty());
    }

    #[test]
    fn test_extract_rejects_multi_word() {
        let ctx = make_ctx("I'm gonna go", "I'm going to go");
        let candidates = extract_candidates(&ctx);
        // "gonna" → "going to" is multi-word, should be rejected
        assert!(candidates.is_empty());
    }

    #[test]
    fn test_extract_rejects_too_different() {
        let ctx = make_ctx("banana is yellow", "elephant is gray");
        let candidates = extract_candidates(&ctx);
        // "banana" → "elephant" is too different
        assert!(candidates.is_empty());
    }

    #[test]
    fn test_extract_skips_short_words() {
        let ctx = make_ctx("I am a test", "I am the test");
        let candidates = extract_candidates(&ctx);
        // "a" → "the" is too short (< 3 chars)
        assert!(candidates.is_empty());
    }

    #[test]
    fn test_extract_ignores_capitalization() {
        let ctx = make_ctx("hello world", "Hello World");
        let candidates = extract_candidates(&ctx);
        // Capitalization-only difference, should be ignored
        assert!(candidates.is_empty());
    }

    #[test]
    fn test_extract_ignores_punctuation_only() {
        let ctx = make_ctx("dr. Smith", "Dr Smith");
        let candidates = extract_candidates(&ctx);
        // Punctuation-only difference, should be ignored
        assert!(candidates.is_empty());
    }

    #[test]
    fn test_extract_rejects_rewrite_mode() {
        let mut ctx = make_ctx("raw text", "polished text");
        ctx.transformation_type = TransformationType::Rewrite;
        let candidates = extract_candidates(&ctx);
        assert!(candidates.is_empty());
    }

    #[test]
    fn test_transformation_type_from_style() {
        assert_eq!(
            TransformationType::from_ai_polish_style("none"),
            TransformationType::Cleanup
        );
        // "clean" is an LLM rewrite (filler removal, grammar fix,
        // rephrasing) — NOT a deterministic cleanup, so extraction
        // must be skipped.
        assert_eq!(
            TransformationType::from_ai_polish_style("clean"),
            TransformationType::Rewrite
        );
        assert_eq!(
            TransformationType::from_ai_polish_style("translate_en"),
            TransformationType::Rewrite
        );
        assert_eq!(
            TransformationType::from_ai_polish_style("bullet_points"),
            TransformationType::Rewrite
        );
        assert_eq!(
            TransformationType::from_ai_polish_style("professional"),
            TransformationType::Rewrite
        );
        // Unknown defaults to Rewrite (safe)
        assert_eq!(
            TransformationType::from_ai_polish_style("unknown_future_mode"),
            TransformationType::Rewrite
        );
    }

    #[test]
    fn test_strip_punctuation() {
        assert_eq!(strip_punctuation("hello"), "hello");
        assert_eq!(strip_punctuation("dr."), "dr");
        assert_eq!(strip_punctuation("'hello'"), "hello");
        assert_eq!(strip_punctuation("C++"), "c");
    }

    // ── Edge cases ───────────────────────────────────────────────

    #[test]
    fn test_extract_empty_original() {
        let ctx = make_ctx("", "something");
        assert!(extract_candidates(&ctx).is_empty());
    }

    #[test]
    fn test_extract_empty_transformed() {
        let ctx = make_ctx("something", "");
        assert!(extract_candidates(&ctx).is_empty());
    }

    #[test]
    fn test_extract_both_empty() {
        let ctx = make_ctx("", "");
        assert!(extract_candidates(&ctx).is_empty());
    }

    #[test]
    fn test_extract_whitespace_only_original() {
        let ctx = make_ctx("   ", "hello world");
        assert!(extract_candidates(&ctx).is_empty());
    }

    #[test]
    fn test_extract_whitespace_only_transformed() {
        let ctx = make_ctx("hello world", "   ");
        assert!(extract_candidates(&ctx).is_empty());
    }

    #[test]
    fn test_extract_rejects_over_50_percent_change() {
        // 3 of 4 words changed = 75% change
        let ctx = make_ctx("the cat sat there", "a dog ran here now");
        assert!(extract_candidates(&ctx).is_empty());
    }

    #[test]
    fn test_extract_exactly_50_percent_change_ok() {
        // 5 words, 1 sub = 2 changed / 5 total = 40% (NOT > 50%)
        // "shunade" → "Sinead" passes Levenshtein (~0.46)
        let ctx = make_ctx("I went to the shunade", "I went to the Sinead");
        let candidates = extract_candidates(&ctx);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].spoken, "shunade");
        assert_eq!(candidates[0].corrected, "Sinead");
    }

    #[test]
    fn test_extract_multiple_substitutions() {
        let ctx = make_ctx("I went to the shunade park", "I went to the Sinead park");
        // Only "shunade" → "Sinead" should be extracted
        let candidates = extract_candidates(&ctx);
        assert_eq!(candidates.len(), 1);
    }

    #[test]
    fn test_extract_levenshtein_boundary_040() {
        // 5 words, 1 sub = 2 changed / 5 total = 40% (NOT > 50%)
        // "shunade" → "Sinead" has similarity ~0.46 >= 0.40
        let ctx = make_ctx("I went to the shunade", "I went to the Sinead");
        let candidates = extract_candidates(&ctx);
        assert_eq!(candidates.len(), 1);
    }

    #[test]
    fn test_extract_levenshtein_below_040() {
        // 5 words, 1 sub. "abc" → "xyz" has very low similarity (~0.0 < 0.40)
        let ctx = make_ctx("I went to the abc", "I went to the xyz");
        assert!(extract_candidates(&ctx).is_empty());
    }

    #[test]
    fn test_extract_preserves_original_case() {
        let ctx = make_ctx("I saw Johnatan there", "I saw Jonathan there");
        let candidates = extract_candidates(&ctx);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].spoken, "Johnatan");
        assert_eq!(candidates[0].corrected, "Jonathan");
    }

    #[test]
    fn test_extract_single_word_identical() {
        let ctx = make_ctx("hello", "hello");
        assert!(extract_candidates(&ctx).is_empty());
    }

    #[test]
    fn test_count_words_basic() {
        assert_eq!(count_words("hello world"), 2);
        assert_eq!(count_words("one"), 1);
        assert_eq!(count_words(""), 0);
        assert_eq!(count_words("  spaced  out  "), 2);
    }

    #[test]
    fn test_process_substitution_single_pair() {
        let mut candidates = Vec::new();
        process_substitution(
            &["shunade".to_string()],
            &["Sinead".to_string()],
            &mut candidates,
        );
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].spoken, "shunade");
        assert_eq!(candidates[0].corrected, "Sinead");
    }

    #[test]
    fn test_process_substitution_multi_word_rejected() {
        let mut candidates = Vec::new();
        process_substitution(
            &["gonna".to_string()],
            &["going".to_string(), "to".to_string()],
            &mut candidates,
        );
        assert!(candidates.is_empty());
    }

    #[test]
    fn test_process_substitution_empty_pair() {
        let mut candidates = Vec::new();
        process_substitution(&[], &[], &mut candidates);
        assert!(candidates.is_empty());
    }
}
