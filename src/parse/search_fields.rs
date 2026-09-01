use std::collections::BTreeSet;

use serde_json::Value;
use url::Url;

use super::session::Agent;

/// Search-only transcript facets produced while a source session is parsed.
///
/// These values are written to dedicated Tantivy fields but are deliberately
/// omitted from stored sessions and JSON/export surfaces.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionSearchFields {
    pub user: Vec<String>,
    pub agent: Vec<String>,
    pub tool_call: Vec<String>,
    pub tool_result: Vec<String>,
    pub dirs: BTreeSet<String>,
    pub files: BTreeSet<String>,
    pub paths: BTreeSet<String>,
}

impl SessionSearchFields {
    pub fn push_user(&mut self, text: impl Into<String>) {
        push_text(&mut self.user, text);
    }

    pub fn push_agent(&mut self, text: impl Into<String>) {
        push_text(&mut self.agent, text);
    }

    pub fn push_tool_call(&mut self, value: &Value) {
        push_text(&mut self.tool_call, readable_tool_text(value));
    }

    pub fn push_tool_result(&mut self, value: &Value) {
        push_text(&mut self.tool_result, readable_tool_text(value));
    }

    pub fn push_tool_call_text(&mut self, text: impl Into<String>) {
        push_text(&mut self.tool_call, text);
    }

    pub fn push_tool_result_text(&mut self, text: impl Into<String>) {
        push_text(&mut self.tool_result, text);
    }

    pub fn add_dir(&mut self, value: &str) {
        if let Some(path) = normalize_path(value) {
            self.paths.insert(path.clone());
            self.dirs.insert(path);
        }
    }

    pub fn add_file(&mut self, value: &str) {
        if let Some(path) = normalize_path(value) {
            self.paths.insert(path.clone());
            self.files.insert(path);
        }
    }

    pub fn add_path(&mut self, value: &str) {
        if let Some(path) = normalize_path(value) {
            self.paths.insert(path);
        }
    }

    /// Capture semantically path-valued properties without guessing from
    /// filesystem existence or arbitrary slash-containing strings.
    pub fn capture_paths(&mut self, source: Agent, value: &Value) {
        let mut context = Vec::new();
        capture_value(self, source, value, &mut context);
    }
}

/// Return user-authored text after removing leading source-generated context.
/// Context-only records become `None`; a real prompt following the preamble is
/// retained.
pub fn authored_user_text(text: &str) -> Option<String> {
    let mut remaining = text.trim();
    loop {
        let before = remaining;
        remaining = strip_project_docs(remaining).unwrap_or(remaining);
        remaining = strip_known_tag(remaining).unwrap_or(remaining);
        remaining = strip_codex_request_heading(remaining);
        remaining = remaining.trim_start();
        if remaining == before {
            break;
        }
    }

    let trimmed = remaining.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

fn strip_project_docs(text: &str) -> Option<&str> {
    let first_line_end = text.find('\n').unwrap_or(text.len());
    let first_line = text[..first_line_end].trim_end();
    let is_header = [
        "AGENTS.md instructions",
        "# AGENTS.md instructions",
        "CLAUDE.md instructions",
        "# CLAUDE.md instructions",
    ]
    .into_iter()
    .any(|header| first_line == header || first_line.starts_with(&format!("{header} for ")));

    if is_header {
        return strip_instructions_block(&text[first_line_end..]);
    }
    strip_instructions_block(text)
}

fn strip_instructions_block(text: &str) -> Option<&str> {
    let text = text.trim_start();
    let rest = text.strip_prefix("<INSTRUCTIONS>")?;
    let end = rest.find("</INSTRUCTIONS>")?;
    Some(&rest[end + "</INSTRUCTIONS>".len()..])
}

fn strip_known_tag(text: &str) -> Option<&str> {
    let text = text.trim_start();
    const TAGS: &[&str] = &[
        "environment_context",
        "skill",
        "user_shell_command",
        "turn_aborted",
        "subagent_notification",
        "recommended_plugins",
        "goal_context",
        "permissions instructions",
        "collaboration_mode",
    ];
    for tag in TAGS {
        let open = format!("<{tag}>");
        let close = format!("</{tag}>");
        if let Some(rest) = text.strip_prefix(&open) {
            let end = find_ignore_ascii_case(rest, &close)?;
            return Some(&rest[end + close.len()..]);
        }
    }

    if text
        .get(.."<codex_internal_context".len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("<codex_internal_context"))
    {
        let open_end = text.find('>')?;
        let rest = &text[open_end + 1..];
        let close = "</codex_internal_context>";
        let end = find_ignore_ascii_case(rest, close)?;
        return Some(&rest[end + close.len()..]);
    }

    if text.starts_with("<external_") {
        let open_end = text.find('>')?;
        let tag = &text[1..open_end];
        let close = format!("</{tag}>");
        let rest = &text[open_end + 1..];
        let end = find_ignore_ascii_case(rest, &close)?;
        return Some(&rest[end + close.len()..]);
    }
    None
}

fn strip_codex_request_heading(text: &str) -> &str {
    const HEADING: &str = "## My request for Codex:";
    text.strip_prefix(HEADING).unwrap_or(text)
}

fn find_ignore_ascii_case(haystack: &str, needle: &str) -> Option<usize> {
    haystack
        .to_ascii_lowercase()
        .find(&needle.to_ascii_lowercase())
}

fn push_text(chunks: &mut Vec<String>, text: impl Into<String>) {
    let text = text.into();
    let trimmed = text.trim();
    if trimmed.is_empty() || chunks.last().is_some_and(|last| last == trimmed) {
        return;
    }
    chunks.push(trimmed.to_owned());
}

pub(crate) fn readable_tool_text(value: &Value) -> String {
    let mut chunks = Vec::new();
    collect_readable(value, None, &mut chunks);
    chunks.join("\n")
}

fn collect_readable(value: &Value, key: Option<&str>, output: &mut Vec<String>) {
    if key.is_some_and(is_opaque_key) {
        return;
    }
    match value {
        Value::String(text) => {
            let trimmed = text.trim();
            if trimmed.is_empty() || is_embedded_media(trimmed) {
                return;
            }
            if matches!(trimmed.as_bytes().first(), Some(b'{') | Some(b'[')) {
                if let Ok(parsed) = serde_json::from_str::<Value>(trimmed) {
                    collect_readable(&parsed, None, output);
                    return;
                }
            }
            push_text(output, trimmed);
        }
        Value::Array(values) => {
            for value in values {
                collect_readable(value, None, output);
            }
        }
        Value::Object(values) => {
            if values
                .get("type")
                .and_then(Value::as_str)
                .is_some_and(|kind| {
                    let kind = canonical_key(kind);
                    kind.contains("image") || kind.contains("audio")
                })
            {
                return;
            }
            for (key, value) in values {
                collect_readable(value, Some(key), output);
            }
        }
        Value::Number(number) => push_text(output, number.to_string()),
        Value::Bool(value) => push_text(output, value.to_string()),
        Value::Null => {}
    }
}

fn is_opaque_key(key: &str) -> bool {
    matches!(
        canonical_key(key).as_str(),
        "meta"
            | "metadata"
            | "callid"
            | "signature"
            | "thinkingsignature"
            | "encryptedcontent"
            | "base64"
            | "imageurl"
            | "audiourl"
            | "mimetype"
    )
}

fn is_embedded_media(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.starts_with("data:image/") || lower.starts_with("data:audio/")
}

fn capture_value(
    fields: &mut SessionSearchFields,
    source: Agent,
    value: &Value,
    context: &mut Vec<String>,
) {
    match value {
        Value::Object(object) => {
            for (key, child) in object {
                let canonical = canonical_key(key);
                if is_path_keyed_map(source, &canonical) {
                    if let Value::Object(entries) = child {
                        for path in entries.keys() {
                            fields.add_file(path);
                        }
                    }
                }

                context.push(canonical.clone());
                classify_property(fields, source, &canonical, child, object, context);
                capture_value(fields, source, child, context);
                context.pop();
            }
        }
        Value::Array(values) => {
            context.push("[]".to_owned());
            for value in values {
                capture_value(fields, source, value, context);
            }
            context.pop();
        }
        Value::String(text)
            if matches!(text.trim().as_bytes().first(), Some(b'{') | Some(b'[')) =>
        {
            if let Ok(embedded) = serde_json::from_str::<Value>(text) {
                capture_value(fields, source, &embedded, context);
            }
        }
        _ => {}
    }
}

fn classify_property(
    fields: &mut SessionSearchFields,
    source: Agent,
    key: &str,
    value: &Value,
    parent: &serde_json::Map<String, Value>,
    context: &[String],
) {
    if is_directory_key(source, key, context) {
        for value in string_values(value) {
            fields.add_dir(value);
        }
        return;
    }
    if is_file_key(key) {
        for value in string_values(value) {
            fields.add_file(value);
        }
        return;
    }
    if matches!(
        key,
        "absolutepath" | "fullpath" | "originalpath" | "outputpath" | "searchpath"
    ) {
        for value in string_values(value) {
            fields.add_path(value);
        }
        return;
    }
    if key == "uri" {
        for value in string_values(value) {
            if value.trim_start().starts_with("file:") {
                fields.add_path(value);
            }
        }
        return;
    }
    if key == "paths" {
        for value in string_values(value) {
            fields.add_path(value);
        }
        return;
    }
    if key != "path" {
        return;
    }

    let kind = parent
        .get("type")
        .and_then(Value::as_str)
        .map(canonical_key)
        .unwrap_or_default();
    let in_context = |needle: &str| context.iter().any(|part| part == needle);
    for value in string_values(value) {
        if kind == "directory" {
            fields.add_dir(value);
        } else if matches!(kind.as_str(), "localimage" | "skill" | "imageview")
            || in_context("memorycitation")
            || in_context("parsedcmd")
        {
            fields.add_file(value);
        } else if in_context("filesystem")
            || in_context("filesystemsandboxpolicy")
            || in_context("permissionprofile")
            || in_context("input")
            || in_context("arguments")
            || in_context("args")
        {
            fields.add_path(value);
        }
    }
}

fn is_directory_key(source: Agent, key: &str, context: &[String]) -> bool {
    matches!(
        key,
        "cwd"
            | "workdir"
            | "workingdir"
            | "originalcwd"
            | "relocatedcwd"
            | "directory"
            | "directorypath"
            | "appdatadir"
            | "artifactdirectorypath"
            | "outputdir"
            | "projectpath"
            | "searchdirectory"
            | "realparentdir"
            | "transcriptdir"
            | "trustedworkspaces"
            | "unaccountedtopleveldirs"
            | "writableroot"
            | "writableroots"
            | "workspaceroot"
            | "workspaceroots"
            | "workspacedirs"
            | "workspacepaths"
            | "workspaceuri"
            | "workspaceuris"
            | "worktreepath"
    ) || (source == Agent::Antigravity
        && key == "workspace"
        && context.iter().any(|part| part == "workspace"))
}

fn is_file_key(key: &str) -> bool {
    matches!(
        key,
        "file"
            | "files"
            | "filepath"
            | "filepaths"
            | "filename"
            | "configfilepath"
            | "difffiles"
            | "displaypath"
            | "hotfiles"
            | "targetfile"
            | "trackingpath"
            | "savepath"
            | "planfilepath"
            | "scriptpath"
            | "outpath"
            | "outputfile"
            | "persistedoutputpath"
            | "backupfilename"
            | "movepath"
            | "scopefiles"
            | "transcriptpath"
    )
}

fn is_path_keyed_map(source: Agent, key: &str) -> bool {
    matches!(
        (source, key),
        (Agent::Claude, "trackedfilebackups") | (Agent::Codex, "changes")
    )
}

fn string_values(value: &Value) -> Vec<&str> {
    match value {
        Value::String(value) => vec![value],
        Value::Array(values) => values.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    }
}

fn normalize_path(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    let value = if value.starts_with("file:") {
        Url::parse(value)
            .ok()?
            .to_file_path()
            .ok()?
            .to_string_lossy()
            .into_owned()
    } else {
        value.to_owned()
    };
    let normalized = value.replace('\\', "/");
    let trimmed = if normalized == "/"
        || (normalized.len() == 3
            && normalized.as_bytes().get(1) == Some(&b':')
            && normalized.ends_with('/'))
    {
        normalized.as_str()
    } else {
        normalized.trim_end_matches('/')
    };
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

fn canonical_key(key: &str) -> String {
    key.chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{authored_user_text, SessionSearchFields};
    use crate::parse::Agent;

    #[test]
    fn authored_user_text_strips_context_and_keeps_following_prompt() {
        let text = "<environment_context>generated</environment_context>\nPlease fix it";
        assert_eq!(authored_user_text(text).as_deref(), Some("Please fix it"));
        assert_eq!(
            authored_user_text("<skill>generated</skill>").as_deref(),
            None
        );
    }

    #[test]
    fn readable_tool_text_skips_opaque_and_media_values() {
        let mut fields = SessionSearchFields::default();
        fields.push_tool_result(&json!({
            "stdout": "visible output",
            "metadata": {"internal": "hidden metadata"},
            "signature": "hidden signature",
            "image_url": "data:image/png;base64,AAAA"
        }));
        assert_eq!(fields.tool_result, ["visible output"]);
    }

    #[test]
    fn path_capture_classifies_known_and_ambiguous_properties() {
        let mut fields = SessionSearchFields::default();
        fields.capture_paths(
            Agent::Codex,
            &json!({
                "payload": {
                    "cwd": "/repo/work",
                    "workspace_roots": ["/repo", "C:\\src"],
                    "changes": {"src/main.rs": {"type": "update"}},
                    "file_system": {"entries": [{"path": {"path": "/tmp/either"}}]}
                },
                "agent_path": "/root/task"
            }),
        );
        assert!(fields.dirs.contains("/repo/work"));
        assert!(fields.dirs.contains("C:/src"));
        assert!(fields.files.contains("src/main.rs"));
        assert!(fields.paths.contains("/tmp/either"));
        assert!(!fields.paths.contains("/root/task"));
    }

    #[test]
    fn paths_is_a_superset_and_map_keys_are_files() {
        let mut fields = SessionSearchFields::default();
        fields.capture_paths(
            Agent::Claude,
            &json!({
                "snapshot": {
                    "trackedFileBackups": {
                        "src/lib.rs": {"realParentDir": "/repo/src"}
                    }
                },
                "tool_calls": [{"args": {"SearchPath": "/repo/maybe"}}]
            }),
        );
        assert!(fields.files.is_subset(&fields.paths));
        assert!(fields.dirs.is_subset(&fields.paths));
        assert!(fields.files.contains("src/lib.rs"));
        assert!(fields.dirs.contains("/repo/src"));
        assert!(fields.paths.contains("/repo/maybe"));
        assert!(!fields.files.contains("/repo/maybe"));
        assert!(!fields.dirs.contains("/repo/maybe"));
    }

    #[test]
    fn claude_original_file_contents_are_not_classified_as_paths() {
        let mut fields = SessionSearchFields::default();
        fields.capture_paths(
            Agent::Claude,
            &json!({
                "toolUseResult": {
                    "filePath": "/repo/src/lib.rs",
                    "originalFile": "fn render() { include_str!(\"assets/icons/ui.svg\"); }"
                }
            }),
        );

        assert!(fields.files.contains("/repo/src/lib.rs"));
        assert!(!fields
            .paths
            .contains("fn render() { include_str!(\"assets/icons/ui.svg\"); }"));
    }
}
