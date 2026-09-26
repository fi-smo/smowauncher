//! Ask Claude from the launcher ("ask …" / "? …") and run AI commands on the copied text.
//! Talks to the Anthropic Messages API over HTTPS with streaming (server-sent events), so
//! the answer appears as it's written. The API key comes from Windows Credential Manager
//! (Settings → AI) or the ANTHROPIC_API_KEY environment variable.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

pub const CREDENTIAL: &str = "Smowauncher/AnthropicApiKey";
/// Models offered in Settings (id, label). The first is the default.
pub const MODELS: [(&str, &str); 3] =
    [("claude-opus-5", "Claude Opus 5"), ("claude-sonnet-5", "Claude Sonnet 5"), ("claude-haiku-4-5", "Claude Haiku 4.5")];
pub const EFFORTS: [&str; 3] = ["low", "medium", "high"];

const SYSTEM: &str = "You are the assistant inside Smowauncher, a keyboard launcher for Windows. \
Answers appear in a small panel as plain text: be concise and direct, use short paragraphs or simple \
lists, and avoid Markdown headings and tables. Latency-sensitive; begin your visible answer immediately.";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Command {
    pub name: String,
    /// Instructions; the copied text is appended.
    pub prompt: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Config {
    pub enabled: bool,
    pub model: String,
    pub effort: String,
    pub commands: Vec<Command>,
}

impl Default for Command {
    fn default() -> Self {
        Self { name: String::new(), prompt: String::new() }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self { enabled: true, model: MODELS[0].0.into(), effort: "medium".into(), commands: default_commands() }
    }
}

pub fn default_commands() -> Vec<Command> {
    [
        ("Fix spelling and grammar", "Fix the spelling and grammar of the text below. Keep its language, meaning, tone and formatting. Reply with only the corrected text."),
        ("Summarize", "Summarize the text below in a few short bullet points, in the text's own language."),
        ("Translate to English", "Translate the text below to English. Reply with only the translation."),
        ("Explain", "Explain the text below simply and briefly."),
    ]
    .map(|(name, prompt)| Command { name: name.into(), prompt: prompt.into() })
    .to_vec()
}

/// The key from Credential Manager, else ANTHROPIC_API_KEY.
pub fn api_key() -> Option<String> {
    crate::platform::credentials::read(CREDENTIAL)
        .or_else(|| std::env::var("ANTHROPIC_API_KEY").ok())
        .map(|k| k.trim().to_owned())
        .filter(|k| !k.is_empty())
}

pub fn model_label(id: &str) -> String {
    MODELS.iter().find(|(m, _)| *m == id).map(|(_, l)| l.to_string()).unwrap_or_else(|| id.to_owned())
}

#[derive(Debug, Clone, PartialEq)]
pub enum Role {
    User,
    Assistant,
}

#[derive(Debug)]
pub enum Event {
    Text(String),
    /// Finished; the text so far is the answer.
    Done,
    /// The model (and any fallback) declined to answer.
    Refused,
    Error(String),
}

/// Builds the Messages API request body.
pub fn request_body(cfg: &Config, messages: &[(Role, String)]) -> Value {
    let msgs: Vec<Value> = messages
        .iter()
        .map(|(role, text)| json!({ "role": if *role == Role::User { "user" } else { "assistant" }, "content": text }))
        .collect();
    let mut body = json!({
        "model": cfg.model,
        "max_tokens": 16000,
        "stream": true,
        "system": SYSTEM,
        "messages": msgs,
    });
    // Effort isn't supported on Claude Haiku 4.5.
    if !cfg.model.starts_with("claude-haiku") && EFFORTS.contains(&cfg.effort.as_str()) {
        body["output_config"] = json!({ "effort": cfg.effort });
    }
    if uses_fallbacks(&cfg.model) {
        // A declined request is re-run on Anthropic's recommended fallback model.
        body["fallbacks"] = json!("default");
    }
    body
}

/// Models with safety classifiers that can decline; they get server-side fallbacks.
fn uses_fallbacks(model: &str) -> bool {
    model == "claude-opus-5"
}

/// Splits server-sent events out of `buf` (which keeps any incomplete tail) and turns them
/// into events. Returns true once the stream is finished.
pub fn parse_sse(buf: &mut String, emit: &mut impl FnMut(Event)) -> bool {
    let mut done = false;
    while let Some(end) = buf.find("\n\n") {
        let raw: String = buf.drain(..end + 2).collect();
        let data: String = raw
            .lines()
            .filter_map(|l| l.strip_prefix("data:"))
            .map(str::trim_start)
            .collect::<Vec<_>>()
            .join("\n");
        let Ok(v) = serde_json::from_str::<Value>(&data) else { continue };
        match v["type"].as_str() {
            Some("content_block_delta") if v["delta"]["type"] == "text_delta" => {
                if let Some(t) = v["delta"]["text"].as_str() {
                    emit(Event::Text(t.to_owned()));
                }
            }
            Some("message_delta") if v["delta"]["stop_reason"] == "refusal" => {
                emit(Event::Refused);
                done = true;
            }
            Some("message_stop") => {
                if !done {
                    emit(Event::Done);
                }
                done = true;
            }
            Some("error") => {
                emit(Event::Error(v["error"]["message"].as_str().unwrap_or("unknown error").to_owned()));
                done = true;
            }
            _ => {}
        }
    }
    done
}

/// A readable message from an API error response ("HTTP 401: {json}").
fn describe_error(e: &str) -> String {
    let json_part = e.find('{').map(|i| &e[i..]).unwrap_or("");
    let message = serde_json::from_str::<Value>(json_part).ok().and_then(|v| v["error"]["message"].as_str().map(str::to_owned));
    match (e.split(':').next().unwrap_or(""), message) {
        ("HTTP 401", _) => "The API key was rejected. Check it in Settings → AI.".into(),
        ("HTTP 429", _) => "Rate limited by the Anthropic API — try again in a moment.".into(),
        ("HTTP 529", _) => "The Anthropic API is overloaded — try again in a moment.".into(),
        (status, Some(m)) => format!("{status}: {m}"),
        _ => e.chars().take(300).collect(),
    }
}

/// Streams an answer on a background thread. `on_event` is called on that thread; the
/// returned flag cancels the request when set.
pub fn ask(cfg: &Config, key: &str, messages: Vec<(Role, String)>, on_event: impl Fn(Event) + Send + 'static) -> Arc<AtomicBool> {
    let cancel = Arc::new(AtomicBool::new(false));
    let body = request_body(cfg, &messages).to_string();
    let mut headers = format!("Content-Type: application/json\r\nx-api-key: {key}\r\nanthropic-version: 2023-06-01\r\n");
    if uses_fallbacks(&cfg.model) {
        headers.push_str("anthropic-beta: server-side-fallback-2026-07-01\r\n");
    }
    let stop = cancel.clone();
    std::thread::Builder::new()
        .name("ai".into())
        .spawn(move || {
            let mut buf = String::new();
            let mut finished = false;
            let mut emit = |e: Event| on_event(e);
            let result = crate::platform::http::post_stream("api.anthropic.com", "/v1/messages", &headers, body.as_bytes(), |chunk| {
                if stop.load(Ordering::Relaxed) {
                    return false;
                }
                buf.push_str(&String::from_utf8_lossy(chunk).replace("\r\n", "\n"));
                finished = parse_sse(&mut buf, &mut emit);
                !finished
            });
            if stop.load(Ordering::Relaxed) {
                return;
            }
            match result {
                Err(e) => emit(Event::Error(describe_error(&e))),
                Ok(()) if !finished => emit(Event::Done),
                Ok(()) => {}
            }
        })
        .expect("spawn ai thread");
    cancel
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn body_shape() {
        let cfg = Config::default();
        let b = request_body(&cfg, &[(Role::User, "hi".into())]);
        assert_eq!(b["model"], "claude-opus-5");
        assert_eq!(b["stream"], true);
        assert_eq!(b["fallbacks"], "default");
        assert_eq!(b["output_config"]["effort"], "medium");
        assert!(b.get("thinking").is_none());
        assert_eq!(b["messages"][0]["role"], "user");
        let haiku = Config { model: "claude-haiku-4-5".into(), ..Config::default() };
        let b = request_body(&haiku, &[(Role::User, "hi".into())]);
        assert!(b.get("output_config").is_none());
        assert!(b.get("fallbacks").is_none());
    }

    #[test]
    fn sse() {
        let mut out = Vec::new();
        let mut emit = |e: Event| out.push(format!("{e:?}"));
        let mut buf = String::from(
            "event: message_start\ndata: {\"type\":\"message_start\"}\n\n\
             event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"Hel\"}}\n\n\
             event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"lo\"}}\n\nevent: mess",
        );
        assert!(!parse_sse(&mut buf, &mut emit));
        assert_eq!(buf, "event: mess");
        buf.push_str("age_stop\ndata: {\"type\":\"message_stop\"}\n\n");
        assert!(parse_sse(&mut buf, &mut emit));
        assert_eq!(out, ["Text(\"Hel\")", "Text(\"lo\")", "Done"]);
    }

    #[test]
    fn refusal_and_errors() {
        let mut out = Vec::new();
        let mut emit = |e: Event| out.push(format!("{e:?}"));
        let mut buf = String::from("data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"refusal\"}}\n\ndata: {\"type\":\"message_stop\"}\n\n");
        assert!(parse_sse(&mut buf, &mut emit));
        assert_eq!(out, ["Refused"]);
        assert_eq!(describe_error("HTTP 401: {}"), "The API key was rejected. Check it in Settings → AI.");
        assert_eq!(
            describe_error(r#"HTTP 400: {"type":"error","error":{"type":"invalid_request_error","message":"bad"}}"#),
            "HTTP 400: bad"
        );
    }
}
