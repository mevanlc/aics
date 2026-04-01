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
    pub search_match_bg: Color,
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
        Self::lazygit()
    }
}

impl Theme {
    pub fn from_name(name: ThemeName) -> Self {
        match name {
            ThemeName::Lazygit => Self::lazygit(),
            ThemeName::Aics => Self::aics(),
            ThemeName::Sunset => Self::sunset(),
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
            search_match_bg: Color::Rgb(92, 72, 20),
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
            search_match_bg: Color::Rgb(96, 96, 0),
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

    /// Warm dusk palette with coral accents and deep indigo surfaces for a
    /// more cinematic contrast than the default terminal-style themes.
    pub fn sunset() -> Self {
        Self {
            border: Color::Rgb(94, 78, 112),
            focus_border: Color::Rgb(255, 122, 89),
            text: Color::Rgb(248, 239, 234),
            muted: Color::Rgb(175, 155, 171),
            accent: Color::Rgb(255, 122, 89),
            selection: Color::Rgb(81, 46, 86),
            highlight: Color::Rgb(255, 209, 102),
            search_match_bg: Color::Rgb(122, 78, 24),
            list_header_bg: Color::Rgb(54, 32, 61),
            list_body_bg: Color::Rgb(29, 21, 38),
            claude: Color::Rgb(255, 170, 110),
            codex: Color::Rgb(108, 210, 196),
            bubble_user: Color::Rgb(63, 35, 74),
            bubble_claude: Color::Rgb(87, 49, 31),
            bubble_codex: Color::Rgb(24, 73, 73),
            bubble_system: Color::Rgb(49, 39, 60),
            bubble_summary: Color::Rgb(92, 67, 31),
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

    pub fn search_match_style(&self) -> Style {
        Style::default()
            .bg(self.search_match_bg)
            .add_modifier(Modifier::BOLD)
    }
}
