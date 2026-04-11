use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use ratatui::Frame;
use tui_input::backend::crossterm::EventHandler;
use tui_input::Input;
use unicode_width::UnicodeWidthStr;

use crate::ring_cursor::RingCursor;
use crate::settings::{Settings, ThemeName};
use crate::tui::keymap_hint;
use crate::tui::layout;
use crate::tui::theme::Theme;
use crate::tui::util::block_title;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SettingsField {
    Theme,
    ClaudeCommand,
    CodexCommand,
    SessionSeparator,
    SnippetLineCount,
}

#[derive(Debug, Clone)]
pub struct SettingsModalState {
    field: RingCursor<SettingsField>,
    theme: RingCursor<ThemeName>,
    claude_input: Input,
    codex_input: Input,
    separator_input: Input,
    snippet_line_count_input: Input,
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
            claude_input: Input::default().with_value(settings.claude_command.clone()),
            codex_input: Input::default().with_value(settings.codex_command.clone()),
            separator_input: Input::default().with_value(settings.session_separator.clone()),
            snippet_line_count_input: Input::default()
                .with_value(settings.snippet_line_count.to_string()),
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
            _ => {}
        }

        match *self.field.current() {
            SettingsField::Theme => match key.code {
                KeyCode::Tab | KeyCode::Down => {
                    self.field.move_next();
                }
                KeyCode::BackTab | KeyCode::Up => {
                    self.field.move_prev();
                }
                KeyCode::Left | KeyCode::Char('h') => {
                    self.theme.move_prev();
                }
                KeyCode::Right | KeyCode::Char('l') => {
                    self.theme.move_next();
                }
                _ => {}
            },
            SettingsField::ClaudeCommand => match key.code {
                KeyCode::Tab | KeyCode::Down => {
                    self.field.move_next();
                }
                KeyCode::BackTab | KeyCode::Up => {
                    self.field.move_prev();
                }
                _ => {
                    self.claude_input.handle_event(&Event::Key(key));
                }
            },
            SettingsField::CodexCommand => match key.code {
                KeyCode::Tab | KeyCode::Down => {
                    self.field.move_next();
                }
                KeyCode::BackTab | KeyCode::Up => {
                    self.field.move_prev();
                }
                _ => {
                    self.codex_input.handle_event(&Event::Key(key));
                }
            },
            SettingsField::SessionSeparator => match key.code {
                KeyCode::Tab | KeyCode::Down => {
                    self.field.move_next();
                }
                KeyCode::BackTab | KeyCode::Up => {
                    self.field.move_prev();
                }
                _ => {
                    self.separator_input.handle_event(&Event::Key(key));
                }
            },
            SettingsField::SnippetLineCount => match key.code {
                KeyCode::Tab | KeyCode::Down => {
                    self.field.move_next();
                }
                KeyCode::BackTab | KeyCode::Up => {
                    self.field.move_prev();
                }
                _ => {
                    self.snippet_line_count_input.handle_event(&Event::Key(key));
                }
            },
        }
        SettingsOutcome::Stay
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let popup = layout::centered_rect(area, 56, 60);
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
            Constraint::Length(1), // 1  theme label
            Constraint::Length(1), // 2  theme value
            Constraint::Length(1), // 3  spacing
            Constraint::Length(1), // 4  claude label
            Constraint::Length(1), // 5  claude input
            Constraint::Length(1), // 6  spacing
            Constraint::Length(1), // 7  codex label
            Constraint::Length(1), // 8  codex input
            Constraint::Length(1), // 9  spacing
            Constraint::Length(1), // 10 separator label
            Constraint::Length(1), // 11 separator input
            Constraint::Length(1), // 12 spacing
            Constraint::Length(1), // 13 snippet lines label
            Constraint::Length(1), // 14 snippet lines input
            Constraint::Min(0),    // 15 fill
            Constraint::Length(1), // 16 separator line
            Constraint::Length(1), // 17 hint
        ])
        .split(inner);

        // Theme field
        let theme_focused = self.field == SettingsField::Theme;
        let label_style = if theme_focused {
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.muted)
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled("  Theme", label_style))),
            rows[1],
        );

        let theme_line = self.render_theme_selector(rows[2].width, theme, theme_focused);
        frame.render_widget(Paragraph::new(theme_line), rows[2]);

        // Claude command field
        let claude_focused = self.field == SettingsField::ClaudeCommand;
        let label_style = if claude_focused {
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.muted)
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "  Claude Code Command",
                label_style,
            ))),
            rows[4],
        );
        self.render_text_input(frame, rows[5], theme, &self.claude_input, claude_focused);

        // Codex command field
        let codex_focused = self.field == SettingsField::CodexCommand;
        let label_style = if codex_focused {
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.muted)
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled("  Codex Command", label_style))),
            rows[7],
        );
        self.render_text_input(frame, rows[8], theme, &self.codex_input, codex_focused);

        // Session separator field
        let sep_focused = self.field == SettingsField::SessionSeparator;
        let label_style = if sep_focused {
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.muted)
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled("  Session Separator", label_style))),
            rows[10],
        );
        self.render_text_input(frame, rows[11], theme, &self.separator_input, sep_focused);

        // Snippet lines field
        let snip_focused = self.field == SettingsField::SnippetLineCount;
        let label_style = if snip_focused {
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.muted)
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled("  Snippet Lines", label_style))),
            rows[13],
        );
        self.render_text_input(
            frame,
            rows[14],
            theme,
            &self.snippet_line_count_input,
            snip_focused,
        );

        // Separator + hint
        frame.render_widget(
            Paragraph::new("─".repeat(inner.width as usize))
                .style(Style::default().fg(theme.focus_border)),
            rows[16],
        );
        const HINTS: [keymap_hint::KeymapHint; 3] = [
            keymap_hint::KeymapHint::new("Tab/↑↓", "navigate"),
            keymap_hint::KeymapHint::new("⏎/^S", "save"),
            keymap_hint::KeymapHint::new("Esc", "cancel"),
        ];
        keymap_hint::render(frame, rows[17], &HINTS, theme, "");
    }

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
            Style::default().fg(theme.text)
        } else {
            Style::default().fg(theme.muted)
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
            ..self.base.clone()
        }
    }
}

fn settings_field_cursor(selected: SettingsField) -> RingCursor<SettingsField> {
    let mut cursor = RingCursor::new(vec![
        SettingsField::Theme,
        SettingsField::ClaudeCommand,
        SettingsField::CodexCommand,
        SettingsField::SessionSeparator,
        SettingsField::SnippetLineCount,
    ]);
    assert!(cursor.set(&selected));
    cursor
}

fn theme_name_cursor(selected: ThemeName) -> RingCursor<ThemeName> {
    let mut cursor = RingCursor::new(ThemeName::ALL.to_vec());
    assert!(cursor.set(&selected));
    cursor
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
