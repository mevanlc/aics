use std::fmt;

pub const WINDOWS_MAX_COMPONENT_LEN: usize = 255;

const WINDOWS_RESERVED_NAMES: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "COM¹", "COM²", "COM³", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8",
    "LPT9", "LPT¹", "LPT²", "LPT³",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WindowsFileNameIssue {
    Empty,
    InvalidCharacter(char),
    ControlCharacter,
    TrailingDotOrSpace,
    ReservedName,
    TooLong {
        actual_bytes: usize,
        max_bytes: usize,
    },
}

impl fmt::Display for WindowsFileNameIssue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("filename is empty"),
            Self::InvalidCharacter(ch) => write!(f, "filename contains invalid character `{ch}`"),
            Self::ControlCharacter => f.write_str("filename contains a control character"),
            Self::TrailingDotOrSpace => f.write_str("filename ends with a space or period"),
            Self::ReservedName => f.write_str("filename uses a reserved Windows device name"),
            Self::TooLong {
                actual_bytes,
                max_bytes,
            } => write!(
                f,
                "filename is too long: {actual_bytes} bytes exceeds {max_bytes}"
            ),
        }
    }
}

impl std::error::Error for WindowsFileNameIssue {}

pub fn validate_windows_filename_component(name: &str) -> Result<(), WindowsFileNameIssue> {
    validate_component(name, WINDOWS_MAX_COMPONENT_LEN)
}

pub fn validate_windows_stem_with_extension(
    stem: &str,
    extension: &str,
) -> Result<(), WindowsFileNameIssue> {
    let extension = extension.trim_start_matches('.');
    if extension.is_empty() {
        return validate_windows_filename_component(stem);
    }

    let actual_bytes = stem.len() + extension.len() + 1;
    if actual_bytes > WINDOWS_MAX_COMPONENT_LEN {
        return Err(WindowsFileNameIssue::TooLong {
            actual_bytes,
            max_bytes: WINDOWS_MAX_COMPONENT_LEN,
        });
    }

    validate_component(stem, WINDOWS_MAX_COMPONENT_LEN - extension.len() - 1)
}

fn validate_component(name: &str, max_bytes: usize) -> Result<(), WindowsFileNameIssue> {
    if name.is_empty() {
        return Err(WindowsFileNameIssue::Empty);
    }

    if name.len() > max_bytes {
        return Err(WindowsFileNameIssue::TooLong {
            actual_bytes: name.len(),
            max_bytes,
        });
    }

    if name.ends_with([' ', '.']) {
        return Err(WindowsFileNameIssue::TrailingDotOrSpace);
    }

    for ch in name.chars() {
        if is_control_character(ch) {
            return Err(WindowsFileNameIssue::ControlCharacter);
        }
        if is_invalid_windows_character(ch) {
            return Err(WindowsFileNameIssue::InvalidCharacter(ch));
        }
    }

    if is_reserved_windows_name(name) {
        return Err(WindowsFileNameIssue::ReservedName);
    }

    Ok(())
}

fn is_invalid_windows_character(ch: char) -> bool {
    matches!(ch, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*')
}

fn is_control_character(ch: char) -> bool {
    (ch as u32) <= 0x1f
}

fn is_reserved_windows_name(name: &str) -> bool {
    let base = name.split_once('.').map(|(base, _)| base).unwrap_or(name);
    let normalized = base.to_ascii_uppercase();
    WINDOWS_RESERVED_NAMES.contains(&normalized.as_str())
}

#[cfg(test)]
mod tests {
    use super::{
        validate_windows_filename_component, validate_windows_stem_with_extension,
        WindowsFileNameIssue,
    };

    #[test]
    fn accepts_plain_cross_platform_names() {
        assert_eq!(validate_windows_filename_component("session-2026"), Ok(()));
        assert_eq!(
            validate_windows_stem_with_extension("session-2026", "txt"),
            Ok(())
        );
    }

    #[test]
    fn rejects_windows_forbidden_characters() {
        assert_eq!(
            validate_windows_filename_component("report<2026>"),
            Err(WindowsFileNameIssue::InvalidCharacter('<'))
        );
        assert_eq!(
            validate_windows_filename_component("draft?.txt"),
            Err(WindowsFileNameIssue::InvalidCharacter('?'))
        );
    }

    #[test]
    fn rejects_control_characters() {
        assert_eq!(
            validate_windows_filename_component("hello\x00world"),
            Err(WindowsFileNameIssue::ControlCharacter)
        );
    }

    #[test]
    fn rejects_trailing_spaces_and_dots() {
        assert_eq!(
            validate_windows_filename_component("session export. "),
            Err(WindowsFileNameIssue::TrailingDotOrSpace)
        );
    }

    #[test]
    fn rejects_reserved_windows_device_names_case_insensitively() {
        assert_eq!(
            validate_windows_filename_component("CON"),
            Err(WindowsFileNameIssue::ReservedName)
        );
        assert_eq!(
            validate_windows_filename_component("nul.txt"),
            Err(WindowsFileNameIssue::ReservedName)
        );
        assert_eq!(
            validate_windows_filename_component("Com1.log"),
            Err(WindowsFileNameIssue::ReservedName)
        );
    }

    #[test]
    fn rejects_reserved_windows_device_names_with_superscripts() {
        assert_eq!(
            validate_windows_filename_component("COM¹"),
            Err(WindowsFileNameIssue::ReservedName)
        );
        assert_eq!(
            validate_windows_filename_component("lpt².txt"),
            Err(WindowsFileNameIssue::ReservedName)
        );
    }

    #[test]
    fn rejects_empty_and_too_long_names() {
        assert_eq!(
            validate_windows_filename_component(""),
            Err(WindowsFileNameIssue::Empty)
        );

        let name = "a".repeat(256);
        assert_eq!(
            validate_windows_filename_component(&name),
            Err(WindowsFileNameIssue::TooLong {
                actual_bytes: 256,
                max_bytes: 255,
            })
        );
    }

    #[test]
    fn rejects_stems_that_do_not_fit_with_extension() {
        let stem = "a".repeat(252);
        assert_eq!(
            validate_windows_stem_with_extension(&stem, "txt"),
            Err(WindowsFileNameIssue::TooLong {
                actual_bytes: 256,
                max_bytes: 255,
            })
        );
    }
}
