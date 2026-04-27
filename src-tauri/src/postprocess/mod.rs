//! Phase 4 postprocess modes — runs after Phase 3 voice commands and before
//! the optional LLM cleanup pass. The recorder selects the mode from the
//! current app's profile (`app_profiles.postprocess_mode`).
//!
//! Modes:
//! * `default` — pass-through (basic capitalization + sentence-end punctuation
//!   already happened in `cleanup_text`).
//! * `plain` — strip Markdown markup so the pasted text reads as prose.
//! * `markdown` — preserve the bullet/numbered list markers Phase 3 emitted.
//! * `code` — only fires when the spoken transcript started with `code mode`;
//!   transforms the text into a single identifier in the per-app code-case.

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Mode {
    Default,
    Plain,
    Markdown,
    Code,
}

impl Mode {
    pub fn from_str(s: &str) -> Self {
        match s {
            "plain" => Mode::Plain,
            "markdown" => Mode::Markdown,
            "code" => Mode::Code,
            _ => Mode::Default,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CodeCase {
    Snake,
    Camel,
    Kebab,
}

impl CodeCase {
    pub fn from_str(s: &str) -> Self {
        match s {
            "camel" => CodeCase::Camel,
            "kebab" => CodeCase::Kebab,
            _ => CodeCase::Snake,
        }
    }
}

/// Apply the postprocess pass.
///
/// `code_mode_active` comes from Phase 3's `CommandResult.code_mode` — only
/// when the user actually said the activator do we run the code transform.
/// In any other case the `Code` mode falls through to `Default`.
pub fn apply(text: &str, mode: Mode, code_mode_active: bool, code_case: CodeCase) -> String {
    match mode {
        Mode::Default => text.to_string(),
        Mode::Markdown => text.to_string(),
        Mode::Plain => strip_markdown(text),
        Mode::Code => {
            if code_mode_active {
                to_code_case(text, code_case)
            } else {
                text.to_string()
            }
        }
    }
}

/// Strip the markup users would commonly want gone when pasting into a
/// chat/email/Slack-style app. Conservative — only removes the markers, never
/// reflows whitespace.
fn strip_markdown(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for line in text.lines() {
        let cleaned = strip_markdown_line(line);
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&cleaned);
    }
    out
}

fn strip_markdown_line(line: &str) -> String {
    let mut s = line.to_string();

    // Heading prefix: leading `#`, `##`, etc.
    s = s
        .trim_start_matches(|c: char| c == '#' || c == ' ')
        .to_string();

    // List markers: `- `, `* `, or `1. `, `2. ` etc.
    if let Some(rest) = s.strip_prefix("- ").or_else(|| s.strip_prefix("* ")) {
        s = rest.to_string();
    } else {
        // Numbered lists.
        let chars: Vec<char> = s.chars().collect();
        let mut i = 0;
        while i < chars.len() && chars[i].is_ascii_digit() {
            i += 1;
        }
        if i > 0 && i + 1 < chars.len() && chars[i] == '.' && chars[i + 1] == ' ' {
            s = chars[i + 2..].iter().collect();
        }
    }

    // Blockquote `> `
    if let Some(rest) = s.strip_prefix("> ") {
        s = rest.to_string();
    }

    // Inline emphasis: **bold**, __bold__, *italic*, _italic_, `code`.
    // Do these in passes; double markers first so they don't get half-stripped.
    s = strip_paired(&s, "**");
    s = strip_paired(&s, "__");
    s = strip_paired(&s, "*");
    s = strip_paired(&s, "_");
    s = strip_paired(&s, "`");

    // Links: `[text](url)` → `text`.
    s = strip_links(&s);

    s
}

/// Remove all balanced occurrences of `marker` ... `marker`, keeping the inner
/// text. Unbalanced trailing marker is left in place to avoid eating user
/// content.
fn strip_paired(s: &str, marker: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find(marker) {
        out.push_str(&rest[..start]);
        let after = &rest[start + marker.len()..];
        if let Some(end) = after.find(marker) {
            out.push_str(&after[..end]);
            rest = &after[end + marker.len()..];
        } else {
            // No closing marker — bail and emit the rest verbatim.
            out.push_str(&rest[start..]);
            return out;
        }
    }
    out.push_str(rest);
    out
}

fn strip_links(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'[' {
            if let Some(close_text) = s[i..].find("](") {
                let text_end = i + close_text;
                if let Some(close_paren_rel) = s[text_end + 2..].find(')') {
                    let close_paren = text_end + 2 + close_paren_rel;
                    out.push_str(&s[i + 1..text_end]);
                    i = close_paren + 1;
                    continue;
                }
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

fn to_code_case(text: &str, case: CodeCase) -> String {
    let words: Vec<String> = text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| w.to_lowercase())
        .collect();

    if words.is_empty() {
        return String::new();
    }

    match case {
        CodeCase::Snake => words.join("_"),
        CodeCase::Kebab => words.join("-"),
        CodeCase::Camel => {
            let mut out = String::new();
            for (i, w) in words.iter().enumerate() {
                if i == 0 {
                    out.push_str(w);
                } else {
                    let mut chars = w.chars();
                    if let Some(first) = chars.next() {
                        out.extend(first.to_uppercase());
                        out.push_str(chars.as_str());
                    }
                }
            }
            out
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_passthrough() {
        assert_eq!(
            apply("Hello world.", Mode::Default, false, CodeCase::Snake),
            "Hello world."
        );
    }

    #[test]
    fn markdown_preserves_bullets() {
        let s = "groceries\n- eggs\n- milk";
        assert_eq!(apply(s, Mode::Markdown, false, CodeCase::Snake), s);
    }

    #[test]
    fn plain_strips_bold_italic_code() {
        assert_eq!(
            apply(
                "**bold** and *italic* and `code`",
                Mode::Plain,
                false,
                CodeCase::Snake
            ),
            "bold and italic and code"
        );
    }

    #[test]
    fn plain_strips_list_markers_and_headings() {
        assert_eq!(
            apply(
                "# Heading\n- one\n- two\n1. first",
                Mode::Plain,
                false,
                CodeCase::Snake
            ),
            "Heading\none\ntwo\nfirst"
        );
    }

    #[test]
    fn plain_strips_links() {
        assert_eq!(
            apply(
                "see [docs](https://example.com) here",
                Mode::Plain,
                false,
                CodeCase::Snake
            ),
            "see docs here"
        );
    }

    #[test]
    fn code_mode_inactive_keeps_text() {
        assert_eq!(
            apply("My New Function", Mode::Code, false, CodeCase::Snake),
            "My New Function"
        );
    }

    #[test]
    fn code_mode_snake() {
        assert_eq!(
            apply("My New Function", Mode::Code, true, CodeCase::Snake),
            "my_new_function"
        );
    }

    #[test]
    fn code_mode_camel() {
        assert_eq!(
            apply("my new function", Mode::Code, true, CodeCase::Camel),
            "myNewFunction"
        );
    }

    #[test]
    fn code_mode_kebab() {
        assert_eq!(
            apply("My New Function", Mode::Code, true, CodeCase::Kebab),
            "my-new-function"
        );
    }

    #[test]
    fn code_mode_strips_punctuation() {
        assert_eq!(
            apply("get-the-thing!", Mode::Code, true, CodeCase::Snake),
            "get_the_thing"
        );
    }
}
