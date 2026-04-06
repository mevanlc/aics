use ratatui::style::Color;

pub trait ColorExt {
    fn brighten(self, amount: f32) -> Self;
    fn darken(self, amount: f32) -> Self;
}

impl ColorExt for Color {
    fn brighten(self, amount: f32) -> Self {
        self.scale_rgb(1.0 + amount.max(0.0))
    }

    fn darken(self, amount: f32) -> Self {
        self.scale_rgb((1.0 - amount).max(0.0))
    }
}

trait ScaleRgb {
    fn scale_rgb(self, scale: f32) -> Self;
}

impl ScaleRgb for Color {
    fn scale_rgb(self, scale: f32) -> Self {
        match self {
            Color::Rgb(r, g, b) => Color::Rgb(
                scale_channel(r, scale),
                scale_channel(g, scale),
                scale_channel(b, scale),
            ),
            other => other,
        }
    }
}

fn scale_channel(channel: u8, scale: f32) -> u8 {
    ((channel as f32 * scale).clamp(0.0, u8::MAX as f32)) as u8
}

#[cfg(test)]
mod tests {
    use ratatui::style::Color;

    use super::ColorExt;

    #[test]
    fn brighten_scales_rgb_channels() {
        assert_eq!(Color::Rgb(32, 36, 43).brighten(0.5), Color::Rgb(48, 54, 64));
    }

    #[test]
    fn darken_scales_rgb_channels_down() {
        assert_eq!(Color::Rgb(32, 36, 43).darken(0.25), Color::Rgb(24, 27, 32));
    }

    #[test]
    fn brighten_clamps_rgb_channels() {
        assert_eq!(
            Color::Rgb(240, 200, 180).brighten(0.5),
            Color::Rgb(255, 255, 255)
        );
    }

    #[test]
    fn non_rgb_colors_are_returned_unchanged() {
        assert_eq!(Color::Blue.brighten(0.5), Color::Blue);
        assert_eq!(Color::Blue.darken(0.5), Color::Blue);
    }
}
