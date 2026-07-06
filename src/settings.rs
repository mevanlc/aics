use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use directories::BaseDirs;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::index::{SearchFilters, SortMode};
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
    #[serde(default)]
    pub display_options: DisplayOptions,
    #[serde(default)]
    pub default_filter: Option<DefaultFilter>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisplayOptions {
    #[serde(default)]
    pub hide_tool_calls: bool,
    #[serde(default)]
    pub hide_tool_results: bool,
    #[serde(default)]
    pub hide_agent_replies: bool,
    #[serde(default)]
    pub hide_user_messages: bool,
    #[serde(default = "default_hide_project_docs_autodump")]
    pub hide_project_docs_autodump: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DefaultFilter {
    #[serde(default)]
    pub scope: DefaultFilterScope,
    #[serde(default)]
    pub sort: SortMode,
    #[serde(default)]
    pub filters: SearchFilters,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum DefaultFilterScope {
    #[default]
    Local,
    Global,
}

#[derive(Debug, Clone, Default)]
pub struct SettingsPatch {
    theme: Option<ThemeName>,
    claude_command: Option<String>,
    claude_args: Option<String>,
    codex_command: Option<String>,
    codex_args: Option<String>,
    show_preview: Option<bool>,
    preview_width_pct: Option<u16>,
    session_separator: Option<String>,
    snippet_line_count: Option<usize>,
    summarize_command: Option<String>,
    summarize_prompt: Option<String>,
    display_options: Option<DisplayOptions>,
    default_filter: Option<Option<DefaultFilter>>,
}

impl SettingsPatch {
    pub fn settings_modal(settings: &Settings) -> Self {
        Self {
            theme: Some(settings.theme),
            claude_command: Some(settings.claude_command.clone()),
            claude_args: Some(settings.claude_args.clone()),
            codex_command: Some(settings.codex_command.clone()),
            codex_args: Some(settings.codex_args.clone()),
            session_separator: Some(settings.session_separator.clone()),
            snippet_line_count: Some(settings.snippet_line_count),
            summarize_command: Some(settings.summarize_command.clone()),
            summarize_prompt: Some(settings.summarize_prompt.clone()),
            ..Self::default()
        }
    }

    pub fn layout(show_preview: bool, preview_width_pct: u16) -> Self {
        Self {
            show_preview: Some(show_preview),
            preview_width_pct: Some(preview_width_pct),
            ..Self::default()
        }
    }

    pub fn display_options(display_options: DisplayOptions) -> Self {
        Self {
            display_options: Some(display_options),
            ..Self::default()
        }
    }

    pub fn default_filter(default_filter: DefaultFilter) -> Self {
        Self {
            default_filter: Some(Some(default_filter)),
            ..Self::default()
        }
    }

    pub fn apply_to(&self, settings: &mut Settings) {
        if let Some(value) = self.theme {
            settings.theme = value;
        }
        if let Some(value) = self.claude_command.as_ref() {
            settings.claude_command.clone_from(value);
        }
        if let Some(value) = self.claude_args.as_ref() {
            settings.claude_args.clone_from(value);
        }
        if let Some(value) = self.codex_command.as_ref() {
            settings.codex_command.clone_from(value);
        }
        if let Some(value) = self.codex_args.as_ref() {
            settings.codex_args.clone_from(value);
        }
        if let Some(value) = self.show_preview {
            settings.show_preview = value;
        }
        if let Some(value) = self.preview_width_pct {
            settings.preview_width_pct = value;
        }
        if let Some(value) = self.session_separator.as_ref() {
            settings.session_separator.clone_from(value);
        }
        if let Some(value) = self.snippet_line_count {
            settings.snippet_line_count = value;
        }
        if let Some(value) = self.summarize_command.as_ref() {
            settings.summarize_command.clone_from(value);
        }
        if let Some(value) = self.summarize_prompt.as_ref() {
            settings.summarize_prompt.clone_from(value);
        }
        if let Some(value) = self.display_options {
            settings.display_options = value;
        }
        if let Some(value) = self.default_filter.as_ref() {
            settings.default_filter.clone_from(value);
        }
    }
}

impl Default for DisplayOptions {
    fn default() -> Self {
        Self {
            hide_tool_calls: false,
            hide_tool_results: false,
            hide_agent_replies: false,
            hide_user_messages: false,
            hide_project_docs_autodump: default_hide_project_docs_autodump(),
        }
    }
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

fn default_hide_project_docs_autodump() -> bool {
    true
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
            display_options: DisplayOptions::default(),
            default_filter: None,
        }
    }
}

impl Settings {
    pub fn load() -> Result<Self> {
        let path = settings_path()?;
        Self::load_from_path(&path)
    }

    fn load_from_path(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let contents = fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let settings: Self = serde_json::from_str(&contents)
            .with_context(|| format!("failed to parse {}", path.display()))?;
        Ok(settings)
    }

    pub fn save(&self) -> Result<()> {
        let path = settings_path()?;
        Self::save_to_path(&path, self)
    }

    pub fn save_patch(patch: &SettingsPatch) -> Result<Self> {
        let path = settings_path()?;
        Self::save_patch_to_path(&path, patch)
    }

    fn save_to_path(path: &Path, settings: &Self) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        let contents = serde_json::to_string_pretty(settings)?;
        fs::write(path, contents).with_context(|| format!("failed to write {}", path.display()))?;
        Ok(())
    }

    fn save_patch_to_path(path: &Path, patch: &SettingsPatch) -> Result<Self> {
        let raw = load_raw_settings(path)?;
        let mut settings = raw
            .as_ref()
            .map(|value| {
                serde_json::from_value(value.clone())
                    .with_context(|| format!("failed to parse {}", path.display()))
            })
            .unwrap_or_else(|| Ok(Self::default()))?;
        patch.apply_to(&mut settings);

        let mut output = serde_json::to_value(&settings)?;
        preserve_unknown_settings_fields(raw.as_ref(), &mut output);
        write_settings_value(path, &output)?;
        Ok(settings)
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

fn load_raw_settings(path: &Path) -> Result<Option<Value>> {
    if !path.exists() {
        return Ok(None);
    }
    let contents =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let value: Value = serde_json::from_str(&contents)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    Ok(Some(value))
}

fn write_settings_value(path: &Path, value: &Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let contents = serde_json::to_string_pretty(value)?;
    fs::write(path, contents).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

fn preserve_unknown_settings_fields(raw: Option<&Value>, output: &mut Value) {
    let Some(Value::Object(raw_object)) = raw else {
        return;
    };
    let Value::Object(output_object) = output else {
        return;
    };

    for (key, value) in raw_object {
        if !SETTINGS_FIELD_NAMES.contains(&key.as_str()) {
            output_object.insert(key.clone(), value.clone());
        }
    }
}

const SETTINGS_FIELD_NAMES: &[&str] = &[
    "theme",
    "claude_command",
    "claude_args",
    "codex_command",
    "codex_args",
    "show_preview",
    "preview_width_pct",
    "session_separator",
    "snippet_line_count",
    "summarize_command",
    "summarize_prompt",
    "display_options",
    "default_filter",
];

fn settings_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("settings.json"))
}

/// Returns the directory that contains `settings.json`.
pub fn config_dir() -> Result<PathBuf> {
    if let Ok(val) = std::env::var("AICS_CONFIG_ROOT") {
        return Ok(PathBuf::from(val));
    }
    let base_dirs = BaseDirs::new().context("failed to locate home directory")?;
    Ok(default_config_dir(base_dirs.home_dir()))
}

fn default_config_dir(home: &Path) -> PathBuf {
    home.join(".config").join("aics")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ring_cursor::RingCursor;
    use tempfile::TempDir;

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
    fn default_config_dir_is_home_relative_dot_config() {
        let home = PathBuf::from("home").join("alice");
        assert_eq!(
            default_config_dir(&home),
            PathBuf::from("home")
                .join("alice")
                .join(".config")
                .join("aics")
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
        assert_eq!(parsed.display_options, DisplayOptions::default());
        assert!(parsed.display_options.hide_project_docs_autodump);
    }

    #[test]
    fn display_options_default_missing_project_docs_to_hidden() {
        let parsed: DisplayOptions = serde_json::from_str(r#"{"hide_tool_calls":true}"#).unwrap();

        assert!(parsed.hide_tool_calls);
        assert!(parsed.hide_project_docs_autodump);
    }

    #[test]
    fn layout_patch_preserves_unpatched_disk_settings() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("settings.json");
        let disk = Settings {
            theme: ThemeName::Sunset,
            session_separator: "---".to_owned(),
            display_options: DisplayOptions {
                hide_tool_calls: true,
                ..DisplayOptions::default()
            },
            show_preview: false,
            preview_width_pct: 60,
            ..Settings::default()
        };
        Settings::save_to_path(&path, &disk).unwrap();

        Settings::save_patch_to_path(&path, &SettingsPatch::layout(true, 33)).unwrap();

        let saved = Settings::load_from_path(&path).unwrap();
        assert_eq!(saved.theme, ThemeName::Sunset);
        assert_eq!(saved.session_separator, "---");
        assert!(saved.display_options.hide_tool_calls);
        assert!(saved.show_preview);
        assert_eq!(saved.preview_width_pct, 33);
    }

    #[test]
    fn settings_modal_patch_preserves_layout_and_display_options() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("settings.json");
        let disk = Settings {
            show_preview: false,
            preview_width_pct: 65,
            display_options: DisplayOptions {
                hide_agent_replies: true,
                ..DisplayOptions::default()
            },
            ..Settings::default()
        };
        Settings::save_to_path(&path, &disk).unwrap();

        let modal = Settings {
            theme: ThemeName::Aics,
            claude_command: "claude-dev".to_owned(),
            session_separator: "~~~".to_owned(),
            snippet_line_count: 5,
            show_preview: true,
            preview_width_pct: 25,
            display_options: DisplayOptions::default(),
            ..Settings::default()
        };
        Settings::save_patch_to_path(&path, &SettingsPatch::settings_modal(&modal)).unwrap();

        let saved = Settings::load_from_path(&path).unwrap();
        assert_eq!(saved.theme, ThemeName::Aics);
        assert_eq!(saved.claude_command, "claude-dev");
        assert_eq!(saved.session_separator, "~~~");
        assert_eq!(saved.snippet_line_count, 5);
        assert!(!saved.show_preview);
        assert_eq!(saved.preview_width_pct, 65);
        assert!(saved.display_options.hide_agent_replies);
    }

    #[test]
    fn default_filter_patch_preserves_other_settings() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("settings.json");
        let disk = Settings {
            theme: ThemeName::Sunset,
            show_preview: false,
            preview_width_pct: 65,
            ..Settings::default()
        };
        Settings::save_to_path(&path, &disk).unwrap();

        let default_filter = DefaultFilter {
            scope: DefaultFilterScope::Global,
            sort: SortMode::Relevance,
            filters: SearchFilters {
                branch: Some("main".to_owned()),
                include_sub_agents: true,
                ..SearchFilters::default()
            },
        };
        Settings::save_patch_to_path(&path, &SettingsPatch::default_filter(default_filter))
            .unwrap();

        let saved = Settings::load_from_path(&path).unwrap();
        assert_eq!(saved.theme, ThemeName::Sunset);
        assert!(!saved.show_preview);
        assert_eq!(saved.preview_width_pct, 65);
        let saved_filter = saved.default_filter.expect("default filter saved");
        assert_eq!(saved_filter.scope, DefaultFilterScope::Global);
        assert_eq!(saved_filter.sort, SortMode::Relevance);
        assert_eq!(saved_filter.filters.branch.as_deref(), Some("main"));
        assert!(saved_filter.filters.include_sub_agents);
    }

    #[test]
    fn patch_preserves_unknown_json_fields() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("settings.json");
        fs::write(&path, r#"{"theme":"sunset","future_option":{"keep":true}}"#).unwrap();

        Settings::save_patch_to_path(&path, &SettingsPatch::layout(false, 44)).unwrap();

        let raw: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(raw["theme"], "sunset");
        assert_eq!(raw["show_preview"], false);
        assert_eq!(raw["preview_width_pct"], 44);
        assert_eq!(raw["future_option"]["keep"], true);
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
