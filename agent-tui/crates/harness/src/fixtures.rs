//! Transcript records for the tests of the parser and the turn reader.

pub fn record(kind: &str, at: &str, content: &str, extra: &str) -> String {
    format!(r#"{{"type":"{kind}","timestamp":"{at}"{extra},"message":{{"content":{content}}}}}"#)
}

pub fn use_of(id: &str, name: &str, input: &str, at: &str) -> String {
    record(
        "assistant",
        at,
        &format!(r#"[{{"type":"tool_use","id":"{id}","name":"{name}","input":{input}}}]"#),
        "",
    )
}

pub fn result_of(id: &str, text: &str, error: bool, at: &str, extra: &str) -> String {
    record(
        "user",
        at,
        &format!(
            r#"[{{"type":"tool_result","tool_use_id":"{id}","is_error":{error},"content":{}}}]"#,
            serde_json::to_string(text).expect("json")
        ),
        extra,
    )
}

/// A tool result of the turn `prompt_id` belongs to.
pub fn answer(prompt_id: &str, id: &str, text: &str, at: &str) -> String {
    result_of(
        id,
        text,
        false,
        at,
        &format!(r#","promptId":"{prompt_id}""#),
    )
}

/// A user record with `content` as JSON.
pub fn user(prompt_id: &str, at: &str, content: &str, extra: &str) -> String {
    record(
        "user",
        at,
        content,
        &format!(r#","promptId":"{prompt_id}"{extra}"#),
    )
}

pub fn typed(prompt_id: &str, at: &str, text: &str) -> String {
    user(
        prompt_id,
        at,
        &serde_json::to_string(text).expect("json"),
        "",
    )
}

pub fn said(at: &str, text: &str) -> String {
    record(
        "assistant",
        at,
        &format!(
            r#"[{{"type":"text","text":{}}}]"#,
            serde_json::to_string(text).expect("json")
        ),
        "",
    )
}

pub fn ended(at: &str, millis: u64) -> String {
    format!(
        r#"{{"type":"system","subtype":"turn_duration","durationMs":{millis},"timestamp":"{at}"}}"#
    )
}
