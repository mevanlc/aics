//! A single word-wrap transform for drawing and locating readable text.

use std::collections::VecDeque;
use std::ops::Range;

use ratatui::buffer::{Buffer, CellWidth};
use ratatui::layout::Alignment;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span, Text};
use unicode_segmentation::UnicodeSegmentation;

use crate::search_match::{merge_ranges, DocumentMap};

#[derive(Debug, Clone)]
pub struct LayoutGrapheme {
    pub rendered: Range<usize>,
    pub bytes: Range<usize>,
    pub columns: Range<u16>,
}

#[derive(Debug, Clone)]
pub struct LayoutRow {
    pub source_line: usize,
    pub graphemes: Vec<LayoutGrapheme>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisualRange {
    pub row: usize,
    pub columns: Range<u16>,
}

#[derive(Debug, Clone)]
pub struct LayoutDocument {
    pub text: Text<'static>,
    pub rows: Vec<LayoutRow>,
    line_rows: Vec<Range<usize>>,
    line_starts: Vec<usize>,
    plain_len: usize,
}

#[derive(Clone)]
struct Glyph<'a> {
    symbol: &'a str,
    style: Style,
    rendered: Range<usize>,
    width: usize,
    whitespace: bool,
}

impl LayoutDocument {
    pub fn new(text: &Text<'_>, width: u16) -> Self {
        let map = DocumentMap::from_text(text);
        let mut output = Self {
            text: Text::default().style(text.style),
            rows: Vec::new(),
            line_rows: Vec::with_capacity(text.lines.len()),
            line_starts: map.line_starts,
            plain_len: map.plain.len(),
        };
        for (line_index, line) in text.lines.iter().enumerate() {
            let start = output.rows.len();
            if width > 0 {
                let mut byte = output.line_starts[line_index];
                let mut glyphs = Vec::new();
                for span in &line.spans {
                    for (offset, symbol) in span.content.grapheme_indices(true) {
                        // Match Ratatui's styled-grapheme control filtering.
                        if symbol.chars().any(char::is_control) {
                            continue;
                        }
                        glyphs.push(Glyph {
                            symbol,
                            style: span.style,
                            rendered: byte + offset..byte + offset + symbol.len(),
                            width: usize::from(symbol.cell_width()),
                            whitespace: is_whitespace(symbol),
                        });
                    }
                    byte += span.content.len();
                }
                for row in wrap_glyphs(glyphs, usize::from(width)) {
                    let mut prepared = Line::default().style(line.style);
                    prepared.alignment = line.alignment.or(text.alignment);
                    let mut mapped = Vec::with_capacity(row.len());
                    let mut bytes = 0;
                    let mut column = 0;
                    for glyph in row {
                        push_span(&mut prepared.spans, glyph.symbol, glyph.style);
                        mapped.push(LayoutGrapheme {
                            rendered: glyph.rendered,
                            bytes: bytes..bytes + glyph.symbol.len(),
                            columns: column as u16..(column + glyph.width) as u16,
                        });
                        bytes += glyph.symbol.len();
                        column += glyph.width;
                    }
                    let alignment = prepared.alignment.unwrap_or(Alignment::Left);
                    let column_offset = match alignment {
                        Alignment::Left => 0,
                        Alignment::Center => (width / 2).saturating_sub(column as u16 / 2),
                        Alignment::Right => width.saturating_sub(column as u16),
                    };
                    for glyph in &mut mapped {
                        glyph.columns =
                            glyph.columns.start + column_offset..glyph.columns.end + column_offset;
                    }
                    output.text.lines.push(prepared);
                    output.rows.push(LayoutRow {
                        source_line: line_index,
                        graphemes: mapped,
                    });
                }
            }
            output.line_rows.push(start..output.rows.len());
        }
        output
    }

    pub fn height(&self) -> usize {
        self.rows.len()
    }

    pub fn rows_for_lines(&self, lines: Range<usize>) -> Range<usize> {
        let start = self
            .line_rows
            .get(lines.start)
            .map_or(self.height(), |rows| rows.start);
        let end = lines
            .end
            .checked_sub(1)
            .and_then(|end| self.line_rows.get(end))
            .map_or(start, |rows| rows.end);
        start..end.max(start)
    }

    pub fn row_for_offset(&self, offset: usize) -> usize {
        let source_line = self
            .line_starts
            .partition_point(|start| *start <= offset.min(self.plain_len))
            .saturating_sub(1);
        let Some(rows) = self.line_rows.get(source_line) else {
            return 0;
        };
        for row in rows.clone() {
            if self.rows[row]
                .graphemes
                .iter()
                .any(|glyph| offset < glyph.rendered.end)
            {
                return row;
            }
        }
        rows.end.saturating_sub(1)
    }

    pub fn offset_for_row(&self, row: usize) -> usize {
        self.rows.get(row).map_or(self.plain_len, |row| {
            row.graphemes.first().map_or_else(
                || {
                    self.line_starts
                        .get(row.source_line)
                        .copied()
                        .unwrap_or(self.plain_len)
                },
                |glyph| glyph.rendered.start,
            )
        })
    }

    pub fn locate(&self, range: Range<usize>) -> Vec<VisualRange> {
        if self.rows.is_empty() {
            return Vec::new();
        }
        if range.is_empty() {
            let row = self.row_for_offset(range.start);
            let Some(line) = self.rows.get(row) else {
                return Vec::new();
            };
            let column = line
                .graphemes
                .iter()
                .find(|glyph| glyph.rendered.end > range.start)
                .map(|glyph| glyph.columns.start)
                .or_else(|| line.graphemes.last().map(|glyph| glyph.columns.end))
                .unwrap_or(0);
            return vec![VisualRange {
                row,
                columns: column..column,
            }];
        }
        let start_row = self.row_for_offset(range.start);
        let end_row = self.row_for_offset(range.end.saturating_sub(1));
        let mut output = Vec::new();
        for row in start_row..=end_row.min(self.height().saturating_sub(1)) {
            let mut columns: Option<Range<u16>> = None;
            for glyph in &self.rows[row].graphemes {
                if glyph.rendered.start < range.end && range.start < glyph.rendered.end {
                    if let Some(columns) = &mut columns {
                        columns.end = glyph.columns.end;
                    } else {
                        columns = Some(glyph.columns.clone());
                    }
                }
            }
            if let Some(columns) = columns {
                output.push(VisualRange { row, columns });
            }
        }
        output
    }

    pub fn highlight_ranges(&self, ranges: &[Range<usize>], style: Style) -> Text<'static> {
        let mut text = self.text.clone();
        self.overlay_ranges(&mut text, ranges, style);
        text
    }

    /// Apply styles to the cached visual rows without changing their geometry.
    pub fn overlay_ranges(&self, text: &mut Text<'static>, ranges: &[Range<usize>], style: Style) {
        let ranges = merge_ranges(ranges.to_vec());
        for (row, line) in self.rows.iter().zip(&mut text.lines) {
            let bytes: Vec<_> = row
                .graphemes
                .iter()
                .filter(|glyph| intersects_any(&ranges, &glyph.rendered))
                .map(|glyph| glyph.bytes.clone())
                .collect();
            if !bytes.is_empty() {
                overlay_line(line, &merge_ranges(bytes), style);
            }
        }
        // An end-of-line insertion position has no following glyph to style.
        for range in ranges.iter().filter(|range| range.is_empty()) {
            let row = self.row_for_offset(range.start);
            if let (Some(mapped), Some(line)) = (self.rows.get(row), text.lines.get_mut(row)) {
                if mapped
                    .graphemes
                    .iter()
                    .all(|glyph| glyph.rendered.end <= range.start)
                {
                    if let Some(glyph) = mapped.graphemes.last() {
                        overlay_line(line, std::slice::from_ref(&glyph.bytes), style);
                    } else {
                        line.spans.push(Span::styled(" ", style));
                    }
                }
            }
        }
    }
}

fn is_whitespace(symbol: &str) -> bool {
    symbol == "\u{200b}" || symbol.chars().all(char::is_whitespace) && symbol != "\u{00a0}"
}

/// Draw prepared rows directly, without the truncation/reflow of Paragraph.
/// WordWrapper may retain a final wide glyph whose trailing cell crosses the
/// right edge. Its starting cell is drawn, just as in Ratatui's wrapped path.
pub fn render_prepared(text: &Text<'_>, area: Rect, buf: &mut Buffer, scroll: usize) {
    let area = area.intersection(buf.area);
    if area.is_empty() {
        return;
    }
    for (row, line) in text
        .lines
        .iter()
        .skip(scroll)
        .take(usize::from(area.height))
        .enumerate()
    {
        let width = line
            .styled_graphemes(text.style)
            .map(|glyph| glyph.symbol.cell_width())
            .fold(0u16, u16::saturating_add);
        let mut column = match line.alignment.or(text.alignment).unwrap_or(Alignment::Left) {
            Alignment::Left => 0,
            Alignment::Center => (area.width / 2).saturating_sub(width / 2),
            Alignment::Right => area.width.saturating_sub(width),
        };
        for glyph in line.styled_graphemes(text.style) {
            let glyph_width = glyph.symbol.cell_width();
            if glyph_width == 0 {
                continue;
            }
            if column >= area.width {
                break;
            }
            buf[(area.x + column, area.y + row as u16)]
                .set_symbol(glyph.symbol)
                .set_style(glyph.style);
            column = column.saturating_add(glyph_width);
        }
    }
}

// Follow Ratatui's WordWrapper with trim=false. Unlike an independently
// inferred match row, the resulting rows are also the rows we actually draw.
fn wrap_glyphs(glyphs: Vec<Glyph<'_>>, width: usize) -> Vec<Vec<Glyph<'_>>> {
    let mut rows = Vec::new();
    let mut pending_line = Vec::new();
    let mut line_width = 0;
    let mut pending_word = Vec::new();
    let mut word_width = 0;
    let mut whitespace = VecDeque::<Glyph<'_>>::new();
    let mut whitespace_width = 0;
    let mut previous_non_whitespace = false;
    for glyph in glyphs {
        if glyph.width > width {
            continue;
        }
        let word_found = previous_non_whitespace && glyph.whitespace;
        let overflow =
            pending_line.is_empty() && word_width + whitespace_width + glyph.width > width;
        if word_found || overflow {
            pending_line.extend(whitespace.drain(..));
            line_width += whitespace_width;
            pending_line.append(&mut pending_word);
            line_width += word_width;
            whitespace_width = 0;
            word_width = 0;
        }
        if line_width >= width
            || glyph.width > 0 && line_width + whitespace_width + word_width >= width
        {
            let mut remaining = width.saturating_sub(line_width);
            rows.push(std::mem::take(&mut pending_line));
            line_width = 0;
            while whitespace
                .front()
                .is_some_and(|glyph| glyph.width <= remaining)
            {
                let removed = whitespace.pop_front().expect("checked front");
                whitespace_width -= removed.width;
                remaining -= removed.width;
            }
            if glyph.whitespace && whitespace.is_empty() {
                continue;
            }
        }
        previous_non_whitespace = !glyph.whitespace;
        if glyph.whitespace {
            whitespace_width += glyph.width;
            whitespace.push_back(glyph);
        } else {
            word_width += glyph.width;
            pending_word.push(glyph);
        }
    }
    pending_line.extend(whitespace);
    pending_line.append(&mut pending_word);
    if !pending_line.is_empty() {
        rows.push(pending_line);
    }
    if rows.is_empty() {
        rows.push(Vec::new());
    }
    rows
}

fn intersects_any(ranges: &[Range<usize>], glyph: &Range<usize>) -> bool {
    let index = ranges.partition_point(|range| {
        range.end < glyph.start || range.end == glyph.start && !range.is_empty()
    });
    ranges.get(index).is_some_and(|range| {
        if range.is_empty() {
            range.start >= glyph.start && range.start < glyph.end
        } else {
            range.start < glyph.end && glyph.start < range.end
        }
    })
}

fn push_span(spans: &mut Vec<Span<'static>>, symbol: &str, style: Style) {
    if let Some(previous) = spans.last_mut().filter(|span| span.style == style) {
        previous.content.to_mut().push_str(symbol);
    } else {
        spans.push(Span::styled(symbol.to_owned(), style));
    }
}

fn overlay_line(line: &mut Line<'static>, ranges: &[Range<usize>], style: Style) {
    let mut spans = Vec::new();
    let mut byte = 0;
    for span in &line.spans {
        for (offset, symbol) in span.content.grapheme_indices(true) {
            let range = byte + offset..byte + offset + symbol.len();
            let overlay = if intersects_any(ranges, &range) {
                span.style.patch(style)
            } else {
                span.style
            };
            push_span(&mut spans, symbol, overlay);
        }
        byte += span.content.len();
    }
    line.spans = spans;
}

/// Overlay unwrapped display ranges while preserving the original line shapes.
pub fn highlight_ranges(text: &Text<'_>, ranges: &[Range<usize>], style: Style) -> Text<'static> {
    let ranges = merge_ranges(ranges.to_vec());
    let mut output = Text::default().style(text.style);
    output.alignment = text.alignment;
    let mut byte = 0;
    for line in &text.lines {
        let mut owned = Line::default().style(line.style);
        owned.alignment = line.alignment;
        for span in &line.spans {
            owned
                .spans
                .push(Span::styled(span.content.to_string(), span.style));
        }
        let len: usize = line.spans.iter().map(|span| span.content.len()).sum();
        let local: Vec<_> = ranges
            .iter()
            .filter_map(|range| {
                let start = range.start.max(byte);
                let end = range.end.min(byte + len);
                (start < end
                    || range.is_empty() && start == end && start >= byte && start <= byte + len)
                    .then_some(start.saturating_sub(byte)..end.saturating_sub(byte))
            })
            .collect();
        overlay_line(&mut owned, &local, style);
        output.lines.push(owned);
        byte += len + 1;
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::style::{Color, Modifier};
    use ratatui::widgets::{Paragraph, Widget, Wrap};

    #[test]
    fn prepared_rows_match_ratatui_buffers_across_widths_styles_and_unicode() {
        let samples = [
            "",
            "a",
            " Hello World  ",
            "abc     def ghi",
            "\talpha\tbeta\t",
            "a\u{00a0}b c\u{200b}def",
            "中文 日本語 abc",
            "ｶﾞ ﾊﾟ aﾞ 紅ﾞ",
            "café e\u{301} 🧑‍💻 👩🏽‍🚀 ❤️",
            "longunbrokenword followed\nby text",
        ];
        for width in [0, 1, 2, 3, 5, 10, 20, 40, 80, 120] {
            for alignment in [Alignment::Left, Alignment::Center, Alignment::Right] {
                for sample in samples {
                    let mut text = Text::from(sample);
                    text.alignment = Some(alignment);
                    for line in &mut text.lines {
                        line.style = Style::default().bg(Color::Blue);
                        let content = line
                            .spans
                            .iter()
                            .map(|span| span.content.as_ref())
                            .collect::<String>();
                        line.spans = content
                            .graphemes(true)
                            .enumerate()
                            .map(|(index, symbol)| {
                                Span::styled(
                                    symbol.to_owned(),
                                    Style::default().fg(if index % 2 == 0 {
                                        Color::Red
                                    } else {
                                        Color::Green
                                    }),
                                )
                            })
                            .collect();
                    }
                    let layout = LayoutDocument::new(&text, width);
                    assert_eq!(
                        layout.height(),
                        Paragraph::new(text.clone())
                            .wrap(Wrap { trim: false })
                            .line_count(width),
                        "width={width}, alignment={alignment:?}, source={sample:?}"
                    );
                    let area = Rect::new(0, 0, width, 40);
                    let mut expected = Buffer::empty(area);
                    let mut actual = Buffer::empty(area);
                    Paragraph::new(text.clone())
                        .wrap(Wrap { trim: false })
                        .render(area, &mut expected);
                    render_prepared(&layout.text, area, &mut actual, 0);
                    assert_eq!(
                        actual, expected,
                        "width={width}, alignment={alignment:?}, source={sample:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn occurrence_offsets_distinguish_repeats_and_cross_wrap_ranges() {
        let layout = LayoutDocument::new(&Text::from("alpha alpha alpha"), 11);
        assert_eq!(layout.row_for_offset(0), 0);
        assert_eq!(layout.row_for_offset(6), 0);
        assert_eq!(layout.row_for_offset(12), 1);
        let highlighted = layout.highlight_ranges(
            std::slice::from_ref(&(6..11)),
            Style::default().bg(Color::Yellow),
        );
        let area = Rect::new(0, 0, 11, 2);
        let mut buffer = Buffer::empty(area);
        Paragraph::new(highlighted).render(area, &mut buffer);
        assert_eq!(buffer[(0, 0)].bg, Color::Reset);
        assert_eq!(buffer[(6, 0)].bg, Color::Yellow);
        assert_eq!(buffer[(0, 1)].bg, Color::Reset);
        assert_eq!(layout.locate(8..15).len(), 2);
    }

    #[test]
    fn overlays_preserve_styles_and_expand_to_grapheme_boundaries() {
        let text = Text::from(Line::from(Span::styled(
            "e\u{301}🧑‍💻",
            Style::default().add_modifier(Modifier::ITALIC),
        )));
        let layout = LayoutDocument::new(&text, 20);
        let result = layout.highlight_ranges(
            std::slice::from_ref(&(1..2)),
            Style::default().add_modifier(Modifier::UNDERLINED),
        );
        assert_eq!(result.lines[0].spans[0].content, "e\u{301}");
        assert!(result.lines[0].spans[0]
            .style
            .add_modifier
            .contains(Modifier::ITALIC | Modifier::UNDERLINED));
    }
}
