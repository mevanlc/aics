use std::path::PathBuf;

use aics::export::{session_to_markdown_with_options, session_to_plain_text_with_options};
use aics::index::{IndexManager, IndexPaths, Scope, SearchFilters, SearchRequest, SortMode};
use aics::parse::{parse_codex_session_file, MessageRole, Session, SessionCell};
use aics::scan::SessionRoots;
use aics::search_projection::SearchProjection;
use aics::search_query::VisibilitySearch;
use aics::settings::DisplayOptions;
use aics::tui::preview::render_session_text_with_options;
use aics::tui::theme::Theme;
use anyhow::Result;
use serde_json::{json, Value};

fn fixture_session() -> Result<Session> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/sessions/codex/user_questions.jsonl");
    Ok(parse_codex_session_file(path)?.unwrap())
}

fn rendered(session: &Session, options: DisplayOptions) -> String {
    render_session_text_with_options(session, &Theme::default(), None, options)
        .lines
        .into_iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn questions_and_answers_are_conversation_turns_across_transcript_surfaces() -> Result<()> {
    let session = fixture_session()?;
    assert_eq!(
        session.messages.iter().map(|m| m.role).collect::<Vec<_>>(),
        [
            MessageRole::User,
            MessageRole::Assistant,
            MessageRole::User,
            MessageRole::Assistant,
            MessageRole::User,
            MessageRole::Assistant,
        ]
    );
    let cells = session
        .cells
        .iter()
        .filter_map(|cell| match cell {
            SessionCell::Message {
                role,
                content,
                timestamp,
                ..
            } => Some((*role, content, timestamp)),
            SessionCell::SessionInfo(_) | SessionCell::Metrics(_) => None,
            _ => panic!("question exchange should contain only message cells"),
        })
        .collect::<Vec<_>>();
    for (message, (role, content, timestamp)) in session.messages.iter().zip(&cells) {
        assert_eq!(
            (message.role, &message.content, &message.timestamp),
            (*role, *content, *timestamp)
        );
    }
    assert_eq!(cells.len(), session.messages.len());
    assert!(session.messages[1]
        .content
        .contains("- project/aics\n- project/codex"));
    assert!(session.messages[3]
        .content
        .contains("- All sessions: Include every provider."));
    assert!(
        session.messages[4].content.find("All sessions").unwrap()
            < session.messages[4].content.find("Three runs.").unwrap()
    );
    assert_eq!(
        session.messages[2].timestamp.unwrap().to_rfc3339(),
        "2026-10-10T12:00:04+00:00"
    );
    assert_eq!(session.search_fields.agent.len(), 3);
    assert_eq!(session.search_fields.user.len(), 3);
    assert!(session.search_fields.tool_call.is_empty());
    assert!(session.search_fields.tool_result.is_empty());
    assert!(session
        .lineage
        .semantic_event_ids
        .contains(&"function_call:async-1".to_owned()));
    assert!(session
        .lineage
        .semantic_event_ids
        .contains(&"message:async-answer".to_owned()));

    // Include the legacy message rendering path and both export formats.
    for message_only in [false, true] {
        let mut session = session.clone();
        if message_only {
            session.cells.clear();
        }
        let options = DisplayOptions::default();
        for output in [
            rendered(&session, options),
            session_to_markdown_with_options(&session, options),
            session_to_plain_text_with_options(&session, options),
        ] {
            for text in [
                "Question",
                "Answer",
                "multiple repos",
                "Include every provider.",
                "Three runs.",
                "Where should the report go?",
            ] {
                assert!(output.contains(text), "missing {text}: {output}");
            }
            for internal in [
                "send_user_message_question_reply",
                "questionItemId",
                "accepted",
                "request_user_input",
                "async-1",
            ] {
                assert!(!output.contains(internal), "leaked {internal}: {output}");
            }
            assert_eq!(output.matches("multiple repos").count(), 1);
        }
    }
    Ok(())
}

#[test]
fn question_visibility_and_search_follow_agent_and_user_filters() -> Result<()> {
    let session = fixture_session()?;
    let tools_hidden = DisplayOptions {
        hide_tool_calls: true,
        hide_tool_results: true,
        ..DisplayOptions::SHOW_ALL
    };
    assert!(rendered(&session, tools_hidden).contains("Where should the report go?"));
    assert!(rendered(&session, tools_hidden).contains("multiple repos"));
    let user_hidden = DisplayOptions {
        hide_user_messages: true,
        ..tools_hidden
    };
    let agent_hidden = DisplayOptions {
        hide_agent_replies: true,
        ..tools_hidden
    };
    assert!(!rendered(&session, user_hidden).contains("multiple repos"));
    assert!(rendered(&session, user_hidden).contains("Where should the report go?"));
    assert!(rendered(&session, agent_hidden).contains("multiple repos"));
    assert!(!rendered(&session, agent_hidden).contains("Where should the report go?"));

    let projection = SearchProjection::from_session(&session);
    for (needle, hidden) in [
        ("multiple repos", user_hidden),
        ("Where should the report go?", agent_hidden),
    ] {
        let segments = projection
            .segments
            .iter()
            .filter(|segment| segment.text.contains(needle))
            .collect::<Vec<_>>();
        assert!(!segments.is_empty());
        for segment in segments {
            assert!(segment.visibility.is_visible(tools_hidden));
            assert!(!segment.visibility.is_visible(hidden));
        }
    }
    Ok(())
}

#[test]
fn indexed_questions_use_conversation_fields_and_rebuild_version_20_caches() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let codex = temp.path().join("codex/sessions");
    std::fs::create_dir_all(&codex)?;
    let path = codex.join("user_questions.jsonl");
    let fixture = include_str!("fixtures/sessions/codex/user_questions.jsonl");
    std::fs::write(&path, fixture)?;
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
    let search = |query: &str, options| {
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
    let options = DisplayOptions::default();
    for query in ["report", "agent:report", "repos", "user:repos"] {
        assert_eq!(search(query, options)?.len(), 1, "{query}");
    }
    for query in [
        "tool_call:report",
        "tool_result:repos",
        "questionItemId",
        "accepted",
    ] {
        assert!(search(query, options)?.is_empty(), "{query}");
    }
    assert!(search(
        "report",
        DisplayOptions {
            hide_agent_replies: true,
            ..options
        }
    )?
    .is_empty());
    assert!(search(
        "repos",
        DisplayOptions {
            hide_user_messages: true,
            ..options
        }
    )?
    .is_empty());
    drop(engine);
    let mut old_state: Value = serde_json::from_str(&std::fs::read_to_string(&paths.state_file)?)?;
    old_state["format_version"] = json!(20);
    std::fs::write(&paths.state_file, serde_json::to_string(&old_state)?)?;
    assert_eq!(manager.sync_with_roots(&roots, false)?.updated, 1);
    let state: Value = serde_json::from_str(&std::fs::read_to_string(&paths.state_file)?)?;
    assert_eq!(state["format_version"], 22);
    assert_eq!(std::fs::read_to_string(&path)?, fixture);
    Ok(())
}
