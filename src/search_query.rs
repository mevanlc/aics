const BOOLEAN_OPERATORS: &[&str] = &["AND", "OR", "NOT"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VisibilitySearch {
    #[default]
    All,
    Visible,
    Hidden,
}

impl VisibilitySearch {
    fn from_modifier(token: &str) -> Option<Self> {
        if token.eq_ignore_ascii_case("all:") {
            Some(Self::All)
        } else if token.eq_ignore_ascii_case("visible:") {
            Some(Self::Visible)
        } else if token.eq_ignore_ascii_case("hidden:") {
            Some(Self::Hidden)
        } else {
            None
        }
    }
}

/// Remove a position-independent visibility modifier from a query.
///
/// Modifiers are recognized only as complete, unquoted tokens. Repeating the
/// same modifier is harmless, while combining different modifiers is rejected
/// because their transcript scopes are mutually exclusive.
pub fn extract_visibility_search(query: &str) -> Result<(String, VisibilitySearch), &'static str> {
    let mut output = String::with_capacity(query.len());
    let mut modifier = None;
    let mut token_start = 0usize;
    let mut quote = None;
    let mut escaped = false;

    for (index, ch) in query.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' {
            escaped = true;
            continue;
        }
        if let Some(delimiter) = quote {
            if ch == delimiter {
                quote = None;
            }
            continue;
        }
        if matches!(ch, '\'' | '"') {
            quote = Some(ch);
            continue;
        }
        if ch.is_whitespace() || matches!(ch, '(' | ')') {
            append_query_token(query, token_start, index, &mut output, &mut modifier)?;
            output.push(ch);
            token_start = index + ch.len_utf8();
        }
    }
    append_query_token(query, token_start, query.len(), &mut output, &mut modifier)?;

    Ok((output.trim().to_owned(), modifier.unwrap_or_default()))
}

fn append_query_token(
    query: &str,
    start: usize,
    end: usize,
    output: &mut String,
    modifier: &mut Option<VisibilitySearch>,
) -> Result<(), &'static str> {
    let token = &query[start..end];
    let Some(found) = VisibilitySearch::from_modifier(token) else {
        output.push_str(token);
        return Ok(());
    };

    if modifier.is_some_and(|existing| existing != found) {
        return Err("visible:, hidden:, and all: are mutually exclusive");
    }
    *modifier = Some(found);
    Ok(())
}

pub fn extract_highlight_terms(query: &str) -> Vec<String> {
    let mut terms = Vec::new();
    let mut token = String::new();
    let mut token_quoted = false;
    let mut in_quotes = false;

    for ch in query.chars() {
        match ch {
            '"' => {
                if in_quotes {
                    push_term(&mut terms, &mut token, true);
                    in_quotes = false;
                    token_quoted = false;
                } else {
                    push_term(&mut terms, &mut token, token_quoted);
                    in_quotes = true;
                    token_quoted = true;
                }
            }
            '(' | ')' if !in_quotes => {
                push_term(&mut terms, &mut token, token_quoted);
                token_quoted = false;
            }
            ch if ch.is_whitespace() => {
                push_term(&mut terms, &mut token, token_quoted);
                token_quoted = in_quotes;
            }
            _ => token.push(ch),
        }
    }

    push_term(&mut terms, &mut token, token_quoted);
    terms
}

pub fn has_explicit_boolean_operators(query: &str) -> bool {
    let mut token = String::new();
    let mut token_quoted = false;
    let mut in_quotes = false;

    for ch in query.chars() {
        match ch {
            '"' => {
                if in_quotes {
                    if is_boolean_operator(&token, true) {
                        return true;
                    }
                    token.clear();
                    in_quotes = false;
                    token_quoted = false;
                } else {
                    if is_boolean_operator(&token, token_quoted) {
                        return true;
                    }
                    token.clear();
                    in_quotes = true;
                    token_quoted = true;
                }
            }
            '(' | ')' if !in_quotes => {
                if is_boolean_operator(&token, token_quoted) {
                    return true;
                }
                token.clear();
                token_quoted = false;
            }
            ch if ch.is_whitespace() && !in_quotes => {
                if is_boolean_operator(&token, token_quoted) {
                    return true;
                }
                token.clear();
                token_quoted = false;
            }
            _ => token.push(ch),
        }
    }

    is_boolean_operator(&token, token_quoted)
}

fn push_term(terms: &mut Vec<String>, token: &mut String, quoted: bool) {
    if token.is_empty() {
        return;
    }

    if !is_boolean_operator(token, quoted) {
        let term = if !quoted && VisibilitySearch::from_modifier(token).is_some() {
            ""
        } else {
            strip_search_field(token)
        };
        if !term.is_empty() {
            terms.push(term.to_ascii_lowercase());
        }
    }
    token.clear();
}

fn strip_search_field(token: &str) -> &str {
    let Some((field, value)) = token.split_once(':') else {
        return token;
    };
    if matches!(
        field,
        "content"
            | "working_dir"
            | "wd"
            | "user"
            | "agent"
            | "toolcall"
            | "toolresult"
            | "dirs"
            | "files"
            | "paths"
    ) {
        value
    } else {
        token
    }
}

fn is_boolean_operator(token: &str, quoted: bool) -> bool {
    !quoted && BOOLEAN_OPERATORS.contains(&token)
}

#[cfg(test)]
mod tests {
    use super::{
        extract_highlight_terms, extract_visibility_search, has_explicit_boolean_operators,
        VisibilitySearch,
    };

    #[test]
    fn extracts_position_independent_visibility_modifiers() {
        for (query, expected_query, expected_mode) in [
            (
                "visible: alpha beta",
                "alpha beta",
                VisibilitySearch::Visible,
            ),
            (
                "alpha hidden: beta",
                "alpha  beta",
                VisibilitySearch::Hidden,
            ),
            ("alpha beta all:", "alpha beta", VisibilitySearch::All),
            (
                "(visible: alpha OR beta)",
                "( alpha OR beta)",
                VisibilitySearch::Visible,
            ),
        ] {
            let (query, mode) = extract_visibility_search(query).unwrap();
            assert_eq!(query, expected_query);
            assert_eq!(mode, expected_mode);
        }
    }

    #[test]
    fn leaves_quoted_and_prefixed_modifier_text_alone() {
        assert_eq!(
            extract_visibility_search(r#""visible:" content:hidden:"#).unwrap(),
            (
                r#""visible:" content:hidden:"#.to_owned(),
                VisibilitySearch::All
            )
        );
    }

    #[test]
    fn rejects_conflicting_visibility_modifiers() {
        assert!(extract_visibility_search("visible: needle hidden:").is_err());
        assert_eq!(
            extract_visibility_search("hidden: needle hidden:").unwrap(),
            ("needle".to_owned(), VisibilitySearch::Hidden)
        );
    }

    #[test]
    fn splits_bare_multi_word_queries_for_highlighting() {
        let terms = extract_highlight_terms("wordA wordB");
        assert_eq!(terms, ["worda", "wordb"]);
    }

    #[test]
    fn ignores_uppercase_boolean_operators_for_highlighting() {
        let terms = extract_highlight_terms("alpha AND beta OR gamma NOT delta");
        assert_eq!(terms, ["alpha", "beta", "gamma", "delta"]);
    }

    #[test]
    fn keeps_lowercase_words_named_like_operators() {
        let terms = extract_highlight_terms("rock and roll or bust");
        assert_eq!(terms, ["rock", "and", "roll", "or", "bust"]);
    }

    #[test]
    fn keeps_quoted_boolean_tokens_as_search_terms() {
        let terms = extract_highlight_terms("\"AND\" OR beta");
        assert_eq!(terms, ["and", "beta"]);
    }

    #[test]
    fn splits_quoted_phrases_into_independent_highlight_terms() {
        let terms = extract_highlight_terms("\"commit all\"");
        assert_eq!(terms, ["commit", "all"]);
    }

    #[test]
    fn keeps_boolean_words_inside_quoted_phrases() {
        let terms = extract_highlight_terms("\"alpha OR beta\"");
        assert_eq!(terms, ["alpha", "or", "beta"]);
    }

    #[test]
    fn detects_explicit_boolean_operators_outside_quotes() {
        assert!(has_explicit_boolean_operators("alpha AND beta"));
        assert!(has_explicit_boolean_operators("(alpha OR beta)"));
        assert!(!has_explicit_boolean_operators("\"alpha OR beta\""));
        assert!(!has_explicit_boolean_operators("alpha and beta"));
    }

    #[test]
    fn strips_search_field_prefixes_for_highlighting() {
        assert_eq!(
            extract_highlight_terms("toolcall:needle paths:src/main"),
            ["needle", "src/main"]
        );
        assert_eq!(
            extract_highlight_terms("toolresult:\"error text\""),
            ["error", "text"]
        );
        assert_eq!(
            extract_highlight_terms("visible: needle hidden:"),
            ["needle"]
        );
        assert_eq!(extract_highlight_terms(r#""visible:""#), ["visible:"]);
    }
}
