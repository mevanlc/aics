use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use ratatui::Frame;
use tui_input::backend::crossterm::EventHandler;
use tui_input::Input;

use crate::settings::{Settings, ThemeName};
use crate::tui::layout;
use crate::tui::theme::Theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SettingsField {
    Theme,
    ClaudeCommand,
    CodexCommand,
}

impl SettingsField {
    fn next(self) -> Self {
        match self {
            Self::Theme => Self::ClaudeCommand,
            Self::ClaudeCommand => Self::CodexCommand,
            Self::CodexCommand => Self::Theme,
        }
    }

    fn prev(self) -> Self {
        match self {
            Self::Theme => Self::CodexCommand,
            Self::ClaudeCommand => Self::Theme,
            Self::CodexCommand => Self::ClaudeCommand,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SettingsModalState {
    field: SettingsField,
    theme: ThemeName,
    claude_input: Input,
    codex_input: Input,
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
            field: SettingsField::Theme,
            theme: settings.theme,
            claude_input: Input::default().with_value(settings.claude_command.clone()),
            codex_input: Input::default().with_value(settings.codex_command.clone()),
        }
    }

    pub fn current_theme(&self) -> ThemeName {
        self.theme
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

        match self.field {
            SettingsField::Theme => match key.code {
                KeyCode::Tab | KeyCode::Down => self.field = self.field.next(),
                KeyCode::BackTab | KeyCode::Up => self.field = self.field.prev(),
                KeyCode::Left | KeyCode::Char('h') => self.theme = self.theme.prev(),
                KeyCode::Right | KeyCode::Char('l') => self.theme = self.theme.next(),
                _ => {}
            },
            SettingsField::ClaudeCommand => match key.code {
                KeyCode::Tab | KeyCode::Down => self.field = self.field.next(),
                KeyCode::BackTab | KeyCode::Up => self.field = self.field.prev(),
                _ => {
                    self.claude_input.handle_event(&Event::Key(key));
                }
            },
            SettingsField::CodexCommand => match key.code {
                KeyCode::Tab | KeyCode::Down => self.field = self.field.next(),
                KeyCode::BackTab | KeyCode::Up => self.field = self.field.prev(),
                _ => {
                    self.codex_input.handle_event(&Event::Key(key));
                }
            },
        }
        SettingsOutcome::Stay
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let popup = layout::centered_rect(area, 56, 40);
        frame.render_widget(Clear, popup);

        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(theme.border_style(true))
            .title("Settings");
        let inner = block.inner(popup);
        frame.render_widget(block, popup);

        let rows = Layout::vertical([
            Constraint::Length(1), // padding
            Constraint::Length(1), // theme label
            Constraint::Length(1), // theme value
            Constraint::Length(1), // spacing
            Constraint::Length(1), // claude label
            Constraint::Length(1), // claude input
            Constraint::Length(1), // spacing
            Constraint::Length(1), // codex label
            Constraint::Length(1), // codex input
            Constraint::Length(1), // spacing
            Constraint::Length(1), // hint
            Constraint::Min(0),    // fill
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

        let theme_line = self.render_theme_selector(theme, theme_focused);
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

        // Hint
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "  Enter save · Ctrl+S save · Esc cancel",
                Style::default().fg(theme.muted),
            ))),
            rows[10],
        );
    }

    fn render_theme_selector(&self, theme: &Theme, focused: bool) -> Line<'static> {
        let mut spans = vec![Span::raw("  ")];
        for (i, name) in ThemeName::ALL.iter().enumerate() {
            if i > 0 {
                spans.push(Span::styled("  ", Style::default().fg(theme.muted)));
            }
            let is_selected = *name == self.theme;
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
            let marker = if is_selected { "● " } else { "○ " };
            spans.push(Span::styled(format!("{marker}{}", name.label()), style));
        }
        if focused {
            spans.push(Span::styled("  ◂▸", Style::default().fg(theme.muted)));
        }
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
        Settings {
            theme: self.theme,
            claude_command: self.claude_input.value().to_owned(),
            codex_command: self.codex_input.value().to_owned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::{SettingsField, SettingsModalState, SettingsOutcome};
    use crate::settings::{Settings, ThemeName};

    #[test]
    fn enter_applies_settings_from_theme_field() {
        let mut state = SettingsModalState::new(&Settings::default());
        state.theme = ThemeName::Sunset;
        state.field = SettingsField::Theme;

        let outcome = state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));

        match outcome {
            SettingsOutcome::Apply(settings) => {
                assert_eq!(settings.theme, ThemeName::Sunset);
            }
            other => panic!("expected apply outcome, got {other:?}"),
        }
    }
}
