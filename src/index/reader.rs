use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::Utc;
use tantivy::collector::TopDocs;
use tantivy::query::{AllQuery, BooleanQuery, BoostQuery, Query, QueryParser};
use tantivy::schema::Value;
use tantivy::{DocAddress, Index, IndexReader, Order, ReloadPolicy, TantivyDocument};

use crate::index::schema::IndexSchema;
use crate::index::writer::{IndexPaths, StoredSession};
use crate::live::LiveSessionTracker;
use crate::parse::{Agent, DerivationType};

#[derive(Debug, Clone)]
pub enum Scope {
    Global,
    CurrentDir(PathBuf),
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
}

const MIN_CANDIDATES: usize = 32;
const PAGE_SIZE: usize = 128;
const MAX_EMPTY_QUERY_CANDIDATES: usize = 5_000;
const MAX_QUERY_CANDIDATES: usize = 2_000;

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

        let query_parser = QueryParser::for_index(&self.index, vec![self.fields.content]);
        let (base_query, _) = query_parser.parse_query_lenient(query_text);
        let final_query = build_phrase_boosted_query(&query_parser, query_text, base_query);

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
        Ok(self.build_hits(candidates, request.query.trim()))
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
        let query_parser = QueryParser::for_index(&self.index, vec![self.fields.content]);
        let (base_query, _) = query_parser.parse_query_lenient(query_text);
        let final_query = build_phrase_boosted_query(&query_parser, query_text, base_query);
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
                let session = self.load_session(searcher, address, session_cache)?;
                let is_live = live_ids.contains(&session.session_id);
                if !matches_request(request, &session, is_live) {
                    continue;
                }

                hits.push(SearchHit {
                    snippet_html: fallback_snippet(&session, query_text),
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

    fn collect_candidates(
        &self,
        searcher: &tantivy::Searcher,
        docs: Vec<(f32, DocAddress)>,
        request: &SearchRequest,
        live_ids: &HashSet<String>,
        session_cache: &mut HashMap<DocAddress, StoredSession>,
        hits: &mut Vec<SearchCandidate>,
    ) -> Result<()> {
        for (score, address) in docs {
            let session = self.load_session(searcher, address, session_cache)?;
            let is_live = live_ids.contains(&session.session_id);
            if !matches_request(request, &session, is_live) {
                continue;
            }

            hits.push(SearchCandidate {
                score: score * recency_boost(session.modified_ts),
                session,
                is_live,
            });
        }

        Ok(())
    }

    fn build_hits(&self, candidates: Vec<SearchCandidate>, query_text: &str) -> Vec<SearchHit> {
        let mut hits = Vec::with_capacity(candidates.len());

        for candidate in candidates {
            let snippet_html = fallback_snippet(&candidate.session, query_text);
            hits.push(SearchHit {
                session: candidate.session,
                snippet_html,
                score: candidate.score,
                is_live: candidate.is_live,
            });
        }

        hits
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
}

fn candidate_limit(request: &SearchRequest, empty_query: bool) -> usize {
    let multiplier = match (&request.scope, empty_query) {
        (Scope::Global, true) => 4,
        (Scope::CurrentDir(_), true) => 8,
        (Scope::Global, false) => 6,
        (Scope::CurrentDir(_), false) => 10,
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
    if query_text.split_whitespace().count() < 2 {
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
        Scope::CurrentDir(current_dir) => {
            let current = current_dir.to_string_lossy();
            let candidates = [
                Some(session.project.as_str()),
                session.cwd.as_deref(),
                session.file_path.to_str(),
            ];

            candidates.into_iter().flatten().any(|candidate| {
                candidate.starts_with(current.as_ref()) || current.starts_with(candidate)
            })
        }
    }
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

fn fallback_snippet(session: &StoredSession, query: &str) -> String {
    let base = if !session.first_user_msg_content.is_empty() {
        &session.first_user_msg_content
    } else if !session.first_msg_content.is_empty() {
        &session.first_msg_content
    } else {
        &session.last_msg_content
    };

    if query.is_empty() {
        base.to_owned()
    } else {
        emphasize_terms(base, query)
    }
}

fn emphasize_terms(text: &str, query: &str) -> String {
    let mut result = text.to_owned();
    for term in query.split_whitespace().filter(|term| !term.is_empty()) {
        result = replace_case_insensitive(&result, term, &format!("<b>{term}</b>"));
    }
    result
}

fn replace_case_insensitive(haystack: &str, needle: &str, replacement: &str) -> String {
    let lower_haystack = haystack.to_ascii_lowercase();
    let lower_needle = needle.to_ascii_lowercase();
    if lower_needle.is_empty() {
        return haystack.to_owned();
    }

    let mut output = String::with_capacity(haystack.len());
    let mut index = 0usize;
    while let Some(found) = lower_haystack[index..].find(&lower_needle) {
        let start = index + found;
        let end = start + needle.len();
        output.push_str(&haystack[index..start]);
        output.push_str(replacement);
        index = end;
    }
    output.push_str(&haystack[index..]);
    output
}
