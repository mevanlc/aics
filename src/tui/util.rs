use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use chrono::{Local, TimeZone, Utc};
use directories::BaseDirs;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span, Text};
use unicode_segmentation::UnicodeSegmentation;
use unicode_truncate::UnicodeTruncateStr;

use crate::index::SearchHit;
use crate::parse::{normalize_session_path, Agent, MessageRole};
use crate::search_query::extract_highlight_terms;
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
            current = base.patch(highlight);
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
    abbreviate_home_path(&session_display_title(
        hit.session.agent,
        &hit.session.project,
        hit.session.custom_title.as_deref(),
    ))
}

pub fn session_display_title(agent: Agent, project: &str, custom_title: Option<&str>) -> String {
    match agent {
        Agent::Claude => project.to_owned(),
        Agent::Codex => custom_title.unwrap_or(project).to_owned(),
    }
}

pub fn list_meta(hit: &SearchHit) -> String {
    let mut meta = format!(
        "{} lines · {}",
        hit.session.lines,
        relative_time(hit.session.modified_ts)
    );
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
        MessageRole::ToolCall => "Tool",
        MessageRole::ToolResult => "Result",
    }
}

pub fn highlight_spans(
    text: &str,
    query: &str,
    base: Style,
    highlight: Style,
) -> Vec<Span<'static>> {
    let terms = extract_highlight_terms(query);
    highlight_spans_with_terms(text, &terms, base, highlight)
}

pub fn highlight_spans_with_terms(
    text: &str,
    terms: &[String],
    base: Style,
    highlight: Style,
) -> Vec<Span<'static>> {
    if terms.is_empty() {
        return vec![Span::styled(text.to_owned(), base)];
    }

    let lower = text.to_ascii_lowercase();
    let mut spans = Vec::new();
    let mut index = 0usize;
    while index < text.len() {
        let mut matched_len = 0usize;
        for term in terms {
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
            .grapheme_indices(true)
            .nth(1)
            .map(|(offset, _)| index + offset)
            .unwrap_or(text.len());
        spans.push(Span::styled(text[index..next].to_owned(), base));
        index = next;
    }

    spans
}

pub fn highlight_styled_spans(
    spans: Vec<Span<'static>>,
    terms: &[String],
    overlay: Style,
) -> Vec<Span<'static>> {
    if terms.is_empty() {
        return spans;
    }

    let mut highlighted = Vec::new();
    for span in spans {
        let style = span.style;
        highlighted.extend(highlight_spans_with_terms(
            span.content.as_ref(),
            terms,
            style,
            style.patch(overlay),
        ));
    }

    highlighted
}

pub fn truncate_plain(value: &str, width: usize) -> String {
    let filtered = value
        .chars()
        .filter(|ch| !ch.is_control())
        .collect::<String>();
    let (truncated, _) = filtered.unicode_truncate(width);
    truncated.to_owned()
}

pub fn wrapped_text_height(text: &Text<'_>, width: u16) -> usize {
    use ratatui::widgets::{Paragraph, Wrap};
    Paragraph::new(text.clone())
        .wrap(Wrap { trim: false })
        .line_count(width)
}

fn unescape_html(value: &str) -> String {
    value
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
}

fn abbreviate_home_path(value: &str) -> String {
    static HOME_DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    abbreviate_home_path_with(value, HOME_DIR.get_or_init(discover_home_dir).as_deref())
}

fn abbreviate_home_path_with(value: &str, home_dir: Option<&Path>) -> String {
    let Some(home_dir) = home_dir else {
        return value.to_owned();
    };
    let normalized_value = normalize_session_path(value);
    let normalized_home = normalize_session_path(&home_dir.to_string_lossy());
    let Ok(relative) = Path::new(normalized_value.as_str()).strip_prefix(&normalized_home) else {
        return value.to_owned();
    };

    if relative.as_os_str().is_empty() {
        return "~".to_owned();
    }

    Path::new("~").join(relative).display().to_string()
}

fn discover_home_dir() -> Option<PathBuf> {
    BaseDirs::new().map(|dirs| dirs.home_dir().to_path_buf())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use ratatui::style::{Color, Style};
    use unicode_segmentation::UnicodeSegmentation;

    use ratatui::text::Text;

    use super::{
        abbreviate_home_path_with, highlight_spans, highlight_styled_spans, parse_highlighted_html,
        session_display_title, truncate_plain, wrapped_text_height,
    };

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

    #[test]
    fn truncate_plain_stays_on_grapheme_boundaries() {
        let value = "Plan 👨‍👩‍👧‍👦 sprint";
        let truncated = truncate_plain(value, 7);
        let graphemes = UnicodeSegmentation::graphemes(value, true).collect::<Vec<_>>();

        assert!((0..=graphemes.len()).any(|count| graphemes[..count].concat() == truncated));
    }

    #[test]
    fn highlight_spans_preserve_complex_unicode_text() {
        let spans = highlight_spans(
            "Ship 👨‍👩‍👧‍👦 plan 漢字",
            "plan",
            Style::default(),
            Style::default(),
        );
        let rendered = spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();

        assert_eq!(rendered, "Ship 👨‍👩‍👧‍👦 plan 漢字");
    }

    #[test]
    fn highlighted_html_preserves_base_foreground_when_overlay_only_sets_background() {
        let base = Style::default().fg(Color::Green);
        let overlay = Style::default().bg(Color::Blue);
        let line = parse_highlighted_html("plain <b>match</b>", base, overlay);

        assert_eq!(line.spans[1].content.as_ref(), "match");
        assert_eq!(line.spans[1].style.fg, Some(Color::Green));
        assert_eq!(line.spans[1].style.bg, Some(Color::Blue));
    }

    #[test]
    fn highlight_styled_spans_preserves_existing_modifiers() {
        let spans = highlight_styled_spans(
            vec![ratatui::text::Span::styled(
                "alpha beta",
                Style::default().add_modifier(ratatui::style::Modifier::ITALIC),
            )],
            &[String::from("alpha")],
            Style::default().add_modifier(ratatui::style::Modifier::BOLD),
        );

        assert_eq!(spans[0].content.as_ref(), "alpha");
        assert!(spans[0]
            .style
            .add_modifier
            .contains(ratatui::style::Modifier::ITALIC));
        assert!(spans[0]
            .style
            .add_modifier
            .contains(ratatui::style::Modifier::BOLD));
    }

    #[test]
    fn wrapped_text_height_counts_wide_wrapped_lines() {
        let text = Text::from("alpha\n漢字漢字\n");
        assert_eq!(wrapped_text_height(&text, 4), 4);
    }

    #[test]
    fn abbreviates_paths_under_home_dir() {
        let abbreviated = abbreviate_home_path_with(
            "/data/data/com.termux/files/home/p/my/aics",
            Some(Path::new("/data/data/com.termux/files/home/p")),
        );

        assert_eq!(abbreviated, "~/my/aics");
    }

    #[test]
    fn leaves_non_home_paths_unchanged() {
        let unchanged = abbreviate_home_path_with(
            "/worktrees/aics",
            Some(Path::new("/data/data/com.termux/files/home/p")),
        );

        assert_eq!(unchanged, "/worktrees/aics");
    }

    #[test]
    fn abbreviates_termux_package_alias_paths_under_home_dir() {
        let abbreviated = abbreviate_home_path_with(
            "/data/data/com/termux/files/home/p/my/aics",
            Some(Path::new("/data/data/com.termux/files/home/p")),
        );

        assert_eq!(abbreviated, "~/my/aics");
    }

    #[test]
    fn claude_display_title_ignores_custom_slug() {
        let title = session_display_title(
            crate::parse::Agent::Claude,
            "/Users/testuser/projects/aics",
            Some("memoized-booping-oasis"),
        );

        assert_eq!(title, "/Users/testuser/projects/aics");
    }

    #[test]
    fn codex_display_title_still_prefers_custom_title_when_present() {
        let title = session_display_title(
            crate::parse::Agent::Codex,
            "/Users/testuser/projects/aics",
            Some("hand-written-title"),
        );

        assert_eq!(title, "hand-written-title");
    }
}
