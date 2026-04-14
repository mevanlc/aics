//! Simple `{{key}}` template expansion used for both the shell command and
//! the prompt text. Unknown keys cause a hard error so typos are caught
//! early instead of silently producing a broken command.

use std::collections::HashMap;
use std::fmt;

#[derive(Debug, PartialEq, Eq)]
pub enum TemplateError {
    UnknownKey(String),
    Unterminated(usize),
}

impl fmt::Display for TemplateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TemplateError::UnknownKey(key) => write!(f, "unknown placeholder `{{{{{key}}}}}`"),
            TemplateError::Unterminated(byte) => {
                write!(f, "unterminated placeholder starting at byte {byte}")
            }
        }
    }
}

impl std::error::Error for TemplateError {}

/// Expand `{{key}}` placeholders using `vars`. Unknown keys produce
/// [`TemplateError::UnknownKey`]. Values are inserted verbatim; callers
/// should quote placeholders in their template if shell-escaping is needed
/// (the built-in shell templates do this).
pub fn expand(template: &str, vars: &HashMap<&str, &str>) -> Result<String, TemplateError> {
    let mut out = String::with_capacity(template.len());
    let bytes = template.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if i + 1 < bytes.len() && bytes[i] == b'{' && bytes[i + 1] == b'{' {
            let start = i + 2;
            let mut j = start;
            let mut found = false;
            while j + 1 < bytes.len() {
                if bytes[j] == b'}' && bytes[j + 1] == b'}' {
                    found = true;
                    break;
                }
                j += 1;
            }
            if !found {
                return Err(TemplateError::Unterminated(i));
            }
            let key = std::str::from_utf8(&bytes[start..j])
                .map_err(|_| TemplateError::UnknownKey("<invalid utf-8>".to_owned()))?
                .trim();
            let Some(value) = vars.get(key) else {
                return Err(TemplateError::UnknownKey(key.to_owned()));
            };
            out.push_str(value);
            i = j + 2;
        } else {
            let ch_start = i;
            let ch_len = utf8_char_len(bytes[i]);
            out.push_str(&template[ch_start..ch_start + ch_len]);
            i += ch_len;
        }
    }
    Ok(out)
}

fn utf8_char_len(first_byte: u8) -> usize {
    match first_byte {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF7 => 4,
        _ => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars_from(pairs: &[(&'static str, &'static str)]) -> HashMap<&'static str, &'static str> {
        pairs.iter().copied().collect()
    }

    #[test]
    fn expands_known_placeholders() {
        let vars = vars_from(&[("a", "alpha"), ("b", "beta")]);
        let out = expand("x={{a}}, y={{b}}, z={{a}}", &vars).unwrap();
        assert_eq!(out, "x=alpha, y=beta, z=alpha");
    }

    #[test]
    fn passthrough_without_placeholders() {
        let vars = HashMap::new();
        let out = expand("no placeholders here", &vars).unwrap();
        assert_eq!(out, "no placeholders here");
    }

    #[test]
    fn rejects_unknown_placeholder() {
        let vars = vars_from(&[("a", "alpha")]);
        let err = expand("hi {{b}}", &vars).unwrap_err();
        assert_eq!(err, TemplateError::UnknownKey("b".to_owned()));
    }

    #[test]
    fn rejects_unterminated_placeholder() {
        let vars = vars_from(&[("a", "alpha")]);
        let err = expand("start {{a", &vars).unwrap_err();
        assert!(matches!(err, TemplateError::Unterminated(_)));
    }

    #[test]
    fn whitespace_inside_placeholder_is_trimmed() {
        let vars = vars_from(&[("a", "alpha")]);
        let out = expand("x={{ a }}", &vars).unwrap();
        assert_eq!(out, "x=alpha");
    }

    #[test]
    fn utf8_passthrough_preserved() {
        let vars = vars_from(&[("name", "Ω")]);
        let out = expand("π={{name}} €", &vars).unwrap();
        assert_eq!(out, "π=Ω €");
    }
}
