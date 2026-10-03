//! Coordinates shared by source search, readable rendering, and visual layout.

use std::collections::HashMap;
use std::ops::Range;

use ratatui::text::Text;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum SourceBlock {
    Cell(usize),
    Message(usize),
    Title,
    Context,
    Summary,
    Metrics(usize),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SourceId {
    pub block: SourceBlock,
    pub part: String,
}

impl SourceId {
    pub fn new(block: SourceBlock, part: impl Into<String>) -> Self {
        Self {
            block,
            part: part.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceRange {
    pub source: SourceId,
    pub range: Range<usize>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SourceMatch {
    pub sources: Vec<SourceRange>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderOrigin {
    pub rendered: Range<usize>,
    pub source: SourceRange,
}

/// Unwrapped display bytes. Newlines join logical lines; wrapping adds no bytes.
#[derive(Debug, Clone, Default)]
pub struct DocumentMap {
    pub plain: String,
    pub line_starts: Vec<usize>,
    pub origins: Vec<RenderOrigin>,
}

impl DocumentMap {
    pub fn from_text(text: &Text<'_>) -> Self {
        let mut map = Self::default();
        for (index, line) in text.lines.iter().enumerate() {
            if index > 0 {
                map.plain.push('\n');
            }
            map.line_starts.push(map.plain.len());
            for span in &line.spans {
                map.plain.push_str(&span.content);
            }
        }
        map
    }

    pub fn extend_origins(&mut self, other: &Self, offset: usize) {
        self.origins
            .extend(other.origins.iter().cloned().map(|mut origin| {
                origin.rendered = origin.rendered.start + offset..origin.rendered.end + offset;
                origin
            }));
    }

    /// Project only recorded provenance, never a textual coincidence elsewhere.
    pub fn project(&self, source: &SourceRange) -> Vec<Range<usize>> {
        project_origins(
            source,
            self.origins
                .iter()
                .filter(|origin| origin.source.source == source.source),
        )
    }

    /// Index source components once for a query's complete occurrence set.
    pub fn project_matches(&self, matches: &[SourceMatch]) -> Vec<Vec<Range<usize>>> {
        let mut origins: HashMap<&SourceId, Vec<&RenderOrigin>> = HashMap::new();
        for origin in &self.origins {
            origins
                .entry(&origin.source.source)
                .or_default()
                .push(origin);
        }
        matches
            .iter()
            .map(|matched| {
                merge_ranges(
                    matched
                        .sources
                        .iter()
                        .flat_map(|source| {
                            project_origins(
                                source,
                                origins.get(&source.source).into_iter().flatten().copied(),
                            )
                        })
                        .collect(),
                )
            })
            .collect()
    }

    pub fn project_match(&self, matched: &SourceMatch) -> Vec<Range<usize>> {
        merge_ranges(
            matched
                .sources
                .iter()
                .flat_map(|source| self.project(source))
                .collect(),
        )
    }
}

fn project_origins<'a>(
    source: &SourceRange,
    origins: impl Iterator<Item = &'a RenderOrigin>,
) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    for origin in origins {
        let original = &origin.source.range;
        if source.range.is_empty() {
            if source.range.start >= original.start && source.range.start <= original.end {
                let position = if original.len() == origin.rendered.len() {
                    origin.rendered.start + source.range.start - original.start
                } else {
                    origin.rendered.start
                };
                ranges.push(position..position);
            }
            continue;
        }
        let start = source.range.start.max(original.start);
        let end = source.range.end.min(original.end);
        if start >= end {
            continue;
        }
        if original.len() == origin.rendered.len() {
            ranges.push(
                origin.rendered.start + start - original.start
                    ..origin.rendered.start + end - original.start,
            );
        } else {
            // A decoded entity, expanded tab, or normalized code character
            // maps to its complete display fragment.
            ranges.push(origin.rendered.clone());
        }
    }
    merge_ranges(ranges)
}

pub fn merge_ranges(mut ranges: Vec<Range<usize>>) -> Vec<Range<usize>> {
    ranges.sort_by_key(|range| (range.start, range.end));
    let mut output: Vec<Range<usize>> = Vec::with_capacity(ranges.len());
    for range in ranges {
        if let Some(previous) = output.last_mut() {
            if range.start <= previous.end {
                previous.end = previous.end.max(range.end);
                continue;
            }
        }
        output.push(range);
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transformed_origins_and_identical_text_have_distinct_coordinates() {
        let source = SourceId::new(SourceBlock::Cell(0), "content");
        let mut map = DocumentMap::from_text(&Text::from("a & a"));
        map.origins = vec![
            RenderOrigin {
                rendered: 0..2,
                source: SourceRange {
                    source: source.clone(),
                    range: 0..2,
                },
            },
            RenderOrigin {
                rendered: 2..3,
                source: SourceRange {
                    source: source.clone(),
                    range: 2..7,
                },
            },
            RenderOrigin {
                rendered: 3..5,
                source: SourceRange {
                    source: source.clone(),
                    range: 7..9,
                },
            },
        ];
        assert_eq!(
            map.project(&SourceRange {
                source: source.clone(),
                range: 2..7
            }),
            vec![2..3]
        );
        assert_eq!(
            map.project(&SourceRange {
                source,
                range: 8..9
            }),
            vec![4..5]
        );
        assert!(map
            .project(&SourceRange {
                source: SourceId::new(SourceBlock::Cell(1), "content"),
                range: 0..1
            })
            .is_empty());
        let matches = vec![
            SourceMatch {
                sources: vec![SourceRange {
                    source: SourceId::new(SourceBlock::Cell(0), "content"),
                    range: 2..7,
                }],
            },
            SourceMatch {
                sources: vec![SourceRange {
                    source: SourceId::new(SourceBlock::Cell(0), "content"),
                    range: 8..9,
                }],
            },
            SourceMatch {
                sources: vec![SourceRange {
                    source: SourceId::new(SourceBlock::Cell(1), "content"),
                    range: 0..1,
                }],
            },
            SourceMatch {
                sources: vec![SourceRange {
                    source: SourceId::new(SourceBlock::Cell(0), "content"),
                    range: 0..0,
                }],
            },
        ];
        assert_eq!(
            map.project_matches(&matches),
            matches
                .iter()
                .map(|matched| map.project_match(matched))
                .collect::<Vec<_>>()
        );
    }
}
