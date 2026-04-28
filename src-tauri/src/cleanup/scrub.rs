//! Deterministic post-processing passes that run on every dictation. Replaces
//! the prior on-device T5 grammar corrector — the corrector cost 3–5 s on CPU
//! per dictation and only earned its keep on grammatical fixes (verb tense,
//! subject-verb agreement) that are rare in deliberate single-speaker
//! dictation. The two cheap rule-based passes here cover the corrector's
//! actually-observed wins (repeated words, canonical hallucinations) at zero
//! runtime cost.

use std::sync::LazyLock;

use aho_corasick::{AhoCorasick, AhoCorasickBuilder, MatchKind};

/// Bag of phrases Whisper is known to hallucinate on silence or non-speech
/// audio. Matched case-insensitively as **whole-line** or **trailing**
/// occurrences only — never mid-sentence — so a user dictating these phrases
/// intentionally still gets through. Defense-in-depth alongside whisper.cpp's
/// `suppress_nst=true`, which already kills the music-token family at decode
/// time.
const HALLUCINATION_PHRASES: &[&str] = &[
    "Thanks for watching!",
    "Thanks for watching.",
    "Thank you for watching.",
    "Thank you for watching!",
    "Thank you.",
    "Thanks.",
    "Subtitles by the Amara.org community",
    "Transcribed by ESO Translates",
    "[Music]",
    "(music)",
    "♪",
];

static HALLUCINATION_AC: LazyLock<AhoCorasick> = LazyLock::new(|| {
    AhoCorasickBuilder::new()
        .ascii_case_insensitive(true)
        .match_kind(MatchKind::LeftmostLongest)
        .build(HALLUCINATION_PHRASES)
        .expect("hallucination phrase set is static and non-empty")
});

/// Collapse case-insensitive immediate word repeats: `i i want` → `i want`,
/// `the the cat` → `the cat`, `Hello hello world` → `Hello world`. Keeps the
/// casing of the **first** occurrence so a sentence-leading `Hello Hello` does
/// not collapse to lowercase.
///
/// Tokenization is whitespace-only; punctuation stays attached to the word it
/// rides on (so `cat, cat` is *not* collapsed because the first token is
/// `cat,` and the second is `cat`). This is intentional: lexical repeats with
/// punctuation between them tend to be deliberate, while bare repeats are the
/// canonical ASR artifact.
pub fn collapse_repeats(text: &str) -> String {
    if text.is_empty() {
        return String::new();
    }

    // Walk the input preserving whitespace runs verbatim (so newlines and
    // multi-space gaps survive). We only drop the duplicated word + the single
    // whitespace separator that follows the kept token.
    let mut out = String::with_capacity(text.len());
    let mut prev_word: Option<String> = None;
    let mut pending_ws = String::new();

    for chunk in split_keep_ws(text) {
        match chunk {
            Chunk::Word(w) => {
                let lc = w.to_lowercase();
                if prev_word.as_deref().map(|p| p.to_lowercase()) == Some(lc.clone()) {
                    // Drop the duplicate and the whitespace that led to it.
                    pending_ws.clear();
                    continue;
                }
                out.push_str(&pending_ws);
                pending_ws.clear();
                out.push_str(w);
                prev_word = Some(w.to_string());
            }
            Chunk::Ws(s) => {
                pending_ws.push_str(s);
            }
        }
    }
    out.push_str(&pending_ws);
    out
}

enum Chunk<'a> {
    Word(&'a str),
    Ws(&'a str),
}

fn split_keep_ws(text: &str) -> Vec<Chunk<'_>> {
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let start = i;
        let is_ws = bytes[i].is_ascii_whitespace();
        while i < bytes.len() && bytes[i].is_ascii_whitespace() == is_ws {
            i += 1;
        }
        let slice = &text[start..i];
        out.push(if is_ws {
            Chunk::Ws(slice)
        } else {
            Chunk::Word(slice)
        });
    }
    out
}

/// Remove canonical Whisper hallucinations (e.g. `"Thanks for watching."`)
/// that appear as a whole-line or trailing occurrence. Mid-sentence matches
/// are left alone so the user can intentionally dictate the phrase.
pub fn scrub_hallucinations(text: &str) -> String {
    if text.is_empty() {
        return String::new();
    }

    let mut out_lines: Vec<String> = Vec::with_capacity(text.lines().count() + 1);
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            out_lines.push(line.to_string());
            continue;
        }
        if HALLUCINATION_AC.is_match(trimmed)
            && full_match(&HALLUCINATION_AC, trimmed)
        {
            // Whole line is exactly one hallucination phrase — drop it.
            continue;
        }
        // Tail-trim: if the line ends with a hallucination phrase, strip it.
        let stripped = strip_trailing_match(line);
        out_lines.push(stripped);
    }

    // Preserve trailing newline if the original had one.
    let mut result = out_lines.join("\n");
    if text.ends_with('\n') {
        result.push('\n');
    }
    result.trim_end_matches(|c: char| c == ' ' || c == '\t').to_string()
}

fn full_match(ac: &AhoCorasick, s: &str) -> bool {
    if let Some(m) = ac.find(s) {
        return m.start() == 0 && m.end() == s.len();
    }
    false
}

fn strip_trailing_match(line: &str) -> String {
    // Find the right-most match and verify it sits flush with the end of the
    // line (allowing trailing whitespace).
    let trimmed_end = line.trim_end();
    if trimmed_end.is_empty() {
        return line.to_string();
    }
    let mut last_match: Option<aho_corasick::Match> = None;
    for m in HALLUCINATION_AC.find_iter(trimmed_end) {
        last_match = Some(m);
    }
    let Some(m) = last_match else {
        return line.to_string();
    };
    if m.end() != trimmed_end.len() {
        return line.to_string();
    }
    let kept = &trimmed_end[..m.start()];
    kept.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    // collapse_repeats --------------------------------------------------

    #[test]
    fn collapse_repeats_empty_passthrough() {
        assert_eq!(collapse_repeats(""), "");
    }

    #[test]
    fn collapse_repeats_no_repeat_passthrough() {
        assert_eq!(collapse_repeats("hello world"), "hello world");
    }

    #[test]
    fn collapse_repeats_single_repeat_collapsed() {
        assert_eq!(collapse_repeats("i i want a sandwich"), "i want a sandwich");
    }

    #[test]
    fn collapse_repeats_case_insensitive_keeps_first_casing() {
        assert_eq!(collapse_repeats("Hello hello world"), "Hello world");
    }

    #[test]
    fn collapse_repeats_multiple_runs_collapsed() {
        assert_eq!(
            collapse_repeats("the the cat sat on on the mat"),
            "the cat sat on the mat"
        );
    }

    #[test]
    fn collapse_repeats_preserves_punctuation_separated_repeat() {
        // `cat, cat` — first token is `cat,`, second is `cat`, so they don't
        // match. Deliberate: punctuation usually signals intentional repeat.
        assert_eq!(collapse_repeats("cat, cat"), "cat, cat");
    }

    #[test]
    fn collapse_repeats_three_in_a_row() {
        assert_eq!(collapse_repeats("no no no really"), "no really");
    }

    // scrub_hallucinations ----------------------------------------------

    #[test]
    fn scrub_empty_passthrough() {
        assert_eq!(scrub_hallucinations(""), "");
    }

    #[test]
    fn scrub_no_match_passthrough() {
        assert_eq!(
            scrub_hallucinations("This is a normal sentence."),
            "This is a normal sentence."
        );
    }

    #[test]
    fn scrub_whole_line_thanks_for_watching_dropped() {
        assert_eq!(
            scrub_hallucinations("Hello there.\nThanks for watching."),
            "Hello there."
        );
    }

    #[test]
    fn scrub_trailing_hallucination_stripped() {
        assert_eq!(
            scrub_hallucinations("Some real content. Thank you."),
            "Some real content."
        );
    }

    #[test]
    fn scrub_mid_sentence_match_preserved() {
        // "Thank you" sitting mid-sentence (followed by more content) is not
        // a hallucination — leave it alone.
        let input = "I said thank you to the team and moved on.";
        assert_eq!(scrub_hallucinations(input), input);
    }

    #[test]
    fn scrub_music_token_line_dropped() {
        // Whole-line `[Music]` is removed entirely (no orphan blank line) so
        // the pasted text reads as continuous prose.
        assert_eq!(
            scrub_hallucinations("real content\n[Music]\nmore content"),
            "real content\nmore content"
        );
    }

    #[test]
    fn scrub_case_insensitive() {
        assert_eq!(
            scrub_hallucinations("THANKS FOR WATCHING."),
            ""
        );
    }

    #[test]
    fn scrub_only_hallucination_input() {
        assert_eq!(scrub_hallucinations("Thanks for watching!"), "");
    }
}
