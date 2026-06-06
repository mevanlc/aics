use std::borrow::Cow;

/// Strip terminal escape/control sequences from transcript text so it is safe to
/// hand to ratatui, which assumes printable, fixed-width cell content. Recorded
/// session output can contain ANSI SGR/CSI, OSC, and other control bytes (and
/// tabs); emitting those raw desyncs ratatui's cell map from the terminal and
/// corrupts rendering. Tabs are expanded to spaces; newlines are preserved.
pub(crate) fn strip_terminal_escapes(text: &str) -> String {
    strip_escape_sequences(&expand_tabs(text))
}

fn expand_tabs(text: &str) -> Cow<'_, str> {
    if text.contains('\t') {
        Cow::Owned(text.replace('\t', "    "))
    } else {
        Cow::Borrowed(text)
    }
}

fn strip_escape_sequences(text: &str) -> String {
    let mut visible = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\x1b' => match chars.peek().copied() {
                // CSI: ESC [ ... <final byte in 0x40..=0x7E>
                Some('[') => {
                    chars.next();
                    for c in chars.by_ref() {
                        if c.is_ascii_alphabetic() {
                            break;
                        }
                    }
                }
                // OSC: ESC ] ... terminated by BEL or ST (ESC \)
                Some(']') => {
                    chars.next();
                    while let Some(c) = chars.next() {
                        if c == '\x07' {
                            break;
                        }
                        if c == '\x1b' {
                            // Consume the trailing '\' of an ST terminator.
                            if chars.peek() == Some(&'\\') {
                                chars.next();
                            }
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
    fn strips_osc_terminated_by_st() {
        assert_eq!(strip_terminal_escapes("a\x1b]0;title\x1b\\b"), "ab");
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
