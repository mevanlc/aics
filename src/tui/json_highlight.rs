//! Syntect-backed JSON highlighter for fallback rendering of arbitrary tool
//! payloads in the viewer. Pretty-prints a `serde_json::Value` (or attempts to
//! parse a string) and produces ratatui [`Line`]s with theme-stable colors.
//!
//! All entry points are infallible: invalid input falls back to plain spans
//! styled with `base_style`, so the renderer never has to branch on errors.

use std::sync::LazyLock;

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use serde_json::Value;
use syntect::easy::HighlightLines;
use syntect::highlighting::{Color as SyntectColor, FontStyle, ThemeSet};
use syntect::parsing::SyntaxSet;
use syntect::util::LinesWithEndings;

use crate::tui::ansi::strip_terminal_escapes;
use crate::tui::markdown::SYNTECT_THEME;
use crate::tui::theme::Theme;
use crate::tui::util::highlight_styled_spans;

static SYNTAX_SET: LazyLock<SyntaxSet> = LazyLock::new(SyntaxSet::load_defaults_newlines);
static THEME_SET: LazyLock<ThemeSet> = LazyLock::new(ThemeSet::load_defaults);

/// Pretty-print and syntax-highlight a JSON value into a list of styled
/// [`Line`]s. Each emitted line carries the requested base style so the
/// surrounding viewer chrome (selection background, etc.) layers cleanly.
///
/// `terms` is the search-query overlay (already extracted by the caller via
/// `extract_highlight_terms`). Pass an empty slice when no overlay is wanted.
pub fn highlight_json_value(
    value: &Value,
    base_style: Style,
    theme: &Theme,
    terms: &[String],
) -> Vec<Line<'static>> {
    let pretty = serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string());
    highlight_json_string(&pretty, base_style, theme, terms)
}

/// Treat `text` as JSON source and produce highlighted lines. If the text
/// cannot be parsed as JSON, emits plain styled lines so the caller can use
/// this unconditionally.
pub fn highlight_json_string(
    text: &str,
    base_style: Style,
    theme: &Theme,
    terms: &[String],
) -> Vec<Line<'static>> {
    let pretty = pretty_print_if_parseable(text);
    let sanitized;
    let source: &str = if let Some(pretty) = pretty.as_deref() {
        pretty
    } else {
        sanitized = strip_terminal_escapes(text);
        &sanitized
    };

    let Some(mut highlighter) = create_highlighter() else {
        return plain_lines(source, base_style, theme, terms);
    };

    let search_overlay = search_overlay_style(theme);
    let mut lines = Vec::new();
    for raw_line in LinesWithEndings::from(source) {
        let line_text = raw_line.strip_suffix('\n').unwrap_or(raw_line);
        let segments = match highlighter.highlight_line(raw_line, &SYNTAX_SET) {
            Ok(segs) => segs,
            Err(_) => {
                lines.push(plain_line(
                    line_text,
                    base_style,
                    theme,
                    terms,
                    search_overlay,
                ));
                continue;
            }
        };

        let mut spans: Vec<Span<'static>> = segments
            .into_iter()
            .map(|(style, text)| {
                Span::styled(text.to_owned(), syntect_style_to_ratatui(style, base_style))
            })
            .collect();
        trim_trailing_newline(&mut spans);
        let spans = highlight_styled_spans(spans, terms, search_overlay);
        let mut line = Line::from(spans);
        line.style = base_style;
        lines.push(line);
    }
    lines
}

/// Returns true iff the given string parses as JSON whose top level is an
/// object or array. Useful as a "should I render this as JSON?" gate; primitive
/// JSON like a bare number or quoted string is uninteresting to highlight.
pub fn looks_like_json_object_or_array(text: &str) -> bool {
    let trimmed = text.trim_start();
    if !(trimmed.starts_with('{') || trimmed.starts_with('[')) {
        return false;
    }
    serde_json::from_str::<Value>(trimmed)
        .ok()
        .is_some_and(|v| matches!(v, Value::Object(_) | Value::Array(_)))
}

fn pretty_print_if_parseable(text: &str) -> Option<String> {
    let trimmed = text.trim_start();
    if !(trimmed.starts_with('{') || trimmed.starts_with('[')) {
        return None;
    }
    let value: Value = serde_json::from_str(trimmed).ok()?;
    serde_json::to_string_pretty(&value).ok()
}

fn create_highlighter() -> Option<HighlightLines<'static>> {
    let syntax = SYNTAX_SET
        .find_syntax_by_token("json")
        .or_else(|| SYNTAX_SET.find_syntax_by_extension("json"))?;
    let theme = THEME_SET.themes.get(SYNTECT_THEME)?;
    Some(HighlightLines::new(syntax, theme))
}

fn plain_lines(
    text: &str,
    base_style: Style,
    theme: &Theme,
    terms: &[String],
) -> Vec<Line<'static>> {
    let overlay = search_overlay_style(theme);
    text.split('\n')
        .map(|line| plain_line(line, base_style, theme, terms, overlay))
        .collect()
}

fn plain_line(
    text: &str,
    base_style: Style,
    _theme: &Theme,
    terms: &[String],
    overlay: Style,
) -> Line<'static> {
    let span = Span::styled(strip_terminal_escapes(text), base_style);
    let spans = highlight_styled_spans(vec![span], terms, overlay);
    let mut line = Line::from(spans);
    line.style = base_style;
    line
}

fn search_overlay_style(theme: &Theme) -> Style {
    Style::default().bg(theme.search_match_bg).fg(theme.text)
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

fn to_ratatui_color(color: SyntectColor) -> ratatui::style::Color {
    ratatui::style::Color::Rgb(color.r, color.g, color.b)
}

fn trim_trailing_newline(spans: &mut Vec<Span<'static>>) {
    if let Some(last) = spans.last_mut() {
        if let Some(stripped) = last.content.strip_suffix('\n') {
            last.content = stripped.to_owned().into();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn theme() -> Theme {
        Theme::default()
    }

    #[test]
    fn highlights_object_into_pretty_lines() {
        let v = json!({"foo": 1, "bar": [true, null]});
        let lines = highlight_json_value(&v, Style::default(), &theme(), &[]);
        let joined: String = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect::<Vec<_>>()
            .join("");
        assert!(joined.contains("\"foo\""));
        assert!(joined.contains("\"bar\""));
        assert!(
            lines.len() >= 4,
            "pretty-printed object should span multiple lines"
        );
    }

    #[test]
    fn highlight_string_round_trips_invalid_json() {
        let lines = highlight_json_string("not json at all", Style::default(), &theme(), &[]);
        assert_eq!(lines.len(), 1);
        let text: String = lines[0]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect::<Vec<_>>()
            .join("");
        assert_eq!(text, "not json at all");
    }

    #[test]
    fn highlight_string_pretty_prints_compact_json() {
        let lines = highlight_json_string(r#"{"a":1,"b":2}"#, Style::default(), &theme(), &[]);
        assert!(
            lines.len() > 1,
            "compact JSON should be expanded over multiple lines"
        );
    }

    #[test]
    fn looks_like_json_object_detects_arrays_and_objects() {
        assert!(looks_like_json_object_or_array(r#"{"a":1}"#));
        assert!(looks_like_json_object_or_array("  [1, 2, 3]"));
        assert!(!looks_like_json_object_or_array("hello"));
        assert!(!looks_like_json_object_or_array("\"just a string\""));
        assert!(!looks_like_json_object_or_array("{not json"));
    }

    #[test]
    fn search_terms_overlay_applied() {
        let terms = vec!["foo".to_owned()];
        let v = json!({"foo": "bar"});
        let lines = highlight_json_value(&v, Style::default(), &theme(), &terms);
        let any_overlay = lines.iter().any(|l| {
            l.spans
                .iter()
                .any(|s| s.style.bg == Some(theme().search_match_bg))
        });
        assert!(
            any_overlay,
            "expected at least one span with the search-overlay background"
        );
    }
}
