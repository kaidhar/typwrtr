//! Phase 3 inline voice commands.
//!
//! Whisper output is plain text — we walk it word-by-word, recognise command
//! phrases, and rewrite the transcript before it lands in the user's app.
//!
//! Run order (per the plan): commands fire **after** the replacement table and
//! **before** any postprocess mode. The recorder enforces that.

#[derive(Debug, Clone, PartialEq)]
enum Action {
    /// Insert N newlines, trimming trailing whitespace from the buffer.
    Newlines(u8),
    /// Append punctuation directly after the last token (no leading space) —
    /// but only when the buffer doesn't already end with that mark.
    Punct(char),
    /// Drop the most recent sentence (back to the last `.!?` or to the start).
    ScratchPrevSentence,
    /// Uppercase the first character of the most recent alphabetic word.
    CapPrevWord,
    /// Toggle the all-caps state for subsequent words.
    SetAllCaps(bool),
    /// Switch on bullet-list mode: every newline from now gets a `- ` prefix.
    BulletList,
    /// Don't paste — leave the rendered text in the clipboard only.
    ClipboardInstead,
    /// Mark the transcript as code intent — Phase 4 postprocess will apply
    /// the per-app `code_case` transformation (snake/camel/kebab).
    CodeMode,
}

/// Static phrase → action table. Ordered longest-phrase-first so a shorter
/// command can't shadow a longer one.
const PHRASES: &[(&[&str], Action)] = &[
    (&["all", "caps", "on"], Action::SetAllCaps(true)),
    (&["all", "caps", "off"], Action::SetAllCaps(false)),
    (&["new", "paragraph"], Action::Newlines(2)),
    (&["new", "line"], Action::Newlines(1)),
    (&["question", "mark"], Action::Punct('?')),
    (&["exclamation", "point"], Action::Punct('!')),
    (&["exclamation", "mark"], Action::Punct('!')),
    (&["scratch", "that"], Action::ScratchPrevSentence),
    (&["delete", "that"], Action::ScratchPrevSentence),
    (&["cap", "that"], Action::CapPrevWord),
    (&["bullet", "list"], Action::BulletList),
    (&["clipboard", "instead"], Action::ClipboardInstead),
    (&["code", "mode"], Action::CodeMode),
    (&["newline"], Action::Newlines(1)),
    (&["period"], Action::Punct('.')),
    (&["comma"], Action::Punct(',')),
];

#[derive(Debug, Clone, Default, PartialEq)]
struct State {
    all_caps: bool,
    bullet_list: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct CommandResult {
    pub text: String,
    /// True if `clipboard instead` fired — caller should set the clipboard but
    /// skip the synthesised paste keystroke.
    pub clipboard_only: bool,
    /// True if `code mode` fired — Phase 4 postprocess will apply the per-app
    /// code-case transformation to the resulting text.
    pub code_mode: bool,
}

/// One row of the `snippets` table, in the shape `apply_voice_commands` needs.
/// Mirrors `db::SnippetRow` but stays decoupled so the commands module doesn't
/// need a DB dep.
#[derive(Debug, Clone)]
pub struct Snippet {
    pub trigger: String,
    pub expansion: String,
    pub is_dynamic: bool,
}

/// Walk `input` token-by-token, applying voice-command phrases. Non-command
/// words pass through (modulo the all-caps state). Returns the rewritten
/// transcript plus any side-effects.
pub fn apply_voice_commands(input: &str) -> CommandResult {
    apply_voice_commands_with_snippets(input, &[], &|_| None)
}

/// Like `apply_voice_commands` but also recognises snippet triggers and
/// expands them. `resolve_var` provides values for `{{date}}`, `{{time}}`,
/// `{{day}}`, `{{clipboard}}`, `{{selection}}` (or any other variable a
/// future snippet uses) — returning `None` leaves the literal `{{var}}`
/// in place so the user can see what didn't resolve.
pub fn apply_voice_commands_with_snippets(
    input: &str,
    snippets: &[Snippet],
    resolve_var: &dyn Fn(&str) -> Option<String>,
) -> CommandResult {
    let raw_tokens: Vec<&str> = input.split_whitespace().collect();
    let normalized: Vec<String> = raw_tokens
        .iter()
        .map(|t| {
            t.trim_matches(|c: char| c.is_ascii_punctuation())
                .to_lowercase()
        })
        .collect();

    let mut out = String::new();
    let mut state = State::default();
    let mut clipboard_only = false;
    let mut code_mode = false;

    // Pre-tokenise each snippet's trigger once — the inner loop runs per word.
    let snippet_tokens: Vec<(Vec<String>, &Snippet)> = snippets
        .iter()
        .map(|s| {
            let toks: Vec<String> = s
                .trigger
                .split_whitespace()
                .map(|t| t.to_lowercase())
                .collect();
            (toks, s)
        })
        .filter(|(toks, _)| !toks.is_empty())
        .collect();
    // Longest triggers first so a 3-word snippet beats a 2-word prefix.
    let mut snippet_sorted = snippet_tokens.clone();
    snippet_sorted.sort_by_key(|(toks, _)| std::cmp::Reverse(toks.len()));

    let mut i = 0;
    while i < normalized.len() {
        if let Some((phrase_len, action)) = match_phrase(&normalized, i) {
            apply_action(
                &action,
                &mut out,
                &mut state,
                &mut clipboard_only,
                &mut code_mode,
            );
            i += phrase_len;
            continue;
        }

        if let Some((phrase_len, snippet)) = match_snippet(&normalized, i, &snippet_sorted) {
            let expanded = if snippet.is_dynamic {
                expand_template(&snippet.expansion, resolve_var)
            } else {
                snippet.expansion.clone()
            };
            push_snippet(&mut out, &expanded);
            i += phrase_len;
            continue;
        }

        let word = if state.all_caps {
            raw_tokens[i].to_uppercase()
        } else {
            raw_tokens[i].to_string()
        };
        push_word(&mut out, &word);
        i += 1;
    }

    CommandResult {
        text: out.trim_end().to_string(),
        clipboard_only,
        code_mode,
    }
}

fn match_snippet<'a>(
    normalized: &[String],
    start: usize,
    snippet_sorted: &'a [(Vec<String>, &'a Snippet)],
) -> Option<(usize, &'a Snippet)> {
    for (toks, snippet) in snippet_sorted {
        if start + toks.len() > normalized.len() {
            continue;
        }
        let slice = &normalized[start..start + toks.len()];
        if slice.iter().zip(toks.iter()).all(|(a, b)| a == b) {
            return Some((toks.len(), *snippet));
        }
    }
    None
}

fn expand_template(template: &str, resolve: &dyn Fn(&str) -> Option<String>) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        if let Some(end) = after.find("}}") {
            let key = after[..end].trim();
            match resolve(key) {
                Some(v) => out.push_str(&v),
                None => {
                    // Unresolved — keep the literal so the user can see what failed.
                    out.push_str("{{");
                    out.push_str(&after[..end]);
                    out.push_str("}}");
                }
            }
            rest = &after[end + 2..];
        } else {
            // No closing braces — bail.
            out.push_str(&rest[start..]);
            return out;
        }
    }
    out.push_str(rest);
    out
}

/// Append a multi-line snippet expansion. Inserts a separating space at the
/// boundary unless the expansion starts with a newline or the buffer ends
/// with whitespace.
fn push_snippet(out: &mut String, expansion: &str) {
    if expansion.is_empty() {
        return;
    }
    let needs_space = !out.is_empty()
        && !out.ends_with(|c: char| c.is_whitespace())
        && !expansion.starts_with(|c: char| c.is_whitespace());
    if needs_space {
        out.push(' ');
    }
    out.push_str(expansion);
}

fn match_phrase(normalized: &[String], start: usize) -> Option<(usize, Action)> {
    for (phrase, action) in PHRASES {
        if start + phrase.len() > normalized.len() {
            continue;
        }
        let slice = &normalized[start..start + phrase.len()];
        if slice.iter().zip(phrase.iter()).all(|(a, b)| a == *b) {
            return Some((phrase.len(), action.clone()));
        }
    }
    None
}

fn apply_action(
    action: &Action,
    out: &mut String,
    state: &mut State,
    clipboard_only: &mut bool,
    code_mode: &mut bool,
) {
    match action {
        Action::Newlines(n) => {
            trim_trailing_inline_ws(out);
            for _ in 0..*n {
                out.push('\n');
            }
            if state.bullet_list {
                out.push_str("- ");
            }
        }
        Action::Punct(c) => {
            trim_trailing_inline_ws(out);
            // Don't double up punctuation — if the buffer already ends with a
            // sentence-ender, skip another period; same for comma after comma.
            let ends_with_same = out.ends_with(*c);
            let ends_with_sentence =
                matches!(out.chars().last(), Some('.') | Some('!') | Some('?'));
            let skip = ends_with_same || (*c == '.' && ends_with_sentence);
            if !skip {
                out.push(*c);
            }
        }
        Action::ScratchPrevSentence => {
            scratch_prev_sentence(out);
        }
        Action::CapPrevWord => {
            cap_prev_word(out);
        }
        Action::SetAllCaps(on) => {
            state.all_caps = *on;
        }
        Action::BulletList => {
            state.bullet_list = true;
            trim_trailing_inline_ws(out);
            if !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            }
            out.push_str("- ");
        }
        Action::ClipboardInstead => {
            *clipboard_only = true;
        }
        Action::CodeMode => {
            *code_mode = true;
        }
    }
}

/// Append `word` with a separating space when the buffer doesn't already end
/// with whitespace, a newline, or "- " (bullet prefix).
fn push_word(out: &mut String, word: &str) {
    if word.is_empty() {
        return;
    }
    let needs_space =
        !out.is_empty() && !out.ends_with(|c: char| c.is_whitespace()) && !out.ends_with('\n');
    if needs_space {
        out.push(' ');
    }
    out.push_str(word);
}

fn trim_trailing_inline_ws(out: &mut String) {
    while out.ends_with(' ') || out.ends_with('\t') {
        out.pop();
    }
}

fn scratch_prev_sentence(out: &mut String) {
    let mut keep_until = 0;
    let bytes = out.as_bytes();
    // Walk backward looking for the last sentence terminator. If none, drop
    // everything (the user is starting over).
    for (idx, b) in bytes.iter().enumerate().rev() {
        if matches!(*b, b'.' | b'!' | b'?') {
            keep_until = idx + 1;
            break;
        }
    }
    out.truncate(keep_until);
    // Eat any whitespace after the terminator we kept.
    while out.ends_with(' ') || out.ends_with('\t') {
        out.pop();
    }
}

fn cap_prev_word(out: &mut String) {
    // Find the last alphabetic run; uppercase its first character.
    let chars: Vec<(usize, char)> = out.char_indices().collect();
    let mut end_idx: Option<usize> = None;
    for (i, c) in chars.iter().rev() {
        if c.is_alphabetic() {
            end_idx = Some(*i);
            break;
        }
    }
    let Some(end_byte) = end_idx else { return };

    let mut start_byte = end_byte;
    for (i, c) in chars.iter().rev() {
        if *i > end_byte {
            continue;
        }
        if c.is_alphabetic() {
            start_byte = *i;
        } else {
            break;
        }
    }

    // Build new string with the first char of the word uppercased.
    let head = &out[..start_byte];
    let word = &out[start_byte..];
    let mut word_chars = word.chars();
    let Some(first) = word_chars.next() else {
        return;
    };
    let upper: String = first.to_uppercase().collect::<String>() + word_chars.as_str();
    let new_string = format!("{}{}", head, upper);
    *out = new_string;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(s: &str) -> String {
        apply_voice_commands(s).text
    }

    #[test]
    fn passthrough_when_no_commands() {
        assert_eq!(run("hello world"), "hello world");
    }

    #[test]
    fn comma_attaches_to_previous_word() {
        assert_eq!(run("hey team comma how are you"), "hey team, how are you");
    }

    #[test]
    fn period_doesnt_double_up_after_existing_period() {
        // Whisper sometimes already adds a period; explicit "period" is then a no-op.
        assert_eq!(run("done. period"), "done.");
    }

    #[test]
    fn new_line_inserts_newline() {
        assert_eq!(run("hello new line world"), "hello\nworld");
    }

    #[test]
    fn new_paragraph_inserts_two_newlines() {
        assert_eq!(run("intro new paragraph body"), "intro\n\nbody");
    }

    #[test]
    fn question_mark_and_exclamation() {
        assert_eq!(run("are you ok question mark"), "are you ok?");
        assert_eq!(run("watch out exclamation point"), "watch out!");
        assert_eq!(run("watch out exclamation mark"), "watch out!");
    }

    #[test]
    fn plan_acceptance_test() {
        // From the plan: "Hey team comma new line we shipped the new build period"
        // → "Hey team,\nwe shipped the new build."
        assert_eq!(
            run("Hey team comma new line we shipped the new build period"),
            "Hey team,\nwe shipped the new build."
        );
    }

    #[test]
    fn scratch_that_drops_previous_sentence() {
        assert_eq!(
            run("First sentence. Second sentence scratch that"),
            "First sentence."
        );
    }

    #[test]
    fn delete_that_alias_works() {
        assert_eq!(run("typo here delete that"), "");
    }

    #[test]
    fn cap_that_uppercases_previous_word() {
        assert_eq!(run("send to kaidhar cap that"), "send to Kaidhar");
    }

    #[test]
    fn all_caps_toggles_state() {
        assert_eq!(
            run("normal all caps on yelling now all caps off back to normal"),
            "normal YELLING NOW back to normal"
        );
    }

    #[test]
    fn bullet_list_prefixes_subsequent_lines() {
        assert_eq!(
            run("groceries bullet list eggs new line milk new line bread"),
            "groceries\n- eggs\n- milk\n- bread"
        );
    }

    #[test]
    fn clipboard_instead_sets_flag() {
        let r = apply_voice_commands("paste this text clipboard instead");
        assert_eq!(r.text, "paste this text");
        assert!(r.clipboard_only);
    }

    #[test]
    fn no_leading_space_after_punctuation() {
        assert_eq!(run("done period now"), "done. now");
    }

    #[test]
    fn code_mode_strips_activator_and_sets_flag() {
        let r = apply_voice_commands("code mode my new function");
        assert_eq!(r.text, "my new function");
        assert!(r.code_mode);
    }

    fn dummy_resolver(key: &str) -> Option<String> {
        match key {
            "date" => Some("2026-04-27".into()),
            "time" => Some("14:32".into()),
            "day" => Some("Monday".into()),
            _ => None,
        }
    }

    #[test]
    fn static_snippet_expansion_inline() {
        let snippets = vec![Snippet {
            trigger: "insert email signature".into(),
            expansion: "Best,\n[your name]".into(),
            is_dynamic: false,
        }];
        let r = apply_voice_commands_with_snippets(
            "thanks insert email signature",
            &snippets,
            &dummy_resolver,
        );
        assert_eq!(r.text, "thanks Best,\n[your name]");
    }

    #[test]
    fn dynamic_snippet_resolves_template_vars() {
        let snippets = vec![Snippet {
            trigger: "insert date".into(),
            expansion: "{{date}}".into(),
            is_dynamic: true,
        }];
        let r =
            apply_voice_commands_with_snippets("today is insert date", &snippets, &dummy_resolver);
        assert_eq!(r.text, "today is 2026-04-27");
    }

    #[test]
    fn dynamic_snippet_unresolved_var_left_literal() {
        let snippets = vec![Snippet {
            trigger: "insert mystery".into(),
            expansion: "{{nope}}".into(),
            is_dynamic: true,
        }];
        let r = apply_voice_commands_with_snippets("hi insert mystery", &snippets, &dummy_resolver);
        assert_eq!(r.text, "hi {{nope}}");
    }

    #[test]
    fn longer_trigger_wins_over_shorter_prefix() {
        let snippets = vec![
            Snippet {
                trigger: "insert".into(),
                expansion: "INSERT_ALONE".into(),
                is_dynamic: false,
            },
            Snippet {
                trigger: "insert date".into(),
                expansion: "{{date}}".into(),
                is_dynamic: true,
            },
        ];
        let r = apply_voice_commands_with_snippets("ok insert date", &snippets, &dummy_resolver);
        assert_eq!(r.text, "ok 2026-04-27");
    }

    #[test]
    fn snippet_doesnt_block_voice_command_in_same_input() {
        let snippets = vec![Snippet {
            trigger: "insert date".into(),
            expansion: "{{date}}".into(),
            is_dynamic: true,
        }];
        let r = apply_voice_commands_with_snippets(
            "today is insert date period",
            &snippets,
            &dummy_resolver,
        );
        assert_eq!(r.text, "today is 2026-04-27.");
    }
}
