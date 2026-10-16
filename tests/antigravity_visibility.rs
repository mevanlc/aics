use std::fs;
use std::path::PathBuf;

use aics::export::{session_to_markdown_with_options, session_to_plain_text_with_options};
use aics::index::{IndexManager, IndexPaths, Scope, SearchFilters, SearchRequest, SortMode};
use aics::parse::session::cells_from_messages;
use aics::parse::{
    parse_antigravity_session_file, AntigravityMessageKind as Kind, MessageRole, Session,
    SessionCell, SessionMessage,
};
use aics::scan::SessionRoots;
use aics::search_projection::{cell_visible, message_visibility, SearchProjection};
use aics::search_query::VisibilitySearch;
use aics::settings::DisplayOptions;
use aics::tui::preview::render_session_text_with_options;
use aics::tui::theme::Theme;
use anyhow::Result;
use serde_json::{json, Value};
use tempfile::TempDir;

const FIXTURE: &str = include_str!("fixtures/sessions/antigravity-visibility.jsonl");
const SESSION_ID: &str = "5c3e9fa8-09bc-4290-964d-977808110d7c";

fn transcript_path(temp: &TempDir) -> Result<PathBuf> {
    let path = temp.path().join(format!(
        "brain/{SESSION_ID}/.system_generated/logs/transcript.jsonl"
    ));
    fs::create_dir_all(path.parent().unwrap())?;
    Ok(path)
}

fn fixture_session(temp: &TempDir) -> Result<Session> {
    let path = transcript_path(temp)?;
    fs::write(&path, FIXTURE)?;
    Ok(parse_antigravity_session_file(path)?.unwrap())
}

fn options_for(kind: Kind) -> DisplayOptions {
    let mut options = DisplayOptions::SHOW_ALL;
    match kind {
        Kind::DirectoryRules => options.hide_project_docs_autodump = true,
        Kind::Checkpoint | Kind::AgentMessage => options.hide_agent_replies = true,
        Kind::Error
        | Kind::TaskCanceled
        | Kind::TaskFinished
        | Kind::TaskStalled
        | Kind::SubagentError => options.hide_tool_results = true,
        Kind::TaskReminder | Kind::RestartNotice | Kind::HookFeedback => {
            options.hide_internal_context = true
        }
    }
    options
}

const CASES: &[(Kind, &str)] = &[
    (Kind::DirectoryRules, "PROJECT_DOCS_BEACON"),
    (Kind::Checkpoint, "CHECKPOINT_BEACON"),
    (Kind::Error, "TOOL_ERROR_BEACON"),
    (Kind::TaskCanceled, "CANCEL_BEACON"),
    (Kind::TaskFinished, "FINISHED_BEACON"),
    (Kind::TaskReminder, "REMINDER_BEACON"),
    (Kind::TaskStalled, "STALLED_BEACON"),
    (Kind::RestartNotice, "RESTART_BEACON"),
    (Kind::AgentMessage, "WORKER_BEACON"),
    (Kind::SubagentError, "SUBAGENT_ERROR_BEACON"),
    (Kind::HookFeedback, "HOOK_BEACON"),
];

#[test]
fn generated_provenance_survives_parse_serialization_and_cell_conversion() -> Result<()> {
    let temp = TempDir::new()?;
    let session = fixture_session(&temp)?;
    let records: Vec<Value> = FIXTURE
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()?;
    assert_eq!(session.messages.len(), records.len());
    for (message, record) in session.messages.iter().zip(&records).skip(1) {
        assert_eq!(message.content, record["content"].as_str().unwrap());
        assert_eq!(
            message.timestamp.unwrap().to_rfc3339(),
            "2026-09-21T20:29:56+00:00"
        );
    }
    for &(kind, beacon) in CASES {
        let message = session
            .messages
            .iter()
            .find(|message| message.antigravity_kind == Some(kind))
            .unwrap();
        assert!(message.content.contains(beacon));
        assert_eq!(
            message.role,
            if kind == Kind::Checkpoint {
                MessageRole::Summary
            } else {
                MessageRole::System
            }
        );
        let round_trip: SessionMessage = serde_json::from_value(serde_json::to_value(message)?)?;
        assert_eq!(&round_trip, message);
        let converted = cells_from_messages(std::slice::from_ref(message));
        let cell_round_trip: SessionCell =
            serde_json::from_value(serde_json::to_value(&converted[0])?)?;
        assert_eq!(cell_round_trip, converted[0]);
        assert!(
            matches!(converted[0], SessionCell::Message { antigravity_kind: Some(actual), .. } if actual == kind)
        );
    }
    assert!(session.messages[0]
        .content
        .contains("QUOTED_CANCELLATION_BEACON"));
    assert_eq!(session.messages[0].antigravity_kind, None);
    assert!(!session.messages[0]
        .content
        .contains("DISCARDED_METADATA_BEACON"));
    assert!(!session.messages[0]
        .content
        .contains("DISCARDED_SKILL_BEACON"));

    let legacy: SessionMessage =
        serde_json::from_value(json!({"role":"system", "content":"legacy", "timestamp":null}))?;
    let legacy_cell: SessionCell =
        serde_json::from_value(json!({"kind":"message", "role":"summary", "content":"legacy"}))?;
    assert_eq!(legacy.antigravity_kind, None);
    assert!(cell_visible(&legacy_cell, &options_for(Kind::Checkpoint)));

    let rules: Value = serde_json::from_str(&aics::rules::session_to_rule_context_json(
        &session,
        &session.file_path,
        false,
        None,
    )?)?;
    assert_eq!(
        rules["turns"]["system"].as_array().unwrap().len(),
        CASES.len() + 1
    );
    assert_eq!(rules["turns"]["agent"].as_array().unwrap().len(), 1);
    Ok(())
}

#[test]
fn visibility_matches_preview_and_both_export_representations() -> Result<()> {
    let temp = TempDir::new()?;
    let session = fixture_session(&temp)?;
    for message_only in [false, true] {
        let mut session = session.clone();
        if message_only {
            session.cells.clear();
        }
        for &(kind, beacon) in CASES {
            for (options, visible) in [(DisplayOptions::SHOW_ALL, true), (options_for(kind), false)]
            {
                let preview =
                    render_session_text_with_options(&session, &Theme::default(), None, options);
                let text = preview
                    .lines
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("\n");
                for rendered in [
                    text,
                    session_to_plain_text_with_options(&session, options),
                    session_to_markdown_with_options(&session, options),
                ] {
                    assert_eq!(
                        rendered.contains(beacon),
                        visible,
                        "{kind:?} message_only={message_only}"
                    );
                    assert!(rendered.contains("AUTHORED_BEACON"));
                    assert!(rendered.contains("QUOTED_CANCELLATION_BEACON"));
                    assert!(rendered.contains("UNKNOWN_BEACON"));
                }
            }
        }
        let unrelated = DisplayOptions {
            hide_user_messages: true,
            hide_tool_calls: true,
            ..DisplayOptions::SHOW_ALL
        };
        let rendered = session_to_plain_text_with_options(&session, unrelated);
        for &(_, beacon) in CASES {
            assert!(rendered.contains(beacon));
        }
        let defaults = session_to_plain_text_with_options(&session, DisplayOptions::default());
        assert!(defaults.contains("CANCEL_BEACON"));
        assert!(defaults.contains("CHECKPOINT_BEACON"));
        assert!(!defaults.contains("REMINDER_BEACON"));
        assert!(!defaults.contains("PROJECT_DOCS_BEACON"));
    }
    // An annotation cannot override the user's role or its visibility.
    assert!(message_visibility(
        MessageRole::User,
        "actual request",
        Some(Kind::HookFeedback)
    )
    .is_visible(options_for(Kind::HookFeedback)));
    Ok(())
}

#[test]
fn malformed_and_unfamiliar_envelopes_stay_unclassified() -> Result<()> {
    let temp = TempDir::new()?;
    let path = transcript_path(&temp)?;
    let original: Value = serde_json::from_str(FIXTURE.lines().nth(4).unwrap())?;
    let content = original["content"].as_str().unwrap();
    let cases = [
        ("USER_EXPLICIT", "SYSTEM_MESSAGE", content.to_owned()),
        ("SYSTEM", "FUTURE_TYPE", content.to_owned()),
        (
            "SYSTEM",
            "SYSTEM_MESSAGE",
            content.replace("timestamp=2026-09-21T20:29:56Z", "timestamp=invalid"),
        ),
        (
            "SYSTEM",
            "SYSTEM_MESSAGE",
            content.replace(&format!("sender={SESSION_ID}/task-48"), "sender=unknown"),
        ),
        (
            "SYSTEM",
            "SYSTEM_MESSAGE",
            content.replace(
                &format!("content=Task id \"{SESSION_ID}/task-48\""),
                &format!("content=Task id \"{SESSION_ID}/task-999\""),
            ),
        ),
        (
            "SYSTEM",
            "SYSTEM_MESSAGE",
            content.trim_end_matches("</SYSTEM_MESSAGE>").to_owned(),
        ),
        (
            "SYSTEM",
            "SYSTEM_MESSAGE",
            content.split_once("\n\n").unwrap().1.to_owned(),
        ),
        (
            "SYSTEM",
            "SYSTEM_MESSAGE",
            format!("{content}\nordinary trailing prose"),
        ),
        (
            "SYSTEM",
            "SYSTEM_MESSAGE",
            content.replace(" was canceled with result:", " has unfamiliar status:"),
        ),
        (
            "SYSTEM",
            "SYSTEM_MESSAGE",
            content.replace("/task-48", "/task-invalid"),
        ),
        (
            "SYSTEM",
            "SYSTEM_MESSAGE",
            content.replace("MESSAGE_PRIORITY_LOW", "invalid"),
        ),
    ];
    let all_hidden = DisplayOptions {
        hide_user_messages: true,
        hide_agent_replies: true,
        hide_tool_calls: true,
        hide_tool_results: true,
        hide_project_docs_autodump: true,
        hide_skill_text_injection: true,
        hide_internal_context: true,
        ..DisplayOptions::SHOW_ALL
    };
    for (source, step_type, content) in cases {
        let mut record = original.clone();
        record["source"] = json!(source);
        record["type"] = json!(step_type);
        record["content"] = json!(content);
        fs::write(&path, format!("{record}\n"))?;
        let session = parse_antigravity_session_file(&path)?.unwrap();
        assert_eq!(session.messages[0].antigravity_kind, None, "{record}");
        assert!(session_to_plain_text_with_options(&session, all_hidden).contains("CANCEL_BEACON"));
    }
    Ok(())
}

#[test]
fn index_visibility_and_format_rebuild_preserve_search_semantics() -> Result<()> {
    let temp = TempDir::new()?;
    let session = fixture_session(&temp)?;
    let roots = SessionRoots {
        live_sessions: Default::default(),
        claude_projects: temp.path().join("missing-claude"),
        codex_sessions: temp.path().join("missing-codex"),
        antigravity_home: temp.path().to_owned(),
        trash: None,
    };
    let paths = IndexPaths::from_root(temp.path().join("index-cache"));
    let manager = IndexManager::with_paths(paths.clone());
    manager.sync_with_roots(&roots, false)?;
    let engine = manager.open_search_engine()?;
    let search = |query: &str, options| -> Result<bool> {
        Ok(!engine
            .search_with_display_options(
                &SearchRequest {
                    query: query.to_owned(),
                    visibility_search: VisibilitySearch::Visible,
                    scope: Scope::Global,
                    limit: 10,
                    sort: SortMode::Relevance,
                    filters: SearchFilters::default(),
                },
                options,
            )?
            .is_empty())
    };
    for &(kind, beacon) in CASES {
        let options = options_for(kind);
        assert!(!search(&format!("visible: {beacon}"), options)?);
        assert!(search(&format!("hidden: {beacon}"), options)?);
        assert!(search(&format!("all: {beacon}"), options)?);
        assert!(search(
            &format!("visible: {beacon}"),
            DisplayOptions::SHOW_ALL
        )?);
    }
    let options = DisplayOptions {
        hide_agent_replies: true,
        hide_tool_results: true,
        hide_internal_context: true,
        ..DisplayOptions::SHOW_ALL
    };
    assert!(search("visible: agent:CHECKPOINT_BEACON", options)?);
    assert!(!search("all: agent:WORKER_BEACON", options)?);
    assert!(!search("all: toolresult:CANCEL_BEACON", options)?);
    assert!(search("visible: UNKNOWN_BEACON", options)?);
    assert!(search("visible: AUTHORED_BEACON", options)?);
    let projection = SearchProjection::from_session(&session);
    assert!(projection.fields.contains_key("_vis_internal_context"));

    drop(engine);
    let mut old_state: Value = serde_json::from_str(&fs::read_to_string(&paths.state_file)?)?;
    old_state["format_version"] = json!(19);
    fs::write(&paths.state_file, serde_json::to_string(&old_state)?)?;
    let rebuilt = manager.sync_with_roots(&roots, false)?;
    assert_eq!(rebuilt.updated, 1);
    let state: Value = serde_json::from_str(&fs::read_to_string(&paths.state_file)?)?;
    assert_eq!(state["format_version"], 23);
    Ok(())
}

#[test]
fn agent_thinking_visibility_controls_planner_thinking_independently_of_agent_replies() -> Result<()>
{
    let temp = TempDir::new()?;
    let path = transcript_path(&temp)?;
    let record = json!({
        "step_index": 0,
        "source": "MODEL",
        "type": "PLANNER_RESPONSE",
        "status": "DONE",
        "created_at": "2026-09-21T20:29:56Z",
        "thinking": "THINKING_BEACON Inspecting visibility options.",
        "content": "ASSISTANT_BEACON Ready to update filters."
    });
    fs::write(&path, format!("{record}\n"))?;
    let session = parse_antigravity_session_file(&path)?.unwrap();
    let theme = Theme::default();

    // Hidden by default
    let defaults = DisplayOptions::default();
    let default_preview = render_session_text_with_options(&session, &theme, None, defaults)
        .lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!default_preview.contains("THINKING_BEACON"));
    assert!(default_preview.contains("ASSISTANT_BEACON"));
    assert!(!session_to_markdown_with_options(&session, defaults).contains("THINKING_BEACON"));
    assert!(!session_to_plain_text_with_options(&session, defaults).contains("THINKING_BEACON"));

    // Visible when hide_agent_thinking is false, even if hide_agent_replies is true
    let thinking_only = DisplayOptions {
        hide_agent_replies: true,
        hide_agent_thinking: false,
        ..DisplayOptions::SHOW_ALL
    };
    let thinking_preview = render_session_text_with_options(&session, &theme, None, thinking_only)
        .lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(thinking_preview.contains("THINKING_BEACON"));
    assert!(!thinking_preview.contains("ASSISTANT_BEACON"));
    assert!(session_to_markdown_with_options(&session, thinking_only).contains("THINKING_BEACON"));
    assert!(!session_to_markdown_with_options(&session, thinking_only).contains("ASSISTANT_BEACON"));

    let roots = SessionRoots {
        live_sessions: Default::default(),
        claude_projects: temp.path().join("missing-claude"),
        codex_sessions: temp.path().join("missing-codex"),
        antigravity_home: temp.path().to_owned(),
        trash: None,
    };
    let paths = IndexPaths::from_root(temp.path().join("index-cache"));
    let manager = IndexManager::with_paths(paths);
    manager.sync_with_roots(&roots, false)?;
    let engine = manager.open_search_engine()?;
    let search = |query: &str, options| -> Result<bool> {
        Ok(!engine
            .search_with_display_options(
                &SearchRequest {
                    query: query.to_owned(),
                    visibility_search: VisibilitySearch::Visible,
                    scope: Scope::Global,
                    limit: 10,
                    sort: SortMode::Relevance,
                    filters: SearchFilters::default(),
                },
                options,
            )?
            .is_empty())
    };
    assert!(!search("visible: THINKING_BEACON", defaults)?);
    assert!(search("hidden: THINKING_BEACON", defaults)?);
    assert!(search("all: THINKING_BEACON", defaults)?);
    assert!(search("visible: THINKING_BEACON", thinking_only)?);
    assert!(!search("visible: ASSISTANT_BEACON", thinking_only)?);
    Ok(())
}
