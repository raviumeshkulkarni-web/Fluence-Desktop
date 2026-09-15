// Fluence Windows - Learner
// Saves extracted correction candidates to the suggestion system.
// Uses the existing suggestion infrastructure (frequency tracking,
// dismiss handling, atomic writes) rather than writing directly
// to the dictionary.
//
// The suggestion system requires multiple observations before
// a correction appears in the UI, preventing accidental learning.

use super::extraction::Candidate;

/// Save correction candidates to the suggestion database.
/// Each call increments the frequency counter for repeated corrections.
/// Dismissed suggestions remain dismissed (user intent is respected).
///
/// Returns the number of candidates saved, or an error string.
pub fn save_corrections(candidates: Vec<Candidate>) -> Result<usize, String> {
    if candidates.is_empty() {
        return Ok(0);
    }

    let count = candidates.len();

    crate::suggestion::upsert_suggestions(candidates)?;

    log::info!(
        "[AutoLearn] Saved {} correction candidates to suggestion database",
        count
    );

    Ok(count)
}

/// Dictionary-key helper now lives in the parent module so non-Windows
/// builds (suggestion.rs) can use it too; see auto_learn::get_current_dictionary.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_save_corrections_empty_returns_zero() {
        let result = save_corrections(Vec::new());
        assert_eq!(result.unwrap(), 0);
    }

    #[test]
    fn test_get_current_dictionary_does_not_panic() {
        // Verify it doesn't panic regardless of filesystem state
        let _dict = crate::auto_learn::get_current_dictionary();
    }
}
