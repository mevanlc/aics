pub(crate) use crate::parse::json_sources::TextOrigin;

#[derive(Debug, Clone)]
pub(crate) struct SanitizedText {
    pub text: String,
    pub origins: Vec<TextOrigin>,
}

/// Strip terminal escape/control sequences from transcript text so it is safe to
/// hand to ratatui, which assumes printable, fixed-width cell content. Recorded
/// session output can contain ANSI SGR/CSI, OSC, and other control bytes (and
/// tabs); emitting those raw desyncs ratatui's cell map from the terminal and
/// corrupts rendering. Tabs are expanded to spaces; newlines are preserved.
pub(crate) fn strip_terminal_escapes(text: &str) -> String {
    sanitize_with_origins(text).text
}

/// Sanitization records the exact input byte range that produced each output
/// run. Removed control sequences have no output range; a tab owns all four
/// spaces produced from it.
pub(crate) fn sanitize_with_origins(text: &str) -> SanitizedText {
    let mut visible = String::with_capacity(text.len());
    let mut origins = Vec::new();
    let mut chars = text.char_indices().peekable();
    while let Some((start, ch)) = chars.next() {
        let output_start = visible.len();
        match ch {
            '\x1b' => match chars.peek().map(|(_, ch)| *ch) {
                // CSI: ESC [ ... <final byte in 0x40..=0x7E>
                Some('[') => {
                    chars.next();
                    for (_, c) in chars.by_ref() {
                        if ('\x40'..='\x7e').contains(&c) {
                            break;
                        }
                    }
                }
                // OSC: ESC ] ... terminated by BEL or ST (ESC \)
                Some(']') => {
                    chars.next();
                    while let Some((_, c)) = chars.next() {
                        if c == '\x07' {
                            break;
                        }
                        if c == '\x1b' {
                            // Consume the trailing '\' of an ST terminator.
                            if chars.peek().is_some_and(|(_, ch)| *ch == '\\') {
                                chars.next();
                            }
                            break;
                        }
                    }
                }
                _ => {}
            },
            '\n' => visible.push('\n'),
            '\t' => visible.push_str("    "),
            ch if ch.is_control() => {}
            ch => visible.push(ch),
        }
        if visible.len() > output_start {
            let origin = TextOrigin {
                rendered: output_start..visible.len(),
                source: start..start + ch.len_utf8(),
            };
            push_origin(&mut origins, origin);
        }
    }
    SanitizedText {
        text: visible,
        origins,
    }
}

pub(crate) fn push_origin(origins: &mut Vec<TextOrigin>, origin: TextOrigin) {
    if let Some(last) = origins.last_mut() {
        if last.source.end == origin.source.start
            && last.rendered.end == origin.rendered.start
            && last.source.len() == last.rendered.len()
            && origin.source.len() == origin.rendered.len()
        {
            last.source.end = origin.source.end;
            last.rendered.end = origin.rendered.end;
            return;
        }
    }
    origins.push(origin);
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

    #[test]
    fn provenance_omits_escape_bytes_and_maps_expanded_tabs() {
        let source = "a\x1b[31m\t界\x1b[0m";
        let safe = sanitize_with_origins(source);
        assert_eq!(safe.text, "a    界");
        assert!(safe
            .origins
            .iter()
            .all(|origin| !source[origin.source.clone()].contains('\x1b')));
        let tab = safe
            .origins
            .iter()
            .find(|origin| &source[origin.source.clone()] == "\t")
            .unwrap();
        assert_eq!(&safe.text[tab.rendered.clone()], "    ");
    }
}
