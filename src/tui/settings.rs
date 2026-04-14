use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use ratatui::Frame;
use tui_input::backend::crossterm::EventHandler;
use tui_input::Input;
use ratatui_textarea::TextArea;
use unicode_width::UnicodeWidthStr;

use crate::ring_cursor::RingCursor;
use crate::settings::{Settings, ThemeName};
use crate::summary::SummarizeBackend;
use crate::tui::keymap_hint;
use crate::tui::layout;
use crate::tui::theme::Theme;
use crate::tui::util::block_title;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SettingsField {
    Theme,
    SessionSeparator,
    SnippetLineCount,
    ClaudeCommand,
    CodexCommand,
    SummarizeBackend,
    SummarizeCommandCustom,
    SummarizePrompt,
}

#[derive(Debug, Clone)]
pub struct SettingsModalState {
    field: RingCursor<SettingsField>,
    theme: RingCursor<ThemeName>,
    separator_input: Input,
    snippet_line_count_input: Input,
    claude_input: Input,
    codex_input: Input,
    backend: RingCursor<SummarizeBackend>,
    custom_command_textarea: TextArea<'static>,
    prompt_textarea: TextArea<'static>,
    base: Settings,
}

#[derive(Debug, Clone)]
pub enum SettingsOutcome {
    Stay,
    Close,
    Apply(Settings),
}

impl SettingsModalState {
    pub fn new(settings: &Settings) -> Self {
        Self {
            field: settings_field_cursor(SettingsField::Theme),
            theme: theme_name_cursor(settings.theme),
            separator_input: Input::default().with_value(settings.session_separator.clone()),
            snippet_line_count_input: Input::default()
                .with_value(settings.snippet_line_count.to_string()),
            claude_input: Input::default().with_value(settings.claude_command.clone()),
            codex_input: Input::default().with_value(settings.codex_command.clone()),
            backend: backend_cursor(settings.summarize_backend),
            custom_command_textarea: build_textarea(&settings.summarize_command_custom),
            prompt_textarea: build_textarea(&settings.summarize_prompt),
            base: settings.clone(),
        }
    }

    pub fn current_theme(&self) -> ThemeName {
        *self.theme.current()
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> SettingsOutcome {
        match key.code {
            KeyCode::Esc => return SettingsOutcome::Close,
            KeyCode::Enter if key.modifiers.is_empty() => {
                return SettingsOutcome::Apply(self.build_settings());
            }
            KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                return SettingsOutcome::Apply(self.build_settings());
            }
            KeyCode::Tab | KeyCode::Down => {
                self.field.move_next();
                return SettingsOutcome::Stay;
            }
            KeyCode::BackTab | KeyCode::Up => {
                self.field.move_prev();
                return SettingsOutcome::Stay;
            }
            _ => {}
        }

        match *self.field.current() {
            SettingsField::Theme => match key.code {
                KeyCode::Left | KeyCode::Char('h') => {
                    self.theme.move_prev();
                }
                KeyCode::Right | KeyCode::Char('l') => {
                    self.theme.move_next();
                }
                _ => {}
            },
            SettingsField::SessionSeparator => {
                self.separator_input.handle_event(&Event::Key(key));
            }
            SettingsField::SnippetLineCount => {
                self.snippet_line_count_input.handle_event(&Event::Key(key));
            }
            SettingsField::ClaudeCommand => {
                self.claude_input.handle_event(&Event::Key(key));
            }
            SettingsField::CodexCommand => {
                self.codex_input.handle_event(&Event::Key(key));
            }
            SettingsField::SummarizeBackend => match key.code {
                KeyCode::Left | KeyCode::Char('h') => {
                    self.backend.move_prev();
                }
                KeyCode::Right | KeyCode::Char('l') => {
                    self.backend.move_next();
                }
                _ => {}
            },
            SettingsField::SummarizeCommandCustom => {
                self.custom_command_textarea.input(key);
            }
            SettingsField::SummarizePrompt => {
                self.prompt_textarea.input(key);
            }
        }
        SettingsOutcome::Stay
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let popup = layout::centered_rect(area, 72, 85);
        frame.render_widget(Clear, popup);

        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(theme.border_style(true))
            .title(block_title("Settings"));
        let inner = block.inner(popup);
        frame.render_widget(block, popup);

        let rows = Layout::vertical([
            Constraint::Length(1), // 0  padding
            Constraint::Length(1), // 1  Theme inline (label + radio)
            Constraint::Length(1), // 2  spacing
            Constraint::Length(1), // 3  Session Separator label
            Constraint::Length(1), // 4  Session Separator input
            Constraint::Length(1), // 5  spacing
            Constraint::Length(1), // 6  Snippet Lines label
            Constraint::Length(1), // 7  Snippet Lines input
            Constraint::Length(1), // 8  spacing
            Constraint::Length(1), // 9  divider
            Constraint::Length(1), // 10 spacing
            Constraint::Length(1), // 11 Claude Command label
            Constraint::Length(1), // 12 Claude Command input
            Constraint::Length(1), // 13 spacing
            Constraint::Length(1), // 14 Codex Command label
            Constraint::Length(1), // 15 Codex Command input
            Constraint::Length(1), // 16 spacing
            Constraint::Length(1), // 17 Session summarizer inline (label + radio)
            Constraint::Length(1), // 18 spacing
            Constraint::Length(4), // 19 custom command textarea
            Constraint::Length(1), // 20 spacing
            Constraint::Length(1), // 21 Session summarizer prompt label
            Constraint::Min(3),    // 22 prompt textarea (absorbs remaining)
            Constraint::Length(1), // 23 divider
            Constraint::Length(1), // 24 hints
        ])
        .split(inner);

        // Theme inline (label + radio)
        let theme_focused = self.field == SettingsField::Theme;
        let theme_line = self.render_inline_theme_row(rows[1].width, theme, theme_focused);
        frame.render_widget(Paragraph::new(theme_line), rows[1]);

        // Session Separator
        let sep_focused = self.field == SettingsField::SessionSeparator;
        render_field_label(frame, rows[3], theme, "Session Separator", sep_focused);
        self.render_text_input(frame, rows[4], theme, &self.separator_input, sep_focused);

        // Snippet Lines
        let snip_focused = self.field == SettingsField::SnippetLineCount;
        render_field_label(frame, rows[6], theme, "Snippet Lines", snip_focused);
        self.render_text_input(
            frame,
            rows[7],
            theme,
            &self.snippet_line_count_input,
            snip_focused,
        );

        // Divider between display prefs and command/summarize fields
        frame.render_widget(
            Paragraph::new("─".repeat(inner.width as usize))
                .style(Style::default().fg(theme.focus_border)),
            rows[9],
        );

        // Claude Code Command
        let claude_focused = self.field == SettingsField::ClaudeCommand;
        render_field_label(frame, rows[11], theme, "Claude Code Command", claude_focused);
        self.render_text_input(frame, rows[12], theme, &self.claude_input, claude_focused);

        // Codex Command
        let codex_focused = self.field == SettingsField::CodexCommand;
        render_field_label(frame, rows[14], theme, "Codex Command", codex_focused);
        self.render_text_input(frame, rows[15], theme, &self.codex_input, codex_focused);

        // Session summarizer backend (inline label + radio)
        let backend_focused = self.field == SettingsField::SummarizeBackend;
        let backend_line =
            self.render_inline_backend_row(rows[17].width, theme, backend_focused);
        frame.render_widget(Paragraph::new(backend_line), rows[17]);

        // Custom command textarea
        let custom_focused = self.field == SettingsField::SummarizeCommandCustom;
        style_textarea(&mut self.custom_command_textarea, theme, custom_focused);
        frame.render_widget(&self.custom_command_textarea, pad_rect(rows[19]));

        // Prompt label + textarea
        let prompt_focused = self.field == SettingsField::SummarizePrompt;
        render_field_label(
            frame,
            rows[21],
            theme,
            "Session summarizer prompt",
            prompt_focused,
        );
        style_textarea(&mut self.prompt_textarea, theme, prompt_focused);
        frame.render_widget(&self.prompt_textarea, pad_rect(rows[22]));

        // Bottom divider + hints
        frame.render_widget(
            Paragraph::new("─".repeat(inner.width as usize))
                .style(Style::default().fg(theme.focus_border)),
            rows[23],
        );
        const HINTS: [keymap_hint::KeymapHint; 4] = [
            keymap_hint::KeymapHint::new("Tab/↑↓", "navigate"),
            keymap_hint::KeymapHint::new("←→", "change value"),
            keymap_hint::KeymapHint::new("⏎/^S", "save"),
            keymap_hint::KeymapHint::new("Esc", "cancel"),
        ];
        keymap_hint::render(frame, rows[24], &HINTS, theme, "");
    }

    fn render_inline_theme_row(&self, width: u16, theme: &Theme, focused: bool) -> Line<'static> {
        inline_radio_row(
            "Theme",
            &ThemeName::ALL
                .iter()
                .map(|name| name.label().to_owned())
                .collect::<Vec<_>>(),
            self.theme.index(),
            width,
            theme,
            focused,
        )
    }

    fn render_inline_backend_row(
        &self,
        width: u16,
        theme: &Theme,
        focused: bool,
    ) -> Line<'static> {
        let labels: Vec<String> = BACKEND_ORDER
            .iter()
            .map(|b| backend_label(*b).to_owned())
            .collect();
        inline_radio_row(
            "Session summarizer",
            &labels,
            self.backend.index(),
            width,
            theme,
            focused,
        )
    }

    #[cfg(test)]
    fn render_theme_selector(&self, width: u16, theme: &Theme, focused: bool) -> Line<'static> {
        let indent = "  ";
        let sep = "  ";
        let arrow_left = " < ";
        let arrow_right = " > ";
        let selected_index = self.theme.index();
        let entries: Vec<String> = ThemeName::ALL
            .iter()
            .map(|name| {
                let marker = if self.theme == *name { "● " } else { "○ " };
                format!("{marker}{}", name.label())
            })
            .collect();

        let available_width = width as usize;
        let indent_w = UnicodeWidthStr::width(indent);
        let sep_w = UnicodeWidthStr::width(sep);
        let arrow_w = UnicodeWidthStr::width(arrow_left);
        let entry_width = |index: usize| UnicodeWidthStr::width(entries[index].as_str());

        let visible_width = available_width.saturating_sub(indent_w);
        let mut start = selected_index;
        let mut end = selected_index + 1;
        let mut total_w = arrow_w * 2 + entry_width(selected_index);

        loop {
            let mut grew = false;

            if start > 0 {
                let candidate_w = sep_w + entry_width(start - 1);
                if total_w + candidate_w <= visible_width {
                    start -= 1;
                    total_w += candidate_w;
                    grew = true;
                }
            }

            if end < entries.len() {
                let candidate_w = sep_w + entry_width(end);
                if total_w + candidate_w <= visible_width {
                    total_w += candidate_w;
                    end += 1;
                    grew = true;
                }
            }

            if !grew {
                break;
            }
        }

        let active_arrow = if focused {
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.muted)
        };
        let inactive_arrow = Style::default().fg(theme.muted_greater);
        let separator_style = Style::default().fg(theme.muted_greater);

        let mut spans = vec![Span::raw(indent)];
        spans.push(Span::styled(
            arrow_left,
            if start > 0 {
                active_arrow
            } else {
                inactive_arrow
            },
        ));

        for index in start..end {
            if index > start {
                spans.push(Span::styled(sep, separator_style));
            }

            let is_selected = index == selected_index;
            let style = if is_selected && focused {
                Style::default()
                    .fg(theme.text)
                    .bg(theme.selection)
                    .add_modifier(Modifier::BOLD)
            } else if is_selected {
                Style::default().fg(theme.text).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.muted)
            };
            spans.push(Span::styled(entries[index].clone(), style));
        }

        spans.push(Span::styled(
            arrow_right,
            if end < entries.len() {
                active_arrow
            } else {
                inactive_arrow
            },
        ));

        Line::from(spans)
    }

    fn render_text_input(
        &self,
        frame: &mut Frame,
        area: Rect,
        theme: &Theme,
        input: &Input,
        focused: bool,
    ) {
        let padded = Rect {
            x: area.x + 2,
            width: area.width.saturating_sub(4),
            ..area
        };

        let style = if focused {
            Style::default()
                .fg(theme.settings_input_fg(true))
                .bg(theme.settings_input_bg(true))
        } else {
            Style::default()
                .fg(theme.settings_input_fg(false))
                .bg(theme.settings_input_bg(false))
        };

        let display_value = input.value();
        let prefix = if focused { "▎" } else { " " };
        let text = format!("{prefix}{display_value}");
        frame.render_widget(Paragraph::new(text).style(style), padded);

        if focused {
            let cursor_x = padded.x + 1 + input.visual_cursor() as u16;
            if cursor_x < padded.right() {
                frame.set_cursor_position((cursor_x, padded.y));
            }
        }
    }

    fn build_settings(&self) -> Settings {
        let snippet_line_count = self
            .snippet_line_count_input
            .value()
            .parse::<usize>()
            .unwrap_or(self.base.snippet_line_count);
        Settings {
            theme: *self.theme.current(),
            claude_command: self.claude_input.value().to_owned(),
            codex_command: self.codex_input.value().to_owned(),
            session_separator: self.separator_input.value().to_owned(),
            snippet_line_count,
            summarize_backend: *self.backend.current(),
            summarize_command_custom: self.custom_command_textarea.lines().join("\n"),
            summarize_prompt: self.prompt_textarea.lines().join("\n"),
            ..self.base.clone()
        }
    }
}

fn settings_field_cursor(selected: SettingsField) -> RingCursor<SettingsField> {
    let mut cursor = RingCursor::new(vec![
        SettingsField::Theme,
        SettingsField::SessionSeparator,
        SettingsField::SnippetLineCount,
        SettingsField::ClaudeCommand,
        SettingsField::CodexCommand,
        SettingsField::SummarizeBackend,
        SettingsField::SummarizeCommandCustom,
        SettingsField::SummarizePrompt,
    ]);
    assert!(cursor.set(&selected));
    cursor
}

fn theme_name_cursor(selected: ThemeName) -> RingCursor<ThemeName> {
    let mut cursor = RingCursor::new(ThemeName::ALL.to_vec());
    assert!(cursor.set(&selected));
    cursor
}

const BACKEND_ORDER: [SummarizeBackend; 3] = [
    SummarizeBackend::Claude,
    SummarizeBackend::Codex,
    SummarizeBackend::Custom,
];

fn backend_cursor(selected: SummarizeBackend) -> RingCursor<SummarizeBackend> {
    let mut cursor = RingCursor::new(BACKEND_ORDER.to_vec());
    assert!(cursor.set(&selected));
    cursor
}

fn backend_label(backend: SummarizeBackend) -> &'static str {
    match backend {
        SummarizeBackend::Claude => "Claude",
        SummarizeBackend::Codex => "Codex",
        SummarizeBackend::Custom => "Custom",
    }
}

fn build_textarea(content: &str) -> TextArea<'static> {
    let mut textarea: TextArea<'static> = if content.is_empty() {
        TextArea::default()
    } else {
        content.split('\n').map(|s| s.to_owned()).collect()
    };
    textarea.set_cursor_line_style(Style::default());
    textarea
}

fn pad_rect(area: Rect) -> Rect {
    Rect {
        x: area.x.saturating_add(2),
        width: area.width.saturating_sub(4),
        ..area
    }
}

fn render_field_label(
    frame: &mut Frame,
    area: Rect,
    theme: &Theme,
    text: &str,
    focused: bool,
) {
    let style = if focused {
        Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(theme.muted)
            .add_modifier(Modifier::BOLD)
    };
    let content = format!("  {text}");
    frame.render_widget(Paragraph::new(content).style(style), area);
}

fn style_textarea(textarea: &mut TextArea<'_>, theme: &Theme, focused: bool) {
    let fg = theme.settings_input_fg(focused);
    let bg = theme.settings_input_bg(focused);
    let base = Style::default().fg(fg).bg(bg);
    textarea.set_style(base);
    textarea.set_cursor_line_style(base);
    textarea.set_block(Block::default().style(base));
    if focused {
        textarea.set_cursor_style(Style::default().add_modifier(Modifier::REVERSED));
    } else {
        textarea.set_cursor_style(base);
    }
}

fn inline_radio_row(
    label: &str,
    items: &[String],
    selected: usize,
    width: u16,
    theme: &Theme,
    focused: bool,
) -> Line<'static> {
    let label_text = format!("  {label}");
    let label_w = UnicodeWidthStr::width(label_text.as_str());
    let label_style = if focused {
        Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(theme.muted)
            .add_modifier(Modifier::BOLD)
    };

    let gap = 2usize;
    let radio_width = (width as usize).saturating_sub(label_w + gap);
    let radio = radio_spans(items, selected, radio_width, theme, focused);

    let mut spans: Vec<Span<'static>> = Vec::with_capacity(2 + radio.len());
    spans.push(Span::styled(label_text, label_style));
    spans.push(Span::raw(" ".repeat(gap)));
    spans.extend(radio);
    Line::from(spans)
}

fn radio_spans(
    items: &[String],
    selected: usize,
    width: usize,
    theme: &Theme,
    focused: bool,
) -> Vec<Span<'static>> {
    if items.is_empty() || width == 0 {
        return Vec::new();
    }
    let entries: Vec<String> = items
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let marker = if i == selected { "● " } else { "○ " };
            format!("{marker}{item}")
        })
        .collect();
    let sep = "  ";
    let arrow_left = "< ";
    let arrow_right = " >";
    let sep_w = UnicodeWidthStr::width(sep);
    let arrow_w = UnicodeWidthStr::width(arrow_left);
    let entry_width = |i: usize| UnicodeWidthStr::width(entries[i].as_str());

    let mut start = selected;
    let mut end = selected + 1;
    let mut total_w = arrow_w * 2 + entry_width(selected);

    loop {
        let mut grew = false;
        if start > 0 {
            let cw = sep_w + entry_width(start - 1);
            if total_w + cw <= width {
                start -= 1;
                total_w += cw;
                grew = true;
            }
        }
        if end < entries.len() {
            let cw = sep_w + entry_width(end);
            if total_w + cw <= width {
                total_w += cw;
                end += 1;
                grew = true;
            }
        }
        if !grew {
            break;
        }
    }

    let active_arrow = if focused {
        Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.muted)
    };
    let inactive_arrow = Style::default().fg(theme.muted_greater);
    let separator_style = Style::default().fg(theme.muted_greater);

    let mut spans: Vec<Span<'static>> = Vec::new();
    spans.push(Span::styled(
        arrow_left,
        if start > 0 { active_arrow } else { inactive_arrow },
    ));

    for i in start..end {
        if i > start {
            spans.push(Span::styled(sep, separator_style));
        }
        let is_selected = i == selected;
        let style = if is_selected && focused {
            Style::default()
                .fg(theme.text)
                .bg(theme.selection)
                .add_modifier(Modifier::BOLD)
        } else if is_selected {
            Style::default().fg(theme.text).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.muted)
        };
        spans.push(Span::styled(entries[i].clone(), style));
    }

    spans.push(Span::styled(
        arrow_right,
        if end < entries.len() {
            active_arrow
        } else {
            inactive_arrow
        },
    ));

    spans
}

#[cfg(test)]
mod tests {
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::style::Style;

    use super::{SettingsField, SettingsModalState, SettingsOutcome};
    use crate::settings::{Settings, ThemeName};
    use crate::tui::theme::Theme;

    #[test]
    fn enter_applies_settings_from_theme_field() {
        let mut state = SettingsModalState::new(&Settings::default());
        assert!(state.theme.set(&ThemeName::Sunset));
        assert!(state.field.set(&SettingsField::Theme));

        let outcome = state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));

        match outcome {
            SettingsOutcome::Apply(settings) => {
                assert_eq!(settings.theme, ThemeName::Sunset);
            }
            other => panic!("expected apply outcome, got {other:?}"),
        }
    }

    #[test]
    fn theme_selector_shows_all_items_when_width_allows() {
        let state = SettingsModalState::new(&Settings::default());

        let rendered = spans_text(&state.render_theme_selector(80, &Theme::default(), false));

        assert_eq!(rendered, "   < ● lazygit  ○ aics  ○ sunset  ○ late.sh > ");
    }

    #[test]
    fn theme_selector_shows_offscreen_arrows_when_width_is_tight() {
        let mut settings = Settings::default();
        settings.theme = ThemeName::Sunset;
        let state = SettingsModalState::new(&settings);
        let theme = Theme::default();

        let rendered = state.render_theme_selector(26, &theme, true);

        assert_eq!(spans_text(&rendered), "   < ○ aics  ● sunset > ");
        assert_eq!(rendered.spans[1].style, active_arrow_style(&theme));
        assert_eq!(rendered.spans[5].style, active_arrow_style(&theme));
    }

    #[test]
    fn theme_selector_dims_arrows_when_no_items_are_hidden() {
        let state = SettingsModalState::new(&Settings::default());
        let theme = Theme::default();

        let rendered = state.render_theme_selector(80, &theme, true);

        assert_eq!(
            rendered.spans[1].style,
            Style::default().fg(theme.muted_greater)
        );
        assert_eq!(
            rendered.spans[9].style,
            Style::default().fg(theme.muted_greater)
        );
    }

    fn spans_text(line: &ratatui::text::Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>()
    }

    fn active_arrow_style(theme: &Theme) -> Style {
        Style::default()
            .fg(theme.accent)
            .add_modifier(ratatui::style::Modifier::BOLD)
    }
}
