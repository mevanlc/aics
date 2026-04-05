use serde_json::Value;

/// Map a raw tool name to a short human-readable label.
pub fn tool_label(raw_name: &str) -> &str {
    match raw_name
        .trim()
        .to_ascii_lowercase()
        .replace('-', "_")
        .as_str()
    {
        "bash" | "exec_command" | "shell" | "shell_command" | "execute" | "terminal" => "bash",
        "read" | "readfile" | "read_file" => "read",
        "write" | "writefile" | "write_file" => "write",
        "edit" | "editfile" | "edit_file" => "edit",
        "glob" => "glob",
        "grep" => "grep",
        "apply_patch" | "patch" => "patch",
        "agent" => "agent",
        "web_search" | "websearch" => "search",
        "web_fetch" | "webfetch" => "fetch",
        // The match borrows a temporary — return raw_name for unknown tools
        _ => raw_name.trim(),
    }
}

/// Format tool call inputs as human-readable text.
pub fn format_tool_call(name: &str, input: &Value) -> String {
    let label = tool_label(name);
    let formatted = match label {
        "bash" => format_bash_call(input),
        "read" => format_file_path_call(input),
        "write" | "edit" => format_file_path_call(input),
        "glob" => format_glob_call(input),
        "grep" => format_grep_call(input),
        "patch" => format_patch_call(input),
        _ => None,
    };

    formatted.unwrap_or_else(|| format_generic_call(input))
}

/// Format tool result output as human-readable text.
pub fn format_tool_result(content: &Value) -> String {
    match content {
        Value::String(text) => text.trim().to_owned(),
        Value::Array(items) => {
            let mut chunks = Vec::new();
            for item in items {
                if let Some(text) = item.get("text").and_then(Value::as_str) {
                    let trimmed = text.trim();
                    if !trimmed.is_empty() {
                        chunks.push(trimmed.to_owned());
                    }
                }
            }
            if chunks.is_empty() {
                compact_json(content)
            } else {
                chunks.join("\n\n")
            }
        }
        Value::Object(map) => {
            // Prefer stdout > output > text fields
            for key in &["stdout", "output", "text", "content"] {
                if let Some(Value::String(text)) = map.get(*key) {
                    let trimmed = text.trim();
                    if !trimmed.is_empty() {
                        return trimmed.to_owned();
                    }
                }
            }
            compact_json(content)
        }
        Value::Null => String::new(),
        _ => compact_json(content),
    }
}

fn format_bash_call(input: &Value) -> Option<String> {
    // Try "command" then "cmd" fields
    for key in &["command", "cmd"] {
        if let Some(Value::String(cmd)) = input.get(*key) {
            let trimmed = cmd.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_owned());
            }
        }
    }
    // Codex exec_command uses a JSON string for arguments
    if let Value::String(text) = input {
        let trimmed = text.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_owned());
        }
    }
    None
}

fn format_file_path_call(input: &Value) -> Option<String> {
    input
        .get("file_path")
        .or_else(|| input.get("path"))
        .and_then(Value::as_str)
        .map(|path| path.trim().to_owned())
        .filter(|path| !path.is_empty())
}

fn format_glob_call(input: &Value) -> Option<String> {
    let pattern = input.get("pattern").and_then(Value::as_str)?;
    let trimmed = pattern.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(path) = input.get("path").and_then(Value::as_str) {
        let path = path.trim();
        if !path.is_empty() {
            return Some(format!("{pattern} in {path}"));
        }
    }
    Some(trimmed.to_owned())
}

fn format_grep_call(input: &Value) -> Option<String> {
    let pattern = input.get("pattern").and_then(Value::as_str)?;
    let trimmed = pattern.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(path) = input.get("path").and_then(Value::as_str) {
        let path = path.trim();
        if !path.is_empty() {
            return Some(format!("{pattern} in {path}"));
        }
    }
    Some(trimmed.to_owned())
}

fn format_patch_call(input: &Value) -> Option<String> {
    let text = match input {
        Value::String(s) => s.as_str(),
        _ => input.get("input").and_then(Value::as_str)?,
    };

    // Find first line that looks like a file path from the patch
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("*** Update File:") || trimmed.starts_with("*** Add File:") {
            return Some(trimmed.to_owned());
        }
    }

    Some("(patch)".to_owned())
}

fn format_generic_call(input: &Value) -> String {
    if let Value::Object(map) = input {
        let mut parts = Vec::new();
        for (key, value) in map {
            match value {
                Value::String(s) => {
                    let truncated = truncate(s.trim(), 100);
                    if !truncated.is_empty() {
                        parts.push(format!("{key}: {truncated}"));
                    }
                }
                Value::Number(n) => parts.push(format!("{key}: {n}")),
                Value::Bool(b) => parts.push(format!("{key}: {b}")),
                _ => {} // Skip nested objects/arrays
            }
            if parts.len() >= 4 {
                break;
            }
        }
        if !parts.is_empty() {
            return parts.join(", ");
        }
    }

    compact_json(input)
}

fn compact_json(value: &Value) -> String {
    let json = serde_json::to_string(value).unwrap_or_default();
    truncate(&json, 200).to_owned()
}

fn truncate(text: &str, max_len: usize) -> &str {
    if text.len() <= max_len {
        text
    } else {
        // Find a char boundary
        let mut end = max_len;
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        &text[..end]
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn tool_label_normalizes_common_names() {
        assert_eq!(tool_label("Bash"), "bash");
        assert_eq!(tool_label("exec_command"), "bash");
        assert_eq!(tool_label("Read"), "read");
        assert_eq!(tool_label("Glob"), "glob");
        assert_eq!(tool_label("apply_patch"), "patch");
        assert_eq!(tool_label("UnknownTool"), "UnknownTool");
    }

    #[test]
    fn format_bash_extracts_command() {
        let input = json!({"command": "ls -la", "description": "list files"});
        assert_eq!(format_tool_call("Bash", &input), "ls -la");
    }

    #[test]
    fn format_bash_extracts_cmd_field() {
        let input = json!({"cmd": "cargo check 2>&1", "workdir": "/tmp"});
        assert_eq!(format_tool_call("exec_command", &input), "cargo check 2>&1");
    }

    #[test]
    fn format_read_extracts_file_path() {
        let input = json!({"file_path": "/src/main.rs"});
        assert_eq!(format_tool_call("Read", &input), "/src/main.rs");
    }

    #[test]
    fn format_glob_with_path() {
        let input = json!({"pattern": "**/*.rs", "path": "/src"});
        assert_eq!(format_tool_call("Glob", &input), "**/*.rs in /src");
    }

    #[test]
    fn format_grep_pattern_only() {
        let input = json!({"pattern": "TODO"});
        assert_eq!(format_tool_call("Grep", &input), "TODO");
    }

    #[test]
    fn format_generic_shows_key_values() {
        let input = json!({"url": "https://example.com", "method": "GET"});
        let result = format_tool_call("WebFetch", &input);
        assert!(result.contains("url: https://example.com"));
    }

    #[test]
    fn format_tool_result_extracts_string() {
        let content = json!("file contents here");
        assert_eq!(format_tool_result(&content), "file contents here");
    }

    #[test]
    fn format_tool_result_extracts_stdout() {
        let content = json!({"stdout": "hello world", "stderr": ""});
        assert_eq!(format_tool_result(&content), "hello world");
    }

    #[test]
    fn format_tool_result_extracts_text_blocks() {
        let content = json!([
            {"type": "text", "text": "first"},
            {"type": "text", "text": "second"}
        ]);
        assert_eq!(format_tool_result(&content), "first\n\nsecond");
    }
}
