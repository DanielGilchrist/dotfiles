//! Transcript records for the tests of the parser and the activity reader.

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

/// A tool result of the activity `prompt_id` belongs to.
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

/// The record Claude Code writes when the user stops a turn, saying `text`.
pub fn stopped(prompt_id: &str, at: &str, text: &str) -> String {
    user(
        prompt_id,
        at,
        &format!(r#"[{{"type":"text","text":"{text}"}}]"#),
        r#","interruptedMessageId":"msg_01""#,
    )
}

pub fn bookkeeping(kind: &str) -> String {
    format!(r#"{{"type":"{kind}","sessionId":"s"}}"#)
}

/// Writes `lines` as a transcript in `dir`.
pub fn write(dir: &std::path::Path, lines: &[String]) -> std::path::PathBuf {
    let path = dir.join("session.jsonl");
    std::fs::write(&path, lines.join("\n") + "\n").expect("writes");
    path
}

/// A user record with no text of interest, which is what makes a transcript
/// one the user has spoken in.
pub fn user_line() -> String {
    r#"{"type":"user","message":{"role":"user","content":"hi"}}"#.to_string()
}

/// What Claude Code appends to a transcript when its session moves to the
/// background as `to`.
pub fn continued(from: &str, to: &str) -> String {
    format!(r#"{{"type":"continued-in","sessionId":"{from}","continuedInSessionId":"{to}"}}"#)
}

/// Writes `lines` as `<id>.jsonl` in `dir`, modified `secs` from now.
pub fn write_at(dir: &std::path::Path, id: &str, lines: &[String], secs: u64) {
    let path = dir.join(format!("{id}.jsonl"));
    std::fs::write(&path, lines.join("\n") + "\n").expect("writes");
    std::fs::File::open(&path)
        .expect("opens")
        .set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(secs))
        .expect("sets mtime");
}
