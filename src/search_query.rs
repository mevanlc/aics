const BOOLEAN_OPERATORS: &[&str] = &["AND", "OR", "NOT"];

use std::collections::{BTreeMap, HashSet};
use std::ops::Range;
use std::sync::Arc;

use anyhow::{anyhow, Result};
use tantivy::query::{Occur, QueryParser};
use tantivy::query_grammar::{UserInputAst, UserInputBound, UserInputLeaf};
use tantivy::tokenizer::Token;
use tantivy::Index;
use tantivy_fst::Automaton;

use crate::index::schema::IndexSchema;
use crate::search_match::SourceMatch;
use crate::search_projection::{default_search_field_names, SearchProjection};
use crate::settings::DisplayOptions;

/// The same parser/tokenizer configuration is used for index retrieval and
/// source occurrence discovery. Path queries retain their zero-distance prefix
/// behavior; transcript regexes match complete indexed terms.
pub(crate) fn query_parser(
    index: &Index,
    fields: &IndexSchema,
    visibility: VisibilitySearch,
    options: DisplayOptions,
) -> (QueryParser, Vec<String>) {
    let names = default_search_field_names(visibility, options);
    let default_fields = names
        .iter()
        .filter_map(|name| fields.schema.get_field(name).ok())
        .collect();
    let mut parser = QueryParser::for_index(index, default_fields);
    parser.set_conjunction_by_default();
    for field in [fields.working_dir, fields.dirs, fields.files, fields.paths] {
        parser.set_field_fuzzy(field, true, 0, false);
    }
    parser.allow_regexes();
    (parser, names.into_iter().map(str::to_owned).collect())
}

#[derive(Debug, Clone)]
enum MatchNode {
    All,
    Empty,
    Clause(Vec<(Occur, MatchNode)>),
    Field { field: String, matcher: LeafMatcher },
}

#[derive(Debug, Clone)]
enum LeafMatcher {
    Phrase {
        terms: Vec<(usize, String)>,
        slop: u32,
        prefix: bool,
        path_prefix: bool,
    },
    Regex(Arc<tantivy_fst::Regex>),
    Range(UserInputBound, UserInputBound),
    Set(Vec<String>),
}

#[derive(Debug, Clone)]
pub struct QueryPlan {
    index: Index,
    fields: IndexSchema,
    root: MatchNode,
    diagnostics: Vec<String>,
    match_is_certain: bool,
}

#[derive(Debug, Clone, Default)]
pub struct QueryMatches {
    pub matches: Vec<SourceMatch>,
    pub is_match: bool,
    /// Multi-term sloppy phrases depend on Tantivy segment statistics that a
    /// standalone session does not carry. Their local status is provisional.
    pub match_is_certain: bool,
    pub diagnostics: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FieldMatch {
    pub field: String,
    pub value: usize,
    pub range: Range<usize>,
}

impl QueryPlan {
    pub fn compile(
        query: &str,
        visibility: VisibilitySearch,
        options: DisplayOptions,
    ) -> Result<Self> {
        let fields = IndexSchema::new();
        let index = Index::create_in_ram(fields.schema.clone());
        IndexSchema::register_tokenizers(&index);
        let (query, override_visibility) =
            extract_visibility_search(query).map_err(|error| anyhow!(error))?;
        let (parser, defaults) = query_parser(
            &index,
            &fields,
            override_visibility.unwrap_or(visibility),
            options,
        );
        let translated =
            crate::index::reader::translate_angle_regexes(&query).unwrap_or_else(|_| query.clone());
        let (mut ast, errors) = tantivy::query_grammar::parse_query_lenient(&translated);
        let mut snippet_fields = Vec::new();
        crate::index::reader::prepare_query_ast(&mut ast, &mut snippet_fields, &defaults);
        let (compiled_query, query_errors) =
            parser.build_query_from_user_input_ast_lenient(ast.clone());
        let mut diagnostics = errors
            .into_iter()
            .map(|error| format!("{error:?}"))
            .collect::<Vec<_>>();
        diagnostics.extend(query_errors.into_iter().map(|error| error.to_string()));
        let mut root = compile_node(&ast, &index, &fields, &defaults, &parser);
        if query.trim().is_empty() || constant_query(&*compiled_query) == Some(true) {
            root = MatchNode::All;
        } else if constant_query(&*compiled_query) == Some(false) {
            root = MatchNode::Empty;
        } else if all_negative(&root) {
            // Tantivy's lenient parser makes a pure negative query match all
            // documents except the exclusions, while retaining its diagnostic.
            root = MatchNode::Clause(vec![(Occur::Must, MatchNode::All), (Occur::Must, root)]);
        }
        let match_is_certain = !has_corpus_dependent_slop(&root);
        if !match_is_certain {
            diagnostics.push("Phrases of three or more terms with slop depend on Tantivy segment statistics; local highlights and expression status may differ from indexed results.".to_owned());
        }
        Ok(Self {
            index,
            fields,
            root,
            diagnostics,
            match_is_certain,
        })
    }

    pub fn matches(&self, projection: &SearchProjection) -> QueryMatches {
        let (is_match, field_matches) = self.field_matches(projection);
        let mut matches = Vec::new();
        let mut seen = HashSet::new();
        let mut origin_indexes = BTreeMap::new();
        for occurrence in field_matches {
            let Some(value) = projection
                .fields
                .get(&occurrence.field)
                .and_then(|values| values.get(occurrence.value))
            else {
                continue;
            };
            let (origins, prefix_ends) = origin_indexes
                .entry((occurrence.field.clone(), occurrence.value))
                .or_insert_with(|| {
                    let mut origins = value.origins.iter().collect::<Vec<_>>();
                    origins.sort_by_key(|origin| (origin.range.start, origin.range.end));
                    let mut maximum = 0;
                    let prefix_ends = origins
                        .iter()
                        .map(|origin| {
                            maximum = maximum.max(origin.range.end);
                            maximum
                        })
                        .collect::<Vec<_>>();
                    (origins, prefix_ends)
                });
            let end = origins.partition_point(|origin| origin.range.start < occurrence.range.end);
            let start = prefix_ends[..end].partition_point(|end| *end <= occurrence.range.start);
            let overlapping = origins[start..end]
                .iter()
                .copied()
                .filter(|origin| origin.range.end > occurrence.range.start)
                .collect();
            for mut matched in crate::search_projection::ProjectedText::project_overlapping(
                occurrence.range,
                overlapping,
            ) {
                matched.sources.sort_by(|left, right| {
                    left.source
                        .cmp(&right.source)
                        .then_with(|| left.range.start.cmp(&right.range.start))
                        .then_with(|| left.range.end.cmp(&right.range.end))
                });
                matched.sources.dedup();
                let key = matched
                    .sources
                    .iter()
                    .map(|source| (source.source.clone(), source.range.start, source.range.end))
                    .collect::<Vec<_>>();
                if !key.is_empty() && seen.insert(key) {
                    matches.push(matched);
                }
            }
        }
        QueryMatches {
            matches,
            is_match,
            match_is_certain: self.match_is_certain,
            diagnostics: self.diagnostics.clone(),
        }
    }

    pub(crate) fn field_matches(&self, projection: &SearchProjection) -> (bool, Vec<FieldMatch>) {
        let mut tokens = BTreeMap::new();
        let mut needed = HashSet::new();
        collect_fields(&self.root, &mut needed);
        for (name, values) in &projection.fields {
            if !needed.contains(name.as_str()) {
                continue;
            }
            let Ok(field) = self.fields.schema.get_field(name) else {
                continue;
            };
            let mut values_tokens = Vec::new();
            for value in values {
                let mut value_tokens = Vec::new();
                if name == "modified_ts" {
                    value_tokens.push(Token {
                        offset_from: 0,
                        offset_to: value.text.len(),
                        position: 0,
                        text: value.text.clone(),
                        position_length: 1,
                    });
                } else if let Ok(mut analyzer) = self.index.tokenizer_for_field(field) {
                    analyzer
                        .token_stream(&value.text)
                        .process(&mut |token| value_tokens.push(token.clone()));
                }
                values_tokens.push(value_tokens);
            }
            tokens.insert(name.clone(), values_tokens);
        }
        let mut occurrences = Vec::new();
        let is_match = evaluate_node(&self.root, &tokens, false, &mut occurrences);
        occurrences.sort_by(|left, right| {
            left.field
                .cmp(&right.field)
                .then_with(|| left.value.cmp(&right.value))
                .then_with(|| left.range.start.cmp(&right.range.start))
                .then_with(|| left.range.end.cmp(&right.range.end))
        });
        occurrences.dedup();
        (is_match, occurrences)
    }
}

fn has_corpus_dependent_slop(node: &MatchNode) -> bool {
    match node {
        MatchNode::Field {
            matcher: LeafMatcher::Phrase { terms, slop, .. },
            ..
        } => terms.len() >= 3 && *slop > 0,
        MatchNode::Clause(children) => children
            .iter()
            .any(|(_, child)| has_corpus_dependent_slop(child)),
        _ => false,
    }
}

fn constant_query(query: &dyn tantivy::query::Query) -> Option<bool> {
    if query.is::<tantivy::query::AllQuery>() {
        return Some(true);
    }
    if query.is::<tantivy::query::EmptyQuery>() {
        return Some(false);
    }
    let boolean = query.downcast_ref::<tantivy::query::BooleanQuery>()?;
    let mut matched_should = 0;
    for (occur, child) in boolean.clauses() {
        let value = constant_query(&**child)?;
        match occur {
            Occur::Must if !value => return Some(false),
            Occur::MustNot if value => return Some(false),
            Occur::Should if value => matched_should += 1,
            _ => {}
        }
    }
    Some(matched_should >= boolean.get_minimum_number_should_match())
}

fn compile_node(
    ast: &UserInputAst,
    index: &Index,
    fields: &IndexSchema,
    defaults: &[String],
    parser: &QueryParser,
) -> MatchNode {
    match ast {
        UserInputAst::Boost(child, _) => compile_node(child, index, fields, defaults, parser),
        UserInputAst::Clause(children) => {
            let children = children
                .iter()
                .filter_map(|(occur, child)| {
                    let node = compile_node(child, index, fields, defaults, parser);
                    (!matches!(node, MatchNode::Empty))
                        .then_some((occur.unwrap_or(Occur::Must), node))
                })
                .collect::<Vec<_>>();
            if children.is_empty() {
                MatchNode::Empty
            } else {
                MatchNode::Clause(children)
            }
        }
        UserInputAst::Leaf(leaf) => {
            if matches!(leaf.as_ref(), UserInputLeaf::All) {
                return MatchNode::All;
            }
            let (query, _) = parser.build_query_from_user_input_ast_lenient(ast.clone());
            if query.is::<tantivy::query::EmptyQuery>() {
                return MatchNode::Empty;
            }
            let explicit = match leaf.as_ref() {
                UserInputLeaf::Literal(literal) => literal.field_name.as_ref(),
                UserInputLeaf::Regex { field, .. }
                | UserInputLeaf::Range { field, .. }
                | UserInputLeaf::Set { field, .. } => field.as_ref(),
                _ => return MatchNode::Empty,
            };
            let names = explicit
                .map(|field| vec![field.clone()])
                .unwrap_or_else(|| defaults.to_vec());
            let mut children = Vec::new();
            for name in names {
                let Ok(field) = fields.schema.get_field(&name) else {
                    continue;
                };
                let matcher = match leaf.as_ref() {
                    UserInputLeaf::Literal(literal) => {
                        let mut terms = Vec::new();
                        if name == "modified_ts" {
                            let Ok(value) = literal.phrase.parse::<u64>() else {
                                continue;
                            };
                            terms.push((0, value.to_string()));
                        } else {
                            let Ok(mut analyzer) = index.tokenizer_for_field(field) else {
                                continue;
                            };
                            analyzer
                                .token_stream(&literal.phrase)
                                .process(&mut |token| {
                                    terms.push((token.position, token.text.clone()))
                                });
                        }
                        if terms.is_empty() || literal.prefix && terms.len() < 2 {
                            continue;
                        }
                        LeafMatcher::Phrase {
                            terms,
                            slop: literal.slop,
                            prefix: literal.prefix,
                            path_prefix: matches!(
                                name.as_str(),
                                "working_dir" | "dirs" | "files" | "paths"
                            ),
                        }
                    }
                    UserInputLeaf::Regex { pattern, .. } => {
                        let Ok(regex) = tantivy_fst::Regex::new(pattern) else {
                            continue;
                        };
                        LeafMatcher::Regex(Arc::new(regex))
                    }
                    UserInputLeaf::Range { lower, upper, .. } => LeafMatcher::Range(
                        normalize_bound(index, field, lower),
                        normalize_bound(index, field, upper),
                    ),
                    UserInputLeaf::Set { elements, .. } => LeafMatcher::Set(
                        elements
                            .iter()
                            .filter_map(|element| normalize_boundary(index, field, element))
                            .collect(),
                    ),
                    _ => continue,
                };
                children.push((
                    Occur::Should,
                    MatchNode::Field {
                        field: name,
                        matcher,
                    },
                ));
            }
            if children.is_empty() {
                MatchNode::Empty
            } else if children.len() == 1 {
                children.remove(0).1
            } else {
                MatchNode::Clause(children)
            }
        }
    }
}

fn all_negative(node: &MatchNode) -> bool {
    match node {
        MatchNode::Clause(children) => children
            .iter()
            .all(|(occur, node)| *occur == Occur::MustNot || all_negative(node)),
        MatchNode::Empty => true,
        _ => false,
    }
}

fn collect_fields<'a>(node: &'a MatchNode, fields: &mut HashSet<&'a str>) {
    match node {
        MatchNode::Field { field, .. } => {
            fields.insert(field);
        }
        MatchNode::Clause(children) => {
            for (_, child) in children {
                collect_fields(child, fields);
            }
        }
        _ => {}
    }
}

fn evaluate_node(
    node: &MatchNode,
    tokens: &BTreeMap<String, Vec<Vec<Token>>>,
    negative: bool,
    occurrences: &mut Vec<FieldMatch>,
) -> bool {
    match node {
        MatchNode::All => true,
        MatchNode::Empty => false,
        MatchNode::Clause(children) => {
            let mut must_count = 0;
            let mut must_match = true;
            let mut should_match = false;
            let mut excluded = false;
            for (occur, child) in children {
                let matched = evaluate_node(
                    child,
                    tokens,
                    negative || *occur == Occur::MustNot,
                    occurrences,
                );
                match occur {
                    Occur::Must => {
                        must_count += 1;
                        must_match &= matched;
                    }
                    Occur::Should => should_match |= matched,
                    Occur::MustNot => excluded |= matched,
                }
            }
            !excluded
                && must_match
                && (must_count > 0
                    || should_match
                    || children.iter().all(|(occur, _)| *occur == Occur::MustNot))
        }
        MatchNode::Field { field, matcher } => {
            let Some(values) = tokens.get(field) else {
                return false;
            };
            let mut is_match = false;
            for (value, tokens) in values.iter().enumerate() {
                for range in leaf_ranges(matcher, tokens, field == "modified_ts") {
                    is_match = true;
                    if !negative {
                        occurrences.push(FieldMatch {
                            field: field.clone(),
                            value,
                            range,
                        });
                    }
                }
            }
            is_match
        }
    }
}

fn leaf_ranges(matcher: &LeafMatcher, tokens: &[Token], numeric: bool) -> Vec<Range<usize>> {
    match matcher {
        LeafMatcher::Phrase {
            terms,
            slop,
            prefix,
            path_prefix,
        } => {
            let mut ranges = Vec::new();
            for token in tokens {
                let first = &terms[0].1;
                if !(token.text == *first || *path_prefix && token.text.starts_with(first)) {
                    continue;
                }
                if terms.len() == 1 {
                    ranges.push(token.offset_from..token.offset_to);
                    continue;
                }
                phrase_ranges(tokens, terms, *slop, *prefix, 1, vec![token], &mut ranges);
            }
            ranges
        }
        LeafMatcher::Regex(regex) => tokens
            .iter()
            .filter(|token| {
                let mut state = regex.start();
                for byte in token.text.bytes() {
                    state = regex.accept(&state, byte);
                }
                regex.is_match(&state)
            })
            .map(|token| token.offset_from..token.offset_to)
            .collect(),
        LeafMatcher::Set(elements) => tokens
            .iter()
            .filter(|token| elements.contains(&token.text))
            .map(|token| token.offset_from..token.offset_to)
            .collect(),
        LeafMatcher::Range(lower, upper) => tokens
            .iter()
            .filter(|token| in_range(&token.text, lower, upper, numeric))
            .map(|token| token.offset_from..token.offset_to)
            .collect(),
    }
}

fn phrase_ranges<'a>(
    tokens: &'a [Token],
    terms: &[(usize, String)],
    slop: u32,
    prefix: bool,
    index: usize,
    chosen: Vec<&'a Token>,
    ranges: &mut Vec<Range<usize>>,
) {
    if index == terms.len() {
        let start = chosen.iter().map(|token| token.offset_from).min().unwrap();
        let end = chosen.iter().map(|token| token.offset_to).max().unwrap();
        ranges.push(start..end);
        return;
    }
    let base = chosen[0].position as i64 - terms[0].0 as i64;
    let expected = base + terms[index].0 as i64;
    let low = expected - i64::from(slop);
    let high = expected + i64::from(slop);
    let start = tokens.partition_point(|token| (token.position as i64) < low);
    let end = tokens.partition_point(|token| (token.position as i64) <= high);
    for token in &tokens[start..end] {
        let term = &terms[index].1;
        if !(token.text == *term
            || prefix && index + 1 == terms.len() && token.text.starts_with(term))
        {
            continue;
        }
        if chosen
            .iter()
            .any(|selected| selected.position == token.position)
        {
            continue;
        }
        let adjustment = token.position as i64 - terms[index].0 as i64;
        if (adjustment - base).unsigned_abs() > u64::from(slop) {
            continue;
        }
        let mut next = chosen.clone();
        next.push(token);
        // Tantivy carries the distance budget through intersections, keeping
        // either endpoint as the anchor for the next term. A simple bounding
        // range would accept three-term transpositions that its scorer rejects.
        if slop > 0 && carried_slop(&next, terms) > u64::from(slop) {
            continue;
        }
        phrase_ranges(tokens, terms, slop, prefix, index + 1, next, ranges);
    }
}

fn carried_slop(chosen: &[&Token], terms: &[(usize, String)]) -> u64 {
    let mut anchors = BTreeMap::from([(chosen[0].position as i64 - terms[0].0 as i64, 0u64)]);
    for (index, token) in chosen.iter().enumerate().skip(1) {
        let position = token.position as i64 - terms[index].0 as i64;
        let mut next = BTreeMap::<i64, u64>::new();
        for (anchor, cost) in anchors {
            let cost = cost + anchor.abs_diff(position);
            for anchor in [anchor, position] {
                next.entry(anchor)
                    .and_modify(|previous| *previous = (*previous).min(cost))
                    .or_insert(cost);
            }
        }
        anchors = next;
    }
    anchors.into_values().min().unwrap_or(0)
}

fn normalize_boundary(index: &Index, field: tantivy::schema::Field, text: &str) -> Option<String> {
    if index.schema().get_field_name(field) == "modified_ts" {
        return text.parse::<u64>().ok().map(|value| value.to_string());
    }
    let mut analyzer = index.tokenizer_for_field(field).ok()?;
    let mut tokens = Vec::new();
    analyzer
        .token_stream(text)
        .process(&mut |token| tokens.push(token.text.clone()));
    (tokens.len() == 1).then(|| tokens.remove(0))
}

fn normalize_bound(
    index: &Index,
    field: tantivy::schema::Field,
    bound: &UserInputBound,
) -> UserInputBound {
    match bound {
        UserInputBound::Inclusive(text) => normalize_boundary(index, field, text)
            .map(UserInputBound::Inclusive)
            .unwrap_or(UserInputBound::Unbounded),
        UserInputBound::Exclusive(text) => normalize_boundary(index, field, text)
            .map(UserInputBound::Exclusive)
            .unwrap_or(UserInputBound::Unbounded),
        UserInputBound::Unbounded => UserInputBound::Unbounded,
    }
}

fn in_range(text: &str, lower: &UserInputBound, upper: &UserInputBound, numeric: bool) -> bool {
    let compare = |bound: &str| {
        if numeric {
            text.parse::<u64>()
                .ok()
                .zip(bound.parse::<u64>().ok())
                .map(|(value, bound)| value.cmp(&bound))
        } else {
            Some(text.cmp(bound))
        }
    };
    let above = match lower {
        UserInputBound::Inclusive(bound) => {
            compare(bound).is_some_and(|ordering| !ordering.is_lt())
        }
        UserInputBound::Exclusive(bound) => compare(bound).is_some_and(|ordering| ordering.is_gt()),
        UserInputBound::Unbounded => true,
    };
    let below = match upper {
        UserInputBound::Inclusive(bound) => {
            compare(bound).is_some_and(|ordering| !ordering.is_gt())
        }
        UserInputBound::Exclusive(bound) => compare(bound).is_some_and(|ordering| ordering.is_lt()),
        UserInputBound::Unbounded => true,
    };
    above && below
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VisibilitySearch {
    All,
    #[default]
    Visible,
    Hidden,
}

impl VisibilitySearch {
    pub fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Visible => "Visible",
            Self::Hidden => "Hidden",
        }
    }

    fn from_modifier(token: &str) -> Option<Self> {
        if token.eq_ignore_ascii_case("all:") {
            Some(Self::All)
        } else if token.eq_ignore_ascii_case("visible:") {
            Some(Self::Visible)
        } else if token.eq_ignore_ascii_case("hidden:") {
            Some(Self::Hidden)
        } else {
            None
        }
    }
}

/// Remove a position-independent visibility modifier from a query.
///
/// Modifiers are recognized only as complete, unquoted tokens. Repeating the
/// same modifier is harmless, while combining different modifiers is rejected
/// because their transcript scopes are mutually exclusive.
/// No modifier returns `None`, leaving the caller's selected mode in effect.
pub fn extract_visibility_search(
    query: &str,
) -> Result<(String, Option<VisibilitySearch>), &'static str> {
    let mut output = String::with_capacity(query.len());
    let mut modifier = None;
    let mut token_start = 0usize;
    let mut quote = None;
    let mut escaped = false;

    for (index, ch) in query.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' {
            escaped = true;
            continue;
        }
        if let Some(delimiter) = quote {
            if ch == delimiter {
                quote = None;
            }
            continue;
        }
        if matches!(ch, '\'' | '"') {
            quote = Some(ch);
            continue;
        }
        if ch.is_whitespace() || matches!(ch, '(' | ')') {
            append_query_token(query, token_start, index, &mut output, &mut modifier)?;
            output.push(ch);
            token_start = index + ch.len_utf8();
        }
    }
    append_query_token(query, token_start, query.len(), &mut output, &mut modifier)?;

    Ok((output.trim().to_owned(), modifier))
}

fn append_query_token(
    query: &str,
    start: usize,
    end: usize,
    output: &mut String,
    modifier: &mut Option<VisibilitySearch>,
) -> Result<(), &'static str> {
    let token = &query[start..end];
    let Some(found) = VisibilitySearch::from_modifier(token) else {
        output.push_str(token);
        return Ok(());
    };

    if modifier.is_some_and(|existing| existing != found) {
        return Err("visible:, hidden:, and all: are mutually exclusive");
    }
    *modifier = Some(found);
    Ok(())
}

pub fn extract_highlight_terms(query: &str) -> Vec<String> {
    let mut terms = Vec::new();
    let mut token = String::new();
    let mut token_quoted = false;
    let mut in_quotes = false;

    for ch in query.chars() {
        match ch {
            '"' => {
                if in_quotes {
                    push_term(&mut terms, &mut token, true);
                    in_quotes = false;
                    token_quoted = false;
                } else {
                    push_term(&mut terms, &mut token, token_quoted);
                    in_quotes = true;
                    token_quoted = true;
                }
            }
            '(' | ')' if !in_quotes => {
                push_term(&mut terms, &mut token, token_quoted);
                token_quoted = false;
            }
            ch if ch.is_whitespace() => {
                push_term(&mut terms, &mut token, token_quoted);
                token_quoted = in_quotes;
            }
            _ => token.push(ch),
        }
    }

    push_term(&mut terms, &mut token, token_quoted);
    terms
}

pub fn has_explicit_boolean_operators(query: &str) -> bool {
    let mut token = String::new();
    let mut token_quoted = false;
    let mut in_quotes = false;

    for ch in query.chars() {
        match ch {
            '"' => {
                if in_quotes {
                    if is_boolean_operator(&token, true) {
                        return true;
                    }
                    token.clear();
                    in_quotes = false;
                    token_quoted = false;
                } else {
                    if is_boolean_operator(&token, token_quoted) {
                        return true;
                    }
                    token.clear();
                    in_quotes = true;
                    token_quoted = true;
                }
            }
            '(' | ')' if !in_quotes => {
                if is_boolean_operator(&token, token_quoted) {
                    return true;
                }
                token.clear();
                token_quoted = false;
            }
            ch if ch.is_whitespace() && !in_quotes => {
                if is_boolean_operator(&token, token_quoted) {
                    return true;
                }
                token.clear();
                token_quoted = false;
            }
            _ => token.push(ch),
        }
    }

    is_boolean_operator(&token, token_quoted)
}

fn push_term(terms: &mut Vec<String>, token: &mut String, quoted: bool) {
    if token.is_empty() {
        return;
    }

    if !is_boolean_operator(token, quoted) {
        let term = if !quoted && VisibilitySearch::from_modifier(token).is_some() {
            ""
        } else {
            strip_search_field(token)
        };
        if !term.is_empty() {
            terms.push(term.to_ascii_lowercase());
        }
    }
    token.clear();
}

fn strip_search_field(token: &str) -> &str {
    let Some((field, value)) = token.split_once(':') else {
        return token;
    };
    if matches!(
        field,
        "content"
            | "working_dir"
            | "wd"
            | "user"
            | "agent"
            | "toolcall"
            | "toolresult"
            | "dirs"
            | "files"
            | "paths"
    ) {
        value
    } else {
        token
    }
}

fn is_boolean_operator(token: &str, quoted: bool) -> bool {
    !quoted && BOOLEAN_OPERATORS.contains(&token)
}

#[cfg(test)]
mod tests {
    use super::{
        extract_highlight_terms, extract_visibility_search, has_explicit_boolean_operators,
        VisibilitySearch,
    };

    fn session_with_cells(cells: Vec<crate::parse::SessionCell>) -> crate::parse::Session {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/sessions/codex/minimal.jsonl");
        let mut session = crate::parse::parse_codex_session_file(path)
            .unwrap()
            .unwrap();
        session.cells = cells;
        session.messages.clear();
        session.search_fields = Default::default();
        session.custom_title = Some("source title".to_owned());
        session
    }

    fn message(role: crate::parse::MessageRole, text: &str) -> crate::parse::SessionCell {
        crate::parse::SessionCell::Message {
            role,
            content: text.to_owned(),
            timestamp: None,
        }
    }

    #[test]
    fn query_occurrences_preserve_phrases_fields_and_negative_clauses() {
        use super::QueryPlan;
        use crate::parse::MessageRole::{Assistant, User};
        use crate::search_projection::SearchProjection;
        use crate::settings::DisplayOptions;
        let session = session_with_cells(vec![
            message(User, "alpha alpha Alpha alphabet beta"),
            message(Assistant, "alpha beta and beta alpha"),
        ]);
        let projection = SearchProjection::from_session(&session);
        let matches = QueryPlan::compile(
            "user:alpha NOT beta",
            VisibilitySearch::All,
            DisplayOptions::SHOW_ALL,
        )
        .unwrap()
        .matches(&projection);
        assert!(!matches.is_match);
        assert_eq!(matches.matches.len(), 3);
        assert!(matches.matches.iter().all(|matched| matched
            .sources
            .iter()
            .all(|source| source.source.block == crate::search_match::SourceBlock::Cell(0))));
        let matches = QueryPlan::compile(
            "agent:\"alpha beta\"",
            VisibilitySearch::All,
            DisplayOptions::SHOW_ALL,
        )
        .unwrap()
        .matches(&projection);
        assert!(matches.is_match);
        assert_eq!(matches.matches.len(), 1);
        assert_eq!(matches.matches[0].sources[0].range, 0..10);
        let matches =
            QueryPlan::compile("<alph.*>", VisibilitySearch::All, DisplayOptions::SHOW_ALL)
                .unwrap()
                .matches(&projection);
        assert_eq!(matches.matches.len(), 6);
        let matches = QueryPlan::compile(
            "<alpha beta>",
            VisibilitySearch::All,
            DisplayOptions::SHOW_ALL,
        )
        .unwrap()
        .matches(&projection);
        assert!(!matches.is_match);
        assert!(matches.matches.is_empty());
    }

    #[test]
    fn query_plan_agrees_with_tantivy_for_boolean_phrase_regex_and_paths() -> anyhow::Result<()> {
        use super::QueryPlan;
        use crate::parse::MessageRole::{Assistant, User};
        use crate::search_projection::SearchProjection;
        use crate::settings::DisplayOptions;
        use tantivy::collector::TopDocs;
        use tantivy::{Index, TantivyDocument};
        let mut sessions = [
            session_with_cells(vec![message(User, "a b c alpha alpha"), message(Assistant, "beta gamma")]),
            session_with_cells(vec![message(User, "b a c alphabet"), message(Assistant, "alpha beta")]),
            session_with_cells(vec![message(User, "a x b x c Straße wide 界"), message(Assistant, "nothing")]),
            session_with_cells(vec![message(User, "AGENTS.md instructions for /repo\n<INSTRUCTIONS>hiddenword</INSTRUCTIONS>\nalpha"), message(Assistant, "beta")]),
            session_with_cells(vec![message(User, "b b b b"), message(Assistant, "unrelated")]),
        ];
        for (session, modified_ts) in sessions.iter_mut().zip([9, 10, 100, 0, 7]) {
            session.modified_ts = modified_ts;
            session.cwd = Some("/repo/source".to_owned());
            session.search_fields.add_file("/repo/src/a.rs");
        }
        let projections = sessions
            .iter()
            .map(SearchProjection::from_session)
            .collect::<Vec<_>>();
        let schema = crate::index::schema::IndexSchema::new();
        let index = Index::create_in_ram(schema.schema.clone());
        crate::index::schema::IndexSchema::register_tokenizers(&index);
        let mut writer = index.writer_with_num_threads(1, 15_000_000)?;
        for projection in &projections {
            let mut document = TantivyDocument::default();
            for (name, values) in &projection.fields {
                for value in values {
                    if name == "modified_ts" {
                        document.add_u64(schema.schema.get_field(name)?, value.text.parse()?);
                    } else {
                        document.add_text(schema.schema.get_field(name)?, &value.text);
                    }
                }
            }
            writer.add_document(document)?;
        }
        writer.commit()?;
        let reader = index.reader()?;
        let searcher = reader.searcher();
        for options in [
            DisplayOptions::SHOW_ALL,
            DisplayOptions {
                hide_agent_replies: true,
                hide_project_docs_autodump: true,
                ..DisplayOptions::SHOW_ALL
            },
        ] {
            for mode in [
                VisibilitySearch::All,
                VisibilitySearch::Visible,
                VisibilitySearch::Hidden,
            ] {
                let (parser, defaults) = super::query_parser(&index, &schema, mode, options);
                for query in [
                    "alpha",
                    "alpha beta",
                    "alpha OR beta",
                    "alpha NOT beta",
                    "NOT beta",
                    "(alpha OR beta) AND gamma",
                    "+alpha beta",
                    "alpha -beta",
                    "-alpha -beta",
                    "user:alpha",
                    "agent:alpha",
                    "content:alpha",
                    "user:hiddenword",
                    "\"a b\"",
                    "\"a b\"~1",
                    "\"a b\"~2",
                    "\"a b c\"~2",
                    "\"a b c\"~3",
                    "\"alpha bet\"*",
                    "<alph.*>",
                    "agent:<alph.*>",
                    "<a.*|b.*>",
                    "NOT <beta>",
                    "wd:repo",
                    "files:src",
                    "files:\"src/a.rs\"",
                    "dirs:src",
                    "paths:/repo",
                    "user:[a TO c]",
                    "agent:IN [alpha beta]",
                    "agent:IN [Alpha Beta]",
                    "user:[A TO C]",
                    "modified_ts:[0 TO 9000000000]",
                    "modified_ts:{1 TO 10}",
                    "modified_ts:[8 TO 20]",
                    "modified_ts:{9 TO 100}",
                    "modified_ts:10",
                    "modified_ts:IN [9 100]",
                    "modified_ts:IN [0 1]",
                    "content:*",
                    "modified_ts:*",
                    "modified_ts:* OR alpha",
                    "NOT modified_ts:*",
                    "user:* OR alpha",
                    "unknown:alpha OR beta",
                    "alpha AND",
                    "(alpha",
                    "<INVALID[>",
                    "content:\"source title\"",
                ] {
                    let translated = crate::index::reader::translate_angle_regexes(query)
                        .unwrap_or_else(|_| query.to_owned());
                    let (mut ast, _) = tantivy::query_grammar::parse_query_lenient(&translated);
                    crate::index::reader::prepare_query_ast(&mut ast, &mut Vec::new(), &defaults);
                    let (actual_query, _) = parser.build_query_from_user_input_ast_lenient(ast);
                    let docs = searcher
                        .search(&*actual_query, &TopDocs::with_limit(10).order_by_score())?;
                    let plan = QueryPlan::compile(query, mode, options)?;
                    for (id, projection) in projections.iter().enumerate() {
                        let expected = docs
                            .iter()
                            .any(|(_, address)| address.doc_id as usize == id);
                        let local = plan.matches(projection);
                        if !local.match_is_certain {
                            assert!(local
                                .diagnostics
                                .iter()
                                .any(|diagnostic| diagnostic.contains("segment statistics")));
                            continue;
                        }
                        assert_eq!(
                            local.is_match,
                            expected,
                            "query={query:?}, mode={mode:?}, doc={id}, options={options:?}, actual={actual_query:?}, plan={:?}", plan.root
                        );
                    }
                }
            }
        }
        Ok(())
    }

    #[test]
    fn multi_term_slop_reports_upstream_corpus_dependent_matching() -> anyhow::Result<()> {
        use crate::parse::MessageRole::User;
        use crate::search_projection::SearchProjection;
        use crate::settings::DisplayOptions;
        use tantivy::{collector::TopDocs, Index, TantivyDocument};
        let target =
            SearchProjection::from_session(&session_with_cells(vec![message(User, "b a c")]));
        let schema = crate::index::schema::IndexSchema::new();
        let engine_match = |add_unrelated: bool| -> anyhow::Result<bool> {
            let index = Index::create_in_ram(schema.schema.clone());
            crate::index::schema::IndexSchema::register_tokenizers(&index);
            let mut writer = index.writer_with_num_threads(1, 15_000_000)?;
            for text in std::iter::once(target.fields["content"][0].text.as_str())
                .chain(add_unrelated.then_some("b"))
            {
                let mut doc = TantivyDocument::default();
                doc.add_text(schema.content, text);
                writer.add_document(doc)?;
            }
            writer.commit()?;
            let query = tantivy::query::QueryParser::for_index(&index, vec![schema.content])
                .parse_query("\"a b c\"~2")?;
            let reader = index.reader()?;
            Ok(reader
                .searcher()
                .search(&*query, &TopDocs::with_limit(10).order_by_score())?
                .iter()
                .any(|(_, address)| address.doc_id == 0))
        };
        assert!(!engine_match(false)?);
        assert!(
            engine_match(true)?,
            "an unrelated term changes upstream intersection order"
        );
        let local = super::QueryPlan::compile(
            "\"a b c\"~2",
            VisibilitySearch::All,
            DisplayOptions::SHOW_ALL,
        )?
        .matches(&target);
        assert!(!local.match_is_certain);
        assert!(local
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.contains("segment statistics")));
        assert!(
            super::QueryPlan::compile(
                "\"a b\"~2",
                VisibilitySearch::All,
                DisplayOptions::SHOW_ALL
            )?
            .matches(&target)
            .match_is_certain
        );
        Ok(())
    }

    #[test]
    fn user_exec_visibility_uses_its_own_toggles_and_all_contains_every_part() {
        use super::QueryPlan;
        use crate::parse::{ExecStatus, SessionCell};
        use crate::search_projection::SearchProjection;
        use crate::settings::DisplayOptions;
        let session = session_with_cells(vec![SessionCell::Exec {
            command: vec!["usercommand".to_owned()],
            cwd: None,
            parsed_summary: None,
            stdout: "useroutput".to_owned(),
            stderr: String::new(),
            exit_code: Some(0),
            duration_ms: None,
            status: ExecStatus::Completed,
            timestamp: None,
            is_user: true,
        }]);
        let projection = SearchProjection::from_session(&session);
        for (hide_calls, hide_results) in
            [(false, false), (true, false), (false, true), (true, true)]
        {
            let options = DisplayOptions {
                hide_user_tool_calls: hide_calls,
                hide_user_tool_results: hide_results,
                hide_tool_calls: true,
                hide_tool_results: true,
                ..DisplayOptions::SHOW_ALL
            };
            for (term, hidden) in [
                ("usercommand", hide_calls),
                ("useroutput", hide_calls || hide_results),
            ] {
                for mode in [
                    VisibilitySearch::All,
                    VisibilitySearch::Visible,
                    VisibilitySearch::Hidden,
                ] {
                    let actual = QueryPlan::compile(term, mode, options)
                        .unwrap()
                        .matches(&projection)
                        .is_match;
                    assert_eq!(
                        actual,
                        mode == VisibilitySearch::All
                            || hidden == (mode == VisibilitySearch::Hidden)
                    );
                }
            }
        }
    }

    #[test]
    fn extracts_position_independent_visibility_modifiers() {
        for (query, expected_query, expected_mode) in [
            (
                "visible: alpha beta",
                "alpha beta",
                VisibilitySearch::Visible,
            ),
            (
                "alpha hidden: beta",
                "alpha  beta",
                VisibilitySearch::Hidden,
            ),
            ("alpha beta all:", "alpha beta", VisibilitySearch::All),
            (
                "(visible: alpha OR beta)",
                "( alpha OR beta)",
                VisibilitySearch::Visible,
            ),
        ] {
            let (query, mode) = extract_visibility_search(query).unwrap();
            assert_eq!(query, expected_query);
            assert_eq!(mode, Some(expected_mode));
        }
    }

    #[test]
    fn leaves_quoted_and_prefixed_modifier_text_alone() {
        assert_eq!(
            extract_visibility_search(r#""visible:" content:hidden:"#).unwrap(),
            (r#""visible:" content:hidden:"#.to_owned(), None)
        );
    }

    #[test]
    fn rejects_conflicting_visibility_modifiers() {
        assert!(extract_visibility_search("visible: needle hidden:").is_err());
        assert_eq!(
            extract_visibility_search("hidden: needle hidden:").unwrap(),
            ("needle".to_owned(), Some(VisibilitySearch::Hidden))
        );
    }

    #[test]
    fn omitted_modifier_is_distinct_from_explicit_all() {
        assert_eq!(
            extract_visibility_search("needle").unwrap(),
            ("needle".to_owned(), None)
        );
        assert_eq!(
            extract_visibility_search("all: needle").unwrap(),
            ("needle".to_owned(), Some(VisibilitySearch::All))
        );
    }

    #[test]
    fn splits_bare_multi_word_queries_for_highlighting() {
        let terms = extract_highlight_terms("wordA wordB");
        assert_eq!(terms, ["worda", "wordb"]);
    }

    #[test]
    fn ignores_uppercase_boolean_operators_for_highlighting() {
        let terms = extract_highlight_terms("alpha AND beta OR gamma NOT delta");
        assert_eq!(terms, ["alpha", "beta", "gamma", "delta"]);
    }

    #[test]
    fn keeps_lowercase_words_named_like_operators() {
        let terms = extract_highlight_terms("rock and roll or bust");
        assert_eq!(terms, ["rock", "and", "roll", "or", "bust"]);
    }

    #[test]
    fn keeps_quoted_boolean_tokens_as_search_terms() {
        let terms = extract_highlight_terms("\"AND\" OR beta");
        assert_eq!(terms, ["and", "beta"]);
    }

    #[test]
    fn splits_quoted_phrases_into_independent_highlight_terms() {
        let terms = extract_highlight_terms("\"commit all\"");
        assert_eq!(terms, ["commit", "all"]);
    }

    #[test]
    fn keeps_boolean_words_inside_quoted_phrases() {
        let terms = extract_highlight_terms("\"alpha OR beta\"");
        assert_eq!(terms, ["alpha", "or", "beta"]);
    }

    #[test]
    fn detects_explicit_boolean_operators_outside_quotes() {
        assert!(has_explicit_boolean_operators("alpha AND beta"));
        assert!(has_explicit_boolean_operators("(alpha OR beta)"));
        assert!(!has_explicit_boolean_operators("\"alpha OR beta\""));
        assert!(!has_explicit_boolean_operators("alpha and beta"));
    }

    #[test]
    fn strips_search_field_prefixes_for_highlighting() {
        assert_eq!(
            extract_highlight_terms("toolcall:needle paths:src/main"),
            ["needle", "src/main"]
        );
        assert_eq!(
            extract_highlight_terms("toolresult:\"error text\""),
            ["error", "text"]
        );
        assert_eq!(
            extract_highlight_terms("visible: needle hidden:"),
            ["needle"]
        );
        assert_eq!(extract_highlight_terms(r#""visible:""#), ["visible:"]);
    }
}
