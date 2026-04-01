use std::path::PathBuf;
use std::sync::LazyLock;

use aics::parse::{parse_session_file, Agent, MessageRole, Session};
use aics::tui::markdown::render_markdown_message;
use aics::tui::preview::render_session_text;
use aics::tui::theme::Theme;
use criterion::{criterion_group, criterion_main, Criterion};
use ratatui::style::Style;
use std::hint::black_box;

static CLAUDE_RICH_SESSION: LazyLock<Session> = LazyLock::new(|| {
    load_session(
        Agent::Claude,
        "tests/fixtures/sessions/claude/rich_content.jsonl",
    )
});
static CODEX_LATEST_SESSION: LazyLock<Session> = LazyLock::new(|| {
    load_session(
        Agent::Codex,
        "tests/fixtures/sessions/codex/latest_format.jsonl",
    )
});

fn load_session(agent: Agent, relative_path: &str) -> Session {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative_path);
    parse_session_file(agent, &path)
        .unwrap_or_else(|error| panic!("failed to parse fixture {}: {error:#}", path.display()))
        .unwrap_or_else(|| panic!("fixture {} parsed to no session", path.display()))
}

fn longest_assistant_message(session: &Session) -> &str {
    session
        .messages
        .iter()
        .filter(|message| message.role == MessageRole::Assistant)
        .max_by_key(|message| message.content.len())
        .map(|message| message.content.as_str())
        .unwrap_or(session.content.as_str())
}

fn bench_markdown_render(c: &mut Criterion) {
    let theme = Theme::default();
    let message = longest_assistant_message(&CLAUDE_RICH_SESSION);
    let base = Style::default().fg(theme.text).bg(theme.bubble_claude);

    let mut group = c.benchmark_group("markdown_render");
    group.bench_function("claude_rich_no_highlight", |b| {
        b.iter(|| {
            render_markdown_message(black_box(message), black_box(&theme), black_box(base), None)
        })
    });
    group.bench_function("claude_rich_with_highlight", |b| {
        b.iter(|| {
            render_markdown_message(
                black_box(message),
                black_box(&theme),
                black_box(base),
                Some(black_box("token rotation auth tests")),
            )
        })
    });
    group.finish();
}

fn bench_preview_render(c: &mut Criterion) {
    let theme = Theme::default();

    let mut group = c.benchmark_group("preview_render");
    group.bench_function("claude_rich_no_highlight", |b| {
        b.iter(|| render_session_text(black_box(&CLAUDE_RICH_SESSION), black_box(&theme), None))
    });
    group.bench_function("claude_rich_with_highlight", |b| {
        b.iter(|| {
            render_session_text(
                black_box(&CLAUDE_RICH_SESSION),
                black_box(&theme),
                Some(black_box("token rotation auth tests")),
            )
        })
    });
    group.bench_function("codex_latest_no_highlight", |b| {
        b.iter(|| render_session_text(black_box(&CODEX_LATEST_SESSION), black_box(&theme), None))
    });
    group.bench_function("codex_latest_with_highlight", |b| {
        b.iter(|| {
            render_session_text(
                black_box(&CODEX_LATEST_SESSION),
                black_box(&theme),
                Some(black_box("health cargo check status")),
            )
        })
    });
    group.finish();
}

criterion_group!(benches, bench_markdown_render, bench_preview_render);
criterion_main!(benches);
