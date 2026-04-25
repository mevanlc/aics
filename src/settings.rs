use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

use crate::summary::prompt::DEFAULT_PROMPT;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ThemeName {
    #[default]
    Lazygit,
    Aics,
    Sunset,
    LateSh,
}

impl ThemeName {
    pub const ALL: [ThemeName; 4] = [
        ThemeName::Lazygit,
        ThemeName::Aics,
        ThemeName::Sunset,
        ThemeName::LateSh,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ThemeName::Lazygit => "lazygit",
            ThemeName::Aics => "aics",
            ThemeName::Sunset => "sunset",
            ThemeName::LateSh => "late.sh",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub theme: ThemeName,
    #[serde(default = "default_claude_command")]
    pub claude_command: String,
    #[serde(default = "default_claude_args")]
    pub claude_args: String,
    #[serde(default = "default_codex_command")]
    pub codex_command: String,
    #[serde(default = "default_codex_args")]
    pub codex_args: String,
    #[serde(default = "default_show_preview")]
    pub show_preview: bool,
    #[serde(default = "default_preview_width_pct")]
    pub preview_width_pct: u16,
    #[serde(default = "default_session_separator")]
    pub session_separator: String,
    #[serde(default = "default_snippet_line_count")]
    pub snippet_line_count: usize,
    #[serde(default)]
    pub summarize_command: String,
    #[serde(default = "default_summarize_prompt")]
    pub summarize_prompt: String,
}

fn default_claude_command() -> String {
    "claude".to_owned()
}

fn default_claude_args() -> String {
    "--dangerously-skip-permissions".to_owned()
}

fn default_codex_command() -> String {
    "codex".to_owned()
}

fn default_codex_args() -> String {
    "--yolo".to_owned()
}

fn default_show_preview() -> bool {
    true
}

fn default_preview_width_pct() -> u16 {
    40
}

fn default_session_separator() -> String {
    " ".to_owned()
}

fn default_snippet_line_count() -> usize {
    3
}

fn default_summarize_prompt() -> String {
    DEFAULT_PROMPT.to_owned()
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: ThemeName::default(),
            claude_command: default_claude_command(),
            claude_args: default_claude_args(),
            codex_command: default_codex_command(),
            codex_args: default_codex_args(),
            show_preview: default_show_preview(),
            preview_width_pct: default_preview_width_pct(),
            session_separator: default_session_separator(),
            snippet_line_count: default_snippet_line_count(),
            summarize_command: String::new(),
            summarize_prompt: default_summarize_prompt(),
        }
    }
}

impl Settings {
    pub fn load() -> Result<Self> {
        let path = settings_path()?;
        if !path.exists() {
            return Ok(Self::default());
        }
        let contents = fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let settings: Self = serde_json::from_str(&contents)
            .with_context(|| format!("failed to parse {}", path.display()))?;
        Ok(settings)
    }

    pub fn save(&self) -> Result<()> {
        let path = settings_path()?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        let contents = serde_json::to_string_pretty(self)?;
        fs::write(&path, contents)
            .with_context(|| format!("failed to write {}", path.display()))?;
        Ok(())
    }

    /// Split a command string into program and arguments.
    pub fn parse_command(command: &str) -> (String, Vec<String>) {
        let parts: Vec<&str> = command.split_whitespace().collect();
        if parts.is_empty() {
            return (String::new(), Vec::new());
        }
        let program = parts[0].to_owned();
        let args = parts[1..].iter().map(|s| s.to_string()).collect();
        (program, args)
    }

    pub fn claude_program_and_args(&self) -> (String, Vec<String>) {
        (
            self.claude_command.clone(),
            self.claude_args
                .split_whitespace()
                .map(str::to_owned)
                .collect(),
        )
    }

    pub fn codex_program_and_args(&self) -> (String, Vec<String>) {
        (
            self.codex_command.clone(),
            self.codex_args
                .split_whitespace()
                .map(str::to_owned)
                .collect(),
        )
    }
}

fn settings_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("settings.json"))
}

/// Returns the directory that contains `settings.json`.
pub fn config_dir() -> Result<PathBuf> {
    if let Ok(val) = std::env::var("AICS_CONFIG_ROOT") {
        return Ok(PathBuf::from(val));
    }
    let project_dirs =
        ProjectDirs::from("", "", "aics").context("failed to locate config directory")?;
    Ok(project_dirs.config_dir().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ring_cursor::RingCursor;

    #[test]
    fn fresh_install_has_preview_pane_on() {
        // When a user launches aics for the first time (no config file yet),
        // `load` is expected to return `Self::default()`, so this guards the
        // contract "preview pane on by default" at that specific entry point.
        let settings = Settings::default();
        assert!(
            settings.show_preview,
            "default settings must enable the preview pane on first run"
        );
    }

    #[test]
    fn default_settings_round_trip() {
        let settings = Settings::default();
        let json = serde_json::to_string(&settings).unwrap();
        let parsed: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.theme, ThemeName::Lazygit);
        assert_eq!(parsed.claude_command, "claude");
        assert_eq!(parsed.claude_args, "--dangerously-skip-permissions");
        assert_eq!(parsed.codex_command, "codex");
        assert_eq!(parsed.codex_args, "--yolo");
        assert!(parsed.show_preview);
        assert_eq!(parsed.preview_width_pct, 40);
    }

    #[test]
    fn parse_command_splits_correctly() {
        let (prog, args) = Settings::parse_command("claude --profile work");
        assert_eq!(prog, "claude");
        assert_eq!(args, vec!["--profile", "work"]);

        let (prog, args) = Settings::parse_command("codex --yolo");
        assert_eq!(prog, "codex");
        assert_eq!(args, vec!["--yolo"]);

        let (prog, args) = Settings::parse_command("");
        assert!(prog.is_empty());
        assert!(args.is_empty());
    }

    #[test]
    fn theme_name_cycles() {
        let mut theme = RingCursor::new(ThemeName::ALL.to_vec());
        assert_eq!(*theme.current(), ThemeName::Lazygit);
        assert_eq!(*theme.move_next(), ThemeName::Aics);
        assert_eq!(*theme.move_next(), ThemeName::Sunset);
        assert_eq!(*theme.move_next(), ThemeName::LateSh);
        assert_eq!(*theme.move_next(), ThemeName::Lazygit);
        assert_eq!(*theme.move_prev(), ThemeName::LateSh);
        assert_eq!(*theme.move_prev(), ThemeName::Sunset);
    }

    #[test]
    fn deserializes_unknown_fields_gracefully() {
        let json = r#"{"theme": "aics", "unknown_field": 42}"#;
        let settings: Settings = serde_json::from_str(json).unwrap();
        assert_eq!(settings.theme, ThemeName::Aics);
    }
}
