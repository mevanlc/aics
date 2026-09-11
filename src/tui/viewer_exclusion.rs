use ratatui::crossterm::event::{
    KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

use super::theme::Theme;
use crate::settings::ViewerFilterExclusion;

#[derive(Debug, Default)]
pub(crate) struct ViewerExclusionDialog {
    close: bool,
    remember: bool,
}

impl ViewerExclusionDialog {
    fn choice(&self) -> (ViewerFilterExclusion, bool) {
        (
            if self.close {
                ViewerFilterExclusion::Close
            } else {
                ViewerFilterExclusion::Keep
            },
            self.remember,
        )
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Option<(ViewerFilterExclusion, bool)> {
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return None;
        }
        match key.code {
            KeyCode::Esc => return Some((ViewerFilterExclusion::Keep, false)),
            KeyCode::Tab | KeyCode::BackTab => self.close = !self.close,
            KeyCode::Enter => return Some(self.choice()),
            KeyCode::Char('k' | 'K') => return Some((ViewerFilterExclusion::Keep, self.remember)),
            KeyCode::Char('c' | 'C') => return Some((ViewerFilterExclusion::Close, self.remember)),
            KeyCode::Char(' ' | 'r' | 'R') => self.remember = !self.remember,
            _ => {}
        }
        None
    }

    pub fn handle_mouse(
        &mut self,
        area: Rect,
        mouse: MouseEvent,
    ) -> Option<(ViewerFilterExclusion, bool)> {
        if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
            return None;
        }
        let (_, _, buttons, checkbox) = geometry(area);
        let position = (mouse.column, mouse.row).into();
        if checkbox.contains(position) {
            self.remember = !self.remember;
        }
        for (index, button) in buttons.iter().enumerate() {
            if button.contains(position) {
                self.close = index == 0;
                return Some(self.choice());
            }
        }
        None
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let (popup, message, buttons, checkbox) = geometry(area);
        frame.render_widget(Clear, popup);
        frame.render_widget(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Thick)
                .border_style(theme.border_style(true)),
            popup,
        );
        frame.render_widget(
            Paragraph::new("Your updated filter settings have excluded this session.")
                .centered()
                .wrap(Wrap { trim: true })
                .style(Style::default().fg(theme.text)),
            message,
        );
        for (index, label) in ["[ Close session ]", "[ Keep session open ]"]
            .iter()
            .enumerate()
        {
            let selected = (index == 0) == self.close;
            let style = if selected {
                Style::default()
                    .fg(theme.text)
                    .bg(theme.selection)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.muted)
            };
            frame.render_widget(
                Paragraph::new(*label).centered().style(style),
                buttons[index],
            );
        }
        frame.render_widget(
            Paragraph::new(format!(
                "[{}] Remember my choice",
                if self.remember { 'x' } else { ' ' }
            ))
            .style(Style::default().fg(theme.text)),
            checkbox,
        );
    }
}

fn geometry(area: Rect) -> (Rect, Rect, [Rect; 2], Rect) {
    let width = area.width.min(64);
    let height = area.height.min(11);
    let popup = Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    );
    let inner = Block::default().borders(Borders::ALL).inner(popup);
    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(2),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(2),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .split(inner);
    let buttons =
        Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).split(rows[3]);
    let checkbox = Rect::new(
        inner.x + inner.width.saturating_sub(23) / 2,
        rows[5].y,
        inner.width.min(23),
        rows[5].height,
    );
    (popup, rows[1], [buttons[0], buttons[1]], checkbox)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn dialog_keyboard_contract() {
        let mut dialog = ViewerExclusionDialog::default();
        assert_eq!(
            dialog.handle_key(key(KeyCode::Enter)),
            Some((ViewerFilterExclusion::Keep, false))
        );
        dialog.handle_key(key(KeyCode::Tab));
        dialog.handle_key(key(KeyCode::Char(' ')));
        assert_eq!(
            dialog.handle_key(key(KeyCode::Enter)),
            Some((ViewerFilterExclusion::Close, true))
        );
        assert_eq!(
            dialog.handle_key(key(KeyCode::Esc)),
            Some((ViewerFilterExclusion::Keep, false))
        );
        dialog.handle_key(key(KeyCode::BackTab));
        assert_eq!(
            dialog.handle_key(key(KeyCode::Enter)),
            Some((ViewerFilterExclusion::Keep, true))
        );
        for ch in ['k', 'K'] {
            assert_eq!(
                dialog.handle_key(key(KeyCode::Char(ch))),
                Some((ViewerFilterExclusion::Keep, true))
            );
        }
        for ch in ['c', 'C'] {
            assert_eq!(
                dialog.handle_key(key(KeyCode::Char(ch))),
                Some((ViewerFilterExclusion::Close, true))
            );
        }
        for ch in ['r', 'R'] {
            let before = dialog.remember;
            assert!(dialog.handle_key(key(KeyCode::Char(ch))).is_none());
            assert_eq!(dialog.remember, !before);
        }
    }

    #[test]
    fn dialog_mouse_buttons_and_entire_checkbox_label() {
        let mut dialog = ViewerExclusionDialog::default();
        let area = Rect::new(0, 0, 100, 30);
        let (_, _, buttons, checkbox) = geometry(area);
        let click = |rect: Rect, offset| MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: rect.x + offset,
            row: rect.y,
            modifiers: KeyModifiers::NONE,
        };
        dialog.handle_mouse(area, click(checkbox, 1));
        assert!(dialog.remember);
        dialog.handle_mouse(area, click(checkbox, checkbox.width - 1));
        assert!(!dialog.remember);
        assert_eq!(
            dialog.handle_mouse(area, click(buttons[0], 3)),
            Some((ViewerFilterExclusion::Close, false))
        );
        assert_eq!(
            dialog.handle_mouse(area, click(buttons[1], 3)),
            Some((ViewerFilterExclusion::Keep, false))
        );
    }
}
