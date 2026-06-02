use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use chrono::{Local, TimeZone, Utc};
use directories::BaseDirs;
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Paragraph, Widget, Wrap};
use unicode_segmentation::UnicodeSegmentation;
use unicode_truncate::UnicodeTruncateStr;

use crate::index::SearchHit;
use crate::parse::{normalize_session_path, Agent, MessageRole, SessionMessage};
use crate::search_query::extract_highlight_terms;
use crate::tui::theme::Theme;

pub const STICKY_HEADER_HEIGHT: u16 = 3;

pub(crate) fn textarea_input_from_key_event(key: KeyEvent) -> ratatui_textarea::Input {
    if matches!(key.kind, KeyEventKind::Release) {
        return ratatui_textarea::Input::default();
    }

    let textarea_key = match key.code {
        KeyCode::Char(ch) => ratatui_textarea::Key::Char(ch),
        KeyCode::Backspace => ratatui_textarea::Key::Backspace,
        KeyCode::Enter => ratatui_textarea::Key::Enter,
        KeyCode::Left => ratatui_textarea::Key::Left,
        KeyCode::Right => ratatui_textarea::Key::Right,
        KeyCode::Up => ratatui_textarea::Key::Up,
        KeyCode::Down => ratatui_textarea::Key::Down,
        KeyCode::Tab | KeyCode::BackTab => ratatui_textarea::Key::Tab,
        KeyCode::Delete => ratatui_textarea::Key::Delete,
        KeyCode::Home => ratatui_textarea::Key::Home,
        KeyCode::End => ratatui_textarea::Key::End,
        KeyCode::PageUp => ratatui_textarea::Key::PageUp,
        KeyCode::PageDown => ratatui_textarea::Key::PageDown,
        KeyCode::Esc => ratatui_textarea::Key::Esc,
        KeyCode::F(n) => ratatui_textarea::Key::F(n),
        _ => ratatui_textarea::Key::Null,
    };

    ratatui_textarea::Input {
        key: textarea_key,
        ctrl: key.modifiers.contains(KeyModifiers::CONTROL),
        alt: key.modifiers.contains(KeyModifiers::ALT),
        shift: key.modifiers.contains(KeyModifiers::SHIFT) || matches!(key.code, KeyCode::BackTab),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StickyHeader {
    pub from: String,
    pub datetime: String,
    pub subject: String,
}

impl StickyHeader {
    pub fn new(
        from: impl Into<String>,
        datetime: impl Into<String>,
        subject: impl Into<String>,
    ) -> Self {
        Self {
            from: from.into(),
            datetime: datetime.into(),
            subject: subject.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StickyLineMarker {
    pub line_index: usize,
    pub header: StickyHeader,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StickyRowMarker {
    pub row: usize,
    pub header: StickyHeader,
}

pub fn sticky_rows_from_line_markers(
    text: &Text<'_>,
    markers: &[StickyLineMarker],
    width: u16,
) -> Vec<StickyRowMarker> {
    if markers.is_empty() || width == 0 {
        return Vec::new();
    }

    let mut markers = markers.to_vec();
    markers.sort_by_key(|marker| marker.line_index);
    let mut rows = Vec::with_capacity(markers.len());
    let mut marker_index = 0usize;
    let mut row = 0usize;

    for (line_index, line) in text.lines.iter().enumerate() {
        while marker_index < markers.len() && markers[marker_index].line_index == line_index {
            rows.push(StickyRowMarker {
                row,
                header: markers[marker_index].header.clone(),
            });
            marker_index += 1;
        }
        row += wrapped_text_height(&Text::from(line.clone()), width).max(1);
    }

    while marker_index < markers.len() {
        rows.push(StickyRowMarker {
            row,
            header: markers[marker_index].header.clone(),
        });
        marker_index += 1;
    }

    rows
}

pub fn sticky_header_for_scroll(
    markers: &[StickyRowMarker],
    scroll: usize,
) -> Option<StickyHeader> {
    markers
        .iter()
        .rev()
        .find(|marker| marker.row <= scroll)
        .or_else(|| markers.first())
        .map(|marker| marker.header.clone())
}

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
    let title = abbreviate_home_path(&session_display_title(
        hit.session.agent,
        &hit.session.project,
    ));
    if hit.session.trashed {
        format!("trashed · {title}")
    } else {
        title
    }
}

pub fn session_display_title(agent: Agent, project: &str) -> String {
    match agent {
        Agent::Claude => project.to_owned(),
        Agent::Codex => project.to_owned(),
    }
}

pub fn block_title<'a>(title: impl Into<Line<'a>>) -> Line<'a> {
    let mut title = title.into();
    title.spans.insert(0, Span::raw("─"));
    title
}

pub fn right_block_title<'a>(title: impl Into<Line<'a>>) -> Line<'a> {
    let mut title = title.into();
    title.spans.push(Span::raw("─"));
    title.right_aligned()
}

pub fn list_meta(hit: &SearchHit) -> String {
    let mut meta = format!(
        "{} lines · {}",
        format_line_count(hit.session.lines),
        relative_time(hit.session.modified_ts)
    );
    if hit.is_live {
        meta.push_str(" · live");
    }
    meta
}

pub fn format_line_count(lines: usize) -> String {
    let digits = lines.to_string();
    let grouped = digits
        .chars()
        .rev()
        .enumerate()
        .flat_map(|(index, ch)| {
            let mut chunk = Vec::new();
            if index > 0 && index % 3 == 0 {
                chunk.push(',');
            }
            chunk.push(ch);
            chunk
        })
        .collect::<Vec<_>>();
    let grouped = grouped.into_iter().rev().collect::<String>();
    format!("{grouped} lines")
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

pub fn session_message_label(message: &SessionMessage) -> String {
    match (&message.role, &message.tool_name) {
        (MessageRole::ToolCall, Some(name)) => format!("\u{203a} {name}"),
        (MessageRole::ToolResult, Some(name)) => format!("\u{2039} {name}"),
        (MessageRole::ToolCall, None) => "\u{203a} tool".to_owned(),
        (MessageRole::ToolResult, None) => "\u{2039} result".to_owned(),
        _ => role_label(message.role).to_owned(),
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

pub struct FullLineBackgroundParagraph<'a> {
    text: Text<'a>,
    block: Option<Block<'a>>,
    scroll: usize,
}

impl<'a> FullLineBackgroundParagraph<'a> {
    pub fn new(text: Text<'a>) -> Self {
        Self {
            text,
            block: None,
            scroll: 0,
        }
    }

    pub fn block(mut self, block: Block<'a>) -> Self {
        self.block = Some(block);
        self
    }

    pub fn scroll(mut self, scroll: usize) -> Self {
        self.scroll = scroll;
        self
    }
}

impl Widget for FullLineBackgroundParagraph<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let inner = self.block.as_ref().map_or(area, |block| block.inner(area));
        let paragraph = Paragraph::new(self.text.clone())
            .wrap(Wrap { trim: false })
            .scroll((self.scroll.min(u16::MAX as usize) as u16, 0));
        let paragraph = if let Some(block) = self.block {
            paragraph.block(block)
        } else {
            paragraph
        };

        paragraph.render(area, buf);
        fill_line_background_tails(&self.text, inner, self.scroll, buf);
    }
}

fn fill_line_background_tails(text: &Text<'_>, area: Rect, scroll: usize, buf: &mut Buffer) {
    if area.is_empty() {
        return;
    }

    let mut rendered_row = 0usize;
    for line in &text.lines {
        let line_height = wrapped_text_height(&Text::from(line.clone()), area.width).max(1);
        for _ in 0..line_height {
            if rendered_row >= scroll {
                let viewport_row = rendered_row - scroll;
                if viewport_row >= area.height as usize {
                    return;
                }
                if let Some(bg) = line.style.bg.filter(|bg| *bg != Color::Reset) {
                    fill_reset_background_cells(area, viewport_row as u16, bg, buf);
                }
            }
            rendered_row += 1;
        }
    }
}

fn fill_reset_background_cells(area: Rect, viewport_row: u16, bg: Color, buf: &mut Buffer) {
    let y = area.y.saturating_add(viewport_row);
    for x in area.x..area.right() {
        let cell = &mut buf[(x, y)];
        if cell.bg == Color::Reset {
            cell.set_bg(bg);
        }
    }
}

pub struct StickyHeaderWidget<'a> {
    header: Option<&'a StickyHeader>,
    theme: &'a Theme,
}

impl<'a> StickyHeaderWidget<'a> {
    pub fn new(header: Option<&'a StickyHeader>, theme: &'a Theme) -> Self {
        Self { header, theme }
    }
}

impl Widget for StickyHeaderWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.is_empty() {
            return;
        }

        let empty = StickyHeader::new("", "", "");
        let header = self.header.unwrap_or(&empty);
        let from_value = if header.datetime.is_empty() {
            header.from.clone()
        } else if header.from.is_empty() {
            header.datetime.clone()
        } else {
            format!("{} - {}", header.from, header.datetime)
        };
        let from = sticky_header_line("From   : ", &from_value, area.width, self.theme);
        let subject = sticky_header_line("Subject: ", &header.subject, area.width, self.theme);
        let rule = Line::from(Span::styled(
            "─".repeat(area.width as usize),
            Style::default().fg(self.theme.border),
        ));

        Paragraph::new(from).render(Rect::new(area.x, area.y, area.width, 1), buf);
        if area.height > 1 {
            Paragraph::new(subject).render(Rect::new(area.x, area.y + 1, area.width, 1), buf);
        }
        if area.height > 2 {
            Paragraph::new(rule).render(Rect::new(area.x, area.y + 2, area.width, 1), buf);
        }
    }
}

fn sticky_header_line(
    label: &'static str,
    value: &str,
    width: u16,
    theme: &Theme,
) -> Line<'static> {
    let label_width = label.len();
    let value_width = width as usize;
    let value_width = value_width.saturating_sub(label_width);
    Line::from(vec![
        Span::styled(
            label,
            Style::default()
                .fg(theme.muted)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            truncate_plain(value, value_width),
            Style::default().fg(theme.text),
        ),
    ])
}

fn unescape_html(value: &str) -> String {
    value
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
}

pub fn abbreviate_home_path(value: &str) -> String {
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
    use std::path::{Path, PathBuf};

    use ratatui::backend::TestBackend;
    use ratatui::style::{Color, Style};
    use ratatui::text::{Line, Span};
    use ratatui::widgets::{Block, Borders};
    use ratatui::Terminal;
    use unicode_segmentation::UnicodeSegmentation;

    use ratatui::text::Text;

    use super::{
        abbreviate_home_path_with, block_title, highlight_spans, highlight_styled_spans,
        list_title, parse_highlighted_html, right_block_title, session_display_title,
        sticky_header_for_scroll, sticky_rows_from_line_markers, truncate_plain,
        wrapped_text_height, FullLineBackgroundParagraph, StickyHeader, StickyLineMarker,
    };
    use crate::index::{SearchHit, StoredSession};
    use crate::parse::{Agent, DerivationType};

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
    fn full_line_background_paragraph_extends_line_bg_to_inner_edge() {
        let bg = Color::Blue;
        let mut line = Line::from(Span::styled("hi", Style::default().fg(Color::White).bg(bg)));
        line.style = Style::default().bg(bg);
        let text = Text::from(line);
        let backend = TestBackend::new(10, 3);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal
            .draw(|frame| {
                frame.render_widget(
                    FullLineBackgroundParagraph::new(text.clone())
                        .block(Block::default().borders(Borders::ALL)),
                    frame.area(),
                );
            })
            .unwrap();

        let rendered = terminal.backend().buffer();
        assert_eq!(rendered[(8, 1)].symbol(), " ");
        assert_eq!(rendered[(8, 1)].bg, bg);
        assert_ne!(rendered[(9, 1)].bg, bg);
    }

    #[test]
    fn full_line_background_paragraph_preserves_search_match_bg() {
        let line_bg = Color::Blue;
        let match_bg = Color::Yellow;
        let mut line = Line::from(vec![
            Span::styled("alpha", Style::default().bg(match_bg)),
            Span::styled(" beta", Style::default().bg(line_bg)),
        ]);
        line.style = Style::default().bg(line_bg);
        let text = Text::from(line);
        let backend = TestBackend::new(14, 1);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal
            .draw(|frame| {
                frame.render_widget(FullLineBackgroundParagraph::new(text.clone()), frame.area());
            })
            .unwrap();

        let rendered = terminal.backend().buffer();
        assert_eq!(rendered[(0, 0)].bg, match_bg);
        assert_eq!(rendered[(4, 0)].bg, match_bg);
        assert_eq!(rendered[(5, 0)].bg, line_bg);
        assert_eq!(rendered[(13, 0)].bg, line_bg);
    }

    #[test]
    fn full_line_background_paragraph_extends_wrapped_and_scrolled_rows() {
        let bg = Color::Green;
        let mut line = Line::from(Span::styled(
            "alpha beta gamma",
            Style::default().fg(Color::White).bg(bg),
        ));
        line.style = Style::default().bg(bg);
        let text = Text::from(line);
        let backend = TestBackend::new(8, 2);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal
            .draw(|frame| {
                frame.render_widget(
                    FullLineBackgroundParagraph::new(text.clone()).scroll(1),
                    frame.area(),
                );
            })
            .unwrap();

        let rendered = terminal.backend().buffer();
        assert_eq!(rendered[(7, 0)].bg, bg);
        assert_eq!(rendered[(7, 1)].bg, bg);
    }

    #[test]
    fn sticky_rows_follow_wrapped_line_positions() {
        let text = Text::from(vec![Line::from("alpha beta gamma"), Line::from("delta")]);
        let markers = vec![
            StickyLineMarker {
                line_index: 0,
                header: StickyHeader::new("User", "", ""),
            },
            StickyLineMarker {
                line_index: 1,
                header: StickyHeader::new("Agent", "", "Reply"),
            },
        ];

        let rows = sticky_rows_from_line_markers(&text, &markers, 8);

        assert_eq!(rows[0].row, 0);
        assert_eq!(rows[1].row, 3);
        assert_eq!(
            sticky_header_for_scroll(&rows, 2).map(|header| header.from),
            Some("User".to_owned())
        );
        assert_eq!(
            sticky_header_for_scroll(&rows, 3).map(|header| header.from),
            Some("Agent".to_owned())
        );
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
        let title =
            session_display_title(crate::parse::Agent::Claude, "/Users/testuser/projects/aics");

        assert_eq!(title, "/Users/testuser/projects/aics");
    }

    #[test]
    fn codex_display_title_uses_project_even_when_custom_title_is_present() {
        let title =
            session_display_title(crate::parse::Agent::Codex, "/Users/testuser/projects/aics");

        assert_eq!(title, "/Users/testuser/projects/aics");
    }

    #[test]
    fn list_title_prefixes_trashed_sessions() {
        let mut hit = SearchHit {
            session: StoredSession {
                session_id: "session-123".to_owned(),
                agent: Agent::Claude,
                project: "/tmp/demo".to_owned(),
                branch: None,
                cwd: Some("/tmp/demo".to_owned()),
                modified_ts: 0,
                lines: 1,
                file_path: PathBuf::from("/tmp/demo/session.jsonl"),
                first_msg_role: None,
                first_msg_content: String::new(),
                last_msg_role: None,
                last_msg_content: String::new(),
                first_user_msg_content: String::new(),
                derivation_type: DerivationType::Original,
                is_sidechain: false,
                custom_title: None,
                session_info: None,
                trashed: false,
                original_path: None,
            },
            snippet_html: String::new(),
            score: 0.0,
            is_live: false,
        };

        assert_eq!(list_title(&hit), "/tmp/demo");
        hit.session.trashed = true;
        assert_eq!(list_title(&hit), "trashed · /tmp/demo");
    }

    #[test]
    fn block_title_prefixes_top_border_dash() {
        let title = block_title(Line::from("Viewer · 9%"));
        let rendered = title
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();

        assert_eq!(rendered, "─Viewer · 9%");
    }

    #[test]
    fn right_block_title_suffixes_top_border_dash() {
        let title = right_block_title(Line::from("^L help"));
        let rendered = title
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();

        assert_eq!(rendered, "^L help─");
        assert_eq!(title.alignment, Some(ratatui::layout::Alignment::Right));
    }

    #[test]
    fn right_block_title_renders_dash_before_corner() {
        let backend = TestBackend::new(16, 3);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal
            .draw(|frame| {
                let block = Block::default()
                    .borders(Borders::ALL)
                    .title(right_block_title("^L help"));
                frame.render_widget(block, frame.area());
            })
            .unwrap();

        let buffer = terminal.backend().buffer();
        let top_border = (0..buffer.area.width)
            .map(|x| buffer[(x, 0)].symbol())
            .collect::<String>();

        assert_eq!(top_border, "┌──────^L help─┐");
    }
}
