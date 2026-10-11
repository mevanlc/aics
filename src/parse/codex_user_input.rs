//! Project persisted question tools onto ordinary assistant/user turns. Both
//! transcript representations and search facets consume the same projection;
//! lineage continues to use the original rollout records.

use std::borrow::Cow;
use std::collections::HashMap;

use serde_json::{json, Value};

use super::codex::{extract_message_text, parse_embedded_json};

const REPLY_OPEN: &str = "<send_user_message_question_reply>";
const REPLY_CLOSE: &str = "</send_user_message_question_reply>";

#[derive(Default)]
pub(super) struct CodexUserInput {
    calls: HashMap<String, QuestionCall>,
}

struct QuestionCall {
    questions: Vec<Question>,
    asynchronous: bool,
}

struct Question {
    id: Option<String>,
    prompt: String,
    display: String,
    secret: bool,
}

impl CodexUserInput {
    pub(super) fn normalize_response_item<'a>(&mut self, payload: &'a Value) -> Cow<'a, Value> {
        match payload.get("type").and_then(Value::as_str) {
            Some("function_call") => {
                let name = payload.get("name").and_then(Value::as_str).unwrap_or("");
                let name = name.rsplit('.').next().unwrap_or(name);
                if matches!(name, "request_user_input" | "request_user_input_async") {
                    if let Some(call) = parse_call(payload, name == "request_user_input_async") {
                        let text = call
                            .questions
                            .iter()
                            .map(|question| question.display.as_str())
                            .collect::<Vec<_>>()
                            .join("\n\n");
                        if let Some(id) = payload.get("call_id").and_then(Value::as_str) {
                            self.calls.insert(id.to_owned(), call);
                        }
                        return Cow::Owned(message("assistant", text));
                    }
                }
            }
            Some("function_call_output") => {
                if let Some(call) = payload
                    .get("call_id")
                    .and_then(Value::as_str)
                    .and_then(|id| self.calls.get(id))
                {
                    let output = parse_embedded_json(payload.get("output").unwrap_or(&Value::Null));
                    if call.asynchronous && output.get("accepted") == Some(&Value::Bool(true)) {
                        // This is delivery confirmation, not a user answer.
                        return Cow::Owned(Value::Null);
                    }
                    if let Some(text) = format_answers(call, &output) {
                        return Cow::Owned(if text.is_empty() {
                            Value::Null
                        } else {
                            message("user", text)
                        });
                    }
                }
            }
            Some("message") if payload.get("role").and_then(Value::as_str) == Some("user") => {
                let content = payload.get("content");
                let has_reply = match content {
                    Some(Value::String(text)) => text.trim_start().starts_with(REPLY_OPEN),
                    Some(Value::Array(items)) => items.iter().any(|item| {
                        item.get("text")
                            .and_then(Value::as_str)
                            .is_some_and(|text| text.trim_start().starts_with(REPLY_OPEN))
                    }),
                    _ => false,
                };
                if !has_reply {
                    return Cow::Borrowed(payload);
                }
                if let Some(text) = extract_message_text(content) {
                    if let Some(reply) = self.normalize_reply(&text) {
                        let mut normalized = payload.clone();
                        normalized["content"] = json!([{"type": "input_text", "text": reply}]);
                        return Cow::Owned(normalized);
                    }
                }
            }
            _ => {}
        }
        Cow::Borrowed(payload)
    }

    pub(super) fn normalize_event_msg<'a>(&self, payload: &'a Value) -> Cow<'a, Value> {
        if payload.get("type").and_then(Value::as_str) == Some("user_message") {
            if let Some(reply) = payload
                .get("message")
                .and_then(Value::as_str)
                .and_then(|text| self.normalize_reply(text))
            {
                let mut normalized = payload.clone();
                normalized["message"] = Value::String(reply);
                return Cow::Owned(normalized);
            }
        }
        Cow::Borrowed(payload)
    }

    fn normalize_reply(&self, raw: &str) -> Option<String> {
        let mut remaining = raw.trim();
        let mut chunks = Vec::new();
        while let Some(body) = remaining.strip_prefix(REPLY_OPEN) {
            let (body, rest) = body.split_once(REPLY_CLOSE)?;
            let replies: Value = serde_json::from_str(body.trim()).ok()?;
            let replies = replies.as_array().filter(|replies| !replies.is_empty())?;
            for reply in replies {
                let question = reply.get("question")?.as_str()?;
                let answer = reply.get("answer")?.as_str()?;
                let secret = reply
                    .get("questionItemId")
                    .and_then(Value::as_str)
                    .and_then(|id| serde_json::from_str::<Value>(id).ok())
                    .and_then(|id| {
                        let call = self.calls.get(id.get(1)?.as_str()?)?;
                        let index = usize::try_from(id.get(2)?.as_u64()?).ok()?;
                        Some(call.questions.get(index)?.secret)
                    })
                    .unwrap_or(false);
                chunks.push(format_answer(question, answer, secret));
            }
            remaining = rest.trim_start();
        }
        if chunks.is_empty() {
            return None;
        }
        if !remaining.is_empty() {
            chunks.push(remaining.to_owned());
        }
        Some(chunks.join("\n\n"))
    }
}

fn parse_call(payload: &Value, asynchronous: bool) -> Option<QuestionCall> {
    let args = parse_embedded_json(payload.get("arguments")?);
    let questions = args.get("questions")?.as_array()?;
    if questions.is_empty() {
        return None;
    }
    // Fall back to the original tool for unfamiliar shapes rather than silently
    // discarding individual questions or choices.
    let questions = questions
        .iter()
        .map(|question| {
            let prompt = question
                .get("question")
                .or_else(|| question.get("title"))?
                .as_str()?
                .trim();
            if prompt.is_empty() {
                return None;
            }
            let header = question
                .get("header")
                .and_then(Value::as_str)
                .filter(|header| !header.trim().is_empty());
            let mut display = match header {
                Some(header) => format!("**Question ({header}):** {prompt}"),
                None => format!("**Question:** {prompt}"),
            };
            if let Some(options) = question.get("options").filter(|value| !value.is_null()) {
                let options = options.as_array()?;
                if !options.is_empty() {
                    display.push('\n');
                }
                for option in options {
                    let label = option.as_str().or_else(|| option.get("label")?.as_str())?;
                    display.push_str(&format!("\n- {label}"));
                    if let Some(description) = option.get("description").and_then(Value::as_str) {
                        if !description.trim().is_empty() {
                            display.push_str(&format!(": {description}"));
                        }
                    }
                }
            }
            Some(Question {
                id: question
                    .get("id")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                prompt: prompt.to_owned(),
                display,
                secret: question.get("isSecret").and_then(Value::as_bool) == Some(true),
            })
        })
        .collect::<Option<Vec<_>>>()?;
    Some(QuestionCall {
        questions,
        asynchronous,
    })
}

fn format_answers(call: &QuestionCall, output: &Value) -> Option<String> {
    let answers = output.get("answers")?.as_object()?;
    // Unknown IDs might carry real answers; keep their original output intact.
    if answers
        .keys()
        .any(|id| !call.questions.iter().any(|q| q.id.as_ref() == Some(id)))
    {
        return None;
    }
    let mut chunks = Vec::new();
    for question in &call.questions {
        let Some(answer) = question.id.as_ref().and_then(|id| answers.get(id)) else {
            continue;
        };
        let values = answer
            .get("answers")?
            .as_array()?
            .iter()
            .map(Value::as_str)
            .collect::<Option<Vec<_>>>()?;
        let values = values
            .into_iter()
            .filter(|value| !value.trim().is_empty())
            .collect::<Vec<_>>();
        if !values.is_empty() {
            chunks.push(format_answer(
                &question.prompt,
                &values.join("\n"),
                question.secret,
            ));
        }
    }
    Some(chunks.join("\n\n"))
}

fn format_answer(question: &str, answer: &str, secret: bool) -> String {
    let answer = if secret { "[hidden]" } else { answer };
    format!(
        "> {}\n\n**Answer:** {answer}",
        question.replace('\n', "\n> ")
    )
}

fn message(role: &str, text: String) -> Value {
    json!({"type": "message", "role": role, "content": [{"type": "text", "text": text}]})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wrapped(replies: Value) -> String {
        format!("{REPLY_OPEN}\n{replies}\n{REPLY_CLOSE}")
    }

    #[test]
    fn standalone_async_replies_keep_multiple_answers_and_trailing_user_text() {
        let input = CodexUserInput::default();
        let raw = format!(
            "{}\n\nAlso measure startup.",
            wrapped(json!([
                {"question": "Which repo?\nWhich runner?", "answer": "café 🦀\nA1", "questionItemId": "opaque"},
                {"question": "How many?", "answer": "Three."}
            ]))
        );
        let normalized = input.normalize_reply(&raw).unwrap();
        assert_eq!(normalized, "> Which repo?\n> Which runner?\n\n**Answer:** café 🦀\nA1\n\n> How many?\n\n**Answer:** Three.\n\nAlso measure startup.");
        let repeated = format!(
            "{}\n{}",
            wrapped(json!([{"question": "First?", "answer": "One"}])),
            wrapped(json!([{"question": "Second?", "answer": "Two"}]))
        );
        assert_eq!(
            input
                .normalize_reply(&repeated)
                .unwrap()
                .matches("**Answer:**")
                .count(),
            2
        );
    }

    #[test]
    fn malformed_and_quoted_replies_are_preserved() {
        let mut input = CodexUserInput::default();
        for raw in [
            format!("{REPLY_OPEN}not json{REPLY_CLOSE}"),
            format!("{REPLY_OPEN}[]"),
            wrapped(json!([])),
            wrapped(json!([{"question": "Missing answer?"}])),
            wrapped(json!([{"question": "Numeric answer?", "answer": 42}])),
            format!(
                "An example:\n{}",
                wrapped(json!([{"question": "Example?", "answer": "Example"}]))
            ),
            format!(
                "```\n{}\n```",
                wrapped(json!([{"question": "Example?", "answer": "Example"}]))
            ),
        ] {
            assert_eq!(input.normalize_reply(&raw), None, "{raw}");
            let payload = message("user", raw.clone());
            assert_eq!(
                input.normalize_response_item(&payload),
                Cow::Borrowed(&payload)
            );
            let payload = json!({"type": "user_message", "message": raw});
            assert_eq!(input.normalize_event_msg(&payload), Cow::Borrowed(&payload));
        }
    }

    #[test]
    fn unfamiliar_question_and_answer_shapes_fall_back_to_original_tools() {
        let mut input = CodexUserInput::default();
        for args in [
            json!({"questions": []}),
            json!({"questions": [{"title": ""}]}),
            json!({"questions": [{"title": "Prompt", "options": [123]}]}),
            json!({"questions": [{"title": "Prompt"}, {"future_prompt": "Keep this"}]}),
        ] {
            let payload = json!({"type": "function_call", "name": "request_user_input", "call_id": "bad", "arguments": args.to_string()});
            assert!(matches!(
                input.normalize_response_item(&payload),
                Cow::Borrowed(_)
            ));
        }
        let call = json!({"type": "function_call", "name": "request_user_input", "call_id": "good", "arguments": {"questions": [{"id": "known", "question": "Prompt"}]}});
        input.normalize_response_item(&call);
        for output in [
            json!({"answers": {"unknown": {"answers": ["Keep this"]}}}),
            json!({"answers": {"known": {"answers": [42]}}}),
            Value::String("request was cancelled".to_owned()),
        ] {
            let payload =
                json!({"type": "function_call_output", "call_id": "good", "output": output});
            assert!(matches!(
                input.normalize_response_item(&payload),
                Cow::Borrowed(_)
            ));
        }
    }

    #[test]
    fn missing_answers_do_not_create_user_turns() {
        let mut input = CodexUserInput::default();
        let call = json!({"type": "function_call", "name": "request_user_input", "call_id": "q", "arguments": {"questions": [{"id": "choice", "question": "Choose?"}]}});
        input.normalize_response_item(&call);
        for answers in [
            json!({}),
            json!({"choice": {"answers": []}}),
            json!({"choice": {"answers": [" "]}}),
        ] {
            let output = json!({"type": "function_call_output", "call_id": "q", "output": {"answers": answers}});
            assert_eq!(*input.normalize_response_item(&output), Value::Null);
        }
    }

    #[test]
    fn known_secret_answers_are_masked_in_both_reply_formats() {
        let mut input = CodexUserInput::default();
        let call = json!({"type": "function_call", "name": "request_user_input", "call_id": "secret", "arguments": {"questions": [{"id": "token", "question": "Token?", "isSecret": true}]}});
        input.normalize_response_item(&call);
        let output = json!({"type": "function_call_output", "call_id": "secret", "output": {"answers": {"token": {"answers": ["private-value"]}}}});
        let normalized = input.normalize_response_item(&output);
        assert_eq!(
            extract_message_text(normalized.get("content")).unwrap(),
            "> Token?\n\n**Answer:** [hidden]"
        );
        let reply = wrapped(
            json!([{"question": "Token?", "answer": "private-value", "questionItemId": "[\"request_user_input\",\"secret\",0]"}]),
        );
        assert_eq!(
            input.normalize_reply(&reply).unwrap(),
            "> Token?\n\n**Answer:** [hidden]"
        );
    }
}
