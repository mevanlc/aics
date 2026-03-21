use chrono::{Local, TimeZone, Utc};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use unicode_truncate::UnicodeTruncateStr;

use crate::index::SearchHit;
use crate::parse::{Agent, MessageRole};
use crate::tui::theme::Theme;

pub fn agent_badge(agent: Agent, theme: &Theme) -> (&'static str, Color) {
    match agent {
        Agent::Claude => ("C", theme.claude),
        Agent::Codex => ("X", theme.codex),
    }
}

pub fn parse_highlighted_html(html: &str, base: Style, highlight: Style) -> Line<'static> {
    let mut spans = Vec::new();
    let mut remaining = html;
    let mut current = base;

    while !remaining.is_empty() {
        if let Some(rest) = remaining.strip_prefix("<b>") {
            current = highlight;
            remaining = rest;
            continue;
        }

        if let Some(rest) = remaining.strip_prefix("</b>") {
            current = base;
            remaining = rest;
            continue;
        }

        let next_tag = remaining.find('<').unwrap_or(remaining.len());
        if next_tag == 0 {
            let mut chars = remaining.chars();
            if let Some(ch) = chars.next() {
                spans.push(Span::styled(ch.to_string(), current));
                remaining = chars.as_str();
                continue;
            }
        } else {
            let chunk = &remaining[..next_tag];
            spans.push(Span::styled(unescape_html(chunk), current));
            remaining = &remaining[next_tag..];
        }
    }

    Line::from(spans)
}

pub fn list_title(hit: &SearchHit) -> String {
    hit.session
        .custom_title
        .clone()
        .unwrap_or_else(|| hit.session.project.clone())
}

pub fn list_meta(hit: &SearchHit) -> String {
    let mut meta = format!("{} lines · {}", hit.session.lines, relative_time(hit.session.modified_ts));
    if hit.is_live {
        meta.push_str(" · live");
    }
    meta
}

pub fn relative_time(modified_ts: u64) -> String {
    let now = Utc::now().timestamp().max(0) as u64;
    let age = now.saturating_sub(modified_ts);
    match age {
        0..=59 => format!("{age}s"),
        60..=3_599 => format!("{}m", age / 60),
        3_600..=86_399 => format!("{}h", age / 3_600),
        86_400..=2_592_000 => format!("{}d", age / 86_400),
        _ => Local
            .timestamp_opt(modified_ts as i64, 0)
            .single()
            .map(|time| time.format("%Y-%m-%d").to_string())
            .unwrap_or_else(|| modified_ts.to_string()),
    }
}

pub fn role_label(role: MessageRole) -> &'static str {
    match role {
        MessageRole::User => "User",
        MessageRole::Assistant => "Assistant",
        MessageRole::System => "System",
        MessageRole::Summary => "Summary",
    }
}

pub fn highlight_spans(text: &str, query: &str, base: Style, highlight: Style) -> Vec<Span<'static>> {
    let terms = query
        .split_whitespace()
        .filter(|term| !term.is_empty())
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>();
    if terms.is_empty() {
        return vec![Span::styled(text.to_owned(), base)];
    }

    let lower = text.to_ascii_lowercase();
    let mut spans = Vec::new();
    let mut index = 0usize;
    while index < text.len() {
        let mut matched_len = 0usize;
        for term in &terms {
            if lower[index..].starts_with(term) {
                matched_len = matched_len.max(term.len());
            }
        }

        if matched_len > 0 {
            spans.push(Span::styled(
                text[index..index + matched_len].to_owned(),
                highlight,
            ));
            index += matched_len;
            continue;
        }

        let next = text[index..]
            .char_indices()
            .nth(1)
            .map(|(offset, _)| index + offset)
            .unwrap_or(text.len());
        spans.push(Span::styled(text[index..next].to_owned(), base));
        index = next;
    }

    spans
}

pub fn truncate_plain(value: &str, width: usize) -> String {
    let filtered = value.chars().filter(|ch| !ch.is_control()).collect::<String>();
    let (truncated, _) = filtered.unicode_truncate(width);
    truncated.to_owned()
}

fn unescape_html(value: &str) -> String {
    value
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
}

#[cfg(test)]
mod tests {
    use ratatui::style::Style;

    use super::parse_highlighted_html;

    #[test]
    fn highlight_parser_handles_literal_angle_brackets() {
        let line = parse_highlighted_html(
            "<environment_context> plain <b>match</b>",
            Style::default(),
            Style::default(),
        );

        let rendered = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();

        assert_eq!(rendered, "<environment_context> plain match");
    }
}
