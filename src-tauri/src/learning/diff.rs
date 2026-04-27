//! Word-level diff that yields structured `(wrong, right, context)` triples
//! ready to feed `corrections` and `vocabulary`. Pure — no I/O, no SQL.
//!
//! Tokens are space-separated runs from a stripped-of-punctuation source.
//! That keeps pairing stable across "report." vs "report" and avoids letting
//! a single comma blow up the diff into a flurry of micro-changes.

use similar::{ChangeTag, TextDiff};

/// One structured pair extracted from the diff. `wrong` is the contiguous run
/// of tokens whisper produced; `right` is what the user wrote in their place.
/// `context` is up to `context_words` tokens on each side from `final_text`,
/// joined by spaces, to disambiguate homographs at lookup time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CorrectionPair {
    pub wrong: String,
    pub right: String,
    pub context: String,
}

impl CorrectionPair {
    /// True if this pair should be promoted into the per-app vocabulary table.
    /// Heuristics from the plan: single token on the right, length ≥ 3, not a
    /// stopword, and either mixed case or all-caps (i.e. proper-noun-shaped).
    pub fn is_proper_noun_candidate(&self) -> bool {
        let term = self.right.trim();
        if term.is_empty() || term.contains(' ') {
            return false;
        }
        let chars = term.chars().count();
        if chars < 3 {
            return false;
        }
        if STOPWORDS.iter().any(|&w| w.eq_ignore_ascii_case(term)) {
            return false;
        }
        let has_upper = term.chars().any(|c| c.is_ascii_uppercase());
        let has_lower = term.chars().any(|c| c.is_ascii_lowercase());
        let all_caps = has_upper && !has_lower;
        let mixed_case = has_upper && has_lower;
        all_caps || mixed_case
    }
}

/// Tokenize on whitespace, stripping leading/trailing ASCII punctuation. Empty
/// tokens (from stray punctuation) are dropped.
fn tokenize(s: &str) -> Vec<String> {
    s.split_whitespace()
        .map(|t| t.trim_matches(|c: char| c.is_ascii_punctuation()))
        .filter(|t| !t.is_empty())
        .map(|t| t.to_string())
        .collect()
}

/// Compute correction pairs from a (cleaned, final) pair.
///
/// `context_words` controls how many tokens of left/right context (from
/// `final_text`) get attached to each pair — the plan's default is 4.
pub fn pairs_from_diff(
    cleaned: &str,
    final_text: &str,
    context_words: usize,
) -> Vec<CorrectionPair> {
    let cleaned_tokens = tokenize(cleaned);
    let final_tokens = tokenize(final_text);

    if cleaned_tokens.is_empty() && final_tokens.is_empty() {
        return Vec::new();
    }
    if cleaned_tokens == final_tokens {
        return Vec::new();
    }

    // similar's TextDiff::from_slices needs &[T] of comparable items; &str works.
    let cleaned_refs: Vec<&str> = cleaned_tokens.iter().map(String::as_str).collect();
    let final_refs: Vec<&str> = final_tokens.iter().map(String::as_str).collect();
    let diff = TextDiff::from_slices(&cleaned_refs, &final_refs);

    // Walk the changes and group runs of consecutive Delete/Insert into a single
    // (wrong, right) pair. We track the index in `final_tokens` so we can pull
    // context off it for each emitted pair.
    let mut pairs: Vec<CorrectionPair> = Vec::new();
    let mut wrong_buf: Vec<&str> = Vec::new();
    let mut right_buf: Vec<&str> = Vec::new();
    let mut final_idx: usize = 0;
    let mut pair_anchor_final_idx: usize = 0;

    let flush = |pairs: &mut Vec<CorrectionPair>,
                 wrong_buf: &mut Vec<&str>,
                 right_buf: &mut Vec<&str>,
                 anchor: usize,
                 final_tokens: &[String],
                 context_words: usize| {
        if wrong_buf.is_empty() && right_buf.is_empty() {
            return;
        }
        let wrong = wrong_buf.join(" ");
        let right = right_buf.join(" ");
        // Skip case-only deltas; they belong in postprocess, not corrections.
        let case_only =
            !wrong.is_empty() && !right.is_empty() && wrong.to_lowercase() == right.to_lowercase();
        if !case_only && (!wrong.is_empty() || !right.is_empty()) {
            let lo = anchor.saturating_sub(context_words);
            let hi = (anchor + right_buf.len() + context_words).min(final_tokens.len());
            let context = final_tokens[lo..hi].join(" ");
            pairs.push(CorrectionPair {
                wrong,
                right,
                context,
            });
        }
        wrong_buf.clear();
        right_buf.clear();
    };

    for change in diff.iter_all_changes() {
        match change.tag() {
            ChangeTag::Equal => {
                // Buffer flush at every equality — runs of Delete/Insert separated
                // by an unchanged token produce distinct pairs.
                flush(
                    &mut pairs,
                    &mut wrong_buf,
                    &mut right_buf,
                    pair_anchor_final_idx,
                    &final_tokens,
                    context_words,
                );
                final_idx += 1;
                pair_anchor_final_idx = final_idx;
            }
            ChangeTag::Delete => {
                if wrong_buf.is_empty() && right_buf.is_empty() {
                    pair_anchor_final_idx = final_idx;
                }
                wrong_buf.push(change.value());
            }
            ChangeTag::Insert => {
                if wrong_buf.is_empty() && right_buf.is_empty() {
                    pair_anchor_final_idx = final_idx;
                }
                right_buf.push(change.value());
                final_idx += 1;
            }
        }
    }
    flush(
        &mut pairs,
        &mut wrong_buf,
        &mut right_buf,
        pair_anchor_final_idx,
        &final_tokens,
        context_words,
    );

    pairs
}

/// A small English stopword set borrowed from the standard NLTK list, trimmed.
/// Used by `is_proper_noun_candidate` to avoid promoting "the" / "and" / etc.
/// Also reused by `recorder` to filter context tokens before fuzzy-matching.
pub const STOPWORDS: &[&str] = &[
    "the", "a", "an", "and", "or", "but", "if", "of", "at", "by", "for", "with", "about", "to",
    "from", "in", "on", "is", "are", "was", "were", "be", "been", "being", "have", "has", "had",
    "do", "does", "did", "will", "would", "shall", "should", "can", "could", "may", "might",
    "must", "this", "that", "these", "those", "i", "you", "he", "she", "it", "we", "they", "them",
    "us", "him", "her", "his", "hers", "their", "our", "your", "my", "me", "as", "so", "than",
    "then", "too", "very", "yes", "no", "not",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_change_yields_no_pairs() {
        assert!(pairs_from_diff("hello world", "hello world", 4).is_empty());
        assert!(pairs_from_diff("Hello world.", "Hello world.", 4).is_empty());
    }

    #[test]
    fn case_only_delta_is_skipped() {
        // Punctuation stripped, then case-only — no correction.
        let pairs = pairs_from_diff("hello world.", "Hello World.", 4);
        assert!(pairs.is_empty(), "got pairs: {:?}", pairs);
    }

    #[test]
    fn single_token_substitution_is_captured_with_context() {
        let pairs = pairs_from_diff(
            "send the report to Kaidhar tomorrow",
            "send the report to KD tomorrow",
            4,
        );
        assert_eq!(pairs.len(), 1);
        let p = &pairs[0];
        assert_eq!(p.wrong, "Kaidhar");
        assert_eq!(p.right, "KD");
        assert!(p.context.contains("KD"));
        assert!(p.context.contains("report"));
    }

    #[test]
    fn adjacent_replacements_group_into_one_pair() {
        let pairs = pairs_from_diff("the rust SQL light handle", "the rusqlite handle", 4);
        // "rust SQL light" -> "rusqlite" should be a single pair, not three.
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].wrong, "rust SQL light");
        assert_eq!(pairs[0].right, "rusqlite");
    }

    #[test]
    fn pure_insertion_emits_pair_with_empty_wrong() {
        let pairs = pairs_from_diff("hello world", "hello new world", 4);
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].wrong, "");
        assert_eq!(pairs[0].right, "new");
    }

    #[test]
    fn pure_deletion_emits_pair_with_empty_right() {
        let pairs = pairs_from_diff("hello strange world", "hello world", 4);
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].wrong, "strange");
        assert_eq!(pairs[0].right, "");
    }

    #[test]
    fn proper_noun_candidate_heuristic() {
        let p = |w: &str, r: &str| CorrectionPair {
            wrong: w.into(),
            right: r.into(),
            context: String::new(),
        };
        assert!(p("foo", "Kaidhar").is_proper_noun_candidate()); // mixed case, len 7
        assert!(p("foo", "API").is_proper_noun_candidate()); // all caps, len 3
        assert!(p("foo", "TypeScript").is_proper_noun_candidate()); // mixed case
                                                                    // Per the plan, lowercase-only terms aren't promoted via the proper-noun
                                                                    // path — they'd come in via app prompt_template if the user wants them.
        assert!(!p("foo", "rusqlite").is_proper_noun_candidate());
        assert!(!p("foo", "KD").is_proper_noun_candidate()); // mixed but only len 2 — plan: ≥3
        assert!(!p("foo", "the").is_proper_noun_candidate()); // stopword
        assert!(!p("foo", "ab").is_proper_noun_candidate()); // too short
        assert!(!p("foo", "regular").is_proper_noun_candidate()); // no caps
        assert!(!p("foo", "two words").is_proper_noun_candidate()); // multi-token
    }
}
