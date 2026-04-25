use std::path::PathBuf;

use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;
use ratatui_textarea::TextArea;
use serde::Deserialize;
use tui_input::backend::crossterm::EventHandler;
use tui_input::Input;
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
    ClaudeArgs,
    CodexCommand,
    CodexArgs,
    EditSummarizer,
}

#[derive(Debug, Clone)]
pub struct SettingsModalState {
    field: RingCursor<SettingsField>,
    theme: RingCursor<ThemeName>,
    separator_input: Input,
    snippet_line_count_input: Input,
    claude_command_input: Input,
    claude_args_input: Input,
    codex_command_input: Input,
    codex_args_input: Input,
    summarizer: Option<SummarizerModalState>,
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
            claude_command_input: Input::default().with_value(settings.claude_command.clone()),
            claude_args_input: Input::default().with_value(settings.claude_args.clone()),
            codex_command_input: Input::default().with_value(settings.codex_command.clone()),
            codex_args_input: Input::default().with_value(settings.codex_args.clone()),
            summarizer: None,
            base: settings.clone(),
        }
    }

    pub fn current_theme(&self) -> ThemeName {
        *self.theme.current()
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> SettingsOutcome {
        if let Some(summarizer) = self.summarizer.as_mut() {
            match summarizer.handle_key(key) {
                SummarizerOutcome::Stay => {}
                SummarizerOutcome::Cancel => {
                    self.summarizer = None;
                }
                SummarizerOutcome::Apply { command, prompt } => {
                    self.base.summarize_command = command;
                    self.base.summarize_prompt = prompt;
                    self.summarizer = None;
                }
            }
            return SettingsOutcome::Stay;
        }

        match key.code {
            KeyCode::Esc => return SettingsOutcome::Close,
            KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                return SettingsOutcome::Apply(self.build_settings());
            }
            KeyCode::Tab => {
                self.field.move_next();
                return SettingsOutcome::Stay;
            }
            KeyCode::BackTab => {
                self.field.move_prev();
                return SettingsOutcome::Stay;
            }
            KeyCode::Enter
                if key.modifiers.is_empty()
                    && *self.field.current() == SettingsField::EditSummarizer =>
            {
                self.summarizer = Some(SummarizerModalState::new(
                    &self.base.summarize_command,
                    &self.base.summarize_prompt,
                ));
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
                self.claude_command_input.handle_event(&Event::Key(key));
            }
            SettingsField::ClaudeArgs => {
                self.claude_args_input.handle_event(&Event::Key(key));
            }
            SettingsField::CodexCommand => {
                self.codex_command_input.handle_event(&Event::Key(key));
            }
            SettingsField::CodexArgs => {
                self.codex_args_input.handle_event(&Event::Key(key));
            }
            SettingsField::EditSummarizer => {}
        }
        SettingsOutcome::Stay
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let popup = layout::centered_rect(area, 72, 70);
        frame.render_widget(Clear, popup);

        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(theme.border_style(true))
            .title(block_title("Settings"));
        let inner = block.inner(popup);
        frame.render_widget(block, popup);

        let rows = Layout::vertical([
            Constraint::Length(1), // 0  top padding
            Constraint::Length(1), // 1  Theme inline
            Constraint::Length(1), // 2  spacing
            Constraint::Length(1), // 3  divider
            Constraint::Length(1), // 4  spacing
            Constraint::Length(1), // 5  Session Separator inline
            Constraint::Length(1), // 6  Snippet Lines inline
            Constraint::Length(1), // 7  spacing
            Constraint::Length(1), // 8  divider
            Constraint::Length(1), // 9  spacing
            Constraint::Length(1), // 10 Claude Code Command inline
            Constraint::Length(1), // 11 Claude Code Args inline
            Constraint::Length(1), // 12 spacing
            Constraint::Length(1), // 13 Codex CLI Command inline
            Constraint::Length(1), // 14 Codex CLI Args inline
            Constraint::Length(1), // 15 spacing
            Constraint::Length(1), // 16 divider
            Constraint::Length(1), // 17 spacing
            Constraint::Length(1), // 18 Edit summarizer button
            Constraint::Min(0),    // 19 flex slack
            Constraint::Length(1), // 20 bottom divider
            Constraint::Length(1), // 21 hints
        ])
        .split(inner);

        // Theme inline
        let theme_focused = self.field == SettingsField::Theme;
        let theme_line = self.render_inline_theme_row(rows[1].width, theme, theme_focused);
        frame.render_widget(Paragraph::new(theme_line), rows[1]);

        frame.render_widget(
            Paragraph::new("─".repeat(inner.width as usize))
                .style(Style::default().fg(theme.focus_border)),
            rows[3],
        );

        let sep_focused = self.field == SettingsField::SessionSeparator;
        render_inline_text_field(
            frame,
            rows[5],
            theme,
            "Session Separator",
            &self.separator_input,
            sep_focused,
        );
        let snip_focused = self.field == SettingsField::SnippetLineCount;
        render_inline_text_field(
            frame,
            rows[6],
            theme,
            "Snippet Lines",
            &self.snippet_line_count_input,
            snip_focused,
        );

        frame.render_widget(
            Paragraph::new("─".repeat(inner.width as usize))
                .style(Style::default().fg(theme.focus_border)),
            rows[8],
        );

        let claude_cmd_focused = self.field == SettingsField::ClaudeCommand;
        render_inline_text_field(
            frame,
            rows[10],
            theme,
            "Claude Code Command",
            &self.claude_command_input,
            claude_cmd_focused,
        );
        let claude_args_focused = self.field == SettingsField::ClaudeArgs;
        render_inline_text_field(
            frame,
            rows[11],
            theme,
            "Claude Code Args",
            &self.claude_args_input,
            claude_args_focused,
        );

        let codex_cmd_focused = self.field == SettingsField::CodexCommand;
        render_inline_text_field(
            frame,
            rows[13],
            theme,
            "Codex CLI Command",
            &self.codex_command_input,
            codex_cmd_focused,
        );
        let codex_args_focused = self.field == SettingsField::CodexArgs;
        render_inline_text_field(
            frame,
            rows[14],
            theme,
            "Codex CLI Args",
            &self.codex_args_input,
            codex_args_focused,
        );

        frame.render_widget(
            Paragraph::new("─".repeat(inner.width as usize))
                .style(Style::default().fg(theme.focus_border)),
            rows[16],
        );

        let button_focused = self.field == SettingsField::EditSummarizer;
        render_edit_summarizer_button(frame, rows[18], theme, button_focused);

        // Bottom divider + hints
        frame.render_widget(
            Paragraph::new("─".repeat(inner.width as usize))
                .style(Style::default().fg(theme.focus_border)),
            rows[20],
        );
        const HINTS: [keymap_hint::KeymapHint; 4] = [
            keymap_hint::KeymapHint::new("Tab/⇧Tab", "navigate"),
            keymap_hint::KeymapHint::new("←→", "change value"),
            keymap_hint::KeymapHint::new("^S", "save"),
            keymap_hint::KeymapHint::new("Esc", "cancel"),
        ];
        keymap_hint::render(frame, rows[21], &HINTS, theme, "");

        if let Some(summarizer) = self.summarizer.as_mut() {
            summarizer.render(frame, popup, theme);
        }
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

        #[allow(clippy::needless_range_loop)]
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

    fn build_settings(&self) -> Settings {
        let snippet_line_count = self
            .snippet_line_count_input
            .value()
            .parse::<usize>()
            .unwrap_or(self.base.snippet_line_count);
        Settings {
            theme: *self.theme.current(),
            claude_command: self.claude_command_input.value().to_owned(),
            claude_args: self.claude_args_input.value().to_owned(),
            codex_command: self.codex_command_input.value().to_owned(),
            codex_args: self.codex_args_input.value().to_owned(),
            session_separator: self.separator_input.value().to_owned(),
            snippet_line_count,
            summarize_command: self.base.summarize_command.clone(),
            summarize_prompt: self.base.summarize_prompt.clone(),
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
        SettingsField::ClaudeArgs,
        SettingsField::CodexCommand,
        SettingsField::CodexArgs,
        SettingsField::EditSummarizer,
    ]);
    assert!(cursor.set(&selected));
    cursor
}

fn theme_name_cursor(selected: ThemeName) -> RingCursor<ThemeName> {
    let mut cursor = RingCursor::new(ThemeName::ALL.to_vec());
    assert!(cursor.set(&selected));
    cursor
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

/// Render a single row with a fixed-width label column on the left and
/// a text input filling the rest.
fn render_inline_text_field(
    frame: &mut Frame,
    area: Rect,
    theme: &Theme,
    label: &str,
    input: &Input,
    focused: bool,
) {
    const LABEL_COL_WIDTH: u16 = 21;
    const LEFT_PAD: u16 = 2;
    const RIGHT_PAD: u16 = 2;

    let label_style = if focused {
        Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(theme.muted)
            .add_modifier(Modifier::BOLD)
    };

    let label_area = Rect {
        x: area.x + LEFT_PAD,
        y: area.y,
        width: LABEL_COL_WIDTH.min(area.width.saturating_sub(LEFT_PAD)),
        height: 1,
    };
    frame.render_widget(
        Paragraph::new(label.to_owned()).style(label_style),
        label_area,
    );

    let input_x = area.x + LEFT_PAD + LABEL_COL_WIDTH;
    let input_right_edge = area.x + area.width;
    if input_x + RIGHT_PAD >= input_right_edge {
        return;
    }
    let input_area = Rect {
        x: input_x,
        y: area.y,
        width: input_right_edge - input_x - RIGHT_PAD,
        height: 1,
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
    let prefix = if focused { "▎" } else { " " };
    let text = format!("{prefix}{}", input.value());
    frame.render_widget(Paragraph::new(text).style(style), input_area);

    if focused {
        let cursor_x = input_area.x + 1 + input.visual_cursor() as u16;
        if cursor_x < input_area.right() {
            frame.set_cursor_position((cursor_x, input_area.y));
        }
    }
}

/// Pick the first variant that fits in `width` (accounting for the 2-col
/// indent added by `render_field_label`). Falls back to the last variant.
fn responsive_label<'a>(width: u16, variants: &[&'a str]) -> &'a str {
    let budget = (width as usize).saturating_sub(2);
    for v in variants {
        if UnicodeWidthStr::width(*v) <= budget {
            return v;
        }
    }
    variants.last().copied().unwrap_or("")
}

fn render_field_label(frame: &mut Frame, area: Rect, theme: &Theme, text: &str, focused: bool) {
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

fn render_edit_summarizer_button(frame: &mut Frame, area: Rect, theme: &Theme, focused: bool) {
    let text = "[ Edit session summarizer settings ⏎ ]";
    let text_w = UnicodeWidthStr::width(text) as u16;
    const LEFT_PAD: u16 = 2;
    let available = area.width.saturating_sub(LEFT_PAD);
    let button_area = Rect {
        x: area.x.saturating_add(LEFT_PAD),
        y: area.y,
        width: text_w.min(available),
        height: 1,
    };
    let style = if focused {
        Style::default()
            .fg(theme.text)
            .bg(theme.selection)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(theme.muted)
            .add_modifier(Modifier::BOLD)
    };
    frame.render_widget(Paragraph::new(text).style(style), button_area);
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
        if start > 0 {
            active_arrow
        } else {
            inactive_arrow
        },
    ));

    #[allow(clippy::needless_range_loop)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TemplateShell {
    Bash,
    Zsh,
}

impl TemplateShell {
    const ALL: [TemplateShell; 2] = [TemplateShell::Bash, TemplateShell::Zsh];

    fn label(self) -> &'static str {
        match self {
            TemplateShell::Bash => "bash",
            TemplateShell::Zsh => "zsh",
        }
    }

    fn prelude(self) -> &'static str {
        match self {
            TemplateShell::Bash => concat!(
                "[ -f \"$HOME/.bash_profile\" ] && . \"$HOME/.bash_profile\"\n",
                "[ -f \"$HOME/.bashrc\" ] && . \"$HOME/.bashrc\"\n",
            ),
            TemplateShell::Zsh => concat!(
                "[ -f \"$HOME/.zprofile\" ] && . \"$HOME/.zprofile\"\n",
                "[ -f \"$HOME/.zshrc\" ] && . \"$HOME/.zshrc\"\n",
            ),
        }
    }
}

fn detect_shell() -> TemplateShell {
    let shell = std::env::var("SHELL").unwrap_or_default();
    let basename = std::path::Path::new(&shell)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    match basename {
        "bash" => TemplateShell::Bash,
        "zsh" => TemplateShell::Zsh,
        _ => TemplateShell::Zsh,
    }
}

const PICKER_BACKENDS: [SummarizeBackend; 2] = [SummarizeBackend::Claude, SummarizeBackend::Codex];

fn backend_label(backend: SummarizeBackend) -> &'static str {
    match backend {
        SummarizeBackend::Claude => "Claude",
        SummarizeBackend::Codex => "Codex",
        SummarizeBackend::Custom => "Custom",
    }
}

fn builtin_body(backend: SummarizeBackend) -> &'static str {
    match backend {
        SummarizeBackend::Claude => concat!(
            "cd '{{jsonl_dir}}'\n",
            "cat '{{prompt_file}}' | '{{claude_command}}' {{claude_args}} ",
            "{{model_flag}} {{effort_flag}} -p > '{{output_file}}'\n",
        ),
        SummarizeBackend::Codex => concat!(
            "cd '{{jsonl_dir}}'\n",
            "cat '{{prompt_file}}' | '{{codex_command}}' {{codex_args}} ",
            "{{model_flag}} {{effort_flag}} exec --full-auto ",
            "--skip-git-repo-check > '{{output_file}}'\n",
        ),
        SummarizeBackend::Custom => "",
    }
}

fn render_preset_template(shell: TemplateShell, backend: SummarizeBackend) -> String {
    format!("{}{}", shell.prelude(), builtin_body(backend))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TemplatePickerField {
    Shell,
    Backend,
    Model,
    Effort,
}

const CLAUDE_MODELS: [Option<&'static str>; 4] =
    [None, Some("opus"), Some("sonnet"), Some("haiku")];
const CLAUDE_EFFORTS: [Option<&'static str>; 5] =
    [None, Some("low"), Some("medium"), Some("high"), Some("max")];

fn option_label(opt: Option<&str>) -> String {
    opt.unwrap_or("unset").to_owned()
}

#[derive(Debug, Clone)]
struct CodexModelEntry {
    slug: String,
    efforts: Vec<String>,
}

#[derive(Debug, Clone)]
enum CodexSelectors {
    Cached {
        models: Vec<CodexModelEntry>,
        model: RingCursor<Option<String>>,
        effort: RingCursor<Option<String>>,
    },
    Freeform {
        model: Input,
        effort: Input,
    },
}

impl CodexSelectors {
    fn load() -> Self {
        match read_codex_cache() {
            Some(models) if !models.is_empty() => {
                let model_items: Vec<Option<String>> = std::iter::once(None)
                    .chain(models.iter().map(|m| Some(m.slug.clone())))
                    .collect();
                let model = RingCursor::new(model_items);
                let effort: RingCursor<Option<String>> = RingCursor::new(vec![None]);
                CodexSelectors::Cached {
                    models,
                    model,
                    effort,
                }
            }
            _ => CodexSelectors::Freeform {
                model: Input::default(),
                effort: Input::default(),
            },
        }
    }

    fn model_value(&self) -> Option<String> {
        match self {
            CodexSelectors::Cached { model, .. } => model.current().clone(),
            CodexSelectors::Freeform { model, .. } => {
                let v = model.value().trim();
                if v.is_empty() {
                    None
                } else {
                    Some(v.to_owned())
                }
            }
        }
    }

    fn effort_value(&self) -> Option<String> {
        match self {
            CodexSelectors::Cached { effort, .. } => effort.current().clone(),
            CodexSelectors::Freeform { effort, .. } => {
                let v = effort.value().trim();
                if v.is_empty() {
                    None
                } else {
                    Some(v.to_owned())
                }
            }
        }
    }
}

#[derive(Debug, Deserialize)]
struct CodexCacheFile {
    #[serde(default)]
    models: Vec<RawCodexModel>,
}

#[derive(Debug, Deserialize)]
struct RawCodexModel {
    #[serde(default)]
    slug: String,
    #[serde(default)]
    supported_reasoning_levels: Vec<RawCodexReasoning>,
}

#[derive(Debug, Deserialize)]
struct RawCodexReasoning {
    #[serde(default)]
    effort: String,
}

fn codex_cache_path() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    Some(PathBuf::from(home).join(".codex").join("models_cache.json"))
}

fn read_codex_cache() -> Option<Vec<CodexModelEntry>> {
    let path = codex_cache_path()?;
    let text = std::fs::read_to_string(&path).ok()?;
    let cache: CodexCacheFile = serde_json::from_str(&text).ok()?;
    Some(
        cache
            .models
            .into_iter()
            .filter(|m| !m.slug.is_empty())
            .map(|m| CodexModelEntry {
                slug: m.slug,
                efforts: m
                    .supported_reasoning_levels
                    .into_iter()
                    .map(|r| r.effort)
                    .filter(|e| !e.is_empty())
                    .collect(),
            })
            .collect(),
    )
}

/// Collapse runs of ASCII spaces into a single space. Preserves newlines and
/// other whitespace. Used after inlining `{{model_flag}}` / `{{effort_flag}}`
/// so an unset flag doesn't leave stray double-spaces in the resolved string.
fn collapse_spaces(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_space = false;
    for c in s.chars() {
        if c == ' ' {
            if !prev_space {
                out.push(' ');
            }
            prev_space = true;
        } else {
            out.push(c);
            prev_space = false;
        }
    }
    out
}

#[derive(Debug, Clone)]
struct TemplatePicker {
    field: RingCursor<TemplatePickerField>,
    shell: RingCursor<TemplateShell>,
    backend: RingCursor<SummarizeBackend>,
    claude_model: RingCursor<Option<&'static str>>,
    claude_effort: RingCursor<Option<&'static str>>,
    codex: CodexSelectors,
}

enum TemplatePickerOutcome {
    Stay,
    Cancel,
    Insert(String),
}

impl TemplatePicker {
    fn new(default_shell: TemplateShell) -> Self {
        let mut shell = RingCursor::new(TemplateShell::ALL.to_vec());
        shell.set(&default_shell);
        let backend = RingCursor::new(PICKER_BACKENDS.to_vec());
        let field = RingCursor::new(vec![
            TemplatePickerField::Shell,
            TemplatePickerField::Backend,
            TemplatePickerField::Model,
            TemplatePickerField::Effort,
        ]);
        let mut claude_model = RingCursor::new(CLAUDE_MODELS.to_vec());
        claude_model.set(&Some("sonnet"));
        let mut claude_effort = RingCursor::new(CLAUDE_EFFORTS.to_vec());
        claude_effort.set(&Some("medium"));
        Self {
            field,
            shell,
            backend,
            claude_model,
            claude_effort,
            codex: CodexSelectors::load(),
        }
    }

    fn model_flag(&self) -> String {
        match *self.backend.current() {
            SummarizeBackend::Claude => match self.claude_model.current() {
                Some(m) => format!("--model {m}"),
                None => String::new(),
            },
            SummarizeBackend::Codex => match self.codex.model_value() {
                Some(s) => format!("--model {s}"),
                None => String::new(),
            },
            SummarizeBackend::Custom => String::new(),
        }
    }

    fn effort_flag(&self) -> String {
        match *self.backend.current() {
            SummarizeBackend::Claude => match self.claude_effort.current() {
                Some(e) => format!("--effort {e}"),
                None => String::new(),
            },
            SummarizeBackend::Codex => match self.codex.effort_value() {
                Some(e) => format!("--config model_reasoning_effort={e}"),
                None => String::new(),
            },
            SummarizeBackend::Custom => String::new(),
        }
    }

    fn resolved(&self) -> String {
        let base = render_preset_template(*self.shell.current(), *self.backend.current());
        let expanded = base
            .replace("{{model_flag}}", &self.model_flag())
            .replace("{{effort_flag}}", &self.effort_flag());
        collapse_spaces(&expanded)
    }

    fn move_model(&mut self, forward: bool) {
        match *self.backend.current() {
            SummarizeBackend::Claude => {
                if forward {
                    self.claude_model.move_next();
                } else {
                    self.claude_model.move_prev();
                }
            }
            SummarizeBackend::Codex => match &mut self.codex {
                CodexSelectors::Cached {
                    models,
                    model,
                    effort,
                } => {
                    if forward {
                        model.move_next();
                    } else {
                        model.move_prev();
                    }
                    let idx = model.index();
                    let items: Vec<Option<String>> = if idx == 0 {
                        vec![None]
                    } else {
                        std::iter::once(None)
                            .chain(models[idx - 1].efforts.iter().cloned().map(Some))
                            .collect()
                    };
                    *effort = RingCursor::new(items);
                }
                CodexSelectors::Freeform { .. } => {}
            },
            SummarizeBackend::Custom => {}
        }
    }

    fn move_effort(&mut self, forward: bool) {
        match *self.backend.current() {
            SummarizeBackend::Claude => {
                if forward {
                    self.claude_effort.move_next();
                } else {
                    self.claude_effort.move_prev();
                }
            }
            SummarizeBackend::Codex => match &mut self.codex {
                CodexSelectors::Cached { effort, .. } => {
                    if forward {
                        effort.move_next();
                    } else {
                        effort.move_prev();
                    }
                }
                CodexSelectors::Freeform { .. } => {}
            },
            SummarizeBackend::Custom => {}
        }
    }

    fn forward_to_input(&mut self, key: KeyEvent, which: TemplatePickerField) {
        if !matches!(*self.backend.current(), SummarizeBackend::Codex) {
            return;
        }
        if let CodexSelectors::Freeform { model, effort } = &mut self.codex {
            let target = match which {
                TemplatePickerField::Model => model,
                TemplatePickerField::Effort => effort,
                _ => return,
            };
            target.handle_event(&Event::Key(key));
        }
    }

    fn is_freeform_input_field(&self, field: TemplatePickerField) -> bool {
        matches!(
            field,
            TemplatePickerField::Model | TemplatePickerField::Effort
        ) && matches!(*self.backend.current(), SummarizeBackend::Codex)
            && matches!(self.codex, CodexSelectors::Freeform { .. })
    }

    fn handle_key(&mut self, key: KeyEvent) -> TemplatePickerOutcome {
        // Enter/Esc always terminate, even in freeform inputs.
        match key.code {
            KeyCode::Esc => return TemplatePickerOutcome::Cancel,
            KeyCode::Enter => return TemplatePickerOutcome::Insert(self.resolved()),
            KeyCode::Tab => {
                self.field.move_next();
                return TemplatePickerOutcome::Stay;
            }
            KeyCode::BackTab => {
                self.field.move_prev();
                return TemplatePickerOutcome::Stay;
            }
            _ => {}
        }

        let field = *self.field.current();
        // Freeform inputs consume everything else (letters, arrows for text
        // cursor, backspace, etc.).
        if self.is_freeform_input_field(field) {
            self.forward_to_input(key, field);
            return TemplatePickerOutcome::Stay;
        }

        match key.code {
            KeyCode::Left | KeyCode::Char('h') => match field {
                TemplatePickerField::Shell => {
                    self.shell.move_prev();
                }
                TemplatePickerField::Backend => {
                    self.backend.move_prev();
                }
                TemplatePickerField::Model => self.move_model(false),
                TemplatePickerField::Effort => self.move_effort(false),
            },
            KeyCode::Right | KeyCode::Char('l') => match field {
                TemplatePickerField::Shell => {
                    self.shell.move_next();
                }
                TemplatePickerField::Backend => {
                    self.backend.move_next();
                }
                TemplatePickerField::Model => self.move_model(true),
                TemplatePickerField::Effort => self.move_effort(true),
            },
            _ => {}
        }
        TemplatePickerOutcome::Stay
    }

    fn render_model_row(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let focused = self.field == TemplatePickerField::Model;
        match *self.backend.current() {
            SummarizeBackend::Claude => {
                let items: Vec<String> = CLAUDE_MODELS.iter().map(|m| option_label(*m)).collect();
                let line = inline_radio_row(
                    "Model",
                    &items,
                    self.claude_model.index(),
                    area.width,
                    theme,
                    focused,
                );
                frame.render_widget(Paragraph::new(line), area);
            }
            SummarizeBackend::Codex => match &self.codex {
                CodexSelectors::Cached { model, .. } => {
                    let items: Vec<String> = model
                        .items()
                        .iter()
                        .map(|o| option_label(o.as_deref()))
                        .collect();
                    let line = inline_radio_row(
                        "Model",
                        &items,
                        model.index(),
                        area.width,
                        theme,
                        focused,
                    );
                    frame.render_widget(Paragraph::new(line), area);
                }
                CodexSelectors::Freeform { model, .. } => {
                    render_inline_text_field(frame, area, theme, "Model", model, focused);
                }
            },
            SummarizeBackend::Custom => {}
        }
    }

    fn render_effort_row(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let focused = self.field == TemplatePickerField::Effort;
        match *self.backend.current() {
            SummarizeBackend::Claude => {
                let items: Vec<String> = CLAUDE_EFFORTS.iter().map(|m| option_label(*m)).collect();
                let line = inline_radio_row(
                    "Effort",
                    &items,
                    self.claude_effort.index(),
                    area.width,
                    theme,
                    focused,
                );
                frame.render_widget(Paragraph::new(line), area);
            }
            SummarizeBackend::Codex => match &self.codex {
                CodexSelectors::Cached { effort, .. } => {
                    let items: Vec<String> = effort
                        .items()
                        .iter()
                        .map(|o| option_label(o.as_deref()))
                        .collect();
                    let line = inline_radio_row(
                        "Effort",
                        &items,
                        effort.index(),
                        area.width,
                        theme,
                        focused,
                    );
                    frame.render_widget(Paragraph::new(line), area);
                }
                CodexSelectors::Freeform { effort, .. } => {
                    render_inline_text_field(frame, area, theme, "Effort", effort, focused);
                }
            },
            SummarizeBackend::Custom => {}
        }
    }

    /// Render the picker as a popup inset by 1 cell on each edge of `host`
    /// (the settings dialog rect).
    fn render(&self, frame: &mut Frame, host: Rect, theme: &Theme) {
        let popup = Rect {
            x: host.x.saturating_add(1),
            y: host.y.saturating_add(1),
            width: host.width.saturating_sub(2),
            height: host.height.saturating_sub(2),
        };
        frame.render_widget(Clear, popup);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(theme.border_style(true))
            .title(block_title("Insert preset template"));
        let inner = block.inner(popup);
        frame.render_widget(block, popup);

        let rows = Layout::vertical([
            Constraint::Length(1), // 0 padding
            Constraint::Length(1), // 1 shell
            Constraint::Length(1), // 2 backend
            Constraint::Length(1), // 3 model
            Constraint::Length(1), // 4 effort
            Constraint::Length(1), // 5 spacing
            Constraint::Length(1), // 6 preview label
            Constraint::Min(3),    // 7 preview
            Constraint::Length(1), // 8 spacing
            Constraint::Length(4), // 9 docs
            Constraint::Length(1), // 10 divider
            Constraint::Length(1), // 11 hints
        ])
        .split(inner);

        let shell_focused = self.field == TemplatePickerField::Shell;
        let shell_line = inline_radio_row(
            "Shell",
            &TemplateShell::ALL
                .iter()
                .map(|s| s.label().to_owned())
                .collect::<Vec<_>>(),
            self.shell.index(),
            rows[1].width,
            theme,
            shell_focused,
        );
        frame.render_widget(Paragraph::new(shell_line), rows[1]);

        let backend_focused = self.field == TemplatePickerField::Backend;
        let backend_line = inline_radio_row(
            "Backend",
            &PICKER_BACKENDS
                .iter()
                .map(|b| backend_label(*b).to_owned())
                .collect::<Vec<_>>(),
            self.backend.index(),
            rows[2].width,
            theme,
            backend_focused,
        );
        frame.render_widget(Paragraph::new(backend_line), rows[2]);

        self.render_model_row(frame, rows[3], theme);
        self.render_effort_row(frame, rows[4], theme);

        render_field_label(frame, rows[6], theme, "Preview", false);
        let preview = self.resolved();
        let bg = theme.settings_input_bg(false);
        let preview_style = Style::default().fg(theme.text).bg(bg);
        frame.render_widget(
            Paragraph::new(preview)
                .style(preview_style)
                .block(Block::default().style(Style::default().bg(bg)))
                .wrap(Wrap { trim: false }),
            pad_rect(rows[7]),
        );

        let docs = concat!(
            "Placeholders: {{jsonl_dir}}, {{prompt_file}}, {{output_file}},\n",
            "  {{claude_command}}, {{claude_args}}, {{codex_command}}, {{codex_args}},\n",
            "  {{model_flag}}, {{effort_flag}} (resolved on insert from pickers above).\n",
            "Edit after inserting; we run it verbatim through your shell."
        );
        frame.render_widget(
            Paragraph::new(docs)
                .style(Style::default().fg(theme.muted))
                .wrap(Wrap { trim: false }),
            pad_rect(rows[9]),
        );

        frame.render_widget(
            Paragraph::new("─".repeat(inner.width as usize))
                .style(Style::default().fg(theme.focus_border)),
            rows[10],
        );
        const HINTS: [keymap_hint::KeymapHint; 3] = [
            keymap_hint::KeymapHint::new("Tab/⇧Tab", "navigate"),
            keymap_hint::KeymapHint::new("⏎", "insert"),
            keymap_hint::KeymapHint::new("Esc", "cancel"),
        ];
        keymap_hint::render(frame, rows[11], &HINTS, theme, "");
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SummarizerField {
    Command,
    Prompt,
}

#[derive(Debug, Clone)]
pub struct SummarizerModalState {
    field: RingCursor<SummarizerField>,
    command_textarea: TextArea<'static>,
    prompt_textarea: TextArea<'static>,
    picker: Option<TemplatePicker>,
}

enum SummarizerOutcome {
    Stay,
    Cancel,
    Apply { command: String, prompt: String },
}

impl SummarizerModalState {
    fn new(command: &str, prompt: &str) -> Self {
        let mut field = RingCursor::new(vec![SummarizerField::Command, SummarizerField::Prompt]);
        field.set(&SummarizerField::Command);
        Self {
            field,
            command_textarea: build_textarea(command),
            prompt_textarea: build_textarea(prompt),
            picker: None,
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> SummarizerOutcome {
        if let Some(picker) = self.picker.as_mut() {
            match picker.handle_key(key) {
                TemplatePickerOutcome::Stay => {}
                TemplatePickerOutcome::Cancel => {
                    self.picker = None;
                }
                TemplatePickerOutcome::Insert(text) => {
                    self.command_textarea = build_textarea(&text);
                    self.picker = None;
                }
            }
            return SummarizerOutcome::Stay;
        }

        match key.code {
            KeyCode::Esc => return SummarizerOutcome::Cancel,
            KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                return SummarizerOutcome::Apply {
                    command: self.command_textarea.lines().join("\n"),
                    prompt: self.prompt_textarea.lines().join("\n"),
                };
            }
            KeyCode::Char('t') | KeyCode::Char('T')
                if key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                self.field.set(&SummarizerField::Command);
                self.picker = Some(TemplatePicker::new(detect_shell()));
                return SummarizerOutcome::Stay;
            }
            KeyCode::Tab => {
                self.field.move_next();
                return SummarizerOutcome::Stay;
            }
            KeyCode::BackTab => {
                self.field.move_prev();
                return SummarizerOutcome::Stay;
            }
            _ => {}
        }

        match *self.field.current() {
            SummarizerField::Command => {
                self.command_textarea.input(key);
            }
            SummarizerField::Prompt => {
                self.prompt_textarea.input(key);
            }
        }
        SummarizerOutcome::Stay
    }

    fn render(&mut self, frame: &mut Frame, host: Rect, theme: &Theme) {
        let popup = Rect {
            x: host.x.saturating_add(1),
            y: host.y.saturating_add(1),
            width: host.width.saturating_sub(2),
            height: host.height.saturating_sub(2),
        };
        frame.render_widget(Clear, popup);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(theme.border_style(true))
            .title(block_title("Session Summarizer Settings"));
        let inner = block.inner(popup);
        frame.render_widget(block, popup);

        let rows = Layout::vertical([
            Constraint::Length(1), // 0 top padding
            Constraint::Length(1), // 1 command label
            Constraint::Length(6), // 2 command textarea
            Constraint::Length(1), // 3 spacing
            Constraint::Length(1), // 4 prompt label
            Constraint::Min(3),    // 5 prompt textarea
            Constraint::Length(1), // 6 divider
            Constraint::Length(1), // 7 hints
        ])
        .split(inner);

        let cmd_focused = self.field == SummarizerField::Command;
        let cmd_label = responsive_label(
            rows[1].width,
            &[
                "Session summarizer command (^T to choose a command template)",
                "Session summarizer command (^T choose template)",
                "Session summarizer command",
            ],
        );
        render_field_label(frame, rows[1], theme, cmd_label, cmd_focused);
        style_textarea(&mut self.command_textarea, theme, cmd_focused);
        frame.render_widget(&self.command_textarea, pad_rect(rows[2]));

        let prompt_focused = self.field == SummarizerField::Prompt;
        render_field_label(
            frame,
            rows[4],
            theme,
            "Session summarizer prompt",
            prompt_focused,
        );
        style_textarea(&mut self.prompt_textarea, theme, prompt_focused);
        frame.render_widget(&self.prompt_textarea, pad_rect(rows[5]));

        frame.render_widget(
            Paragraph::new("─".repeat(inner.width as usize))
                .style(Style::default().fg(theme.focus_border)),
            rows[6],
        );
        const HINTS: [keymap_hint::KeymapHint; 4] = [
            keymap_hint::KeymapHint::new("Tab/⇧Tab", "navigate"),
            keymap_hint::KeymapHint::new("⏎", "newline"),
            keymap_hint::KeymapHint::new("^S", "save"),
            keymap_hint::KeymapHint::new("Esc", "cancel"),
        ];
        keymap_hint::render(frame, rows[7], &HINTS, theme, "");

        if let Some(picker) = self.picker.as_ref() {
            picker.render(frame, popup, theme);
        }
    }
}

#[cfg(test)]
mod tests {
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::style::Style;

    use super::{
        collapse_spaces, SettingsField, SettingsModalState, SettingsOutcome, TemplatePicker,
        TemplatePickerField, TemplateShell,
    };
    use crate::settings::{Settings, ThemeName};
    use crate::summary::SummarizeBackend;
    use crate::tui::theme::Theme;

    #[test]
    fn ctrl_s_applies_settings_from_theme_field() {
        let mut state = SettingsModalState::new(&Settings::default());
        assert!(state.theme.set(&ThemeName::Sunset));
        assert!(state.field.set(&SettingsField::Theme));

        let outcome = state.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));

        match outcome {
            SettingsOutcome::Apply(settings) => {
                assert_eq!(settings.theme, ThemeName::Sunset);
            }
            other => panic!("expected apply outcome, got {other:?}"),
        }
    }

    #[test]
    fn enter_on_theme_field_does_not_apply() {
        let mut state = SettingsModalState::new(&Settings::default());
        assert!(state.field.set(&SettingsField::Theme));

        let outcome = state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));

        assert!(matches!(outcome, SettingsOutcome::Stay));
    }

    #[test]
    fn enter_on_edit_summarizer_button_opens_submodal() {
        let mut state = SettingsModalState::new(&Settings::default());
        assert!(state.field.set(&SettingsField::EditSummarizer));

        let outcome = state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));

        assert!(matches!(outcome, SettingsOutcome::Stay));
        assert!(state.summarizer.is_some());
    }

    #[test]
    fn summarizer_ctrl_s_commits_values_into_parent_draft() {
        let mut state = SettingsModalState::new(&Settings::default());
        assert!(state.field.set(&SettingsField::EditSummarizer));
        state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));
        let summarizer = state.summarizer.as_mut().expect("submodal should be open");
        summarizer.command_textarea = super::build_textarea("echo hi");
        summarizer.prompt_textarea = super::build_textarea("be brief");

        state.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));

        assert!(state.summarizer.is_none());
        let outcome = state.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        match outcome {
            SettingsOutcome::Apply(settings) => {
                assert_eq!(settings.summarize_command, "echo hi");
                assert_eq!(settings.summarize_prompt, "be brief");
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
        let settings = Settings {
            theme: ThemeName::Sunset,
            ..Settings::default()
        };
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

    #[test]
    fn collapse_spaces_reduces_runs_but_keeps_newlines() {
        assert_eq!(collapse_spaces("a  b   c"), "a b c");
        assert_eq!(collapse_spaces("x\n  y"), "x\n y");
        assert_eq!(collapse_spaces("trailing   "), "trailing ");
    }

    #[test]
    fn claude_picker_defaults_include_sonnet_and_medium_flags() {
        let picker = TemplatePicker::new(TemplateShell::Zsh);
        assert!(picker.backend == SummarizeBackend::Claude);
        let resolved = picker.resolved();
        assert!(resolved.contains("--model sonnet"), "got: {resolved}");
        assert!(resolved.contains("--effort medium"), "got: {resolved}");
        // No stray double-spaces after inlining flags.
        assert!(!resolved.contains("  "), "got: {resolved}");
    }

    #[test]
    fn claude_picker_with_unset_model_drops_model_flag() {
        let mut picker = TemplatePicker::new(TemplateShell::Zsh);
        assert!(picker.field.set(&TemplatePickerField::Model));
        // Left from sonnet -> opus, from opus -> unset.
        let ev = KeyEvent::new(KeyCode::Left, KeyModifiers::empty());
        picker.handle_key(ev);
        picker.handle_key(ev);
        let resolved = picker.resolved();
        assert!(!resolved.contains("--model"), "got: {resolved}");
        assert!(resolved.contains("--effort medium"));
    }

    #[test]
    fn codex_picker_defaults_to_unset_flags() {
        let mut picker = TemplatePicker::new(TemplateShell::Zsh);
        picker.backend.set(&SummarizeBackend::Codex);
        let resolved = picker.resolved();
        assert!(!resolved.contains("--model"), "got: {resolved}");
        assert!(
            !resolved.contains("model_reasoning_effort"),
            "got: {resolved}"
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
