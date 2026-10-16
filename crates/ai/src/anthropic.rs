use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{json, Value};

use crate::openai::{model_ids, truncate};
use crate::sse::SseParser;
use crate::types::{Message, Provider, Request, Role, ToolCall};

pub const ANTHROPIC_BASE_URL: &str = "https://api.anthropic.com/v1";
const VERSION: &str = "2023-06-01";
const MAX_TOKENS: u32 = 64_000;
/// Key under [`Message::extra`] for the reply's raw content blocks, sent back
/// unchanged so thinking blocks stay valid across tool calls.
const EXTRA: &str = "anthropic";

/// Anthropic's Messages API (`/v1/messages`), with prompt caching.
pub struct Anthropic {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
    model: String,
}

impl Anthropic {
    pub fn new(base_url: &str, api_key: &str, model: &str) -> Self {
        Self {
            http: reqwest::Client::builder()
                .connect_timeout(std::time::Duration::from_secs(30))
                .build()
                .expect("http client"),
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key: api_key.to_string(),
            model: model.to_string(),
        }
    }

    /// Ids of the models the key can use, sorted.
    pub async fn models(&self) -> Result<Vec<String>> {
        let url = format!("{}/models?limit=1000", self.base_url);
        let send = || self.http.get(&url).header("x-api-key", &self.api_key).header("anthropic-version", VERSION);
        let response = crate::retry::send(send, &self.base_url).await?;
        model_ids(response).await
    }

    fn body(&self, request: &Request<'_>) -> Value {
        let mut body = json!({
            "model": self.model,
            "max_tokens": MAX_TOKENS,
            "stream": true,
            // Caches the prompt up to the newest message, so each tool step
            // of a turn rereads the system prompt and history from cache.
            "cache_control": { "type": "ephemeral" },
            "system": request.system,
            "messages": wire_messages(request.messages),
        });
        if !request.tools.is_empty() {
            body["tools"] = request
                .tools
                .iter()
                .map(|t| json!({ "name": t.name, "description": t.description, "input_schema": t.parameters }))
                .collect();
        }
        body
    }
}

/// Converts the transcript to alternating user/assistant messages: tool
/// results become `tool_result` blocks in a user message, and neighbours
/// with the same role are merged.
fn wire_messages(messages: &[Message]) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    for message in messages {
        let (role, blocks) = match message.role {
            Role::User => ("user", text_block(&message.content)),
            Role::Tool => (
                "user",
                vec![json!({
                    "type": "tool_result",
                    "tool_use_id": message.tool_call_id,
                    "content": message.content,
                })],
            ),
            Role::Assistant => ("assistant", assistant_blocks(message)),
        };
        if blocks.is_empty() {
            continue;
        }
        match out.last_mut() {
            Some(last) if last["role"] == role => {
                last["content"].as_array_mut().expect("content is an array").extend(blocks)
            }
            _ => out.push(json!({ "role": role, "content": blocks })),
        }
    }
    out
}

fn text_block(text: &str) -> Vec<Value> {
    if text.is_empty() {
        return Vec::new();
    }
    vec![json!({ "type": "text", "text": text })]
}

/// The reply's own blocks when it came from Anthropic, otherwise text and
/// `tool_use` blocks rebuilt from the message.
fn assistant_blocks(message: &Message) -> Vec<Value> {
    if let Some(blocks) = message.extra.get(EXTRA).and_then(Value::as_array) {
        return blocks.clone();
    }
    let mut blocks = text_block(&message.content);
    blocks.extend(message.tool_calls.iter().map(|call| {
        let input: Value = serde_json::from_str(&call.arguments).unwrap_or_else(|_| json!({}));
        json!({ "type": "tool_use", "id": call.id, "name": call.name, "input": input })
    }));
    blocks
}

#[async_trait]
impl Provider for Anthropic {
    async fn complete(
        &self,
        request: Request<'_>,
        on_text: &mut (dyn for<'s> FnMut(&'s str) + Send),
    ) -> Result<Message> {
        let (url, body) = (format!("{}/messages", self.base_url), self.body(&request));
        let send =
            || self.http.post(&url).header("x-api-key", &self.api_key).header("anthropic-version", VERSION).json(&body);
        let response = crate::retry::send(send, &self.base_url).await?;
        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            bail!("Anthropic returned {status}: {}", truncate(&text, 1000));
        }

        let mut parser = SseParser::default();
        let mut reply = Accumulator::default();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            for payload in parser.push(&chunk.context("model stream interrupted")?) {
                let event: Value = serde_json::from_str(&payload)
                    .with_context(|| format!("bad stream event: {}", truncate(&payload, 200)))?;
                if let Some(text) = reply.apply(&event)? {
                    on_text(&text);
                }
            }
        }
        reply.finish()
    }
}

/// Builds the assistant message out of streamed Messages API events.
#[derive(Default)]
struct Accumulator {
    /// Content blocks as the API sends them back, by index.
    blocks: Vec<Value>,
    /// Streamed JSON input of `tool_use` blocks, by index.
    inputs: Vec<String>,
    stop_reason: Option<String>,
}

impl Accumulator {
    /// Applies one event; returns the text delta it carried, if any.
    fn apply(&mut self, event: &Value) -> Result<Option<String>> {
        let index = event["index"].as_u64().unwrap_or(0) as usize;
        match event["type"].as_str().unwrap_or_default() {
            "error" => {
                bail!("model error: {}", event.pointer("/error/message").and_then(Value::as_str).unwrap_or("unknown"))
            }
            "content_block_start" => {
                while self.blocks.len() <= index {
                    self.blocks.push(Value::Null);
                    self.inputs.push(String::new());
                }
                self.blocks[index] = event["content_block"].clone();
            }
            "content_block_delta" => {
                let Some(block) = self.blocks.get_mut(index) else { return Ok(None) };
                let delta = &event["delta"];
                let append = |block: &mut Value, field: &str, text: &str| {
                    let current = block[field].as_str().unwrap_or_default();
                    block[field] = json!(format!("{current}{text}"));
                };
                match delta["type"].as_str().unwrap_or_default() {
                    "text_delta" => {
                        let text = delta["text"].as_str().unwrap_or_default();
                        append(block, "text", text);
                        return Ok(Some(text.to_string()).filter(|t| !t.is_empty()));
                    }
                    "input_json_delta" => {
                        self.inputs[index].push_str(delta["partial_json"].as_str().unwrap_or_default())
                    }
                    "thinking_delta" => append(block, "thinking", delta["thinking"].as_str().unwrap_or_default()),
                    "signature_delta" => append(block, "signature", delta["signature"].as_str().unwrap_or_default()),
                    _ => {}
                }
            }
            "content_block_stop" => {
                if let Some(block) = self.blocks.get_mut(index) {
                    if block["type"] == "tool_use" && !self.inputs[index].trim().is_empty() {
                        block["input"] = serde_json::from_str(&self.inputs[index]).unwrap_or_else(|_| json!({}));
                    }
                }
            }
            "message_delta" => {
                if let Some(reason) = event.pointer("/delta/stop_reason").and_then(Value::as_str) {
                    self.stop_reason = Some(reason.to_string());
                }
            }
            _ => {}
        }
        Ok(None)
    }

    fn finish(self) -> Result<Message> {
        if self.stop_reason.as_deref() == Some("refusal") {
            bail!("the model declined to answer this request");
        }
        let mut message = Message::assistant("");
        for (block, input) in self.blocks.iter().zip(&self.inputs) {
            match block["type"].as_str() {
                Some("text") => message.content.push_str(block["text"].as_str().unwrap_or_default()),
                Some("tool_use") => message.tool_calls.push(ToolCall {
                    id: block["id"].as_str().unwrap_or_default().to_string(),
                    name: block["name"].as_str().unwrap_or_default().to_string(),
                    // The raw stream, so invalid JSON is reported to the model.
                    arguments: if input.trim().is_empty() { "{}".into() } else { input.clone() },
                    ..Default::default()
                }),
                _ => {}
            }
        }
        let blocks: Vec<Value> = self.blocks.into_iter().filter(|b| !b.is_null()).collect();
        message.extra = json!({ EXTRA: blocks });
        Ok(message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ToolSpec;

    fn stream(events: &[Value]) -> Accumulator {
        let mut acc = Accumulator::default();
        for event in events {
            acc.apply(event).unwrap();
        }
        acc
    }

    #[test]
    fn accumulates_thinking_text_and_tool_use_and_replays_them_unchanged() {
        let acc = stream(&[
            json!({"type":"message_start","message":{}}),
            json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"","signature":""}}),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"sig"}}),
            json!({"type":"content_block_stop","index":0}),
            json!({"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}),
            json!({"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"Let me look"}}),
            json!({"type":"content_block_stop","index":1}),
            json!({"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"t1","name":"read_note","input":{}}}),
            json!({"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"{\"path\":"}}),
            json!({"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"\"a.md\"}"}}),
            json!({"type":"content_block_stop","index":2}),
            json!({"type":"message_delta","delta":{"stop_reason":"tool_use"}}),
        ]);
        let reply = acc.finish().unwrap();
        assert_eq!(reply.content, "Let me look");
        assert_eq!(reply.tool_calls[0].name, "read_note");
        assert_eq!(reply.tool_calls[0].arguments, r#"{"path":"a.md"}"#);

        let transcript = [Message::user("hi"), reply, Message::tool_result("t1", "contents"), Message::user("thanks")];
        let wire = wire_messages(&transcript);
        assert_eq!(wire.len(), 3, "the tool result and the next user message share one user turn");
        assert_eq!(wire[1]["content"][0], json!({"type":"thinking","thinking":"","signature":"sig"}));
        assert_eq!(wire[1]["content"][2]["input"], json!({"path":"a.md"}));
        assert_eq!(wire[2]["content"][0]["type"], "tool_result");
        assert_eq!(wire[2]["content"][1]["text"], "thanks");
    }

    #[test]
    fn rebuilds_messages_from_other_providers_and_skips_empty_ones() {
        let mut other = Message::assistant("");
        other.tool_calls = vec![ToolCall {
            id: "c0".into(),
            name: "list_notes".into(),
            arguments: "{bad".into(),
            ..Default::default()
        }];
        let wire = wire_messages(&[Message::user("hi"), Message::assistant(""), other]);
        assert_eq!(wire.len(), 2);
        assert_eq!(wire[1]["content"][0], json!({"type":"tool_use","id":"c0","name":"list_notes","input":{}}));
    }

    #[test]
    fn requests_caching_and_reports_refusals_and_errors() {
        let tools = [ToolSpec { name: "t".into(), description: "d".into(), parameters: json!({"type":"object"}) }];
        let provider = Anthropic::new(ANTHROPIC_BASE_URL, "k", "claude-sonnet-5-5");
        let body = provider.body(&Request { system: "sys", messages: &[Message::user("hi")], tools: &tools });
        assert_eq!(body["cache_control"]["type"], "ephemeral");
        assert_eq!(body["tools"][0]["input_schema"]["type"], "object");

        let refused = stream(&[json!({"type":"message_delta","delta":{"stop_reason":"refusal"}})]);
        assert!(refused.finish().unwrap_err().to_string().contains("declined"));
        let mut acc = Accumulator::default();
        let err = acc.apply(&json!({"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}));
        assert!(err.unwrap_err().to_string().contains("Overloaded"));
    }
}
