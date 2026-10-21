use std::fs;
use std::path::PathBuf;

use aics::export::{
    session_to_markdown, session_to_markdown_with_options, session_to_plain_text,
    session_to_plain_text_with_options,
};
use aics::index::{IndexManager, IndexPaths, Scope, SearchFilters, SearchRequest, SortMode};
use aics::parse::{parse_codex_session_file, Agent, MessageRole, Session, SessionCell};
use aics::scan::SessionRoots;
use aics::search_projection::{cell_visible, SearchProjection};
use aics::search_query::{QueryPlan, VisibilitySearch};
use aics::settings::DisplayOptions;
use aics::tui::preview::render_session_text_with_options;
use aics::tui::theme::Theme;
use anyhow::Result;
use serde_json::{json, Value};
use tempfile::TempDir;

const FIXTURE: &str = include_str!("fixtures/sessions/codex/memory_citations.jsonl");

fn fixture_session() -> Result<Session> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/sessions/codex/memory_citations.jsonl");
    Ok(parse_codex_session_file(path)?.unwrap())
}

#[test]
fn memory_citations_remain_in_original_messages_and_rules() -> Result<()> {
    let session = fixture_session()?;
    let records = FIXTURE
        .lines()
        .map(serde_json::from_str::<Value>)
        .collect::<Result<Vec<_>, _>>()?;
    assert_eq!(session.messages.len(), records.len() - 1);
    for (message, record) in session.messages.iter().zip(records.iter().skip(1)) {
        assert_eq!(
            message.content,
            record["payload"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .trim()
        );
        assert_eq!(
            message.timestamp.unwrap().to_rfc3339(),
            record["timestamp"].as_str().unwrap().replace('Z', "+00:00")
        );
    }
    assert_eq!(
        session
            .cells
            .iter()
            .filter(|cell| matches!(cell, SessionCell::Message { .. }))
            .count(),
        session.messages.len()
    );
    let rules: Value = serde_json::from_str(&aics::rules::session_to_rule_context_json(
        &session,
        &session.file_path,
        false,
        None,
    )?)?;
    assert_eq!(rules["turns"]["agent"].as_array().unwrap().len(), 5);
    assert!(serde_json::to_string(&rules)?.contains("MemoryCitationNeedle"));
    assert!(serde_json::to_string(&session)?.contains("MemoryCitationNeedle"));
    Ok(())
}

#[test]
fn memory_footer_hiding_preserves_reply_rendering_and_exports() -> Result<()> {
    let parsed = fixture_session()?;
    for agent in [Agent::Claude, Agent::Codex, Agent::Antigravity] {
        for message_only in [false, true] {
            let mut session = parsed.clone();
            session.agent = agent;
            if message_only {
                session.cells.clear();
            }
            let projection = SearchProjection::from_session(&session);
            let reply = projection
                .segments
                .iter()
                .find(|segment| segment.text.contains("ReplyNeedle"))
                .unwrap();
            let citation = projection
                .segments
                .iter()
                .find(|segment| segment.text.contains("MemoryCitationNeedle"))
                .unwrap();
            assert_eq!(reply.source, citation.source);
            assert_eq!(reply.source_range.end, citation.source_range.start);
            let hidden = DisplayOptions::default();
            let preview =
                render_session_text_with_options(&session, &Theme::default(), None, hidden)
                    .lines
                    .into_iter()
                    .map(|line| {
                        line.spans
                            .into_iter()
                            .map(|span| span.content)
                            .collect::<String>()
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
            assert!(preview.contains("Reply café 🦀: ReplyNeedle"));
            for output in [
                preview,
                session_to_plain_text_with_options(&session, hidden),
                session_to_markdown_with_options(&session, hidden),
            ] {
                for retained in [
                    "ReplyNeedle",
                    "UserQuoteNeedle",
                    "QuotedCitationNeedle",
                    "MiddleCitationNeedle",
                    "AfterFooterNeedle",
                    "IncompleteCitationNeedle",
                ] {
                    assert!(output.contains(retained), "{agent:?}: {retained}");
                }
                assert!(!output.contains("MemoryCitationNeedle"));
                assert!(!output.contains("CitationOnlyNeedle"));
            }
            for output in [
                session_to_plain_text(&session),
                session_to_markdown(&session),
            ] {
                assert!(output.contains("MemoryCitationNeedle"));
                assert!(output.contains("CitationOnlyNeedle"));
            }
            let hidden_agent = DisplayOptions {
                hide_agent_replies: true,
                ..DisplayOptions::SHOW_ALL
            };
            let output = session_to_markdown_with_options(&session, hidden_agent);
            assert!(!output.contains("ReplyNeedle"));
            assert!(!output.contains("MemoryCitationNeedle"));
        }
    }
    let pure = parsed.cells.iter().find(|cell| {
        matches!(cell, SessionCell::Message { content, role: MessageRole::Assistant, .. } if content.contains("CitationOnlyNeedle"))
    }).unwrap();
    assert!(!cell_visible(pure, &DisplayOptions::default()));
    assert!(cell_visible(pure, &DisplayOptions::SHOW_ALL));
    Ok(())
}

#[test]
fn memory_citation_search_spans_keep_original_offsets() -> Result<()> {
    let parsed = fixture_session()?;
    for message_only in [false, true] {
        let mut session = parsed.clone();
        if message_only {
            session.cells.clear();
        }
        let projection = SearchProjection::from_session(&session);
        let original = session
            .messages
            .iter()
            .find(|message| message.content.contains("ReplyNeedle"))
            .unwrap();
        assert!(projection.fields["content"][0]
            .text
            .contains(original.content.trim()));
        assert!(projection.fields["agent"][0]
            .text
            .contains(original.content.trim()));
        for (needle, visible, hidden, all) in [
            ("ReplyNeedle", 1, 0, 1),
            ("MemoryCitationNeedle", 0, 1, 1),
            ("RepeatedMemoryNeedle", 1, 1, 2),
        ] {
            for (mode, expected) in [
                (VisibilitySearch::Visible, visible),
                (VisibilitySearch::Hidden, hidden),
                (VisibilitySearch::All, all),
            ] {
                let matched = QueryPlan::compile(needle, mode, DisplayOptions::default())?
                    .matches(&projection);
                assert_eq!(matched.matches.len(), expected, "{needle}: {mode:?}");
                for occurrence in &matched.matches {
                    assert_eq!(occurrence.sources.len(), 1);
                    let source = &occurrence.sources[0];
                    assert_eq!(&original.content[source.range.clone()], needle);
                }
            }
        }
        let explicit = QueryPlan::compile(
            "visible: agent:MemoryCitationNeedle",
            VisibilitySearch::Visible,
            DisplayOptions::default(),
        )?
        .matches(&projection);
        assert!(explicit.is_match);
        assert_eq!(explicit.matches.len(), 1);
    }
    Ok(())
}

#[test]
fn indexed_memory_footer_visibility_and_version_19_rebuild() -> Result<()> {
    let temp = TempDir::new()?;
    let codex = temp.path().join("codex/sessions");
    fs::create_dir_all(&codex)?;
    let path = codex.join("rollout-memory-citations.jsonl");
    fs::write(&path, FIXTURE)?;
    let roots = SessionRoots {
        live_sessions: Default::default(),
        claude_projects: temp.path().join("missing-claude"),
        codex_sessions: codex,
        antigravity_home: temp.path().join("missing-antigravity"),
        trash: None,
    };
    let paths = IndexPaths::from_root(temp.path().join("index-cache"));
    let manager = IndexManager::with_paths(paths.clone());
    manager.sync_with_roots(&roots, false)?;
    let engine = manager.open_search_engine()?;
    for hide_agent_replies in [false, true] {
        for hide_internal_context in [false, true] {
            let options = DisplayOptions {
                hide_agent_replies,
                hide_internal_context,
                ..DisplayOptions::SHOW_ALL
            };
            let search = |query: &str| {
                engine.search_with_display_options(
                    &SearchRequest {
                        query: query.to_owned(),
                        visibility_search: VisibilitySearch::Visible,
                        scope: Scope::Global,
                        limit: 10,
                        sort: SortMode::Relevance,
                        filters: SearchFilters::default(),
                    },
                    options,
                )
            };
            for (needle, hidden) in [
                ("ReplyNeedle", hide_agent_replies),
                (
                    "MemoryCitationNeedle",
                    hide_agent_replies || hide_internal_context,
                ),
            ] {
                assert_eq!(!search(&format!("visible: {needle}"))?.is_empty(), !hidden);
                assert_eq!(!search(&format!("hidden: {needle}"))?.is_empty(), hidden);
                assert_eq!(search(&format!("all: {needle}"))?.len(), 1);
            }
            assert_eq!(search("visible: agent:MemoryCitationNeedle")?.len(), 1);
            if !hide_agent_replies && hide_internal_context {
                let visible = search("visible: ReplyNeedle")?;
                assert!(!visible[0].snippet_html.contains("MemoryCitationNeedle"));
                let hidden = search("hidden: MemoryCitationNeedle")?;
                assert!(hidden[0].snippet_html.contains("MemoryCitationNeedle"));
                assert!(!hidden[0].snippet_html.contains("ReplyNeedle"));
            }
        }
    }
    drop(engine);
    let mut old_state: Value = serde_json::from_str(&fs::read_to_string(&paths.state_file)?)?;
    old_state["format_version"] = json!(19);
    fs::write(&paths.state_file, serde_json::to_string(&old_state)?)?;
    let rebuilt = manager.sync_with_roots(&roots, false)?;
    assert_eq!(rebuilt.updated, 1);
    let state: Value = serde_json::from_str(&fs::read_to_string(&paths.state_file)?)?;
    assert_eq!(state["format_version"], 25);
    assert_eq!(fs::read_to_string(&path)?, FIXTURE);
    Ok(())
}
