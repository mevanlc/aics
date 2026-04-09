use ratatui::style::{Color, Modifier, Style};

use crate::settings::ThemeName;
use crate::tui::color_ext::ColorExt;

#[derive(Debug, Clone)]
pub struct Theme {
    pub border: Color,
    pub focus_border: Color,
    pub text: Color,
    pub muted: Color,
    pub muted_greater: Color,
    pub accent: Color,
    pub selection: Color,
    pub highlight: Color,
    pub search_match_bg: Color,
    pub active_match_bg: Color,
    pub list_header_bg: Color,
    pub list_body_bg: Color,
    pub claude: Color,
    pub codex: Color,
    pub bubble_user: Color,
    pub bubble_claude: Color,
    pub bubble_codex: Color,
    pub bubble_system: Color,
    pub bubble_summary: Color,
    pub tool: Color,
    pub bubble_tool: Color,
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
            ThemeName::LateSh => Self::late_sh(),
        }
    }

    pub fn aics() -> Self {
        let focus_border = Color::Rgb(144, 191, 255);
        let list_header_bg = Color::Rgb(32, 36, 43);
        let list_body_bg = list_header_bg.darken(0.223);
        let selection = list_body_bg.brighten(0.485);
        let muted_greater = Color::Rgb(50, 53, 58);
        let codex = Color::Rgb(86, 194, 131);
        let tool = Color::Rgb(100, 200, 210);

        Self {
            border: muted_greater.brighten(0.449),
            focus_border,
            text: Color::Rgb(230, 232, 236),
            muted: muted_greater.brighten(1.600),
            muted_greater,
            accent: focus_border,
            selection,
            highlight: Color::Rgb(255, 215, 90),
            search_match_bg: Color::Rgb(92, 72, 20),
            active_match_bg: Color::Rgb(160, 130, 20),
            list_header_bg,
            list_body_bg,
            claude: Color::Rgb(242, 153, 74),
            codex,
            bubble_user: selection.brighten(0.080),
            bubble_claude: Color::Rgb(74, 50, 28),
            bubble_codex: codex.darken(0.671),
            bubble_system: list_body_bg.brighten(0.963),
            bubble_summary: Color::Rgb(60, 54, 32),
            tool,
            bubble_tool: tool.darken(0.775),
        }
    }

    /// Theme inspired by lazygit's default color scheme: green focused borders,
    /// blue selection highlights, higher contrast, terminal-native feel.
    pub fn lazygit() -> Self {
        let border = Color::Rgb(68, 68, 68);
        let focus_border = Color::Rgb(50, 205, 50);
        let highlight = Color::Rgb(255, 255, 0);
        let claude = Color::Rgb(242, 153, 74);
        let codex = Color::Rgb(86, 194, 131);
        let tool = Color::Rgb(80, 190, 200);

        Self {
            border,
            focus_border,
            text: border.brighten(2.545),
            muted: border.brighten(1.059),
            muted_greater: border.darken(0.236),
            accent: focus_border,
            selection: Color::Rgb(0, 0, 128),
            highlight,
            search_match_bg: highlight.darken(0.62),
            active_match_bg: highlight.darken(0.33),
            list_header_bg: border.darken(0.545),
            list_body_bg: border.darken(0.692),
            claude,
            codex,
            bubble_user: Color::Rgb(20, 20, 50),
            bubble_claude: claude.darken(0.784),
            bubble_codex: codex.darken(0.791),
            bubble_system: border.darken(0.471),
            bubble_summary: Color::Rgb(45, 40, 20),
            tool,
            bubble_tool: tool.darken(0.800),
        }
    }

    /// Warm dusk palette with coral accents and deep indigo surfaces for a
    /// more cinematic contrast than the default terminal-style themes.
    pub fn sunset() -> Self {
        let focus_border = Color::Rgb(255, 122, 89);
        let codex = Color::Rgb(108, 210, 196);
        let muted = Color::Rgb(164, 155, 162);
        let list_header_bg = Color::Rgb(43, 36, 49);
        let selection = list_header_bg.brighten(0.445);
        let bubble_claude = Color::Rgb(87, 49, 31);

        Self {
            border: Color::Rgb(88, 80, 96),
            focus_border,
            text: Color::Rgb(248, 239, 234),
            muted,
            muted_greater: muted.darken(0.622),
            accent: focus_border,
            selection,
            highlight: Color::Rgb(255, 209, 102),
            search_match_bg: Color::Rgb(122, 78, 24),
            active_match_bg: Color::Rgb(200, 140, 30),
            list_header_bg,
            list_body_bg: list_header_bg.darken(0.349),
            claude: bubble_claude.brighten(2.484),
            codex,
            bubble_user: list_header_bg.brighten(0.143),
            bubble_claude,
            bubble_codex: Color::Rgb(24, 73, 73),
            bubble_system: selection.darken(0.286),
            bubble_summary: Color::Rgb(92, 67, 31),
            tool: codex,
            bubble_tool: Color::Rgb(24, 55, 55),
        }
    }

    /// Theme sampled from the user's "late.sh" screenshots with Pillow:
    /// black surfaces, copper framing, ash text, amber highlights, and muted
    /// olive/violet secondary accents.
    pub fn late_sh() -> Self {
        let border = Color::Rgb(96, 64, 32);
        let focus_border = Color::Rgb(184, 120, 40);
        let text = Color::Rgb(136, 128, 120);
        let muted = Color::Rgb(112, 104, 104);
        let muted_greater = Color::Rgb(64, 56, 56);
        let list_header_bg = Color::Rgb(8, 8, 8);
        let list_body_bg = Color::Rgb(0, 0, 0);
        let selection = Color::Rgb(64, 40, 24);
        let highlight = Color::Rgb(208, 166, 89);
        let claude = Color::Rgb(184, 120, 40);
        let codex = Color::Rgb(104, 136, 88);
        let tool = Color::Rgb(104, 72, 128);

        Self {
            border,
            focus_border,
            text,
            muted,
            muted_greater,
            accent: focus_border,
            selection,
            highlight,
            search_match_bg: Color::Rgb(80, 56, 24),
            active_match_bg: Color::Rgb(120, 80, 32),
            list_header_bg,
            list_body_bg,
            claude,
            codex,
            bubble_user: Color::Rgb(16, 12, 10),
            bubble_claude: Color::Rgb(32, 20, 12),
            bubble_codex: Color::Rgb(18, 24, 16),
            bubble_system: Color::Rgb(16, 16, 16),
            bubble_summary: Color::Rgb(24, 30, 18),
            tool,
            bubble_tool: Color::Rgb(24, 18, 32),
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

    pub fn selected_list_header_bg(&self) -> Color {
        self.list_header_bg.brighten(1.0)
    }

    pub fn selected_list_body_bg(&self) -> Color {
        self.list_body_bg.brighten(1.0)
    }

    pub fn search_match_style(&self) -> Style {
        Style::default()
            .bg(self.search_match_bg)
            .add_modifier(Modifier::BOLD)
    }
}

#[cfg(test)]
mod tests {
    use ratatui::style::Color;

    use super::Theme;

    #[test]
    fn selected_list_colors_are_fifty_percent_brighter() {
        let theme = Theme::aics();

        assert_eq!(theme.selected_list_header_bg(), Color::Rgb(64, 72, 86));
        assert_eq!(theme.selected_list_body_bg(), Color::Rgb(48, 54, 66));
    }
}
