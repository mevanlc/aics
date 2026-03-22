use ratatui::style::{Color, Modifier, Style};

use crate::settings::ThemeName;

#[derive(Debug, Clone)]
pub struct Theme {
    pub border: Color,
    pub focus_border: Color,
    pub text: Color,
    pub muted: Color,
    pub accent: Color,
    pub selection: Color,
    pub highlight: Color,
    pub list_header_bg: Color,
    pub list_body_bg: Color,
    pub claude: Color,
    pub codex: Color,
    pub bubble_user: Color,
    pub bubble_claude: Color,
    pub bubble_codex: Color,
    pub bubble_system: Color,
    pub bubble_summary: Color,
}

impl Default for Theme {
    fn default() -> Self {
        Self::aics()
    }
}

impl Theme {
    pub fn from_name(name: ThemeName) -> Self {
        match name {
            ThemeName::Aics => Self::aics(),
            ThemeName::Lazygit => Self::lazygit(),
        }
    }

    pub fn aics() -> Self {
        Self {
            border: Color::Rgb(70, 76, 86),
            focus_border: Color::Rgb(144, 191, 255),
            text: Color::Rgb(230, 232, 236),
            muted: Color::Rgb(132, 138, 148),
            accent: Color::Rgb(144, 191, 255),
            selection: Color::Rgb(34, 40, 50),
            highlight: Color::Rgb(255, 215, 90),
            list_header_bg: Color::Rgb(32, 36, 43),
            list_body_bg: Color::Rgb(24, 27, 33),
            claude: Color::Rgb(242, 153, 74),
            codex: Color::Rgb(86, 194, 131),
            bubble_user: Color::Rgb(35, 43, 55),
            bubble_claude: Color::Rgb(74, 50, 28),
            bubble_codex: Color::Rgb(28, 61, 46),
            bubble_system: Color::Rgb(49, 53, 64),
            bubble_summary: Color::Rgb(60, 54, 32),
        }
    }

    /// Theme inspired by lazygit's default color scheme: green focused borders,
    /// blue selection highlights, higher contrast, terminal-native feel.
    pub fn lazygit() -> Self {
        Self {
            border: Color::Rgb(68, 68, 68),
            focus_border: Color::Rgb(50, 205, 50),
            text: Color::Rgb(241, 241, 241),
            muted: Color::Rgb(140, 140, 140),
            accent: Color::Rgb(50, 205, 50),
            selection: Color::Rgb(0, 0, 128),
            highlight: Color::Rgb(255, 255, 0),
            list_header_bg: Color::Rgb(30, 30, 30),
            list_body_bg: Color::Rgb(20, 20, 20),
            claude: Color::Rgb(242, 153, 74),
            codex: Color::Rgb(86, 194, 131),
            bubble_user: Color::Rgb(20, 20, 50),
            bubble_claude: Color::Rgb(50, 35, 15),
            bubble_codex: Color::Rgb(15, 40, 30),
            bubble_system: Color::Rgb(35, 35, 35),
            bubble_summary: Color::Rgb(45, 40, 20),
        }
    }

    pub fn border_style(&self, focused: bool) -> Style {
        Style::default().fg(if focused {
            self.focus_border
        } else {
            self.border
        })
    }

    pub fn highlight_style(&self) -> Style {
        Style::default()
            .fg(self.highlight)
            .add_modifier(Modifier::BOLD)
    }
}
