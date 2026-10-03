//! Canonical searchable source text and its correspondence to transcript parts.
//!
//! Search fields contain source bytes, never Markdown's rendered text. The maps
//! also retain parser-only facets, whose matches may have no display location.

use std::collections::BTreeMap;
use std::ops::Range;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::parse::search_fields::{authored_user_text, readable_tool_text};
use crate::parse::{
    is_internal_context_injection, is_project_docs_autodump, is_skill_text_injection, MessageRole,
    Session, SessionCell,
};
use crate::search_match::{SourceBlock, SourceId, SourceMatch, SourceRange};
use crate::search_query::VisibilitySearch;
use crate::settings::DisplayOptions;

const USER: u16 = 1;
const AGENT: u16 = 2;
const TOOL_CALL: u16 = 4;
const TOOL_RESULT: u16 = 8;
const PROJECT_DOCS: u16 = 16;
const SKILL: u16 = 32;
const INTERNAL_CONTEXT: u16 = 64;
const USER_TOOL_CALL: u16 = 128;
const USER_TOOL_RESULT: u16 = 256;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VisibilityRequirement(pub u16);

impl VisibilityRequirement {
    pub fn is_visible(self, options: DisplayOptions) -> bool {
        let hidden_bit = |hidden, bit| if hidden { bit } else { 0 };
        let hidden = hidden_bit(options.hide_user_messages, USER)
            | hidden_bit(options.hide_agent_replies, AGENT)
            | hidden_bit(options.hide_tool_calls, TOOL_CALL)
            | hidden_bit(options.hide_tool_results, TOOL_RESULT)
            | hidden_bit(options.hide_project_docs_autodump, PROJECT_DOCS)
            | hidden_bit(options.hide_skill_text_injection, SKILL)
            | hidden_bit(options.hide_internal_context, INTERNAL_CONTEXT)
            | hidden_bit(options.hide_user_tool_calls, USER_TOOL_CALL)
            | hidden_bit(options.hide_user_tool_results, USER_TOOL_RESULT);
        self.0 & hidden == 0
    }

    fn field(self) -> &'static str {
        match self.0 {
            0 => "_vis_always",
            USER => "_vis_user",
            AGENT => "_vis_agent",
            TOOL_CALL => "_vis_toolcall",
            TOOL_RESULT => "_vis_toolresult",
            value if value == TOOL_CALL | TOOL_RESULT => "_vis_toolcall_result",
            PROJECT_DOCS => "_vis_projectdocs",
            value if value == USER | PROJECT_DOCS => "_vis_user_projectdocs",
            value if value == USER | SKILL => "_vis_user_skill",
            value if value == USER | INTERNAL_CONTEXT => "_vis_user_internal_context",
            USER_TOOL_CALL => "_vis_user_toolcall",
            value if value == USER_TOOL_CALL | USER_TOOL_RESULT => "_vis_user_toolcall_result",
            _ => unreachable!("unsupported visibility requirement"),
        }
    }
}

pub fn message_visibility(role: MessageRole, content: &str) -> VisibilityRequirement {
    let user = if role == MessageRole::User { USER } else { 0 };
    VisibilityRequirement(if is_project_docs_autodump(role, content) {
        user | PROJECT_DOCS
    } else if is_skill_text_injection(role, content) {
        USER | SKILL
    } else if is_internal_context_injection(role, content) {
        USER | INTERNAL_CONTEXT
    } else {
        match role {
            MessageRole::User => USER,
            MessageRole::Assistant => AGENT,
            MessageRole::ToolCall => TOOL_CALL,
            MessageRole::ToolResult => TOOL_RESULT,
            MessageRole::System | MessageRole::Summary => 0,
        }
    })
}

pub fn cell_visible(cell: &SessionCell, options: &DisplayOptions) -> bool {
    match cell {
        SessionCell::Message { role, content, .. } => {
            message_visibility(*role, content).is_visible(*options)
        }
        SessionCell::Reasoning { .. } => !options.hide_agent_replies,
        SessionCell::Exec { is_user: true, .. } => !options.hide_user_tool_calls,
        SessionCell::ToolCall { .. }
        | SessionCell::Exec { .. }
        | SessionCell::Patch { .. }
        | SessionCell::WebSearch { .. } => !options.hide_tool_calls,
        SessionCell::ToolResult { .. } => !options.hide_tool_results,
        _ => true,
    }
}

pub fn default_search_field_names(
    mode: VisibilitySearch,
    options: DisplayOptions,
) -> Vec<&'static str> {
    if mode == VisibilitySearch::All {
        return vec!["content"];
    }
    [
        0,
        USER,
        AGENT,
        TOOL_CALL,
        TOOL_RESULT,
        TOOL_CALL | TOOL_RESULT,
        PROJECT_DOCS,
        USER | PROJECT_DOCS,
        USER | SKILL,
        USER | INTERNAL_CONTEXT,
        USER_TOOL_CALL,
        USER_TOOL_CALL | USER_TOOL_RESULT,
    ]
    .into_iter()
    .map(VisibilityRequirement)
    .filter(|requirement| requirement.is_visible(options) == (mode == VisibilitySearch::Visible))
    .map(VisibilityRequirement::field)
    .collect()
}

#[derive(Debug, Clone)]
pub struct SearchSegment {
    pub source: SourceId,
    pub text: String,
    pub visibility: VisibilityRequirement,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectedRange {
    #[serde(rename = "r")]
    pub range: Range<usize>,
    #[serde(rename = "s")]
    pub source: SourceRange,
}

impl ProjectedRange {
    fn clip(&self, range: &Range<usize>) -> Option<SourceRange> {
        let start = range.start.max(self.range.start);
        let end = range.end.min(self.range.end);
        if start >= end {
            return None;
        }
        Some(SourceRange {
            source: self.source.source.clone(),
            range: if self.range.len() == self.source.range.len() {
                self.source.range.start + start - self.range.start
                    ..self.source.range.start + end - self.range.start
            } else {
                // A decoded Unicode character or escape is an atomic source
                // unit: any overlap owns its complete lexical byte interval.
                self.source.range.clone()
            },
        })
    }
}

#[derive(Debug, Clone, Default)]
pub struct ProjectedText {
    pub text: String,
    pub origins: Vec<ProjectedRange>,
}

impl ProjectedText {
    pub fn project_occurrences(&self, range: Range<usize>) -> Vec<SourceMatch> {
        let overlapping = self
            .origins
            .iter()
            .filter(|origin| origin.range.start < range.end && origin.range.end > range.start)
            .collect::<Vec<_>>();
        Self::project_overlapping(range, overlapping)
    }

    pub(crate) fn project_overlapping(
        range: Range<usize>,
        overlapping: Vec<&ProjectedRange>,
    ) -> Vec<SourceMatch> {
        let mut alternatives = BTreeMap::<SourceBlock, Vec<&ProjectedRange>>::new();
        for origin in &overlapping {
            if matches!(
                origin.source.source.block,
                SourceBlock::Cell(_) | SourceBlock::Message(_)
            ) {
                alternatives
                    .entry(origin.source.source.block.clone())
                    .or_default()
                    .push(origin);
            }
        }
        if alternatives.len() > 1 {
            let coverage = |origins: &[&ProjectedRange]| {
                let mut spans = origins
                    .iter()
                    .map(|origin| {
                        range.start.max(origin.range.start)..range.end.min(origin.range.end)
                    })
                    .collect::<Vec<_>>();
                spans.sort_by_key(|span| span.start);
                let mut merged = Vec::<Range<usize>>::new();
                for span in spans {
                    if let Some(last) = merged.last_mut().filter(|last| last.end >= span.start) {
                        last.end = last.end.max(span.end);
                    } else {
                        merged.push(span);
                    }
                }
                merged
            };
            let patterns = alternatives
                .values()
                .map(|origins| coverage(origins))
                .collect::<Vec<_>>();
            if patterns.iter().all(|pattern| pattern == &patterns[0]) {
                // The same parser chunk may originate in repeated structured
                // cells. Keep those alternate records independent, while a
                // phrase spanning genuinely different intervals stays one match.
                return alternatives
                    .into_values()
                    .flat_map(|origins| Self::project_overlapping(range.clone(), origins))
                    .collect();
            }
        }
        let mut by_source = BTreeMap::<SourceId, Vec<&ProjectedRange>>::new();
        for origin in &overlapping {
            by_source
                .entry(origin.source.source.clone())
                .or_default()
                .push(origin);
        }
        let mut groups = Vec::<Vec<&ProjectedRange>>::new();
        for mut origins in by_source.into_values() {
            origins.sort_by_key(|origin| (origin.source.range.start, origin.source.range.end));
            let mut group = Vec::<&ProjectedRange>::new();
            let mut end = 0;
            for origin in origins {
                if !group.is_empty() && origin.source.range.start > end {
                    groups.push(std::mem::take(&mut group));
                }
                end = if group.is_empty() {
                    origin.source.range.end
                } else {
                    end.max(origin.source.range.end)
                };
                group.push(origin);
            }
            if !group.is_empty() {
                groups.push(group);
            }
        }
        if groups.len() > 1 {
            let patterns = groups
                .iter()
                .map(|group| {
                    let mut ranges = group
                        .iter()
                        .map(|origin| {
                            range.start.max(origin.range.start)..range.end.min(origin.range.end)
                        })
                        .collect::<Vec<_>>();
                    ranges.sort_by_key(|range| (range.start, range.end));
                    let mut merged = Vec::<Range<usize>>::new();
                    for range in ranges {
                        if let Some(last) = merged.last_mut().filter(|last| last.end >= range.start)
                        {
                            last.end = last.end.max(range.end);
                        } else {
                            merged.push(range);
                        }
                    }
                    merged
                })
                .collect::<Vec<_>>();
            if patterns.iter().all(|pattern| pattern == &patterns[0]) {
                // One decoded primitive may comprise several escape units.
                // Group contiguous lexical units before separating duplicate
                // primitive occurrences, including within the same source ID.
                return groups
                    .into_iter()
                    .map(|group| SourceMatch {
                        sources: group
                            .into_iter()
                            .filter_map(|origin| origin.clip(&range))
                            .collect(),
                    })
                    .collect();
            }
        }
        vec![SourceMatch {
            sources: overlapping
                .into_iter()
                .filter_map(|origin| origin.clip(&range))
                .collect(),
        }]
    }

    pub fn project(&self, range: Range<usize>) -> SourceMatch {
        SourceMatch {
            sources: self
                .origins
                .iter()
                .filter_map(|origin| origin.clip(&range))
                .collect(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct SearchProjection {
    pub segments: Vec<SearchSegment>,
    pub fields: BTreeMap<String, Vec<ProjectedText>>,
}

/// Text already lives in stored Tantivy fields; only coordinate maps are stored
/// here. This never becomes part of a session's JSON or Markdown export.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProjectionProvenance {
    #[serde(rename = "s")]
    sources: Vec<SourceId>,
    /// Entries are [field start, field end, source ID, source start, source end].
    #[serde(rename = "f")]
    fields: BTreeMap<String, Vec<Vec<[usize; 5]>>>,
}

impl SearchProjection {
    pub fn from_session(session: &Session) -> Self {
        let mut result = Self::default();
        if let Some(title) = &session.custom_title {
            result.segment(
                SourceBlock::Title,
                "content",
                title,
                VisibilityRequirement(0),
            );
        }
        for (index, body) in session.search_fields.native_summaries.iter().enumerate() {
            result.segment(
                SourceBlock::Context,
                &format!("native_summary/{index}"),
                body,
                VisibilityRequirement(0),
            );
        }
        if session.cells.is_empty() {
            for (index, message) in session.messages.iter().enumerate() {
                if message.role == MessageRole::Summary
                    && !session.search_fields.native_summaries.is_empty()
                {
                    continue;
                }
                result.segment(
                    SourceBlock::Message(index),
                    "content",
                    &message.content,
                    message_visibility(message.role, &message.content),
                );
                if let Some(source) = session.search_fields.tool_call_sources.get(&index) {
                    result.segment(
                        SourceBlock::Message(index),
                        "raw_name",
                        &source.raw_name,
                        VisibilityRequirement(TOOL_CALL),
                    );
                    let mut values = Vec::new();
                    readable_values(&source.input, "input", &mut values);
                    for (part, text) in values {
                        result.segment(
                            SourceBlock::Message(index),
                            &part,
                            &text,
                            VisibilityRequirement(TOOL_CALL),
                        );
                    }
                }
            }
            // Some synthetic sessions have only flattened source text.
            if session.messages.is_empty() {
                result.segment(
                    SourceBlock::Context,
                    "source",
                    &session.content,
                    VisibilityRequirement(0),
                );
            }
        } else {
            for (index, cell) in session.cells.iter().enumerate() {
                if matches!(
                    cell,
                    SessionCell::Message {
                        role: MessageRole::Summary,
                        ..
                    }
                ) && !session.search_fields.native_summaries.is_empty()
                {
                    continue;
                }
                result.cell(
                    index,
                    cell,
                    session.search_fields.tool_call_sources.get(&index),
                );
            }
        }
        // Preserve parser-produced semantic membership and exclusions exactly.
        // The same byte ranges are reused when a facet points into a cell.
        for (field, chunks) in [
            ("user", &session.search_fields.user),
            ("agent", &session.search_fields.agent),
            ("toolcall", &session.search_fields.tool_call),
            ("toolresult", &session.search_fields.tool_result),
        ] {
            if !chunks.is_empty() {
                let values = result.semantic_text(field, chunks, session);
                result.fields.insert(field.to_owned(), vec![values]);
            } else {
                // Hand-constructed sessions and old message-only sessions still
                // have useful field semantics without parser facets.
                result.fallback_semantic_field(field, session);
            }
        }
        let path_sources = result.path_sources();
        for (field, paths) in [
            ("dirs", &session.search_fields.dirs),
            ("files", &session.search_fields.files),
            ("paths", &session.search_fields.paths),
        ] {
            for path in paths {
                result.path(field, path, &path_sources);
            }
        }
        if let Some(cwd) = &session.cwd {
            result.path("working_dir", cwd, &path_sources);
        }
        for (field, text) in [
            (
                "file_path",
                session.file_path.to_string_lossy().into_owned(),
            ),
            ("modified_ts", session.modified_ts.to_string()),
        ] {
            result.fields.insert(
                field.to_owned(),
                vec![ProjectedText {
                    origins: vec![ProjectedRange {
                        range: 0..text.len(),
                        source: SourceRange {
                            source: SourceId::new(SourceBlock::Context, format!("index/{field}")),
                            range: 0..text.len(),
                        },
                    }],
                    text,
                }],
            );
        }
        result
    }

    fn segment(
        &mut self,
        block: SourceBlock,
        part: &str,
        text: &str,
        visibility: VisibilityRequirement,
    ) {
        if text.trim().is_empty() {
            return;
        }
        let source = SourceId::new(block, part);
        let components = if part == "input" || part.starts_with("input/") {
            readable_string_components(source.block.clone(), part, text)
        } else {
            vec![trimmed_source_text(source.block.clone(), part, text)]
        };
        for component in components {
            for field in ["content", visibility.field()] {
                self.append(field, &component);
            }
        }
        self.segments.push(SearchSegment {
            source,
            text: text.to_owned(),
            visibility,
        });
    }

    fn append(&mut self, field: &str, component: &ProjectedText) {
        let value = self
            .fields
            .entry(field.to_owned())
            .or_insert_with(|| vec![ProjectedText::default()]);
        let value = &mut value[0];
        if !value.text.is_empty() {
            value.text.push_str("\n\n");
        }
        let offset = value.text.len();
        value.text.push_str(&component.text);
        for origin in &component.origins {
            value.origins.push(ProjectedRange {
                range: offset + origin.range.start..offset + origin.range.end,
                source: origin.source.clone(),
            });
        }
    }

    fn cell(
        &mut self,
        index: usize,
        cell: &SessionCell,
        tool_source: Option<&crate::parse::ToolCallSource>,
    ) {
        let block = SourceBlock::Cell(index);
        let mut add = |part: &str, text: &str, requirement: u16| {
            self.segment(
                block.clone(),
                part,
                text,
                VisibilityRequirement(requirement),
            );
        };
        match cell {
            SessionCell::Message { role, content, .. } => {
                add("content", content, message_visibility(*role, content).0);
            }
            SessionCell::Reasoning { header, body, .. } => {
                if let Some(header) = header {
                    add("header", header, AGENT);
                }
                add("body", body, AGENT);
            }
            SessionCell::ToolCall {
                tool,
                raw_name,
                summary,
                input,
                ..
            } => {
                add("tool", tool, TOOL_CALL);
                add(
                    "raw_name",
                    tool_source.map_or(raw_name.as_str(), |source| source.raw_name.as_str()),
                    TOOL_CALL,
                );
                add("summary", summary, TOOL_CALL);
                let mut values = Vec::new();
                readable_values(
                    tool_source.map_or(input, |source| &source.input),
                    "input",
                    &mut values,
                );
                for (path, text) in values {
                    add(&path, &text, TOOL_CALL);
                }
            }
            SessionCell::ToolResult {
                tool,
                output,
                call_summary,
                ..
            } => {
                if let Some(tool) = tool {
                    add("tool", tool, TOOL_RESULT);
                }
                if let Some(summary) = call_summary {
                    add("call_summary", summary, TOOL_RESULT);
                }
                add("output", output, TOOL_RESULT);
            }
            SessionCell::Exec {
                command,
                cwd,
                parsed_summary,
                stdout,
                stderr,
                is_user,
                ..
            } => {
                let call = if *is_user { USER_TOOL_CALL } else { TOOL_CALL };
                let output = call
                    | if *is_user {
                        USER_TOOL_RESULT
                    } else {
                        TOOL_RESULT
                    };
                add("command", &command.join(" "), call);
                if let Some(cwd) = cwd {
                    add("cwd", cwd, call);
                }
                if let Some(summary) = parsed_summary {
                    add("parsed_summary", summary, call);
                }
                add("stdout", stdout, output);
                add("stderr", stderr, output);
            }
            SessionCell::Patch {
                files,
                stdout,
                stderr,
                ..
            } => {
                for (index, file) in files.iter().enumerate() {
                    add(&format!("files/{index}/path"), &file.path, TOOL_CALL);
                    if let Some(content) = &file.content {
                        add(&format!("files/{index}/content"), content, TOOL_CALL);
                    }
                }
                add("stdout", stdout, TOOL_CALL | TOOL_RESULT);
                add("stderr", stderr, TOOL_CALL | TOOL_RESULT);
            }
            SessionCell::WebSearch { query, queries, .. } => {
                add("query", query, TOOL_CALL);
                for (index, query) in queries.iter().enumerate() {
                    add(&format!("queries/{index}"), query, TOOL_CALL);
                }
            }
            SessionCell::Plan { items, .. } => {
                for (index, item) in items.iter().enumerate() {
                    add(&format!("items/{index}/step"), &item.step, 0);
                }
            }
            SessionCell::SessionInfo(_) | SessionCell::Metrics(_) => {}
        }
    }

    fn semantic_text(&self, field: &str, chunks: &[String], session: &Session) -> ProjectedText {
        let mut candidates = semantic_candidates(field, session);
        for available in candidates.values_mut() {
            available.reverse();
        }
        let mut counts = BTreeMap::<&str, usize>::new();
        for chunk in chunks {
            *counts.entry(chunk.as_str()).or_default() += 1;
        }
        let mut value = ProjectedText::default();
        for (index, chunk) in chunks.iter().enumerate() {
            if !value.text.is_empty() {
                value.text.push_str("\n\n");
            }
            let start = value.text.len();
            value.text.push_str(chunk);
            let remaining = counts.get_mut(chunk.as_str()).expect("counted chunk");
            let mut mapped = false;
            if let Some(available) = candidates.get_mut(chunk) {
                // Parser facets collapse adjacent duplicate chunks. Spread the
                // canonical records over their remaining indexed occurrences,
                // retaining every original component exactly once.
                let take = available.len().div_ceil(*remaining);
                for _ in 0..take {
                    let candidate = available.pop().expect("counted candidate");
                    for origin in candidate.origins {
                        mapped = true;
                        value.origins.push(ProjectedRange {
                            range: start + origin.range.start..start + origin.range.end,
                            source: origin.source,
                        });
                    }
                }
            }
            *remaining -= 1;
            if !mapped {
                // Never claim a fabricated transcript location for source-only data.
                value.origins.push(ProjectedRange {
                    range: start..value.text.len(),
                    source: SourceRange {
                        source: SourceId::new(
                            SourceBlock::Context,
                            format!("index/{field}/{index}"),
                        ),
                        range: 0..chunk.len(),
                    },
                });
            }
        }
        value
    }

    fn fallback_semantic_field(&mut self, field: &str, session: &Session) {
        let mut chunks = Vec::new();
        let mut visit = |role: MessageRole, text: &str| match (field, role) {
            ("user", MessageRole::User) => {
                if let Some(text) = authored_user_text(text) {
                    chunks.push(text);
                }
            }
            ("agent", MessageRole::Assistant | MessageRole::Summary)
            | ("toolcall", MessageRole::ToolCall)
            | ("toolresult", MessageRole::ToolResult) => chunks.push(text.to_owned()),
            _ => {}
        };
        if session.cells.is_empty() {
            for message in &session.messages {
                visit(message.role, &message.content);
            }
        } else {
            for cell in &session.cells {
                if let SessionCell::Message { role, content, .. } = cell {
                    visit(*role, content);
                }
            }
        }
        if !chunks.is_empty() {
            self.fields.insert(
                field.to_owned(),
                vec![self.semantic_text(field, &chunks, session)],
            );
        }
    }

    fn path_sources(&self) -> BTreeMap<String, Vec<SourceRange>> {
        let mut sources = BTreeMap::<String, Vec<SourceRange>>::new();
        for segment in &self.segments {
            let part = &segment.source.part;
            if !(part == "cwd"
                || part.starts_with("files/") && part.ends_with("/path")
                || part.starts_with("input/")
                    && part.split('/').skip(1).any(|key| {
                        let key = key
                            .chars()
                            .filter(|ch| ch.is_ascii_alphanumeric())
                            .collect::<String>()
                            .to_ascii_lowercase();
                        matches!(
                            key.as_str(),
                            "path"
                                | "paths"
                                | "file"
                                | "filepath"
                                | "cwd"
                                | "workdir"
                                | "workingdir"
                                | "directory"
                                | "directorypath"
                                | "dir"
                                | "targetfile"
                        )
                    }))
            {
                continue;
            }
            let normalized = segment.text.trim().replace('\\', "/");
            let normalized = normalized.trim_end_matches('/');
            let start = segment.text.len() - segment.text.trim_start().len();
            sources
                .entry(normalized.to_owned())
                .or_default()
                .push(SourceRange {
                    source: segment.source.clone(),
                    range: start..start + normalized.len(),
                });
        }
        sources
    }

    fn path(&mut self, field: &str, path: &str, path_sources: &BTreeMap<String, Vec<SourceRange>>) {
        let normalized = path.replace('\\', "/");
        let normalized = normalized.trim_end_matches('/');
        if normalized.is_empty() {
            return;
        }
        for start in
            std::iter::once(0).chain(normalized.match_indices('/').map(|(index, _)| index + 1))
        {
            let text = &normalized[start..];
            if text.is_empty() {
                continue;
            }
            let mut origins = if field == "working_dir" {
                Vec::new()
            } else {
                path_sources
                    .get(normalized)
                    .into_iter()
                    .flatten()
                    .map(|source| SourceRange {
                        source: source.source.clone(),
                        range: source.range.start + start..source.range.end,
                    })
                    .collect::<Vec<_>>()
            };
            if origins.is_empty() {
                origins.push(SourceRange {
                    source: SourceId::new(
                        SourceBlock::Context,
                        if field == "working_dir" { "cwd" } else { field },
                    ),
                    range: start..normalized.len(),
                });
            }
            let origins = origins
                .into_iter()
                .map(|source| ProjectedRange {
                    range: 0..text.len(),
                    source,
                })
                .collect();
            self.fields
                .entry(field.to_owned())
                .or_default()
                .push(ProjectedText {
                    text: text.to_owned(),
                    origins,
                });
        }
    }

    pub fn provenance(&self) -> ProjectionProvenance {
        let mut provenance = ProjectionProvenance::default();
        let mut ids = BTreeMap::new();
        for (field, values) in &self.fields {
            let values = values
                .iter()
                .map(|value| {
                    value
                        .origins
                        .iter()
                        .map(|origin| {
                            let source =
                                *ids.entry(origin.source.source.clone()).or_insert_with(|| {
                                    let id = provenance.sources.len();
                                    provenance.sources.push(origin.source.source.clone());
                                    id
                                });
                            [
                                origin.range.start,
                                origin.range.end,
                                source,
                                origin.source.range.start,
                                origin.source.range.end,
                            ]
                        })
                        .collect()
                })
                .collect();
            provenance.fields.insert(field.clone(), values);
        }
        provenance
    }

    pub fn from_stored_fields(
        fields: BTreeMap<String, Vec<String>>,
        provenance: ProjectionProvenance,
    ) -> Self {
        Self {
            segments: Vec::new(),
            fields: fields
                .into_iter()
                .map(|(field, texts)| {
                    let maps = provenance.fields.get(&field);
                    let values = texts
                        .into_iter()
                        .enumerate()
                        .map(|(index, text)| ProjectedText {
                            text,
                            origins: maps
                                .and_then(|maps| maps.get(index))
                                .into_iter()
                                .flatten()
                                .filter_map(|entry| {
                                    provenance.sources.get(entry[2]).cloned().map(|source| {
                                        ProjectedRange {
                                            range: entry[0]..entry[1],
                                            source: SourceRange {
                                                source,
                                                range: entry[3]..entry[4],
                                            },
                                        }
                                    })
                                })
                                .collect(),
                        })
                        .collect();
                    (field, values)
                })
                .collect(),
        }
    }
}

/// Reconstruct parser facet chunks from their actual source components. A
/// whole-component key is an identity check; it never searches unrelated text
/// for coincidentally equal words or assigns a derived summary to raw input.
fn semantic_candidates(field: &str, session: &Session) -> BTreeMap<String, Vec<ProjectedText>> {
    let mut candidates = BTreeMap::<String, Vec<ProjectedText>>::new();
    let mut insert = |candidate: ProjectedText| {
        if !candidate.text.is_empty() {
            candidates
                .entry(candidate.text.clone())
                .or_default()
                .push(candidate);
        }
    };
    let message = |block, role, text: &str| {
        let selected = match (field, role) {
            ("user", MessageRole::User) => authored_user_text(text),
            ("agent", MessageRole::Assistant | MessageRole::Summary)
            | ("toolcall", MessageRole::ToolCall)
            | ("toolresult", MessageRole::ToolResult) => Some(text.trim().to_owned()),
            _ => None,
        };
        selected.map(|selected| {
            // Authored text is the suffix after a known generated preamble.
            let end = text.trim_end().len();
            let offset = if field == "user" {
                end - selected.len()
            } else {
                text.len() - text.trim_start().len()
            };
            source_text(block, "content", &selected, offset)
        })
    };
    if session.cells.is_empty() {
        for (index, item) in session.messages.iter().enumerate() {
            if item.role == MessageRole::Summary
                && !session.search_fields.native_summaries.is_empty()
            {
                continue;
            }
            if field == "toolcall" {
                if let Some(source) = session.search_fields.tool_call_sources.get(&index) {
                    insert(trimmed_source_text(
                        SourceBlock::Message(index),
                        "raw_name",
                        &source.raw_name,
                    ));
                    insert(readable_input_candidate(
                        SourceBlock::Message(index),
                        &source.input,
                    ));
                    continue;
                }
            }
            if let Some(candidate) = message(SourceBlock::Message(index), item.role, &item.content)
            {
                insert(candidate);
            }
        }
    } else {
        for (index, cell) in session.cells.iter().enumerate() {
            let block = SourceBlock::Cell(index);
            match cell {
                SessionCell::Message { role, content, .. } => {
                    if *role != MessageRole::Summary
                        || session.search_fields.native_summaries.is_empty()
                    {
                        if let Some(candidate) = message(block, *role, content) {
                            insert(candidate);
                        }
                    }
                }
                SessionCell::Reasoning { header, body, .. } if field == "agent" => {
                    if let Some(header) = header {
                        for separator in ["", "\n", "\n\n"] {
                            let mut candidate = ProjectedText {
                                text: format!("**{header}**{separator}{body}"),
                                origins: Vec::new(),
                            };
                            candidate.origins.push(ProjectedRange {
                                range: 2..2 + header.len(),
                                source: SourceRange {
                                    source: SourceId::new(block.clone(), "header"),
                                    range: 0..header.len(),
                                },
                            });
                            let offset = 4 + header.len() + separator.len();
                            candidate.origins.push(ProjectedRange {
                                range: offset..offset + body.len(),
                                source: SourceRange {
                                    source: SourceId::new(block.clone(), "body"),
                                    range: 0..body.len(),
                                },
                            });
                            insert(candidate);
                        }
                    } else {
                        insert(trimmed_source_text(block, "body", body));
                    }
                }
                SessionCell::ToolCall {
                    raw_name, input, ..
                } if field == "toolcall" => {
                    let tool_source = session.search_fields.tool_call_sources.get(&index);
                    let raw_name =
                        tool_source.map_or(raw_name.as_str(), |source| source.raw_name.as_str());
                    let input = tool_source.map_or(input, |source| &source.input);
                    insert(trimmed_source_text(block.clone(), "raw_name", raw_name));
                    insert(readable_input_candidate(block, input));
                }
                SessionCell::ToolResult { output, .. } if field == "toolresult" => {
                    insert(trimmed_source_text(block, "output", output));
                }
                SessionCell::Exec {
                    command,
                    cwd,
                    stdout,
                    stderr,
                    ..
                } => {
                    if field == "toolcall" {
                        let mut candidate = ProjectedText::default();
                        let mut previous = None;
                        let mut source_offset = 0;
                        for part in command {
                            let text = part.trim();
                            let leading = part.len() - part.trim_start().len();
                            let component = source_text(
                                block.clone(),
                                "command",
                                text,
                                source_offset + leading,
                            );
                            insert(component.clone());
                            join_candidate(&mut candidate, component, "\n", &mut previous);
                            source_offset += part.len() + 1;
                        }
                        insert(candidate.clone());
                        if let Some(cwd) = cwd {
                            let component = trimmed_source_text(block.clone(), "cwd", cwd);
                            insert(component.clone());
                            join_candidate(&mut candidate, component, "\n", &mut previous);
                            insert(candidate);
                        }
                    } else if field == "toolresult" {
                        insert(trimmed_source_text(block.clone(), "stdout", stdout));
                        insert(trimmed_source_text(block, "stderr", stderr));
                    }
                }
                SessionCell::Patch { stdout, stderr, .. } if field == "toolresult" => {
                    insert(trimmed_source_text(block.clone(), "stdout", stdout));
                    insert(trimmed_source_text(block, "stderr", stderr));
                }
                SessionCell::WebSearch { query, queries, .. } if field == "toolcall" => {
                    insert(trimmed_source_text(block.clone(), "query", query));
                    let mut candidate = ProjectedText::default();
                    let mut previous = None;
                    for (index, query) in queries.iter().enumerate() {
                        let component =
                            trimmed_source_text(block.clone(), &format!("queries/{index}"), query);
                        insert(component.clone());
                        join_candidate(&mut candidate, component, "\n", &mut previous);
                    }
                    insert(candidate);
                }
                _ => {}
            }
        }
    }
    if field == "agent" {
        for (index, body) in session.search_fields.native_summaries.iter().enumerate() {
            insert(trimmed_source_text(
                SourceBlock::Context,
                &format!("native_summary/{index}"),
                body,
            ));
        }
    }
    // Composite candidates can coincide with one of their individual parts.
    // A source interval remains one candidate occurrence in that case.
    for available in candidates.values_mut() {
        let mut seen = std::collections::BTreeSet::new();
        available.retain(|candidate| {
            let key = candidate
                .origins
                .iter()
                .map(|origin| {
                    (
                        origin.range.start,
                        origin.range.end,
                        origin.source.source.clone(),
                        origin.source.range.start,
                        origin.source.range.end,
                    )
                })
                .collect::<Vec<_>>();
            seen.insert(key)
        });
    }
    candidates
}

fn source_text(block: SourceBlock, part: &str, text: &str, offset: usize) -> ProjectedText {
    ProjectedText {
        text: text.to_owned(),
        origins: vec![ProjectedRange {
            range: 0..text.len(),
            source: SourceRange {
                source: SourceId::new(block, part),
                range: offset..offset + text.len(),
            },
        }],
    }
}

fn trimmed_source_text(block: SourceBlock, part: &str, text: &str) -> ProjectedText {
    source_text(
        block,
        part,
        text.trim(),
        text.len() - text.trim_start().len(),
    )
}

fn join_candidate(
    target: &mut ProjectedText,
    component: ProjectedText,
    separator: &str,
    previous: &mut Option<(String, usize)>,
) {
    if component.text.is_empty() {
        return;
    }
    let offset = if let Some((_, offset)) = previous
        .as_ref()
        .filter(|(text, _)| text == &component.text)
    {
        *offset
    } else {
        if !target.text.is_empty() {
            target.text.push_str(separator);
        }
        let offset = target.text.len();
        target.text.push_str(&component.text);
        offset
    };
    *previous = Some((component.text.clone(), offset));
    target
        .origins
        .extend(component.origins.into_iter().map(|origin| ProjectedRange {
            range: offset + origin.range.start..offset + origin.range.end,
            source: origin.source,
        }));
}

fn readable_input_candidate(block: SourceBlock, input: &Value) -> ProjectedText {
    let mut values = Vec::new();
    readable_values(input, "input", &mut values);
    let mut candidate = ProjectedText::default();
    let mut previous = None;
    for (part, text) in values {
        for component in readable_string_components(block.clone(), &part, &text) {
            join_candidate(&mut candidate, component, "\n", &mut previous);
        }
    }
    if candidate.text == readable_tool_text(input) {
        candidate
    } else {
        ProjectedText {
            text: readable_tool_text(input),
            origins: Vec::new(),
        }
    }
}

fn readable_string_components(block: SourceBlock, part: &str, text: &str) -> Vec<ProjectedText> {
    if !matches!(text.trim().as_bytes().first(), Some(b'{') | Some(b'[')) {
        return vec![trimmed_source_text(block, part, text)];
    }
    let Ok(parsed) = serde_json::from_str::<Value>(text) else {
        return vec![trimmed_source_text(block, part, text)];
    };
    let Some(leaves) = crate::parse::json_sources::decoded_json_leaves(text) else {
        return Vec::new();
    };
    let leaves = leaves
        .into_iter()
        .map(|leaf| (format!("input{}", leaf.path), leaf))
        .collect::<BTreeMap<_, _>>();
    let mut readable = Vec::new();
    readable_values(&parsed, "input", &mut readable);
    let mut components = Vec::new();
    for (path, _) in readable {
        let Some(leaf) = leaves.get(&path) else {
            continue;
        };
        for mut component in readable_string_components(block.clone(), part, &leaf.text) {
            let mut mapped = Vec::<ProjectedRange>::new();
            for origin in component.origins {
                for lexical in &leaf.origins {
                    let start = origin.source.range.start.max(lexical.rendered.start);
                    let end = origin.source.range.end.min(lexical.rendered.end);
                    if start >= end {
                        continue;
                    }
                    let field = if origin.range.len() == origin.source.range.len() {
                        origin.range.start + start - origin.source.range.start
                            ..origin.range.start + end - origin.source.range.start
                    } else {
                        origin.range.clone()
                    };
                    let raw = if lexical.rendered.len() == lexical.source.len() {
                        lexical.source.start + start - lexical.rendered.start
                            ..lexical.source.start + end - lexical.rendered.start
                    } else {
                        lexical.source.clone()
                    };
                    let projected = ProjectedRange {
                        range: field,
                        source: SourceRange {
                            source: origin.source.source.clone(),
                            range: raw,
                        },
                    };
                    if let Some(last) = mapped.last_mut().filter(|last| {
                        last.range == projected.range
                            && last.source.source == projected.source.source
                            && last.source.range.end == projected.source.range.start
                    }) {
                        // Several outer escapes can encode one inner source
                        // unit. Keep its complete lexical interval atomic.
                        last.source.range.end = projected.source.range.end;
                    } else {
                        mapped.push(projected);
                    }
                }
            }
            component.origins = mapped;
            components.push(component);
        }
    }
    components
}

fn readable_values(value: &Value, path: &str, output: &mut Vec<(String, String)>) {
    // Reuse the parser's opaque/media filtering before recursing, so semantic
    // tool fields and content never acquire signatures or embedded media.
    if readable_tool_text(value).is_empty() {
        return;
    }
    match value {
        Value::Object(values) => {
            for (key, child) in values {
                let singleton = serde_json::json!({key: child});
                if !readable_tool_text(&singleton).is_empty() {
                    let key = key.replace('~', "~0").replace('/', "~1");
                    readable_values(child, &format!("{path}/{key}"), output);
                }
            }
        }
        Value::Array(values) => {
            for (index, child) in values.iter().enumerate() {
                readable_values(child, &format!("{path}/{index}"), output);
            }
        }
        Value::String(text) => {
            // Keep the raw string component intact. Decoded nested JSON leaf
            // coordinates are composed by readable_string_components.
            output.push((path.to_owned(), text.to_owned()));
        }
        _ => output.push((path.to_owned(), value.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::{ExecStatus, ToolStatus};
    use crate::search_query::QueryPlan;

    fn session() -> Session {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/sessions/codex/minimal.jsonl");
        let mut session = crate::parse::parse_codex_session_file(path)
            .unwrap()
            .unwrap();
        session.messages.clear();
        session.cells.clear();
        session.search_fields = Default::default();
        session.custom_title = Some("titleword".to_owned());
        session
    }

    #[test]
    fn all_is_union_of_visibility_segments_and_source_offsets_survive_trimming() {
        let mut session = session();
        session.cells = vec![
            SessionCell::Message {
                role: MessageRole::User,
                content: "  userword  ".to_owned(),
                timestamp: None,
            },
            SessionCell::Exec {
                command: vec!["callword".to_owned()],
                cwd: None,
                parsed_summary: None,
                stdout: "outword".to_owned(),
                stderr: "errword".to_owned(),
                exit_code: None,
                duration_ms: None,
                status: ExecStatus::Completed,
                timestamp: None,
                is_user: false,
            },
        ];
        let projection = SearchProjection::from_session(&session);
        let all = &projection.fields["content"][0].text;
        for segment in &projection.segments {
            assert_eq!(all.matches(segment.text.trim()).count(), 1);
        }
        let options = DisplayOptions {
            hide_tool_results: true,
            ..DisplayOptions::SHOW_ALL
        };
        for word in ["titleword", "userword", "callword", "outword", "errword"] {
            let matched = |mode| {
                QueryPlan::compile(word, mode, options)
                    .unwrap()
                    .matches(&projection)
                    .is_match
            };
            assert!(matched(VisibilitySearch::All));
            assert_ne!(
                matched(VisibilitySearch::Visible),
                matched(VisibilitySearch::Hidden)
            );
        }
        let matches = QueryPlan::compile("userword", VisibilitySearch::All, options)
            .unwrap()
            .matches(&projection);
        assert_eq!(matches.matches[0].sources[0].range, 2..10);
    }

    #[test]
    fn semantic_and_path_origins_never_use_unrelated_text() {
        let mut session = session();
        session.cwd = Some("/repo/needle".to_owned());
        session.cells = vec![
            SessionCell::Message {
                role: MessageRole::User,
                content: "/repo/needle needle".to_owned(),
                timestamp: None,
            },
            SessionCell::ToolCall {
                tool: "Read".to_owned(),
                raw_name: "Read".to_owned(),
                summary: "/repo/needle".to_owned(),
                input: serde_json::json!({"file_path":"/repo/needle", "contents":"needle", "metadata":"opaque"}),
                status: ToolStatus::Completed,
                timestamp: None,
            },
        ];
        session.search_fields.push_tool_call_text("needle");
        session.search_fields.add_file("/repo/needle");
        let projection = SearchProjection::from_session(&session);
        let call = QueryPlan::compile(
            "toolcall:needle",
            VisibilitySearch::All,
            DisplayOptions::SHOW_ALL,
        )
        .unwrap()
        .matches(&projection);
        assert!(call
            .matches
            .iter()
            .flat_map(|matched| &matched.sources)
            .all(|source| source.source.block != SourceBlock::Cell(0)));
        let cwd = QueryPlan::compile(
            "working_dir:needle",
            VisibilitySearch::All,
            DisplayOptions::SHOW_ALL,
        )
        .unwrap()
        .matches(&projection);
        assert!(cwd
            .matches
            .iter()
            .flat_map(|matched| &matched.sources)
            .all(|source| source.source.block == SourceBlock::Context));
        let file = QueryPlan::compile(
            "files:needle",
            VisibilitySearch::All,
            DisplayOptions::SHOW_ALL,
        )
        .unwrap()
        .matches(&projection);
        assert!(file
            .matches
            .iter()
            .flat_map(|matched| &matched.sources)
            .all(|source| source.source.part == "input/file_path"));
        assert!(!projection.fields["content"][0].text.contains("opaque"));
    }

    #[test]
    fn compact_provenance_roundtrip_retains_occurrences() {
        let mut session = session();
        session.cells.push(SessionCell::Message {
            role: MessageRole::User,
            content: "alpha alpha **beta**".to_owned(),
            timestamp: None,
        });
        let projection = SearchProjection::from_session(&session);
        let fields = projection
            .fields
            .iter()
            .map(|(field, values)| {
                (
                    field.clone(),
                    values.iter().map(|value| value.text.clone()).collect(),
                )
            })
            .collect();
        let json = serde_json::to_string(&projection.provenance()).unwrap();
        assert!(!json.contains("alpha alpha"));
        let restored =
            SearchProjection::from_stored_fields(fields, serde_json::from_str(&json).unwrap());
        let plan = QueryPlan::compile(
            "alpha OR beta",
            VisibilitySearch::All,
            DisplayOptions::SHOW_ALL,
        )
        .unwrap();
        assert_eq!(
            plan.matches(&projection).matches,
            plan.matches(&restored).matches
        );
    }

    #[test]
    fn repeated_structured_tool_facets_remain_independent_occurrences() {
        let mut session = session();
        for _ in 0..2 {
            session.cells.push(SessionCell::ToolCall {
                tool: "Read".to_owned(),
                raw_name: "Read".to_owned(),
                summary: String::new(),
                input: serde_json::json!({"a":"alpha", "b":"beta"}),
                status: ToolStatus::Completed,
                timestamp: None,
            });
            session.search_fields.push_tool_call_text("Read");
            session
                .search_fields
                .push_tool_call(&serde_json::json!({"a":"alpha", "b":"beta"}));
        }
        let projection = SearchProjection::from_session(&session);
        for query in ["toolcall:alpha", "toolcall:\"alpha beta\""] {
            let matches =
                QueryPlan::compile(query, VisibilitySearch::All, DisplayOptions::SHOW_ALL)
                    .unwrap()
                    .matches(&projection);
            assert_eq!(matches.matches.len(), 2, "{query}");
            for (index, matched) in matches.matches.iter().enumerate() {
                assert!(matched
                    .sources
                    .iter()
                    .all(|source| source.source.block == SourceBlock::Cell(index)));
            }
        }
    }

    #[test]
    fn parsed_tool_facets_identify_each_primitive_without_summary_or_key_coincidence() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("repeated-input.jsonl");
        let records = [
            serde_json::json!({"type":"session_meta", "payload":{"id":"repeated-input", "cwd":"/tmp"}}),
            serde_json::json!({"type":"response_item", "payload":{"type":"message", "role":"user", "content":[{"type":"input_text", "text":"alpha"}]}}),
            serde_json::json!({"type":"response_item", "payload":{"type":"function_call", "name":"alpha", "call_id":"call", "arguments":"{\"alpha\":\"alpha\",\"other\":\"alpha\"}"}}),
        ];
        std::fs::write(
            &path,
            records
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n"),
        )
        .unwrap();
        let session = crate::parse::parse_codex_session_file(&path)
            .unwrap()
            .unwrap();
        let tool_index = session
            .cells
            .iter()
            .position(|cell| matches!(cell, SessionCell::ToolCall { .. }))
            .unwrap();
        let projection = SearchProjection::from_session(&session);
        let matched = QueryPlan::compile(
            "toolcall:alpha",
            VisibilitySearch::All,
            DisplayOptions::SHOW_ALL,
        )
        .unwrap()
        .matches(&projection);
        assert!(matched.is_match);
        assert_eq!(matched.matches.len(), 3);
        let parts = matched
            .matches
            .iter()
            .map(|matched| {
                assert_eq!(matched.sources.len(), 1);
                let source = &matched.sources[0];
                assert_eq!(source.source.block, SourceBlock::Cell(tool_index));
                assert_eq!(source.range, 0..5);
                source.source.part.as_str()
            })
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            parts,
            std::collections::BTreeSet::from(["raw_name", "input/alpha", "input/other"])
        );

        let mut session = session;
        if let SessionCell::ToolCall { input, .. } = &mut session.cells[tool_index] {
            *input = serde_json::json!({"a":"alpha", "b":"beta", "c":"alpha"});
        }
        session.search_fields.tool_call.clear();
        session
            .search_fields
            .push_tool_call(&serde_json::json!({"a":"alpha", "b":"beta", "c":"alpha"}));
        let projection = SearchProjection::from_session(&session);
        let matched = QueryPlan::compile(
            "toolcall:alpha",
            VisibilitySearch::All,
            DisplayOptions::SHOW_ALL,
        )
        .unwrap()
        .matches(&projection);
        assert_eq!(matched.matches.len(), 2);
        assert_eq!(matched.matches[0].sources[0].source.part, "input/a");
        assert_eq!(matched.matches[1].sources[0].source.part, "input/c");
    }

    #[test]
    fn native_claude_summaries_remain_always_searchable_and_have_record_origins() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("native.jsonl");
        let records = [
            serde_json::json!({"type":"user","sessionId":"native", "message":{"role":"user","content":"ordinary prompt"}}),
            serde_json::json!({"type":"system","subtype":"away_summary","content":"NativeAwayNeedle"}),
            serde_json::json!({"type":"summary","summary":"NativeSummaryNeedle"}),
        ];
        std::fs::write(
            &path,
            records
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n"),
        )
        .unwrap();
        let session = crate::parse::parse_claude_session_file(&path)
            .unwrap()
            .unwrap();
        assert_eq!(
            session.search_fields.native_summaries,
            ["NativeAwayNeedle", "NativeSummaryNeedle"]
        );
        let projection = SearchProjection::from_session(&session);
        let options = DisplayOptions {
            hide_agent_replies: true,
            ..DisplayOptions::SHOW_ALL
        };
        for (index, needle) in ["NativeAwayNeedle", "NativeSummaryNeedle"]
            .into_iter()
            .enumerate()
        {
            for query in [needle.to_owned(), format!("agent:{needle}")] {
                for mode in [VisibilitySearch::All, VisibilitySearch::Visible] {
                    let matched = QueryPlan::compile(&query, mode, options)
                        .unwrap()
                        .matches(&projection);
                    assert!(matched.is_match, "{query} {mode:?}");
                    assert_eq!(matched.matches.len(), 1);
                    assert_eq!(
                        matched.matches[0].sources[0].source,
                        SourceId::new(SourceBlock::Context, format!("native_summary/{index}"))
                    );
                }
            }
            assert!(
                !QueryPlan::compile(needle, VisibilitySearch::Hidden, options)
                    .unwrap()
                    .matches(&projection)
                    .is_match
            );
        }
        let summary_only = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/sessions/claude/summary_session.jsonl");
        let session = crate::parse::parse_claude_session_file(&summary_only)
            .unwrap()
            .unwrap();
        let projection = SearchProjection::from_session(&session);
        assert!(projection.segments.iter().all(|segment| !matches!(
            segment.source.block,
            SourceBlock::Cell(_) | SourceBlock::Message(_)
        )));
        let matched = QueryPlan::compile("\"Invalid API key\"", VisibilitySearch::All, options)
            .unwrap()
            .matches(&projection);
        assert_eq!(
            matched.matches.len(),
            2,
            "identical source records remain separate"
        );
    }

    #[test]
    fn nested_json_unicode_escapes_preserve_atomic_raw_ranges_and_opaque_exclusions() {
        let raw = r#"{"text":"\u754c\u754c alpha","metadata":"OpaqueNeedle","nested":"{\"value\":\"\\u0061lpha\"}"}"#;
        let mut session = session();
        let input = serde_json::json!({"encoded":raw,"plain":"gamma"});
        session.search_fields.push_tool_call(&input);
        session.cells.push(SessionCell::ToolCall {
            tool: "unknown".to_owned(),
            raw_name: "unknown".to_owned(),
            summary: String::new(),
            input,
            status: ToolStatus::Completed,
            timestamp: None,
        });
        let projection = SearchProjection::from_session(&session);
        for field in ["content", "toolcall"] {
            assert!(!projection.fields[field][0].text.contains("OpaqueNeedle"));
        }
        let matched = QueryPlan::compile(
            "toolcall:界界",
            VisibilitySearch::All,
            DisplayOptions::SHOW_ALL,
        )
        .unwrap()
        .matches(&projection);
        assert_eq!(matched.matches.len(), 1);
        let encoded = matched.matches[0]
            .sources
            .iter()
            .map(|source| {
                assert_eq!(source.source.part, "input/encoded");
                &raw[source.range.clone()]
            })
            .collect::<String>();
        assert_eq!(encoded, "\\u754c\\u754c");
        let matched = QueryPlan::compile(
            "toolcall:alpha OR toolcall:gamma",
            VisibilitySearch::All,
            DisplayOptions::SHOW_ALL,
        )
        .unwrap()
        .matches(&projection);
        assert_eq!(matched.matches.len(), 3);
        assert!(matched.matches.iter().any(|matched| matched
            .sources
            .iter()
            .any(|source| source.source.part == "input/plain")));
        assert!(matched
            .matches
            .iter()
            .flat_map(|matched| &matched.sources)
            .all(|source| {
                source.source.block == SourceBlock::Cell(0)
                    && (source.source.part != "input/encoded"
                        || raw.is_char_boundary(source.range.start)
                            && raw.is_char_boundary(source.range.end))
            }));
        let atomic = ProjectedText {
            text: "界".to_owned(),
            origins: vec![ProjectedRange {
                range: 0..3,
                source: SourceRange {
                    source: SourceId::new(SourceBlock::Cell(0), "input/encoded"),
                    range: 0..6,
                },
            }],
        };
        assert_eq!(atomic.project(0..1).sources[0].range, 0..6);
    }
}
