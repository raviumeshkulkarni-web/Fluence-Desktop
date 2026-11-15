// Fluence Windows - Spoken formatting commands
//
// Deterministic, opt-in rewrite of the FINAL transcript: standalone command
// phrases ("comma", "new paragraph", ...) become punctuation or line breaks.
//
// The command words are ordinary English words ("in this period..."), which
// is why the feature is a user toggle, default OFF. It runs on final text
// only - never on the STT recognition prompt.

use std::sync::OnceLock;

enum Replacement {
    /// Attach to the preceding word (drop whitespace before, one space after).
    Attach(&'static str),
    /// Break the line (drop whitespace before and after, no inserted space).
    Newline(&'static str),
}

struct Command {
    phrase: &'static str,
    replacement: Replacement,
}

const COMMANDS: &[Command] = &[
    Command {
        phrase: "new paragraph",
        replacement: Replacement::Newline("\n\n"),
    },
    Command {
        phrase: "new line",
        replacement: Replacement::Newline("\n"),
    },
    Command {
        phrase: "exclamation mark",
        replacement: Replacement::Attach("!"),
    },
    Command {
        phrase: "exclamation point",
        replacement: Replacement::Attach("!"),
    },
    Command {
        phrase: "question mark",
        replacement: Replacement::Attach("?"),
    },
    Command {
        phrase: "semicolon",
        replacement: Replacement::Attach(";"),
    },
    Command {
        phrase: "period",
        replacement: Replacement::Attach("."),
    },
    Command {
        phrase: "comma",
        replacement: Replacement::Attach(","),
    },
    Command {
        phrase: "colon",
        replacement: Replacement::Attach(":"),
    },
];

fn command_regex() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(
            r"(?i)\b(?:new paragraph|new line|exclamation mark|exclamation point|question mark|semicolon|period|comma|colon)\b",
        )
        .expect("spoken formatting regex is valid")
    })
}

fn skip_whitespace(text: &str, mut idx: usize) -> usize {
    while let Some(c) = text[idx..].chars().next() {
        if !c.is_whitespace() {
            break;
        }
        idx += c.len_utf8();
    }
    idx
}

/// A dropped leading command can leave the mark spoken after it (STT often
/// punctuates naturally). Skip that orphan mark so the transcript does not
/// start with punctuation.
fn skip_leading_command_remnants(text: &str, mut idx: usize) -> usize {
    idx = skip_whitespace(text, idx);
    if let Some(c) = text[idx..].chars().next() {
        if matches!(c, '.' | ',' | ';' | ':' | '!' | '?') {
            idx += c.len_utf8();
            idx = skip_whitespace(text, idx);
        }
    }
    idx
}

/// STT frequently already punctuates around a spoken command, e.g.
/// "school, comma, on". Consume those matching marks so the command does not
/// turn one intended comma into `,,,`.
fn skip_matching_marks(text: &str, mut idx: usize, mark: &str) -> usize {
    idx = skip_whitespace(text, idx);
    while text[idx..].starts_with(mark) {
        idx += mark.len();
        idx = skip_whitespace(text, idx);
    }
    idx
}

fn needs_trailing_space(next: Option<char>) -> bool {
    matches!(next, Some(c) if !c.is_whitespace() && !matches!(c, '.' | ',' | '!' | '?' | ';' | ':' | ')' | ']' | '}' | '"' | '\''))
}

/// Rewrite standalone spoken commands in `text`. Text without a command is
/// returned byte-identical (fast path).
pub fn apply(text: &str) -> String {
    if text.is_empty() {
        return String::new();
    }

    let mut out = String::with_capacity(text.len());
    let mut cursor = 0usize;
    let mut changed = false;

    for m in command_regex().find_iter(text) {
        let phrase = m.as_str().to_ascii_lowercase();
        let Some(command) = COMMANDS.iter().find(|c| c.phrase == phrase) else {
            continue;
        };

        // A word boundary also matches before a possessive apostrophe
        // ("period's"), which is ordinary prose rather than a command.
        if text[m.end()..].starts_with('\'') || text[m.end()..].starts_with('\u{2019}') {
            continue;
        }

        out.push_str(&text[cursor..m.start()]);
        // Drop only horizontal whitespace at the insertion point, and
        // normalize spaces that sit immediately before a line break.
        while out.ends_with([' ', '\t']) {
            out.pop();
        }
        let content_end = out.trim_end_matches(['\r', '\n']).len();
        let line_breaks = out[content_end..].to_string();
        let content_base = out[..content_end].trim_end_matches([' ', '\t']).len();
        if content_base < content_end {
            out.truncate(content_base);
            out.push_str(&line_breaks);
        }
        let has_line_breaks = !line_breaks.is_empty();

        if out.len() == line_breaks.len() {
            // Nothing to attach a mark to: drop the command (and any
            // punctuation the STT added after it) rather than emit leading
            // punctuation. Existing leading line breaks are preserved.
            cursor = skip_leading_command_remnants(text, m.end());
            changed = true;
            continue;
        }

        match command.replacement {
            Replacement::Attach(mark) => {
                let content_len = out.len() - line_breaks.len();
                let already_has_mark = out[..content_len].ends_with(mark);
                if has_line_breaks {
                    // Keep the line break but put the mark before it:
                    // "a new line comma b" -> "a,\nb", not "a\n, b".
                    if !already_has_mark {
                        out.truncate(content_len);
                        out.push_str(mark);
                        out.push_str(&line_breaks);
                    }
                } else if !already_has_mark {
                    out.push_str(mark);
                }
                cursor = skip_matching_marks(text, m.end(), mark);
                if !has_line_breaks && needs_trailing_space(text[cursor..].chars().next()) {
                    out.push(' ');
                }
            }
            Replacement::Newline(breaks) => {
                out.push_str(breaks);
                cursor = skip_whitespace(text, m.end());
            }
        }
        changed = true;
    }

    if !changed {
        return text.to_string();
    }
    out.push_str(&text[cursor..]);
    out
}

#[cfg(test)]
mod tests {
    use super::apply;

    #[test]
    fn attaches_punctuation_and_normalizes_spacing() {
        assert_eq!(apply("hello comma world"), "hello, world");
        assert_eq!(apply("hello  comma   world"), "hello, world");
        assert_eq!(apply("really question mark"), "really?");
        assert_eq!(apply("hey exclamation mark"), "hey!");
        assert_eq!(apply("hey exclamation point"), "hey!");
        assert_eq!(apply("note colon done"), "note: done");
        assert_eq!(apply("first semicolon second"), "first; second");
    }

    #[test]
    fn inserts_line_breaks_without_surrounding_spaces() {
        assert_eq!(apply("first new line second"), "first\nsecond");
        assert_eq!(apply("first new paragraph second"), "first\n\nsecond");
        assert_eq!(
            apply("first new paragraph second new line third"),
            "first\n\nsecond\nthird"
        );
    }

    #[test]
    fn leaves_plain_text_byte_identical() {
        let text = "A plain sentence with no command words.";
        assert_eq!(apply(text), text);
        assert_eq!(apply(""), "");
    }

    #[test]
    fn command_words_convert_even_mid_sentence() {
        // The documented trade-off of the opt-in toggle: a real use of the
        // word converts. This is why the setting defaults to OFF.
        assert_eq!(
            apply("In this period, commas and periods stay put."),
            "In this., commas and periods stay put."
        );
    }

    #[test]
    fn does_not_match_inside_words_or_possessives() {
        assert_eq!(apply("commas and periods"), "commas and periods");
        assert_eq!(apply("new lineup"), "new lineup");
        assert_eq!(apply("this period's report"), "this period's report");
    }

    #[test]
    fn drops_leading_command_without_punctuation_target() {
        assert_eq!(apply("period"), "");
        assert_eq!(apply("comma hello"), "hello");
        assert_eq!(apply("period, hello"), "hello");
        assert_eq!(apply("colon: hello"), "hello");
        assert_eq!(apply("new line, hello"), "hello");
    }

    #[test]
    fn command_before_existing_punctuation_does_not_add_a_space() {
        assert_eq!(apply("done period. next"), "done. next");
        assert_eq!(apply("done comma)"), "done,)");
    }

    #[test]
    fn spoken_marks_do_not_duplicate_stt_punctuation() {
        assert_eq!(
            apply("I am going to school, comma, on Monday"),
            "I am going to school, on Monday",
        );
        assert_eq!(
            apply("following pen, comma, notebook"),
            "following pen, notebook",
        );
        assert_eq!(apply("if I say comma, then"), "if I say, then");
        assert_eq!(apply("say, comma"), "say,");
        assert_eq!(apply("done period. next"), "done. next");
    }

    #[test]
    fn attach_commands_stay_before_existing_line_breaks() {
        // "new line" then "comma": the break survives and the comma attaches
        // before it. Attach-then-newline is unchanged from before.
        assert_eq!(apply("a new line comma b"), "a,\nb");
        assert_eq!(apply("a comma new line b"), "a,\nb");
        // A line break that was already in the transcript is also kept.
        assert_eq!(apply("hello\ncomma world"), "hello,\nworld");
        // Stray spaces before the break do not leak in front of the mark.
        assert_eq!(apply("hello \ncomma world"), "hello,\nworld");
    }
}
