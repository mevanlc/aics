use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeName {
    Aics,
    Lazygit,
}

impl ThemeName {
    pub const ALL: [ThemeName; 2] = [ThemeName::Aics, ThemeName::Lazygit];

    pub fn label(self) -> &'static str {
        match self {
            ThemeName::Aics => "aics",
            ThemeName::Lazygit => "lazygit",
        }
    }

    pub fn next(self) -> Self {
        match self {
            ThemeName::Aics => ThemeName::Lazygit,
            ThemeName::Lazygit => ThemeName::Aics,
        }
    }

    pub fn prev(self) -> Self {
        self.next()
    }
}

impl Default for ThemeName {
    fn default() -> Self {
        ThemeName::Aics
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub theme: ThemeName,
    #[serde(default = "default_claude_command")]
    pub claude_command: String,
    #[serde(default = "default_codex_command")]
    pub codex_command: String,
}

fn default_claude_command() -> String {
    "claude".to_owned()
}

fn default_codex_command() -> String {
    "codex".to_owned()
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: ThemeName::default(),
            claude_command: default_claude_command(),
            codex_command: default_codex_command(),
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
        Self::parse_command(&self.claude_command)
    }

    pub fn codex_program_and_args(&self) -> (String, Vec<String>) {
        Self::parse_command(&self.codex_command)
    }
}

fn settings_path() -> Result<PathBuf> {
    if let Ok(val) = std::env::var("AICS_CONFIG_ROOT") {
        return Ok(PathBuf::from(val).join("settings.json"));
    }
    let project_dirs =
        ProjectDirs::from("", "", "aics").context("failed to locate config directory")?;
    Ok(project_dirs.config_dir().join("settings.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_settings_round_trip() {
        let settings = Settings::default();
        let json = serde_json::to_string(&settings).unwrap();
        let parsed: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.theme, ThemeName::Aics);
        assert_eq!(parsed.claude_command, "claude");
        assert_eq!(parsed.codex_command, "codex");
    }

    #[test]
    fn parse_command_splits_correctly() {
        let (prog, args) = Settings::parse_command("claude --profile work");
        assert_eq!(prog, "claude");
        assert_eq!(args, vec!["--profile", "work"]);

        let (prog, args) = Settings::parse_command("codex");
        assert_eq!(prog, "codex");
        assert!(args.is_empty());

        let (prog, args) = Settings::parse_command("");
        assert!(prog.is_empty());
        assert!(args.is_empty());
    }

    #[test]
    fn theme_name_cycles() {
        assert_eq!(ThemeName::Aics.next(), ThemeName::Lazygit);
        assert_eq!(ThemeName::Lazygit.next(), ThemeName::Aics);
    }

    #[test]
    fn deserializes_unknown_fields_gracefully() {
        let json = r#"{"theme": "aics", "unknown_field": 42}"#;
        let settings: Settings = serde_json::from_str(json).unwrap();
        assert_eq!(settings.theme, ThemeName::Aics);
    }
}
