use std::borrow::Cow;

use ansi_to_tui::IntoText;

pub(crate) fn strip_terminal_escapes(text: &str) -> String {
    let expanded = expand_tabs(text);
    match expanded.as_bytes().into_text() {
        Ok(parsed) => strip_remaining_controls(&flatten_ansi_text(parsed)),
        Err(_) => strip_escape_sequences(&expanded),
    }
}

fn expand_tabs(text: &str) -> Cow<'_, str> {
    if text.contains('\t') {
        Cow::Owned(text.replace('\t', "    "))
    } else {
        Cow::Borrowed(text)
    }
}

fn flatten_ansi_text(text: impl IntoAnsiTextParts) -> String {
    text.into_plain_text()
}

trait IntoAnsiTextParts {
    fn into_plain_text(self) -> String;
}

impl IntoAnsiTextParts for ratatui::text::Text<'static> {
    fn into_plain_text(self) -> String {
        let mut plain = String::new();
        for (line_index, line) in self.lines.into_iter().enumerate() {
            if line_index > 0 {
                plain.push('\n');
            }
            for span in line.spans {
                plain.push_str(span.content.as_ref());
            }
        }
        plain
    }
}

fn strip_remaining_controls(text: &str) -> String {
    text.chars()
        .filter(|ch| *ch == '\n' || !ch.is_control())
        .collect()
}

fn strip_escape_sequences(text: &str) -> String {
    let mut visible = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\x1b' => match chars.peek().copied() {
                Some('[') => {
                    chars.next();
                    for c in chars.by_ref() {
                        if c.is_ascii_alphabetic() {
                            break;
                        }
                    }
                }
                Some(']') => {
                    chars.next();
                    for c in chars.by_ref() {
                        if c == '\x07' {
                            break;
                        }
                    }
                }
                _ => {}
            },
            '\n' => visible.push('\n'),
            ch if ch.is_control() => {}
            ch => visible.push(ch),
        }
    }
    visible
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_sgr_escape_sequences() {
        assert_eq!(strip_terminal_escapes("\x1b[31mRED\x1b[0m"), "RED");
    }

    #[test]
    fn strips_osc_escape_sequences() {
        assert_eq!(strip_terminal_escapes("a\x1b]52;c;SGk=\x07b"), "ab");
    }

    #[test]
    fn strips_remaining_control_characters() {
        assert_eq!(strip_terminal_escapes("a\x07b\x7fc"), "abc");
    }

    #[test]
    fn expands_tabs_like_codex() {
        assert_eq!(strip_terminal_escapes("a\tb"), "a    b");
    }
}
