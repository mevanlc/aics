const BOOLEAN_OPERATORS: &[&str] = &["AND", "OR", "NOT"];

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
            ch if ch.is_whitespace() && !in_quotes => {
                push_term(&mut terms, &mut token, token_quoted);
                token_quoted = false;
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
        terms.push(token.to_ascii_lowercase());
    }
    token.clear();
}

fn is_boolean_operator(token: &str, quoted: bool) -> bool {
    !quoted && BOOLEAN_OPERATORS.contains(&token)
}

#[cfg(test)]
mod tests {
    use super::{extract_highlight_terms, has_explicit_boolean_operators};

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
    fn detects_explicit_boolean_operators_outside_quotes() {
        assert!(has_explicit_boolean_operators("alpha AND beta"));
        assert!(has_explicit_boolean_operators("(alpha OR beta)"));
        assert!(!has_explicit_boolean_operators("\"alpha OR beta\""));
        assert!(!has_explicit_boolean_operators("alpha and beta"));
    }
}
