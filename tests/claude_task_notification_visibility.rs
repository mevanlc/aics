use std::fs;
use std::path::PathBuf;

use aics::export::{
    session_to_markdown, session_to_markdown_with_options, session_to_plain_text,
    session_to_plain_text_with_options,
};
use aics::index::{IndexManager, IndexPaths, Scope, SearchFilters, SearchRequest, SortMode};
use aics::parse::session::cells_from_messages;
use aics::parse::{parse_claude_session_file, MessageRole, Session, SessionCell};
use aics::scan::SessionRoots;
use aics::search_projection::SearchProjection;
use aics::search_query::{QueryPlan, VisibilitySearch};
use aics::settings::DisplayOptions;
use aics::tui::preview::render_session_text_with_options;
use aics::tui::theme::Theme;
use anyhow::Result;
use serde_json::{json, Value};
use tempfile::TempDir;

const FIXTURE: &str = include_str!("fixtures/sessions/claude/task_notifications.jsonl");
const TASK_NEEDLES: [&str; 5] = [
    "OriginTaskNeedle",
    "LegacyBashTaskNeedle",
    "ArrayAgentTaskNeedle",
    "MonitorEventTaskNeedle",
    "OriginPlainFailureNeedle",
];
const USER_NEEDLES: [&str; 5] = [
    "PromptNeedle",
    "HumanPastedTaskNeedle",
    "QuotedTaskNeedle",
    "MixedHumanPromptNeedle",
    "MalformedOriginNeedle",
];

fn fixture_session() -> Result<Session> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/sessions/claude/task_notifications.jsonl");
    Ok(parse_claude_session_file(path)?.unwrap())
}

#[test]
fn task_notification_provenance_preserves_source_and_authored_user_fields() -> Result<()> {
    let session = fixture_session()?;
    assert_eq!(session.messages.len(), 12);
    for (index, message) in session.messages.iter().enumerate() {
        assert_eq!(message.is_task_notification, index < 5, "index {index}");
        assert_eq!(message.is_compact_summary, index == 10, "index {index}");
        if index < 11 {
            assert_eq!(message.role, MessageRole::User, "index {index}");
        } else {
            assert_eq!(message.role, MessageRole::Assistant, "index {index}");
        }
    }
    let first_record: Value = serde_json::from_str(FIXTURE.lines().next().unwrap())?;
    assert_eq!(
        session.messages[0].content,
        first_record["message"]["content"]
    );
    assert_eq!(
        session.first_user_msg_content,
        "Please inspect the incremental output. PromptNeedle."
    );
    let serialized = serde_json::to_value(&session)?;
    assert_eq!(serialized["messages"][0]["is_task_notification"], true);
    assert!(serialized["messages"][5]
        .get("is_task_notification")
        .is_none());
    let round_trip: Session = serde_json::from_value(serialized)?;
    assert_eq!(round_trip.messages, session.messages);
    assert_eq!(round_trip.cells, session.cells);
    assert_eq!(cells_from_messages(&session.messages), session.cells);

    for needle in TASK_NEEDLES {
        assert!(session_to_markdown(&session).contains(needle), "{needle}");
        assert!(session_to_plain_text(&session).contains(needle), "{needle}");
        assert!(
            !session
                .search_fields
                .user
                .iter()
                .any(|text| text.contains(needle)),
            "task needle leaked into user field: {needle}"
        );
        assert!(
            !session
                .search_fields
                .tool_result
                .iter()
                .any(|text| text.contains(needle)),
            "task needle leaked into tool_result field: {needle}"
        );
    }

    assert!(!session
        .search_fields
        .user
        .iter()
        .any(|text| text.contains("MixedTaskPreambleNeedle")));
    for needle in USER_NEEDLES {
        assert!(
            session
                .search_fields
                .user
                .iter()
                .any(|text| text.contains(needle)),
            "missing user needle: {needle}"
        );
    }
    Ok(())
}

#[test]
fn task_notification_visibility_follows_tool_results_independently_of_user_and_internal_context(
) -> Result<()> {
    let parsed = fixture_session()?;
    for message_only in [false, true] {
        let mut session = parsed.clone();
        if message_only {
            session.cells.clear();
        }
        let projection = SearchProjection::from_session(&session);
        for hide_tool_results in [false, true] {
            for hide_user_messages in [false, true] {
                for hide_internal_context in [false, true] {
                    let options = DisplayOptions {
                        hide_tool_results,
                        hide_user_messages,
                        hide_internal_context,
                        ..DisplayOptions::SHOW_ALL
                    };
                    let preview = render_session_text_with_options(
                        &session,
                        &Theme::default(),
                        None,
                        options,
                    )
                    .lines
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("\n");
                    for output in [
                        preview,
                        session_to_markdown_with_options(&session, options),
                        session_to_plain_text_with_options(&session, options),
                    ] {
                        for needle in TASK_NEEDLES {
                            assert_eq!(
                                output.contains(needle),
                                !hide_tool_results,
                                "{needle}, message_only={message_only}, {options:?}"
                            );
                        }
                        for needle in USER_NEEDLES {
                            assert_eq!(
                                output.contains(needle),
                                !hide_user_messages,
                                "{needle}, message_only={message_only}, {options:?}"
                            );
                        }
                        assert_eq!(
                            output.contains("MixedTaskPreambleNeedle"),
                            !hide_user_messages,
                            "mixed turn remains a user message"
                        );
                        assert_eq!(
                            output.contains("CompactTurnOriginNeedle"),
                            !hide_internal_context && !hide_user_messages
                        );
                        assert!(output.contains("AssistantReplyNeedle"));
                    }

                    for needle in TASK_NEEDLES {
                        for (mode, expected) in [
                            (VisibilitySearch::Visible, !hide_tool_results),
                            (VisibilitySearch::Hidden, hide_tool_results),
                            (VisibilitySearch::All, true),
                        ] {
                            let matched =
                                QueryPlan::compile(needle, mode, options)?.matches(&projection);
                            assert_eq!(matched.is_match, expected, "{needle}: {mode:?}");
                            if expected {
                                assert_eq!(matched.matches.len(), 1);
                                let source = &matched.matches[0].sources[0];
                                let text = match source.source.block {
                                    aics::search_match::SourceBlock::Message(index) => {
                                        &session.messages[index].content
                                    }
                                    aics::search_match::SourceBlock::Cell(index) => {
                                        match &session.cells[index] {
                                            SessionCell::Message { content, .. } => content,
                                            _ => panic!("expected a message"),
                                        }
                                    }
                                    _ => panic!("expected original transcript source"),
                                };
                                assert_eq!(&text[source.range.clone()], needle);
                            }
                        }
                        for semantic in ["user", "toolresult"] {
                            assert!(
                                !QueryPlan::compile(
                                    &format!("{semantic}:{needle}"),
                                    VisibilitySearch::All,
                                    options,
                                )?
                                .matches(&projection)
                                .is_match
                            );
                        }
                    }

                    assert!(
                        !QueryPlan::compile(
                            "user:MixedTaskPreambleNeedle",
                            VisibilitySearch::All,
                            options
                        )?
                        .matches(&projection)
                        .is_match
                    );
                    let mixed = QueryPlan::compile(
                        "user:MixedHumanPromptNeedle",
                        VisibilitySearch::All,
                        options,
                    )?
                    .matches(&projection);
                    assert!(mixed.is_match);
                    assert_eq!(mixed.matches.len(), 1);
                    let source = &mixed.matches[0].sources[0];
                    let text = match source.source.block {
                        aics::search_match::SourceBlock::Message(index) => {
                            &session.messages[index].content
                        }
                        aics::search_match::SourceBlock::Cell(index) => {
                            match &session.cells[index] {
                                SessionCell::Message { content, .. } => content,
                                _ => panic!("expected a message"),
                            }
                        }
                        _ => panic!("expected original transcript source"),
                    };
                    assert_eq!(&text[source.range.clone()], "MixedHumanPromptNeedle");
                }
            }
        }
    }
    Ok(())
}

#[test]
fn task_notification_only_sessions_and_adjacent_identical_text_preserve_provenance() -> Result<()> {
    let temp = TempDir::new()?;
    let path = temp.path().join("task-only.jsonl");
    fs::write(&path, FIXTURE.lines().next().unwrap())?;
    let mut session = parse_claude_session_file(&path)?.unwrap();
    assert!(session.first_user_msg_content.is_empty());
    for message_only in [false, true] {
        if message_only {
            session.cells.clear();
        }
        let projection = SearchProjection::from_session(&session);
        assert!(!projection.fields.contains_key("user"));
        assert!(!projection.fields.contains_key("toolresult"));
        assert!(
            session_to_markdown_with_options(&session, DisplayOptions::default())
                .contains("OriginTaskNeedle")
        );
        assert!(session_to_markdown_with_options(
            &session,
            DisplayOptions {
                hide_tool_results: true,
                ..DisplayOptions::default()
            }
        )
        .find("OriginTaskNeedle")
        .is_none());
    }

    // Equal text on adjacent records must keep their different provenance.
    let mut record: Value = serde_json::from_str(FIXTURE.lines().next().unwrap())?;
    let generated = serde_json::to_string(&record)?;
    record["origin"] = json!({"kind": "human"});
    record.as_object_mut().unwrap().remove("promptSource");
    record.as_object_mut().unwrap().remove("turnOrigin");
    let authored = serde_json::to_string(&record)?;
    fs::write(&path, format!("{generated}\n{authored}\n{generated}\n"))?;
    let session = parse_claude_session_file(path)?.unwrap();
    assert_eq!(session.messages.len(), 3);
    assert_eq!(
        session
            .messages
            .iter()
            .map(|message| message.is_task_notification)
            .collect::<Vec<_>>(),
        [true, false, true]
    );
    Ok(())
}

#[test]
fn indexed_task_notification_visibility_rebuilds_unchanged_version_26_sources() -> Result<()> {
    let temp = TempDir::new()?;
    let claude = temp.path().join("claude/projects/project");
    fs::create_dir_all(&claude)?;
    let path = claude.join("task-notifications.jsonl");
    fs::write(&path, FIXTURE)?;
    let roots = SessionRoots {
        live_sessions: Default::default(),
        claude_projects: temp.path().join("claude/projects"),
        codex_sessions: temp.path().join("missing-codex"),
        antigravity_home: temp.path().join("missing-antigravity"),
        trash: None,
    };
    let paths = IndexPaths::from_root(temp.path().join("cache"));
    let manager = IndexManager::with_paths(paths.clone());
    manager.sync_with_roots(&roots, false)?;
    let mut old_state: Value = serde_json::from_str(&fs::read_to_string(&paths.state_file)?)?;
    old_state["format_version"] = json!(26);
    fs::write(&paths.state_file, serde_json::to_vec(&old_state)?)?;
    assert_eq!(manager.sync_with_roots(&roots, false)?.updated, 1);
    let state: Value = serde_json::from_str(&fs::read_to_string(&paths.state_file)?)?;
    assert_eq!(state["format_version"], 27);
    let engine = manager.open_search_engine()?;
    for hide_tool_results in [false, true] {
        for hide_user_messages in [false, true] {
            let options = DisplayOptions {
                hide_tool_results,
                hide_user_messages,
                hide_internal_context: true,
                ..DisplayOptions::SHOW_ALL
            };
            let search = |query: String| {
                engine.search_with_display_options(
                    &SearchRequest {
                        query,
                        visibility_search: VisibilitySearch::Visible,
                        scope: Scope::Global,
                        limit: 10,
                        sort: SortMode::Relevance,
                        filters: SearchFilters::default(),
                    },
                    options,
                )
            };
            for needle in TASK_NEEDLES {
                assert_eq!(
                    search(format!("visible: {needle}"))?.len(),
                    usize::from(!hide_tool_results),
                    "{needle}"
                );
                assert_eq!(
                    search(format!("hidden: {needle}"))?.len(),
                    usize::from(hide_tool_results),
                    "{needle}"
                );
                assert_eq!(search(format!("all: {needle}"))?.len(), 1);
                assert_eq!(search(format!("visible: content:{needle}"))?.len(), 1);
                assert!(search(format!("user:{needle}"))?.is_empty());
                assert!(search(format!("toolresult:{needle}"))?.is_empty());
            }
            for needle in USER_NEEDLES {
                assert_eq!(
                    search(format!("visible: {needle}"))?.len(),
                    usize::from(!hide_user_messages),
                    "{needle}"
                );
                assert_eq!(search(format!("user:{needle}"))?.len(), 1, "{needle}");
            }
            assert!(search("user:MixedTaskPreambleNeedle".to_owned())?.is_empty());
        }
    }
    assert_eq!(fs::read_to_string(path)?, FIXTURE);
    Ok(())
}
