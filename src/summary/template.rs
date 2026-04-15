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
/// [`TemplateError::UnknownKey`]. Values are inserted verbatim.
///
/// Use this for non-shell contexts (e.g. prompt text). For shell commands
/// use [`expand_shell`] which applies single-quote escaping to path-like
/// values while leaving arg strings raw.
pub fn expand(template: &str, vars: &HashMap<&str, &str>) -> Result<String, TemplateError> {
    expand_with(template, |key| vars.get(key).copied().map(str::to_owned))
}

/// Expand `{{key}}` placeholders where values in `escape_vars` are
/// single-quote-escaped (safe to paste inside `'…'` in the template) and
/// values in `raw_vars` are inserted verbatim (for shell-interpreted
/// fragments like argument lists). `escape_vars` is consulted first, so a
/// key present in both maps uses the escaped form.
pub fn expand_shell(
    template: &str,
    escape_vars: &HashMap<&str, &str>,
    raw_vars: &HashMap<&str, &str>,
) -> Result<String, TemplateError> {
    expand_with(template, |key| {
        if let Some(v) = escape_vars.get(key) {
            Some(escape_sq(v))
        } else {
            raw_vars.get(key).copied().map(str::to_owned)
        }
    })
}

/// Escape a string so it is safe to paste between single quotes in a POSIX
/// shell context. Only `'` needs treatment — it is replaced with `'\''`
/// (close-sq, escaped apostrophe, reopen-sq). Every other byte (including
/// newlines, backslashes, `$`, and control chars) is preserved literally,
/// which is what sq quoting guarantees.
///
/// The caller is expected to surround the resulting substring with literal
/// single quotes in the template (e.g. `'{{path}}'`).
pub fn escape_sq(s: &str) -> String {
    if !s.contains('\'') {
        return s.to_owned();
    }
    let mut out = String::with_capacity(s.len() + 4);
    for ch in s.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out
}

fn expand_with<F>(template: &str, lookup: F) -> Result<String, TemplateError>
where
    F: Fn(&str) -> Option<String>,
{
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
            let Some(value) = lookup(key) else {
                return Err(TemplateError::UnknownKey(key.to_owned()));
            };
            out.push_str(&value);
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

    #[test]
    fn escape_sq_leaves_safe_strings_alone() {
        assert_eq!(escape_sq(""), "");
        assert_eq!(escape_sq("hello world"), "hello world");
        assert_eq!(escape_sq("/tmp/path with spaces/x.jsonl"), "/tmp/path with spaces/x.jsonl");
        assert_eq!(escape_sq("$var\\n!*\"weird\""), "$var\\n!*\"weird\"");
    }

    #[test]
    fn escape_sq_escapes_apostrophes() {
        assert_eq!(escape_sq("Don't"), "Don'\\''t");
        assert_eq!(escape_sq("'"), "'\\''");
        assert_eq!(escape_sq("a'b'c"), "a'\\''b'\\''c");
    }

    #[test]
    fn escape_sq_preserves_newlines_verbatim() {
        assert_eq!(escape_sq("line1\nline2"), "line1\nline2");
    }

    #[test]
    fn expand_shell_escapes_path_like_vars() {
        let mut esc = HashMap::new();
        esc.insert("p", "Don't Panic");
        let raw: HashMap<&str, &str> = HashMap::new();
        let out = expand_shell("cat '{{p}}' > out", &esc, &raw).unwrap();
        assert_eq!(out, "cat 'Don'\\''t Panic' > out");
    }

    #[test]
    fn expand_shell_leaves_raw_vars_verbatim() {
        let esc = HashMap::new();
        let mut raw = HashMap::new();
        raw.insert("args", "--flag --other");
        let out = expand_shell("prog {{args}}", &esc, &raw).unwrap();
        assert_eq!(out, "prog --flag --other");
    }

    #[test]
    fn expand_shell_escape_map_wins_over_raw() {
        let mut esc = HashMap::new();
        esc.insert("k", "a'b");
        let mut raw = HashMap::new();
        raw.insert("k", "a'b");
        let out = expand_shell("'{{k}}'", &esc, &raw).unwrap();
        assert_eq!(out, "'a'\\''b'");
    }

    #[test]
    fn expand_shell_rejects_unknown_key() {
        let esc = HashMap::new();
        let raw = HashMap::new();
        let err = expand_shell("hi {{missing}}", &esc, &raw).unwrap_err();
        assert_eq!(err, TemplateError::UnknownKey("missing".to_owned()));
    }
}
