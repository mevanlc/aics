use std::sync::LazyLock;

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
use crate::tui::ansi::strip_terminal_escapes;
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
    MarkdownRenderer::new(theme, base_style, highlight_query).render(parser)
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
        }
    }

    fn render<'b>(mut self, parser: Parser<'b>) -> MarkdownRender {
        for event in parser {
            self.handle_event(event);
        }

        self.finish_line();
        if self.lines.last().is_some_and(|line| line.spans.is_empty()) {
            self.lines.pop();
        }

        MarkdownRender {
            text: Text::from(self.lines),
            headings: self.headings,
        }
    }

    fn handle_event<'b>(&mut self, event: Event<'b>) {
        if self.code_block.is_some() {
            match event {
                Event::Text(text) => self.push_code_text(text.as_ref()),
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
                self.push_text(text.as_ref(), self.current_style())
            }
            Event::Code(code) => self.push_text(code.as_ref(), self.inline_code_style()),
            Event::SoftBreak | Event::HardBreak => self.finish_line(),
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

        self.ensure_line_prefix();
        self.current_line.extend(highlight_spans_with_terms(
            &text,
            &self.terms,
            style,
            style.patch(self.search_highlight),
        ));
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
    }

    fn finish_line(&mut self) {
        if self.current_line.is_empty() {
            return;
        }

        let spans = std::mem::take(&mut self.current_line);
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
