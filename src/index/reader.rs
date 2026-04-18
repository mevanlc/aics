use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::Utc;
use log::warn;
use tantivy::collector::TopDocs;
use tantivy::query::{AllQuery, BooleanQuery, BoostQuery, Query, QueryParser};
use tantivy::schema::Value;
use tantivy::snippet::{Snippet, SnippetGenerator};
use tantivy::{DocAddress, Index, IndexReader, Order, ReloadPolicy, TantivyDocument};

use crate::index::schema::IndexSchema;
use crate::index::writer::{IndexPaths, StoredSession};
use crate::live::LiveSessionTracker;
use crate::parse::{Agent, DerivationType};
use crate::search_query::{extract_highlight_terms, has_explicit_boolean_operators};

#[derive(Debug, Clone)]
pub enum Scope {
    Global,
    /// The original path plus an optional canonical form (when it differs).
    /// Both are precomputed so `matches_scope` does no filesystem I/O.
    CurrentDir(PathBuf, Option<PathBuf>),
}

impl Scope {
    pub fn current_dir(path: PathBuf) -> Self {
        let canonical = path.canonicalize().ok().filter(|c| c != &path);
        Self::CurrentDir(path, canonical)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortMode {
    Relevance,
    Time,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchFilters {
    pub agent: Option<Agent>,
    pub branch: Option<String>,
    pub after_ts: Option<u64>,
    pub before_ts: Option<u64>,
    pub min_lines: Option<usize>,
    pub include_original: bool,
    pub include_trimmed: bool,
    pub include_continued: bool,
    pub include_sub_agents: bool,
    pub live_only: bool,
}

impl Default for SearchFilters {
    fn default() -> Self {
        Self {
            agent: None,
            branch: None,
            after_ts: None,
            before_ts: None,
            min_lines: None,
            include_original: true,
            include_trimmed: true,
            include_continued: true,
            include_sub_agents: false,
            live_only: false,
        }
    }
}

impl SearchFilters {
    pub fn active_count(&self) -> usize {
        usize::from(self.agent.is_some())
            + usize::from(self.branch.is_some())
            + usize::from(self.after_ts.is_some())
            + usize::from(self.before_ts.is_some())
            + usize::from(self.min_lines.is_some())
            + usize::from(!self.include_original)
            + usize::from(!self.include_trimmed)
            + usize::from(!self.include_continued)
            + usize::from(self.include_sub_agents)
            + usize::from(self.live_only)
    }

    fn allows_derivation(&self, derivation: DerivationType) -> bool {
        match derivation {
            DerivationType::Original => self.include_original,
            DerivationType::Trimmed => self.include_trimmed,
            DerivationType::Continued => self.include_continued,
            DerivationType::SubAgent => self.include_sub_agents,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SearchRequest {
    pub query: String,
    pub scope: Scope,
    pub limit: usize,
    pub sort: SortMode,
    pub filters: SearchFilters,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SearchHit {
    pub session: StoredSession,
    pub snippet_html: String,
    pub score: f32,
    pub is_live: bool,
}

#[derive(Debug)]
struct SearchCandidate {
    session: StoredSession,
    score: f32,
    is_live: bool,
    snippet_html: String,
}

const MIN_CANDIDATES: usize = 32;
const PAGE_SIZE: usize = 128;
const MAX_EMPTY_QUERY_CANDIDATES: usize = 5_000;
const MAX_QUERY_CANDIDATES: usize = 2_000;
const SNIPPET_MAX_CHARS: usize = 240;

pub struct SearchEngine {
    index: Index,
    reader: IndexReader,
    fields: IndexSchema,
    live_sessions: LiveSessionTracker,
}

impl SearchEngine {
    pub fn open(paths: &IndexPaths) -> Result<Self> {
        Self::open_with_live_sessions(paths, LiveSessionTracker::discover())
    }

    pub fn open_with_live_sessions(
        paths: &IndexPaths,
        live_sessions: LiveSessionTracker,
    ) -> Result<Self> {
        let index = Index::open_in_dir(&paths.index_dir)
            .with_context(|| format!("failed to open {}", paths.index_dir.display()))?;
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::OnCommitWithDelay)
            .try_into()
            .context("failed to build tantivy reader")?;
        let fields = IndexSchema::from_schema(&index.schema())?;

        Ok(Self {
            index,
            reader,
            fields,
            live_sessions,
        })
    }

    pub fn search(&self, request: &SearchRequest) -> Result<Vec<SearchHit>> {
        // Keep long-lived readers in sync with in-process index mutations such as delete actions.
        self.reader
            .reload()
            .context("failed to reload tantivy reader")?;
        let searcher = self.reader.searcher();
        if searcher.num_docs() == 0 {
            return Ok(Vec::new());
        }

        let limit = request.limit.max(1);
        let query_text = request.query.trim();
        let live_ids = self.live_sessions.live_session_ids();
        let mut session_cache = HashMap::new();

        if query_text.is_empty() {
            return self.search_recent(&searcher, request, limit, &live_ids, &mut session_cache);
        }

        if matches!(request.sort, SortMode::Time) {
            return self.search_by_time(
                &searcher,
                request,
                limit,
                query_text,
                &live_ids,
                &mut session_cache,
            );
        }

        let query_parser = self.default_query_parser();
        let (base_query, _) = query_parser.parse_query_lenient(query_text);
        let final_query = build_phrase_boosted_query(&query_parser, query_text, base_query);
        let snippet_generator = self.make_snippet_generator(&searcher, &*final_query);

        let mut candidates = Vec::new();
        let mut offset = 0usize;
        let candidate_limit = candidate_limit(request, false);

        while candidates.len() < limit && offset < candidate_limit {
            let batch_size = PAGE_SIZE.min(candidate_limit.saturating_sub(offset));
            let docs = searcher.search(
                &*final_query,
                &TopDocs::with_limit(batch_size).and_offset(offset),
            )?;
            if docs.is_empty() {
                break;
            }

            let docs_len = docs.len();
            offset += docs_len;
            self.collect_candidates(
                &searcher,
                docs,
                request,
                snippet_generator.as_ref(),
                &live_ids,
                &mut session_cache,
                &mut candidates,
            )?;
            if batch_size == 0 || docs_len < batch_size {
                break;
            }
        }

        candidates.sort_by(|left, right| {
            right
                .score
                .partial_cmp(&left.score)
                .unwrap_or(Ordering::Equal)
                .then_with(|| right.session.modified_ts.cmp(&left.session.modified_ts))
                .then_with(|| left.session.file_path.cmp(&right.session.file_path))
        });
        candidates.truncate(limit);
        Ok(self.build_hits(candidates))
    }

    fn search_recent(
        &self,
        searcher: &tantivy::Searcher,
        request: &SearchRequest,
        limit: usize,
        live_ids: &HashSet<String>,
        session_cache: &mut HashMap<DocAddress, StoredSession>,
    ) -> Result<Vec<SearchHit>> {
        let modified_ts_field = self.fields.schema.get_field_name(self.fields.modified_ts);
        let candidate_limit = candidate_limit(request, true);
        let mut hits = Vec::with_capacity(limit);
        let mut offset = 0usize;

        while hits.len() < limit && offset < candidate_limit {
            let batch_size = PAGE_SIZE.min(candidate_limit.saturating_sub(offset));
            let docs = searcher.search(
                &AllQuery,
                &TopDocs::with_limit(batch_size)
                    .and_offset(offset)
                    .order_by_u64_field(modified_ts_field, Order::Desc),
            )?;
            if docs.is_empty() {
                break;
            }

            let docs_len = docs.len();
            offset += docs_len;
            for (_, address) in docs {
                let session = self.load_session(searcher, address, session_cache)?;
                let is_live = live_ids.contains(&session.session_id);
                if !matches_request(request, &session, is_live) {
                    continue;
                }

                hits.push(SearchHit {
                    snippet_html: fallback_snippet(&session, ""),
                    score: session.modified_ts as f32,
                    session,
                    is_live,
                });
                if hits.len() >= limit {
                    break;
                }
            }

            if batch_size == 0 || docs_len < batch_size {
                break;
            }
        }

        Ok(hits)
    }

    fn search_by_time(
        &self,
        searcher: &tantivy::Searcher,
        request: &SearchRequest,
        limit: usize,
        query_text: &str,
        live_ids: &HashSet<String>,
        session_cache: &mut HashMap<DocAddress, StoredSession>,
    ) -> Result<Vec<SearchHit>> {
        let query_parser = self.default_query_parser();
        let (base_query, _) = query_parser.parse_query_lenient(query_text);
        let final_query = build_phrase_boosted_query(&query_parser, query_text, base_query);
        let snippet_generator = self.make_snippet_generator(searcher, &*final_query);
        let modified_ts_field = self.fields.schema.get_field_name(self.fields.modified_ts);
        let candidate_limit = candidate_limit(request, false);

        let mut hits = Vec::with_capacity(limit);
        let mut offset = 0usize;
        while hits.len() < limit && offset < candidate_limit {
            let batch_size = PAGE_SIZE.min(candidate_limit.saturating_sub(offset));
            let docs = searcher.search(
                &*final_query,
                &TopDocs::with_limit(batch_size)
                    .and_offset(offset)
                    .order_by_u64_field(modified_ts_field, Order::Desc),
            )?;
            if docs.is_empty() {
                break;
            }

            let docs_len = docs.len();
            offset += docs_len;
            for (_, address) in docs {
                let document = searcher.doc::<TantivyDocument>(address)?;
                let session =
                    self.session_from_doc(address, &document, session_cache)?;
                let is_live = live_ids.contains(&session.session_id);
                if !matches_request(request, &session, is_live) {
                    continue;
                }

                let snippet_html = build_snippet_html(
                    snippet_generator.as_ref(),
                    &document,
                    &session,
                    query_text,
                );
                hits.push(SearchHit {
                    snippet_html,
                    score: session.modified_ts as f32,
                    session,
                    is_live,
                });
                if hits.len() >= limit {
                    break;
                }
            }

            if batch_size == 0 || docs_len < batch_size {
                break;
            }
        }

        Ok(hits)
    }

    fn default_query_parser(&self) -> QueryParser {
        let mut query_parser = QueryParser::for_index(&self.index, vec![self.fields.content]);
        query_parser.set_conjunction_by_default();
        query_parser
    }

    fn collect_candidates(
        &self,
        searcher: &tantivy::Searcher,
        docs: Vec<(f32, DocAddress)>,
        request: &SearchRequest,
        snippet_generator: Option<&SnippetGenerator>,
        live_ids: &HashSet<String>,
        session_cache: &mut HashMap<DocAddress, StoredSession>,
        hits: &mut Vec<SearchCandidate>,
    ) -> Result<()> {
        for (score, address) in docs {
            let document = searcher.doc::<TantivyDocument>(address)?;
            let session = self.session_from_doc(address, &document, session_cache)?;
            let is_live = live_ids.contains(&session.session_id);
            if !matches_request(request, &session, is_live) {
                continue;
            }

            let snippet_html = build_snippet_html(
                snippet_generator,
                &document,
                &session,
                request.query.trim(),
            );
            hits.push(SearchCandidate {
                score: score * recency_boost(session.modified_ts),
                session,
                is_live,
                snippet_html,
            });
        }

        Ok(())
    }

    fn build_hits(&self, candidates: Vec<SearchCandidate>) -> Vec<SearchHit> {
        candidates
            .into_iter()
            .map(|candidate| SearchHit {
                session: candidate.session,
                snippet_html: candidate.snippet_html,
                score: candidate.score,
                is_live: candidate.is_live,
            })
            .collect()
    }

    fn load_session(
        &self,
        searcher: &tantivy::Searcher,
        address: DocAddress,
        session_cache: &mut HashMap<DocAddress, StoredSession>,
    ) -> Result<StoredSession> {
        if let Some(session) = session_cache.get(&address).cloned() {
            return Ok(session);
        }

        let document = searcher.doc::<TantivyDocument>(address)?;
        let session = stored_session_from_document(&document, self.fields.session_json)?;
        session_cache.insert(address, session.clone());
        Ok(session)
    }

    fn session_from_doc(
        &self,
        address: DocAddress,
        document: &TantivyDocument,
        session_cache: &mut HashMap<DocAddress, StoredSession>,
    ) -> Result<StoredSession> {
        if let Some(session) = session_cache.get(&address).cloned() {
            return Ok(session);
        }
        let session = stored_session_from_document(document, self.fields.session_json)?;
        session_cache.insert(address, session.clone());
        Ok(session)
    }

    fn make_snippet_generator(
        &self,
        searcher: &tantivy::Searcher,
        query: &dyn Query,
    ) -> Option<SnippetGenerator> {
        match SnippetGenerator::create(searcher, query, self.fields.content) {
            Ok(mut generator) => {
                generator.set_max_num_chars(SNIPPET_MAX_CHARS);
                Some(generator)
            }
            Err(error) => {
                warn!("failed to build snippet generator: {error:#}");
                None
            }
        }
    }
}

fn candidate_limit(request: &SearchRequest, empty_query: bool) -> usize {
    let multiplier = match (&request.scope, empty_query) {
        (Scope::Global, true) => 4,
        (Scope::CurrentDir(..), true) => 8,
        (Scope::Global, false) => 6,
        (Scope::CurrentDir(..), false) => 10,
    };
    let ceiling = if empty_query {
        MAX_EMPTY_QUERY_CANDIDATES
    } else {
        MAX_QUERY_CANDIDATES
    };

    request
        .limit
        .max(1)
        .saturating_mul(multiplier)
        .clamp(MIN_CANDIDATES, ceiling)
}

fn build_phrase_boosted_query(
    query_parser: &QueryParser,
    query_text: &str,
    base_query: Box<dyn Query>,
) -> Box<dyn Query> {
    if extract_highlight_terms(query_text).len() < 2 || has_explicit_boolean_operators(query_text) {
        return base_query;
    }

    let quoted = format!("\"{query_text}\"");
    let (phrase_query, _) = query_parser.parse_query_lenient(&quoted);
    Box::new(BooleanQuery::union(vec![
        base_query,
        Box::new(BoostQuery::new(phrase_query, 5.0)),
    ]))
}

fn stored_session_from_document(
    document: &TantivyDocument,
    field: tantivy::schema::Field,
) -> Result<StoredSession> {
    let payload = document
        .get_first(field)
        .and_then(|value| value.as_str())
        .context("missing stored session field")?;
    serde_json::from_str(payload).context("failed to deserialize stored session field")
}

fn matches_scope(scope: &Scope, session: &StoredSession) -> bool {
    match scope {
        Scope::Global => true,
        Scope::CurrentDir(original, canonical) => {
            let stored = [Some(session.project.as_str()), session.cwd.as_deref()];

            // Check the original path first; then the canonical form if present.
            // This way a symlinked working dir still matches sessions that
            // recorded the symlink path, and also matches sessions that
            // recorded the resolved real path.
            let original_str = original.to_string_lossy();
            stored.iter().flatten().any(|s| {
                paths_equal(&original_str, s)
                    || canonical
                        .as_ref()
                        .is_some_and(|c| paths_equal(&c.to_string_lossy(), s))
            })
        }
    }
}

fn paths_equal(a: &str, b: &str) -> bool {
    if cfg!(windows) {
        paths_equal_windows(a, b)
    } else {
        // `Path` equality compares by components, so trailing separators
        // (e.g. Claude Code sometimes records `cwd` as `/foo/bar/`) are ignored.
        Path::new(a) == Path::new(b)
    }
}

/// Normalize and compare two paths using Windows rules: case-insensitive,
/// forward/backslash equivalent, trailing separators ignored. Extracted so
/// it can be tested on any platform.
fn paths_equal_windows(a: &str, b: &str) -> bool {
    fn normalize(p: &str) -> String {
        let s = p.replace('\\', "/");
        s.trim_end_matches('/').to_ascii_lowercase()
    }
    normalize(a) == normalize(b)
}

fn matches_request(request: &SearchRequest, session: &StoredSession, is_live: bool) -> bool {
    if !matches_scope(&request.scope, session) {
        return false;
    }
    matches_filters(&request.filters, session, is_live)
}

fn matches_filters(filters: &SearchFilters, session: &StoredSession, is_live: bool) -> bool {
    if let Some(agent) = filters.agent {
        if session.agent != agent {
            return false;
        }
    }

    if let Some(branch) = filters.branch.as_deref() {
        if session.branch.as_deref() != Some(branch) {
            return false;
        }
    }

    if let Some(after_ts) = filters.after_ts {
        if session.modified_ts < after_ts {
            return false;
        }
    }

    if let Some(before_ts) = filters.before_ts {
        if session.modified_ts > before_ts {
            return false;
        }
    }

    if let Some(min_lines) = filters.min_lines {
        if session.lines < min_lines {
            return false;
        }
    }

    if !filters.allows_derivation(session.derivation_type) {
        return false;
    }

    if filters.live_only && !is_live {
        return false;
    }

    true
}

fn recency_boost(modified_ts: u64) -> f32 {
    let now = Utc::now().timestamp().max(0) as u64;
    let age_seconds = now.saturating_sub(modified_ts) as f32;
    let half_life = 7.0 * 24.0 * 60.0 * 60.0;
    1.0 + f32::exp(-age_seconds / half_life)
}

fn build_snippet_html(
    snippet_generator: Option<&SnippetGenerator>,
    document: &TantivyDocument,
    session: &StoredSession,
    query: &str,
) -> String {
    if let Some(generator) = snippet_generator {
        let snippet = generator.snippet_from_doc(document);
        if !snippet.fragment().is_empty() && !snippet.highlighted().is_empty() {
            return render_snippet_html(&snippet);
        }
    }
    fallback_snippet(session, query)
}

/// Build `<b>…</b>` markup from a Tantivy snippet without HTML-escaping the
/// surrounding text. The TUI renderer (`parse_highlighted_html`) expects plain
/// text with literal `<b>` tags and treats other angle brackets as literals,
/// so `Snippet::to_html` (which escapes entities) would leak `&lt;` into the
/// UI.
fn render_snippet_html(snippet: &Snippet) -> String {
    let fragment = snippet.fragment();
    let mut ranges: Vec<_> = snippet.highlighted().to_vec();
    ranges.sort_by_key(|range| range.start);

    let mut out = String::with_capacity(fragment.len() + ranges.len() * 7);
    let mut cursor = 0usize;
    for range in ranges {
        let start = range.start.min(fragment.len());
        let end = range.end.min(fragment.len());
        if start < cursor || end <= start {
            continue;
        }
        out.push_str(fragment.get(cursor..start).unwrap_or(""));
        out.push_str("<b>");
        out.push_str(fragment.get(start..end).unwrap_or(""));
        out.push_str("</b>");
        cursor = end;
    }
    out.push_str(fragment.get(cursor..).unwrap_or(""));
    out
}

fn fallback_snippet(session: &StoredSession, query: &str) -> String {
    let base = if !session.first_user_msg_content.is_empty() {
        &session.first_user_msg_content
    } else if !session.first_msg_content.is_empty() {
        &session.first_msg_content
    } else {
        &session.last_msg_content
    };
    let snippet = snippet_display_text(base);

    if query.is_empty() {
        snippet
    } else {
        emphasize_terms(&snippet, query)
    }
}

fn snippet_display_text(base: &str) -> String {
    let base = base.trim();
    let stripped = strip_leading_global_boilerplate(base);
    if stripped.is_empty() {
        base.to_owned()
    } else {
        stripped.to_owned()
    }
}

fn strip_leading_global_boilerplate(mut text: &str) -> &str {
    loop {
        let trimmed = text.trim_start();
        let next = strip_one_leading_boilerplate_block(trimmed);
        if next == trimmed {
            return trimmed;
        }
        text = next;
    }
}

fn strip_one_leading_boilerplate_block(text: &str) -> &str {
    if let Some(rest) = strip_agents_header_line(text) {
        return rest;
    }

    for tag in [
        "INSTRUCTIONS",
        "environment_context",
        "permissions instructions",
        "collaboration_mode",
    ] {
        if let Some(rest) = strip_tag_block(text, tag) {
            return rest;
        }
    }

    text
}

fn strip_agents_header_line(text: &str) -> Option<&str> {
    let header = [
        "AGENTS.md instructions for ",
        "# AGENTS.md instructions for ",
    ]
    .into_iter()
    .find(|header| text.starts_with(header))?;
    let rest = &text[header.len()..];
    match rest.find('\n') {
        Some(newline) => Some(&rest[newline + 1..]),
        None => Some(""),
    }
}

fn strip_tag_block<'a>(text: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let rest = text.strip_prefix(&open)?;
    let end = rest.find(&close)?;
    Some(&rest[end + close.len()..])
}

fn emphasize_terms(text: &str, query: &str) -> String {
    let mut result = text.to_owned();
    for term in extract_highlight_terms(query) {
        result = replace_case_insensitive(&result, &term);
    }
    result
}

fn replace_case_insensitive(haystack: &str, needle: &str) -> String {
    let lower_haystack = haystack.to_ascii_lowercase();
    let lower_needle = needle.to_ascii_lowercase();
    if lower_needle.is_empty() {
        return haystack.to_owned();
    }

    let mut output = String::with_capacity(haystack.len());
    let mut index = 0usize;
    while let Some(found) = lower_haystack[index..].find(&lower_needle) {
        let start = index + found;
        let end = start + lower_needle.len();
        output.push_str(&haystack[index..start]);
        output.push_str("<b>");
        output.push_str(&haystack[start..end]);
        output.push_str("</b>");
        index = end;
    }
    output.push_str(&haystack[index..]);
    output
}

#[cfg(test)]
mod tests {
    use super::{
        emphasize_terms, fallback_snippet, matches_scope, paths_equal, paths_equal_windows,
        replace_case_insensitive, snippet_display_text, Scope,
    };
    use crate::index::writer::StoredSession;
    use crate::parse::{Agent, DerivationType};
    use std::path::PathBuf;

    fn stub_session(project: &str, cwd: Option<&str>) -> StoredSession {
        StoredSession {
            session_id: "test".into(),
            agent: Agent::Claude,
            project: project.into(),
            branch: None,
            cwd: cwd.map(Into::into),
            modified_ts: 0,
            lines: 0,
            file_path: PathBuf::new(),
            first_msg_role: None,
            first_msg_content: String::new(),
            last_msg_role: None,
            last_msg_content: String::new(),
            first_user_msg_content: String::new(),
            derivation_type: DerivationType::Original,
            is_sidechain: false,
            custom_title: None,
        }
    }

    // -- paths_equal ---------------------------------------------------------

    #[test]
    fn paths_equal_matches_identical_unix_paths() {
        assert!(paths_equal("/home/user/repo", "/home/user/repo"));
    }

    #[test]
    fn paths_equal_rejects_different_unix_paths() {
        assert!(!paths_equal("/home/user/repo", "/home/user/other"));
    }

    #[test]
    fn paths_equal_windows_case_insensitive() {
        assert!(paths_equal_windows(
            "C:\\Users\\Dev\\Repo",
            "c:\\users\\dev\\repo"
        ));
    }

    #[test]
    fn paths_equal_windows_mixed_separators() {
        assert!(paths_equal_windows("C:\\Repo", "c:/repo"));
    }

    #[test]
    fn paths_equal_windows_trailing_separator() {
        assert!(paths_equal_windows("C:\\Repo", "C:\\Repo\\"));
        assert!(paths_equal_windows("C:\\Repo\\", "C:\\Repo"));
        assert!(paths_equal_windows("/home/user/repo/", "/home/user/repo"));
    }

    #[test]
    fn paths_equal_windows_rejects_different_paths() {
        assert!(!paths_equal_windows("C:\\Repo", "C:\\Other"));
    }

    // -- matches_scope: original path ----------------------------------------

    #[test]
    fn scope_matches_session_project_by_original_path() {
        let scope = Scope::CurrentDir(PathBuf::from("/work/myproject"), None);
        let session = stub_session("/work/myproject", None);
        assert!(matches_scope(&scope, &session));
    }

    #[test]
    fn scope_matches_session_cwd_by_original_path() {
        let scope = Scope::CurrentDir(PathBuf::from("/work/myproject"), None);
        let session = stub_session("something-else", Some("/work/myproject"));
        assert!(matches_scope(&scope, &session));
    }

    #[test]
    fn scope_rejects_unrelated_session() {
        let scope = Scope::CurrentDir(PathBuf::from("/work/myproject"), None);
        let session = stub_session("/other/project", Some("/other/project"));
        assert!(!matches_scope(&scope, &session));
    }

    // -- matches_scope: canonical fallback -----------------------------------

    #[test]
    fn scope_matches_via_canonical_when_original_differs() {
        // Simulates: user is in /link/repo (symlink), session stored /real/repo
        let scope = Scope::CurrentDir(
            PathBuf::from("/link/repo"),
            Some(PathBuf::from("/real/repo")),
        );
        let session = stub_session("/real/repo", None);
        assert!(matches_scope(&scope, &session));
    }

    #[test]
    fn scope_still_matches_original_when_canonical_is_present() {
        // Simulates: user is in /link/repo, session also recorded /link/repo
        let scope = Scope::CurrentDir(
            PathBuf::from("/link/repo"),
            Some(PathBuf::from("/real/repo")),
        );
        let session = stub_session("/link/repo", None);
        assert!(matches_scope(&scope, &session));
    }

    #[test]
    fn scope_canonical_matches_cwd_field_too() {
        let scope = Scope::CurrentDir(
            PathBuf::from("/link/repo"),
            Some(PathBuf::from("/real/repo")),
        );
        let session = stub_session("unrelated", Some("/real/repo"));
        assert!(matches_scope(&scope, &session));
    }

    #[test]
    fn scope_matches_cwd_with_trailing_slash() {
        // Claude Code sometimes records `cwd` with a trailing separator;
        // `env::current_dir()` does not. Both forms must match.
        let scope = Scope::CurrentDir(PathBuf::from("/Users/me/proj"), None);
        let session = stub_session("irrelevant", Some("/Users/me/proj/"));
        assert!(matches_scope(&scope, &session));
    }

    // -- Scope::current_dir constructor --------------------------------------

    #[test]
    fn current_dir_constructor_resolves_dot_dot_components() {
        // Build a path with `..` that resolves to a real directory.
        let tmp = std::env::temp_dir();
        let with_dotdot = tmp.join("definitely-not-real/.."); // collapses to tmp
        let scope = Scope::current_dir(with_dotdot.clone());
        match &scope {
            Scope::CurrentDir(original, canonical) => {
                assert_eq!(original, &with_dotdot);
                // canonicalize resolves .. so canonical should differ
                // (or be None if the original already was canonical).
                // Either way, the canonical form should not contain "..".
                if let Some(c) = canonical {
                    assert!(!c.to_string_lossy().contains(".."));
                }
            }
            _ => panic!("expected CurrentDir"),
        }
    }

    #[test]
    fn global_scope_matches_everything() {
        let session = stub_session("/any/path", Some("/another/path"));
        assert!(matches_scope(&Scope::Global, &session));
    }

    #[test]
    fn snippet_display_text_skips_leading_agents_instructions_block() {
        let snippet = snippet_display_text(
            "AGENTS.md instructions for /repo\n\n<INSTRUCTIONS>You are running on Android Termux.\nUse $PREFIX/tmp instead of /tmp.\n</INSTRUCTIONS>\n\nImplement the Codex resume preview logic.",
        );

        assert_eq!(snippet, "Implement the Codex resume preview logic.");
    }

    #[test]
    fn fallback_snippet_prefers_session_specific_request_over_agents_preamble() {
        let mut session = stub_session("/repo", Some("/repo"));
        session.first_user_msg_content = "AGENTS.md instructions for /repo\n<INSTRUCTIONS>Use $PREFIX/tmp instead of /tmp.</INSTRUCTIONS>\n\nFix the snippet parser.".to_owned();

        let snippet = fallback_snippet(&session, "parser");

        assert_eq!(snippet, "Fix the snippet <b>parser</b>.");
    }

    #[test]
    fn replace_case_insensitive_preserves_original_match_casing() {
        let highlighted = replace_case_insensitive("INSTRUCTIONS", "on");
        assert_eq!(highlighted, "INSTRUCTI<b>ON</b>S");
    }

    #[test]
    fn emphasize_terms_preserves_original_match_casing() {
        let highlighted = emphasize_terms(
            "INSTRUCTIONS You are running on Android.",
            "running on android",
        );
        assert!(highlighted.contains("INSTRUCTI<b>ON</b>S"));
        assert!(highlighted.contains("<b>running</b>"));
        assert!(highlighted.contains("<b>Android</b>"));
    }
}
