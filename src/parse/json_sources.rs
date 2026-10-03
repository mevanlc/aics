//! Lexical JSON coordinates shared by source search and readable rendering.

use std::{collections::BTreeMap, ops::Range};

use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TextOrigin {
    pub rendered: Range<usize>,
    pub source: Range<usize>,
}

#[derive(Clone)]
pub(crate) struct JsonToken {
    pub path: String,
    pub key: bool,
    pub range: Range<usize>,
    pub string: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct JsonLeaf {
    pub path: String,
    pub text: String,
    pub origins: Vec<TextOrigin>,
}

/// Decode primitive leaves while retaining the lexical bytes that produced
/// each decoded run. JSON punctuation and object keys are not readable leaves.
pub(crate) fn decoded_json_leaves(text: &str) -> Option<Vec<JsonLeaf>> {
    let parsed: Value = serde_json::from_str(text).ok()?;
    let mut leaves = Vec::new();
    for token in json_tokens(text).into_iter().filter(|token| !token.key) {
        let value = parsed.pointer(&token.path)?;
        let (decoded, origins) = match value {
            Value::String(decoded) => (decoded.clone(), json_string_units(text, token.range)),
            Value::Number(_) | Value::Bool(_) => {
                let decoded = value.to_string();
                let len = decoded.len();
                (
                    decoded,
                    vec![TextOrigin {
                        rendered: 0..len,
                        source: token.range,
                    }],
                )
            }
            _ => continue,
        };
        let mut compacted: Vec<TextOrigin> = Vec::new();
        for origin in origins {
            if let Some(last) = compacted.last_mut() {
                if last.source.end == origin.source.start
                    && last.rendered.end == origin.rendered.start
                    && last.source.len() == last.rendered.len()
                    && origin.source.len() == origin.rendered.len()
                {
                    last.source.end = origin.source.end;
                    last.rendered.end = origin.rendered.end;
                    continue;
                }
            }
            compacted.push(origin);
        }
        leaves.push(JsonLeaf {
            path: token.path,
            text: decoded,
            origins: compacted,
        });
    }
    Some(leaves)
}

pub(crate) fn json_string_units(text: &str, range: Range<usize>) -> Vec<TextOrigin> {
    let mut cursor = range.start + 1;
    let mut decoded_offset = 0;
    let mut units = Vec::new();
    while cursor + 1 < range.end {
        let start = cursor;
        let ch = text[cursor..].chars().next().unwrap();
        let decoded = if ch == '\\' {
            let escape = text.as_bytes().get(cursor + 1).copied().unwrap_or_default();
            let mut length = if escape == b'u' { 6 } else { 2 };
            if escape == b'u' {
                if let Some(hex) = text
                    .get(cursor + 2..cursor + 6)
                    .and_then(|hex| u16::from_str_radix(hex, 16).ok())
                {
                    if (0xd800..=0xdbff).contains(&hex) {
                        length = 12;
                    }
                }
            }
            let Some(raw) = text.get(cursor..cursor + length) else {
                break;
            };
            let wrapped = format!("\"{raw}\"");
            let Ok(decoded) = serde_json::from_str::<String>(&wrapped) else {
                break;
            };
            cursor += length;
            decoded
        } else {
            cursor += ch.len_utf8();
            ch.to_string()
        };
        units.push(TextOrigin {
            rendered: decoded_offset..decoded_offset + decoded.len(),
            source: start..cursor,
        });
        decoded_offset += decoded.len();
    }
    units
}

pub(crate) fn json_tokens(text: &str) -> Vec<JsonToken> {
    let Ok(parsed) = serde_json::from_str::<Value>(text) else {
        return Vec::new();
    };
    fn skip_space(text: &str, cursor: &mut usize) {
        while text
            .as_bytes()
            .get(*cursor)
            .is_some_and(u8::is_ascii_whitespace)
        {
            *cursor += 1;
        }
    }
    fn string_end(text: &str, cursor: &mut usize) {
        *cursor += 1;
        while let Some(byte) = text.as_bytes().get(*cursor) {
            *cursor += 1;
            match byte {
                b'\\' => *cursor += 1,
                b'"' => break,
                _ => {}
            }
        }
    }
    fn value(text: &str, cursor: &mut usize, path: String, tokens: &mut Vec<JsonToken>) {
        skip_space(text, cursor);
        let start = *cursor;
        match text.as_bytes().get(*cursor).copied() {
            Some(b'{') => {
                *cursor += 1;
                loop {
                    skip_space(text, cursor);
                    if text.as_bytes().get(*cursor) == Some(&b'}') {
                        *cursor += 1;
                        break;
                    }
                    if text.as_bytes().get(*cursor) != Some(&b'"') {
                        break;
                    }
                    let key_start = *cursor;
                    string_end(text, cursor);
                    let key: String =
                        serde_json::from_str(&text[key_start..*cursor]).unwrap_or_default();
                    let key_path = format!("{path}/{}", key.replace('~', "~0").replace('/', "~1"));
                    tokens.push(JsonToken {
                        path: key_path.clone(),
                        key: true,
                        range: key_start..*cursor,
                        string: true,
                    });
                    skip_space(text, cursor);
                    *cursor += 1; // colon (input has already been validated)
                    value(text, cursor, key_path, tokens);
                    skip_space(text, cursor);
                    if text.as_bytes().get(*cursor) == Some(&b',') {
                        *cursor += 1;
                    } else {
                        if text.as_bytes().get(*cursor) == Some(&b'}') {
                            *cursor += 1;
                        }
                        break;
                    }
                }
            }
            Some(b'[') => {
                *cursor += 1;
                let mut index = 0;
                loop {
                    skip_space(text, cursor);
                    if text.as_bytes().get(*cursor) == Some(&b']') {
                        *cursor += 1;
                        break;
                    }
                    value(text, cursor, format!("{path}/{index}"), tokens);
                    index += 1;
                    skip_space(text, cursor);
                    if text.as_bytes().get(*cursor) == Some(&b',') {
                        *cursor += 1;
                    } else {
                        if text.as_bytes().get(*cursor) == Some(&b']') {
                            *cursor += 1;
                        }
                        break;
                    }
                }
            }
            Some(b'"') => {
                string_end(text, cursor);
                tokens.push(JsonToken {
                    path,
                    key: false,
                    range: start..*cursor,
                    string: true,
                });
            }
            Some(_) => {
                while text
                    .as_bytes()
                    .get(*cursor)
                    .is_some_and(|byte| !byte.is_ascii_whitespace() && !b",]}".contains(byte))
                {
                    *cursor += 1;
                }
                tokens.push(JsonToken {
                    path,
                    key: false,
                    range: start..*cursor,
                    string: false,
                });
            }
            None => {}
        }
    }
    let mut tokens = Vec::new();
    value(text, &mut 0, String::new(), &mut tokens);
    // Serde keeps the final value for duplicate object keys. Earlier values,
    // including descendants of an overwritten object, were not rendered.
    let last: BTreeMap<_, _> = tokens
        .iter()
        .enumerate()
        .map(|(index, token)| ((token.path.clone(), token.key), index))
        .collect();
    tokens
        .into_iter()
        .enumerate()
        .filter_map(|(index, token)| {
            if last.get(&(token.path.clone(), token.key)) != Some(&index) {
                return None;
            }
            let actual = parsed.pointer(&token.path)?;
            if !token.key
                && serde_json::from_str::<Value>(&text[token.range.clone()])
                    .ok()
                    .as_ref()
                    != Some(actual)
            {
                return None;
            }
            Some(token)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decoded_json_leaf_ranges_cover_escapes_surrogates_and_pointer_keys() {
        let raw = r#" {"a/b~c":"alpha\n\uD83D\uDE00","flags":[true,23]} "#;
        let leaves = decoded_json_leaves(raw).unwrap();
        let leaf = leaves.iter().find(|leaf| leaf.path == "/a~1b~0c").unwrap();
        assert_eq!(leaf.text, "alpha\n😀");
        for (decoded, encoded) in [(5..6, "\\n"), (6..10, "\\uD83D\\uDE00")] {
            let origin = leaf
                .origins
                .iter()
                .find(|origin| origin.rendered == decoded)
                .unwrap();
            assert_eq!(&raw[origin.source.clone()], encoded);
        }
        assert!(leaves.iter().all(|leaf| leaf.origins.iter().all(|origin| {
            origin.rendered.end <= leaf.text.len()
                && origin.source.end <= raw.len()
                && raw.is_char_boundary(origin.source.start)
                && raw.is_char_boundary(origin.source.end)
        })));
    }

    #[test]
    fn overwritten_json_object_leaves_never_acquire_final_value_coordinates() {
        let raw = r#"{"a":{"x":"old"},"a":{"y":"alpha"},"z":"alpha old","z":"alpha"}"#;
        let leaves = decoded_json_leaves(raw).unwrap();
        assert_eq!(leaves.len(), 2);
        assert!(!leaves.iter().any(|leaf| leaf.path == "/a/x"));
        let z = leaves.iter().find(|leaf| leaf.path == "/z").unwrap();
        assert_eq!(z.text, "alpha");
        assert_eq!(z.origins[0].source.start, raw.rfind("alpha").unwrap());
    }
}
