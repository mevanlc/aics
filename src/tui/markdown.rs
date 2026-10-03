use std::{ops::Range, sync::LazyLock};

use pulldown_cmark::{
    CodeBlockKind, Event, HeadingLevel, Options as ParseOptions, Parser, Tag, TagEnd,
};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use syntect::easy::HighlightLines;
use syntect::highlighting::{Color as SyntectColor, FontStyle, ThemeSet};
use syntect::parsing::SyntaxSet;
use syntect::util::LinesWithEndings;

use crate::search_query::extract_highlight_terms;
use crate::tui::ansi::{push_origin, sanitize_with_origins, strip_terminal_escapes, TextOrigin};
use crate::tui::theme::Theme;
use crate::tui::util::{highlight_spans_with_terms, highlight_styled_spans};

static SYNTAX_SET: LazyLock<SyntaxSet> = LazyLock::new(SyntaxSet::load_defaults_newlines);
static THEME_SET: LazyLock<ThemeSet> = LazyLock::new(ThemeSet::load_defaults);
pub(crate) const SYNTECT_THEME: &str = "base16-ocean.dark";

pub fn render_markdown_message(
    content: &str,
    theme: &Theme,
    base_style: Style,
    highlight_query: Option<&str>,
) -> Text<'static> {
    render_markdown_message_with_headings(content, theme, base_style, highlight_query).text
}

#[derive(Debug, Clone)]
pub struct MarkdownRender {
    pub text: Text<'static>,
    pub headings: Vec<MarkdownHeading>,
    pub(crate) origins: Vec<TextOrigin>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkdownHeading {
    pub line_index: usize,
    /// 1..=6 — H1 through H6.
    pub level: u8,
    /// The heading's own text, without ancestors.
    pub text: String,
    /// Full ancestor path joined by ` › ` (e.g. `Top › Section › Sub`). When
    /// the heading has no ancestors it's identical to `text`. Sticky headers
    /// use this to give scroll-context for the current section.
    pub breadcrumb: String,
}

pub fn render_markdown_message_with_headings(
    content: &str,
    theme: &Theme,
    base_style: Style,
    highlight_query: Option<&str>,
) -> MarkdownRender {
    let mut options = ParseOptions::empty();
    options.insert(ParseOptions::ENABLE_STRIKETHROUGH);
    options.insert(ParseOptions::ENABLE_TASKLISTS);
    options.insert(ParseOptions::ENABLE_SUPERSCRIPT);
    options.insert(ParseOptions::ENABLE_SUBSCRIPT);

    let parser = Parser::new_ext(content, options);
    MarkdownRenderer::new(theme, base_style, highlight_query).render(parser, content)
}

struct MarkdownRenderer<'a> {
    theme: &'a Theme,
    base_style: Style,
    search_highlight: Style,
    terms: Vec<String>,
    lines: Vec<Line<'static>>,
    headings: Vec<MarkdownHeading>,
    current_line: Vec<Span<'static>>,
    inline_styles: Vec<Style>,
    list_stack: Vec<ListState>,
    blockquote_depth: usize,
    code_block: Option<CodeBlockState>,
    /// Ancestor path of (level, text) pairs used to build heading breadcrumbs.
    /// On a new heading at level N, all entries with level >= N are popped
    /// before the new one is pushed.
    heading_path: Vec<(u8, String)>,
    /// Track the current heading's level between `start_tag(Heading)` and
    /// `end_tag(Heading)` since `TagEnd::Heading` doesn't carry the level.
    current_heading_level: Option<u8>,
    completed_bytes: usize,
    origins: Vec<TextOrigin>,
}

#[derive(Clone, Copy)]
struct ListState {
    next_index: Option<u64>,
}

struct CodeBlockState {
    highlighter: Option<HighlightLines<'static>>,
    base_style: Style,
}

impl<'a> MarkdownRenderer<'a> {
    fn new(theme: &'a Theme, base_style: Style, highlight_query: Option<&str>) -> Self {
        Self {
            theme,
            base_style,
            search_highlight: theme.search_match_style(),
            terms: extract_highlight_terms(highlight_query.unwrap_or_default()),
            lines: Vec::new(),
            headings: Vec::new(),
            current_line: Vec::new(),
            inline_styles: vec![base_style],
            list_stack: Vec::new(),
            blockquote_depth: 0,
            code_block: None,
            heading_path: Vec::new(),
            current_heading_level: None,
            completed_bytes: 0,
            origins: Vec::new(),
        }
    }

    fn render<'b>(mut self, parser: Parser<'b>, content: &str) -> MarkdownRender {
        for (event, range) in parser.into_offset_iter() {
            self.handle_event(event, range, content);
        }

        self.finish_line();
        if self.lines.last().is_some_and(|line| line.spans.is_empty()) {
            self.lines.pop();
        }
        let end = self
            .lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.len())
                    .sum::<usize>()
            })
            .sum::<usize>()
            + self.lines.len().saturating_sub(1);
        self.origins.retain_mut(|origin| {
            if origin.rendered.start >= end {
                return false;
            }
            if origin.rendered.end > end {
                if origin.rendered.len() == origin.source.len() {
                    origin.source.end -= origin.rendered.end - end;
                }
                origin.rendered.end = end;
            }
            true
        });

        MarkdownRender {
            text: Text::from(self.lines),
            headings: self.headings,
            origins: self.origins,
        }
    }

    fn handle_event<'b>(&mut self, event: Event<'b>, range: Range<usize>, content: &str) {
        if self.code_block.is_some() {
            match event {
                Event::Text(text) => {
                    let offset = self.completed_bytes;
                    let safe = sanitize_with_origins(text.as_ref());
                    let mapped = event_origins(content, range, text.as_ref(), false);
                    self.push_code_text(text.as_ref());
                    self.record_origins(offset, &safe.origins, &mapped);
                }
                Event::End(TagEnd::CodeBlock) => {
                    self.code_block = None;
                }
                _ => {}
            }
            return;
        }

        match event {
            Event::Start(tag) => self.start_tag(tag),
            Event::End(tag) => self.end_tag(tag),
            Event::Text(text) | Event::Html(text) | Event::InlineHtml(text) => {
                self.push_source_text(text.as_ref(), self.current_style(), range, content, false)
            }
            Event::Code(code) => self.push_source_text(
                code.as_ref(),
                self.inline_code_style(),
                range,
                content,
                true,
            ),
            Event::SoftBreak | Event::HardBreak => {
                if !self.current_line.is_empty() {
                    let start = self.completed_bytes
                        + self
                            .current_line
                            .iter()
                            .map(|span| span.content.len())
                            .sum::<usize>();
                    push_origin(
                        &mut self.origins,
                        TextOrigin {
                            rendered: start..start + 1,
                            source: range,
                        },
                    );
                }
                self.finish_line();
            }
            Event::Rule => self.push_rule(),
            Event::TaskListMarker(checked) => {
                let marker = if checked { "[x] " } else { "[ ] " };
                self.push_text(marker, self.current_style());
            }
            Event::FootnoteReference(label) => {
                let text = format!("[^{label}]");
                self.push_text(&text, self.base_style.add_modifier(Modifier::DIM));
            }
            Event::InlineMath(text) | Event::DisplayMath(text) => {
                self.push_text(text.as_ref(), self.inline_code_style());
            }
        }
    }

    fn start_tag<'b>(&mut self, tag: Tag<'b>) {
        match tag {
            Tag::Paragraph => self.start_block(),
            Tag::Heading { level, .. } => {
                self.start_block();
                self.inline_styles.push(self.heading_style(level));
                self.current_heading_level = Some(heading_level_to_u8(level));
            }
            Tag::BlockQuote(_) => {
                self.start_block();
                self.blockquote_depth += 1;
            }
            Tag::CodeBlock(kind) => self.start_code_block(kind),
            Tag::List(start_index) => self.list_stack.push(ListState {
                next_index: start_index,
            }),
            Tag::Item => self.start_item(),
            Tag::Emphasis => {
                self.push_inline_style(Style::default().add_modifier(Modifier::ITALIC))
            }
            Tag::Strong => self.push_inline_style(Style::default().add_modifier(Modifier::BOLD)),
            Tag::Strikethrough => {
                self.push_inline_style(Style::default().add_modifier(Modifier::CROSSED_OUT))
            }
            Tag::Link { .. } => self.push_inline_style(Style::default().fg(self.theme.accent)),
            Tag::Image { .. } => {}
            Tag::MetadataBlock(_) => {}
            Tag::Table(_) | Tag::TableHead | Tag::TableRow | Tag::TableCell => {}
            Tag::FootnoteDefinition(_) => {}
            Tag::DefinitionList | Tag::DefinitionListTitle | Tag::DefinitionListDefinition => {}
            Tag::HtmlBlock => self.start_block(),
            Tag::Superscript => {
                self.push_inline_style(Style::default().add_modifier(Modifier::DIM))
            }
            Tag::Subscript => self.push_inline_style(Style::default().add_modifier(Modifier::DIM)),
        }
    }

    fn end_tag(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph | TagEnd::HtmlBlock => self.finish_line(),
            TagEnd::Heading(_) => {
                let subject = spans_text(&self.current_line).trim().to_owned();
                let level = self.current_heading_level.take().unwrap_or(1);
                self.pop_inline_style();
                self.finish_line();
                if !subject.is_empty() {
                    // Drop any ancestors at this level or deeper, then push.
                    while self
                        .heading_path
                        .last()
                        .is_some_and(|(prev_level, _)| *prev_level >= level)
                    {
                        self.heading_path.pop();
                    }
                    self.heading_path.push((level, subject.clone()));
                    let breadcrumb = self
                        .heading_path
                        .iter()
                        .map(|(_, text)| text.as_str())
                        .collect::<Vec<_>>()
                        .join(" \u{203a} ");
                    self.headings.push(MarkdownHeading {
                        line_index: self.lines.len().saturating_sub(1),
                        level,
                        text: subject,
                        breadcrumb,
                    });
                }
            }
            TagEnd::BlockQuote(_) => {
                self.blockquote_depth = self.blockquote_depth.saturating_sub(1);
                self.finish_line();
            }
            TagEnd::CodeBlock => {}
            TagEnd::List(_) => {
                self.list_stack.pop();
                self.finish_line();
            }
            TagEnd::Item => self.finish_line(),
            TagEnd::Emphasis
            | TagEnd::Strong
            | TagEnd::Strikethrough
            | TagEnd::Link
            | TagEnd::Superscript
            | TagEnd::Subscript => self.pop_inline_style(),
            TagEnd::Image => {}
            TagEnd::MetadataBlock(_) => {}
            TagEnd::Table | TagEnd::TableHead | TagEnd::TableRow | TagEnd::TableCell => {
                self.finish_line()
            }
            TagEnd::FootnoteDefinition => self.finish_line(),
            TagEnd::DefinitionList
            | TagEnd::DefinitionListTitle
            | TagEnd::DefinitionListDefinition => self.finish_line(),
        }
    }

    fn start_block(&mut self) {
        self.finish_line();
        if !self.lines.is_empty() && !self.lines.last().is_some_and(|line| line.spans.is_empty()) {
            self.lines.push(self.blank_line());
            self.completed_bytes += 1;
        }
    }

    fn start_item(&mut self) {
        self.finish_line();
        let indent = "  ".repeat(self.list_stack.len().saturating_sub(1));
        let marker = if let Some(list) = self.list_stack.last_mut() {
            if let Some(index) = list.next_index {
                let marker = format!("{index}. ");
                list.next_index = Some(index + 1);
                marker
            } else {
                "• ".to_owned()
            }
        } else {
            "• ".to_owned()
        };

        self.push_text(
            &format!("{indent}{marker}"),
            self.base_style.add_modifier(Modifier::BOLD),
        );
    }

    fn start_code_block<'b>(&mut self, kind: CodeBlockKind<'b>) {
        self.start_block();
        let language = match kind {
            CodeBlockKind::Indented => None,
            CodeBlockKind::Fenced(lang) => normalize_code_fence_lang(lang.as_ref()),
        };

        let highlighter = language.as_deref().and_then(create_highlighter);
        self.code_block = Some(CodeBlockState {
            highlighter,
            base_style: self.code_block_style(),
        });
    }

    fn push_text(&mut self, text: &str, style: Style) {
        if text.is_empty() {
            return;
        }

        let text = strip_terminal_escapes(text);
        if text.is_empty() {
            return;
        }

        for chunk in text.split_inclusive('\n') {
            self.ensure_line_prefix();
            let content = chunk.strip_suffix('\n').unwrap_or(chunk);
            self.current_line.extend(highlight_spans_with_terms(
                content,
                &self.terms,
                style,
                style.patch(self.search_highlight),
            ));
            if chunk.ends_with('\n') {
                if self.current_line.is_empty() {
                    self.lines.push(self.blank_line());
                    self.completed_bytes += 1;
                } else {
                    self.finish_line();
                }
            }
        }
    }

    fn push_source_text(
        &mut self,
        text: &str,
        style: Style,
        range: Range<usize>,
        content: &str,
        inline_code: bool,
    ) {
        let safe = sanitize_with_origins(text);
        let mapped = event_origins(content, range, text, inline_code);
        let mut chunk_start = 0;
        for chunk in safe.text.split_inclusive('\n') {
            self.ensure_line_prefix();
            let offset = self.completed_bytes
                + self
                    .current_line
                    .iter()
                    .map(|span| span.content.len())
                    .sum::<usize>();
            let chunk_end = chunk_start + chunk.len();
            let visible: Vec<_> = safe
                .origins
                .iter()
                .filter_map(|origin| {
                    let start = origin.rendered.start.max(chunk_start);
                    let end = origin.rendered.end.min(chunk_end);
                    if start >= end {
                        return None;
                    }
                    let source = if origin.rendered.len() == origin.source.len() {
                        origin.source.start + start - origin.rendered.start
                            ..origin.source.start + end - origin.rendered.start
                    } else {
                        origin.source.clone()
                    };
                    Some(TextOrigin {
                        rendered: start - chunk_start..end - chunk_start,
                        source,
                    })
                })
                .collect();
            self.push_text(chunk, style);
            self.record_origins(offset, &visible, &mapped);
            chunk_start = chunk_end;
        }
    }

    fn record_origins(&mut self, offset: usize, safe: &[TextOrigin], mapped: &[TextOrigin]) {
        let mut first = 0;
        for visible in safe {
            while first < mapped.len() && mapped[first].rendered.end <= visible.source.start {
                first += 1;
            }
            for original in &mapped[first..] {
                if original.rendered.start >= visible.source.end {
                    break;
                }
                let start = visible.source.start.max(original.rendered.start);
                let end = visible.source.end.min(original.rendered.end);
                if start >= end {
                    continue;
                }
                let rendered = if visible.source.len() == visible.rendered.len() {
                    visible.rendered.start + start - visible.source.start
                        ..visible.rendered.start + end - visible.source.start
                } else {
                    visible.rendered.clone()
                };
                let source = if original.source.len() == original.rendered.len() {
                    original.source.start + start - original.rendered.start
                        ..original.source.start + end - original.rendered.start
                } else {
                    original.source.clone()
                };
                push_origin(
                    &mut self.origins,
                    TextOrigin {
                        rendered: offset + rendered.start..offset + rendered.end,
                        source,
                    },
                );
            }
        }
    }

    fn push_code_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }

        let Some(code_block) = self.code_block.as_mut() else {
            return;
        };

        for raw_line in LinesWithEndings::from(text) {
            let safe_raw_line = strip_terminal_escapes(raw_line);
            let line = safe_raw_line.strip_suffix('\n').unwrap_or(&safe_raw_line);
            let spans = if let Some(highlighter) = &mut code_block.highlighter {
                match highlighter.highlight_line(&safe_raw_line, &SYNTAX_SET) {
                    Ok(segments) => {
                        let mut spans = segments
                            .into_iter()
                            .map(|(style, text)| {
                                Span::styled(
                                    text.to_owned(),
                                    syntect_style_to_ratatui(style, code_block.base_style),
                                )
                            })
                            .collect::<Vec<_>>();
                        trim_trailing_newline(&mut spans);
                        highlight_styled_spans(spans, &self.terms, self.search_highlight)
                    }
                    Err(_) => highlight_spans_with_terms(
                        line,
                        &self.terms,
                        code_block.base_style,
                        code_block.base_style.patch(self.search_highlight),
                    ),
                }
            } else {
                highlight_spans_with_terms(
                    line,
                    &self.terms,
                    code_block.base_style,
                    code_block.base_style.patch(self.search_highlight),
                )
            };

            self.lines
                .push(Self::line_with_style(spans, code_block.base_style));
            self.completed_bytes += line.len() + 1;
        }
    }

    fn push_rule(&mut self) {
        self.start_block();
        self.lines.push(Self::line_with_style(
            vec![Span::styled(
                "────────────────────────",
                self.base_style.fg(self.theme.muted),
            )],
            self.base_style,
        ));
        self.completed_bytes += "────────────────────────".len() + 1;
    }

    fn finish_line(&mut self) {
        if self.current_line.is_empty() {
            return;
        }

        let spans = std::mem::take(&mut self.current_line);
        self.completed_bytes += spans.iter().map(|span| span.content.len()).sum::<usize>() + 1;
        self.lines
            .push(Self::line_with_style(spans, self.base_style));
    }

    fn ensure_line_prefix(&mut self) {
        if !self.current_line.is_empty() || self.blockquote_depth == 0 {
            return;
        }

        let prefix = "> ".repeat(self.blockquote_depth);
        self.current_line.push(Span::styled(
            prefix,
            self.base_style
                .fg(self.theme.muted)
                .add_modifier(Modifier::BOLD),
        ));
    }

    fn current_style(&self) -> Style {
        self.inline_styles
            .last()
            .copied()
            .unwrap_or(self.base_style)
    }

    fn push_inline_style(&mut self, style: Style) {
        self.inline_styles.push(self.current_style().patch(style));
    }

    fn pop_inline_style(&mut self) {
        if self.inline_styles.len() > 1 {
            self.inline_styles.pop();
        }
    }

    fn inline_code_style(&self) -> Style {
        self.base_style
            .fg(self.theme.highlight)
            .add_modifier(Modifier::BOLD)
    }

    fn code_block_style(&self) -> Style {
        self.base_style
            .fg(self.theme.text)
            .add_modifier(Modifier::DIM)
    }

    fn heading_style(&self, level: HeadingLevel) -> Style {
        let color = match level {
            HeadingLevel::H1 | HeadingLevel::H2 => self.theme.accent,
            HeadingLevel::H3 | HeadingLevel::H4 => self.theme.highlight,
            HeadingLevel::H5 | HeadingLevel::H6 => self.theme.text,
        };

        self.base_style.fg(color).add_modifier(Modifier::BOLD)
    }

    fn blank_line(&self) -> Line<'static> {
        Self::line_with_style(Vec::new(), self.base_style)
    }

    fn line_with_style(spans: Vec<Span<'static>>, style: Style) -> Line<'static> {
        let mut line = Line::from(spans);
        line.style = style;
        line
    }
}

/// Map a parser event to its own source interval. Parsing has already identified
/// the interval, so repeated words outside it can never acquire this event's
/// location. Markdown's two text substitutions are decoded explicitly.
fn event_origins(
    content: &str,
    mut range: Range<usize>,
    text: &str,
    inline_code: bool,
) -> Vec<TextOrigin> {
    if inline_code {
        while content.as_bytes().get(range.start) == Some(&b'`') {
            range.start += 1;
        }
        while range.end > range.start && content.as_bytes().get(range.end - 1) == Some(&b'`') {
            range.end -= 1;
        }
        let raw = &content[range.clone()];
        let normalized_space = |ch| matches!(ch, ' ' | '\n' | '\r');
        let normalized_len = raw.len() - raw.match_indices("\r\n").count();
        if raw.chars().next().is_some_and(normalized_space)
            && raw.chars().next_back().is_some_and(normalized_space)
            && raw.chars().any(|ch| !normalized_space(ch))
            && text.len() + 2 == normalized_len
        {
            range.start += if raw.starts_with("\r\n") { 2 } else { 1 };
            range.end -= if raw.ends_with("\r\n") { 2 } else { 1 };
        }
    }
    let raw = &content[range.clone()];
    if raw == text {
        return vec![TextOrigin {
            rendered: 0..text.len(),
            source: range,
        }];
    }
    let mut origins = Vec::new();
    let mut input = 0;
    let mut output = 0;
    while input < raw.len() && output < text.len() {
        let ch = raw[input..].chars().next().unwrap();
        let mut consumed = ch.len_utf8();
        let mut decoded = ch.to_string();
        if !inline_code && ch == '\\' {
            if let Some(next) = raw[input + 1..]
                .chars()
                .next()
                .filter(|ch| ch.is_ascii_punctuation())
            {
                consumed += next.len_utf8();
                decoded = next.to_string();
            }
        } else if !inline_code && ch == '&' {
            if let Some(end) = raw[input..].find(';').filter(|end| *end < 40) {
                let entity = &raw[input..input + end + 1];
                let decoded_entity = Parser::new(entity)
                    .filter_map(|event| match event {
                        Event::Text(text) => Some(text.into_string()),
                        _ => None,
                    })
                    .collect::<String>();
                if decoded_entity != entity && !decoded_entity.is_empty() {
                    consumed = entity.len();
                    decoded = decoded_entity;
                }
            }
        } else if inline_code && matches!(ch, '\n' | '\r') {
            if ch == '\r' && raw.as_bytes().get(input + 1) == Some(&b'\n') {
                consumed += 1;
            }
            decoded = " ".to_owned();
        }
        if !text[output..].starts_with(&decoded) {
            // Indented code removes indentation at the beginning of each line.
            // No other unmatched text is assigned an invented display location.
            if ch == ' '
                && raw[..input]
                    .rsplit('\n')
                    .next()
                    .is_some_and(|prefix| prefix.chars().all(|ch| ch == ' '))
            {
                input += consumed;
                continue;
            }
            break;
        }
        push_origin(
            &mut origins,
            TextOrigin {
                rendered: output..output + decoded.len(),
                source: range.start + input..range.start + input + consumed,
            },
        );
        input += consumed;
        output += decoded.len();
    }
    origins
}

fn normalize_code_fence_lang(lang: &str) -> Option<String> {
    let token = lang.split_whitespace().next()?.trim().to_ascii_lowercase();
    if token.is_empty() {
        return None;
    }

    let normalized = match token.as_str() {
        "rs" => "rust",
        "js" => "javascript",
        "ts" => "typescript",
        "sh" => "bash",
        other => other,
    };
    Some(normalized.to_owned())
}

fn create_highlighter(language: &str) -> Option<HighlightLines<'static>> {
    let syntax = SYNTAX_SET.find_syntax_by_token(language)?;
    let theme = THEME_SET.themes.get(SYNTECT_THEME)?;
    Some(HighlightLines::new(syntax, theme))
}

fn heading_level_to_u8(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

fn syntect_style_to_ratatui(style: syntect::highlighting::Style, base: Style) -> Style {
    let mut rendered = base.fg(to_ratatui_color(style.foreground));
    if style.font_style.contains(FontStyle::BOLD) {
        rendered = rendered.add_modifier(Modifier::BOLD);
    }
    if style.font_style.contains(FontStyle::ITALIC) {
        rendered = rendered.add_modifier(Modifier::ITALIC);
    }
    if style.font_style.contains(FontStyle::UNDERLINE) {
        rendered = rendered.add_modifier(Modifier::UNDERLINED);
    }
    rendered
}

fn to_ratatui_color(color: SyntectColor) -> Color {
    Color::Rgb(color.r, color.g, color.b)
}

fn trim_trailing_newline(spans: &mut Vec<Span<'static>>) {
    if let Some(last) = spans.last_mut() {
        if let Some(stripped) = last.content.strip_suffix('\n') {
            last.content = stripped.to_owned().into();
        }
    }
}

fn spans_text(spans: &[Span<'_>]) -> String {
    spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>()
}

#[cfg(test)]
mod tests {
    use ratatui::style::{Modifier, Style};

    use super::{render_markdown_message, render_markdown_message_with_headings};
    use crate::tui::theme::Theme;

    fn mapped_text(source: &str, range: std::ops::Range<usize>) -> Vec<String> {
        use crate::search_match::{DocumentMap, RenderOrigin, SourceBlock, SourceId, SourceRange};
        let rendered = render_markdown_message_with_headings(
            source,
            &Theme::default(),
            Style::default(),
            None,
        );
        let id = SourceId::new(SourceBlock::Message(0), "content");
        let mut map = DocumentMap::from_text(&rendered.text);
        map.origins = rendered
            .origins
            .into_iter()
            .map(|origin| RenderOrigin {
                rendered: origin.rendered,
                source: SourceRange {
                    source: id.clone(),
                    range: origin.source,
                },
            })
            .collect();
        assert!(map
            .origins
            .iter()
            .all(|origin| origin.rendered.end <= map.plain.len()));
        map.project(&SourceRange { source: id, range })
            .into_iter()
            .map(|range| map.plain[range].to_owned())
            .collect()
    }

    #[test]
    fn provenance_decodes_entities_and_escaped_punctuation_at_exact_positions() {
        let source = "same **same** &amp; \\* &copy; [same](https://same.invalid)";
        assert_eq!(mapped_text(source, 7..11), ["same"]);
        let entity = source.find("&amp;").unwrap();
        assert_eq!(mapped_text(source, entity..entity + 5), ["&"]);
        let escaped = source.find("\\*").unwrap();
        assert_eq!(mapped_text(source, escaped..escaped + 2), ["*"]);
        let entity = source.find("&copy;").unwrap();
        assert_eq!(mapped_text(source, entity..entity + 6), ["©"]);
        let url = source.find("https://").unwrap();
        assert!(mapped_text(source, url..source.len() - 1).is_empty());
    }

    #[test]
    fn provenance_preserves_inline_code_normalization_and_control_removal() {
        let source = "` alpha\nbeta `\n\n```text\na\t\x1b[31m界\x1b[0m\n```";
        let alpha = source.find("alpha").unwrap();
        assert_eq!(mapped_text(source, alpha..alpha + 10), ["alpha beta"]);
        let tab = source.find('\t').unwrap();
        assert_eq!(mapped_text(source, tab..tab + 1), ["    "]);
        let escape = source.find("\x1b[31m").unwrap();
        assert!(mapped_text(source, escape..escape + 5).is_empty());
        let wide = source.find('界').unwrap();
        assert_eq!(mapped_text(source, wide..wide + 3), ["界"]);
        let newline_padded = "`\nalpha\n`";
        assert_eq!(
            mapped_text(newline_padded, 2..7),
            ["alpha"],
            "{:?}",
            pulldown_cmark::Parser::new(newline_padded)
                .into_offset_iter()
                .collect::<Vec<_>>()
        );
        let crlf_padded = "`\r\nalpha\r\n`";
        assert_eq!(
            mapped_text(crlf_padded, 3..8),
            ["alpha"],
            "{:?}",
            pulldown_cmark::Parser::new(crlf_padded)
                .into_offset_iter()
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn provenance_keeps_repeated_words_distinct_across_style_events() {
        let source = "same **same** _same_";
        let first = mapped_text(source, 0..4);
        let second = mapped_text(source, 7..11);
        let third = mapped_text(source, 15..19);
        assert_eq!(first, ["same"]);
        assert_eq!(second, ["same"]);
        assert_eq!(third, ["same"]);
        let rendered = render_markdown_message_with_headings(
            source,
            &Theme::default(),
            Style::default(),
            None,
        );
        let positions: Vec<_> = rendered
            .origins
            .iter()
            .filter(|origin| origin.source == (7..11))
            .map(|origin| origin.rendered.clone())
            .collect();
        assert_eq!(positions.as_slice(), std::slice::from_ref(&(5..9)));
    }

    #[test]
    fn provenance_handles_indented_code_and_multiline_html_as_logical_lines() {
        let source = "    alpha\n    beta\n\n<div>\nalpha\n</div>\n";
        let beta = source.find("beta").unwrap();
        assert_eq!(mapped_text(source, beta..beta + 4), ["beta"]);
        let html_alpha = source.rfind("alpha").unwrap();
        assert_eq!(mapped_text(source, html_alpha..html_alpha + 5), ["alpha"]);
        let rendered = render_markdown_message_with_headings(
            source,
            &Theme::default(),
            Style::default(),
            None,
        );
        assert!(rendered
            .text
            .lines
            .iter()
            .all(|line| line.spans.iter().all(|span| !span.content.contains('\n'))));
    }

    #[test]
    fn renders_basic_markdown_without_literal_markup() {
        let theme = Theme::default();
        let base = Style::default().fg(theme.text).bg(theme.bubble_user);
        let text = render_markdown_message(
            "# Title\n\nSome **bold** and _italic_ text.",
            &theme,
            base,
            None,
        );

        let rendered = text
            .lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>();

        assert_eq!(rendered, vec!["Title", "", "Some bold and italic text."]);

        let body_line = &text.lines[2];
        assert!(body_line.spans.iter().any(|span| {
            span.content.as_ref() == "bold" && span.style.add_modifier.contains(Modifier::BOLD)
        }));
        assert!(body_line.spans.iter().any(|span| {
            span.content.as_ref() == "italic" && span.style.add_modifier.contains(Modifier::ITALIC)
        }));
    }

    #[test]
    fn fenced_code_blocks_render_code_without_fence_markers() {
        let theme = Theme::default();
        let base = Style::default().fg(theme.text).bg(theme.bubble_claude);
        let text = render_markdown_message("```rust\nfn alpha() {}\n```", &theme, base, None);

        let rendered = text
            .lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>();

        assert_eq!(rendered, vec!["fn alpha() {}"]);
        assert!(text.lines[0].spans.len() > 1);
    }

    #[test]
    fn query_highlighting_preserves_existing_markdown_modifiers() {
        let theme = Theme::default();
        let base = Style::default().fg(theme.text).bg(theme.bubble_user);
        let text = render_markdown_message("**alpha** beta", &theme, base, Some("alpha"));

        let alpha = text.lines[0]
            .spans
            .iter()
            .find(|span| span.content.as_ref() == "alpha")
            .expect("alpha span");

        assert!(alpha.style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(alpha.style.fg, Some(theme.text));
        assert_eq!(alpha.style.bg, Some(theme.search_match_bg));
    }

    #[test]
    fn query_highlighting_overrides_markdown_foreground() {
        let theme = Theme::default();
        let base = Style::default().fg(theme.text).bg(theme.bubble_user);
        let text = render_markdown_message("# alpha", &theme, base, Some("alpha"));

        let alpha = text.lines[0]
            .spans
            .iter()
            .find(|span| span.content.as_ref() == "alpha")
            .expect("alpha span");

        assert!(alpha.style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(alpha.style.fg, Some(theme.text));
        assert_eq!(alpha.style.bg, Some(theme.search_match_bg));
    }

    #[test]
    fn query_highlighting_overrides_code_syntax_foreground() {
        let theme = Theme::default();
        let base = Style::default().fg(theme.text).bg(theme.bubble_claude);
        let text = render_markdown_message("```rust\nfn alpha() {}\n```", &theme, base, Some("fn"));

        let keyword = text.lines[0]
            .spans
            .iter()
            .find(|span| span.content.as_ref() == "fn")
            .expect("fn span");

        assert_eq!(keyword.style.fg, Some(theme.text));
        assert_eq!(keyword.style.bg, Some(theme.search_match_bg));
    }

    #[test]
    fn records_markdown_heading_line_indices_for_sticky_subjects() {
        let theme = Theme::default();
        let base = Style::default().fg(theme.text).bg(theme.bubble_user);
        let rendered =
            render_markdown_message_with_headings("Intro\n\n## Plan\n\nBody", &theme, base, None);

        assert_eq!(rendered.headings.len(), 1);
        assert_eq!(rendered.headings[0].line_index, 2);
        assert_eq!(rendered.headings[0].text, "Plan");
        assert_eq!(rendered.headings[0].level, 2);
        assert_eq!(rendered.headings[0].breadcrumb, "Plan");
    }

    #[test]
    fn nested_headings_build_full_breadcrumb_path() {
        let theme = Theme::default();
        let base = Style::default().fg(theme.text).bg(theme.bubble_user);
        let src = "# Top\n\n## Section A\n\n### Sub 1\n\nbody\n\n### Sub 2\n\n## Section B\n\nbody";
        let rendered = render_markdown_message_with_headings(src, &theme, base, None);

        let crumbs: Vec<&str> = rendered
            .headings
            .iter()
            .map(|h| h.breadcrumb.as_str())
            .collect();
        assert_eq!(
            crumbs,
            vec![
                "Top",
                "Top \u{203a} Section A",
                "Top \u{203a} Section A \u{203a} Sub 1",
                "Top \u{203a} Section A \u{203a} Sub 2",
                "Top \u{203a} Section B",
            ]
        );
    }

    #[test]
    fn skipping_a_heading_level_keeps_breadcrumb_consistent() {
        let theme = Theme::default();
        let base = Style::default().fg(theme.text).bg(theme.bubble_user);
        // H1 -> H3 (skip H2). The H3 should still hang off H1.
        let rendered =
            render_markdown_message_with_headings("# Top\n\n### Deep\n\nbody", &theme, base, None);
        assert_eq!(rendered.headings[1].breadcrumb, "Top \u{203a} Deep");
    }

    #[test]
    fn dropping_back_to_higher_level_pops_intermediate_ancestors() {
        let theme = Theme::default();
        let base = Style::default().fg(theme.text).bg(theme.bubble_user);
        let src = "# Top\n\n## Section\n\n### Detail\n\n# Top Two";
        let rendered = render_markdown_message_with_headings(src, &theme, base, None);
        // Last heading is a fresh H1 — its breadcrumb should be just itself.
        assert_eq!(rendered.headings.last().unwrap().breadcrumb, "Top Two");
    }

    #[test]
    fn unknown_fence_language_falls_back_to_plain_code_styling() {
        let theme = Theme::default();
        let base = Style::default().fg(theme.text).bg(theme.bubble_claude);
        let text =
            render_markdown_message("```not-a-lang\nalpha\n```", &theme, base, Some("alpha"));

        assert_eq!(text.lines.len(), 1);
        let alpha = text.lines[0]
            .spans
            .iter()
            .find(|span| span.content.as_ref() == "alpha")
            .expect("alpha span");

        assert_eq!(alpha.style.fg, Some(theme.text));
        assert_eq!(alpha.style.bg, Some(theme.search_match_bg));
    }
}
