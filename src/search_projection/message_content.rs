//! Visibility within message bodies, preserving the original source bytes.

use std::ops::Range;

use pulldown_cmark::{Event, Parser, Tag, TagEnd};

use super::{message_visibility, VisibilityRequirement, INTERNAL_CONTEXT};
use crate::parse::{AntigravityMessageKind, MessageRole};
use crate::settings::DisplayOptions;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageContentSpan {
    pub range: Range<usize>,
    pub visibility: VisibilityRequirement,
}

pub fn message_content_spans(
    role: MessageRole,
    content: &str,
    antigravity_kind: Option<AntigravityMessageKind>,
) -> Vec<MessageContentSpan> {
    let visibility = message_visibility(role, content, antigravity_kind);
    if role == MessageRole::Assistant {
        if let Some(footer) = memory_citation_footer_range(content) {
            let mut spans = Vec::with_capacity(2);
            if footer.start > 0 {
                spans.push(MessageContentSpan {
                    range: 0..footer.start,
                    visibility,
                });
            }
            spans.push(MessageContentSpan {
                range: footer,
                visibility: VisibilityRequirement(visibility.0 | INTERNAL_CONTEXT),
            });
            return spans;
        }
    }
    vec![MessageContentSpan {
        range: 0..content.len(),
        visibility,
    }]
}

/// Retained content is always a prefix: citations inherit the reply's visibility
/// and can additionally be hidden by Internal Context. Its offsets remain valid
/// in the original message, including when trailing whitespace is removed.
pub fn visible_message_content(
    role: MessageRole,
    content: &str,
    antigravity_kind: Option<AntigravityMessageKind>,
    options: DisplayOptions,
) -> Option<&str> {
    if !message_visibility(role, content, antigravity_kind).is_visible(options) {
        return None;
    }
    let retained = if role == MessageRole::Assistant && options.hide_internal_context {
        match memory_citation_footer_range(content) {
            Some(footer) => content[..footer.start].trim_end(),
            None => content,
        }
    } else {
        content
    };
    (!retained.trim().is_empty()).then_some(retained)
}

const CITATION_OPEN: &str = "<oai-mem-citation>";
const CITATION_CLOSE: &str = "</oai-mem-citation>";

fn memory_citation_footer_range(content: &str) -> Option<Range<usize>> {
    let mut end = content.trim_end().len();
    if !content[..end].ends_with(CITATION_CLOSE) {
        return None;
    }
    // Collect complete footers from the end backwards. Earlier mentions do not
    // prevent a genuine footer from being recognized, and remain ordinary text.
    let mut footers = Vec::new();
    while content[..end].ends_with(CITATION_CLOSE) {
        let Some(start) = content[..end]
            .rmatch_indices(CITATION_OPEN)
            .map(|(start, _)| start)
            .find(|&start| {
                let line_start = content[..start].rfind('\n').map_or(0, |index| index + 1);
                content[line_start..start]
                    .chars()
                    .all(|ch| matches!(ch, ' ' | '\t' | '\r'))
                    && complete_citation(&content[start..end])
            })
        else {
            break;
        };
        footers.push(start..end);
        end = content[..start].trim_end().len();
    }
    if footers.is_empty() {
        return None;
    }

    // A source tag inside Markdown code or a blockquote is an example, not a
    // generated footer. Inspect offsets only after the cheap suffix checks.
    let mut protected = vec![false; footers.len()];
    let mut depth = 0usize;
    for (event, range) in Parser::new(content).into_offset_iter() {
        if matches!(event, Event::Start(Tag::CodeBlock(_) | Tag::BlockQuote(_))) {
            depth += 1;
        }
        if depth > 0 || matches!(event, Event::Code(_)) {
            for (footer, protected) in footers.iter().zip(&mut protected) {
                if range.start < footer.start + CITATION_OPEN.len() && range.end > footer.start {
                    *protected = true;
                }
            }
        }
        if matches!(event, Event::End(TagEnd::CodeBlock | TagEnd::BlockQuote(_))) {
            depth = depth.saturating_sub(1);
        }
    }
    let start = footers
        .iter()
        .zip(protected)
        .take_while(|(_, protected)| !protected)
        .last()?
        .0
        .start;
    Some(start..content.len())
}

fn complete_citation(content: &str) -> bool {
    let Some(body) = content
        .strip_prefix(CITATION_OPEN)
        .and_then(|body| body.strip_suffix(CITATION_CLOSE))
    else {
        return false;
    };
    let mut remaining = body.trim();
    for (open, close) in [
        ("<citation_entries>", "</citation_entries>"),
        ("<rollout_ids>", "</rollout_ids>"),
    ] {
        let Some(body) = remaining.strip_prefix(open) else {
            return false;
        };
        let Some((_, rest)) = body.split_once(close) else {
            return false;
        };
        remaining = rest.trim();
    }
    remaining.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    const CITATION: &str = "<oai-mem-citation>\n<citation_entries>\nMEMORY.md:4322-4322|note=[diagnostic provenance]\n</citation_entries>\n<rollout_ids>\n01a0f24a-3c44-7c92-beff-172a47d6f10e\n</rollout_ids>\n</oai-mem-citation>";

    #[test]
    fn complete_memory_footers_have_separate_visibility_and_original_offsets() {
        for body in [
            format!("Reply café 🦀.\n\n{CITATION}\n"),
            format!("Earlier mention <oai-mem-citation> stays.\n\n{CITATION}"),
            format!("Reply.\r\n\r\n{}", CITATION.replace('\n', "\r\n")),
            format!("Reply.\n\n{CITATION}\n\n{CITATION}\t\n"),
            format!(
                "Reply.\n\n{}",
                CITATION.replace("diagnostic provenance", "<opaque>")
            ),
        ] {
            let spans = message_content_spans(MessageRole::Assistant, &body, None);
            assert_eq!(spans.len(), 2, "{body}");
            let reply = &spans[0];
            let footer = &spans[1];
            assert_eq!(reply.range.start, 0);
            assert_eq!(reply.range.end, footer.range.start);
            assert_eq!(footer.range.end, body.len());
            assert_eq!(
                reply.visibility,
                message_visibility(MessageRole::Assistant, &body, None)
            );
            assert!(reply.visibility.is_visible(DisplayOptions::default()));
            assert!(!footer.visibility.is_visible(DisplayOptions::default()));
            assert!(footer.visibility.is_visible(DisplayOptions::SHOW_ALL));
            assert!(!footer.visibility.is_visible(DisplayOptions {
                hide_agent_replies: true,
                ..DisplayOptions::SHOW_ALL
            }));
            assert_eq!(
                visible_message_content(
                    MessageRole::Assistant,
                    &body,
                    None,
                    DisplayOptions::default()
                ),
                Some(body[..footer.range.start].trim_end())
            );
            assert_eq!(
                visible_message_content(
                    MessageRole::Assistant,
                    &body,
                    None,
                    DisplayOptions::SHOW_ALL
                ),
                Some(body.as_str())
            );
            if body.matches(CITATION).count() == 2 {
                assert_eq!(footer.range.start, body.find(CITATION).unwrap());
            }
        }
    }

    #[test]
    fn quoted_malformed_and_nonfooter_memory_tags_remain_visible() {
        let indented = CITATION
            .lines()
            .map(|line| format!("    {line}\n"))
            .collect::<String>();
        let quoted = CITATION
            .lines()
            .map(|line| format!("> {line}\n"))
            .collect::<String>();
        for body in [
            format!("Inline mention {CITATION}"),
            format!("{CITATION}\n\nMore reply text."),
            format!("~~~xml\n{CITATION}\n~~~"),
            format!("~~~xml\n{CITATION}"),
            indented,
            quoted,
            CITATION.replace("</oai-mem-citation>", ""),
            CITATION.replace("</citation_entries>", "</wrong>"),
            CITATION.replace("<oai-mem-citation>", "<oai-mem-citation-example>"),
            CITATION.replace("<rollout_ids>", "Unexpected prose.\n<rollout_ids>"),
            CITATION.replace("<rollout_ids>", "<unknown>"),
            CITATION.replace("</rollout_ids>", "</wrong>"),
            "<oai-mem-citation>\nprose\n</oai-mem-citation>".to_owned(),
        ] {
            assert_eq!(
                visible_message_content(
                    MessageRole::Assistant,
                    &body,
                    None,
                    DisplayOptions::default()
                ),
                Some(body.as_str()),
                "{body}"
            );
            assert_eq!(
                message_content_spans(MessageRole::Assistant, &body, None).len(),
                1,
                "{body}"
            );
        }
        for role in [
            MessageRole::User,
            MessageRole::System,
            MessageRole::Summary,
            MessageRole::ToolCall,
            MessageRole::ToolResult,
        ] {
            assert_eq!(
                message_content_spans(role, CITATION, None).len(),
                1,
                "{role:?}"
            );
            assert_eq!(
                visible_message_content(role, CITATION, None, DisplayOptions::default()),
                Some(CITATION),
                "{role:?}"
            );
        }
    }

    #[test]
    fn citation_only_messages_leave_no_empty_body_when_hidden() {
        for content in [CITATION.to_owned(), format!("\n  {CITATION}\n\n")] {
            assert_eq!(
                visible_message_content(
                    MessageRole::Assistant,
                    &content,
                    None,
                    DisplayOptions::default()
                ),
                None
            );
            assert_eq!(
                visible_message_content(
                    MessageRole::Assistant,
                    &content,
                    None,
                    DisplayOptions::SHOW_ALL
                ),
                Some(content.as_str())
            );
        }
    }
}
