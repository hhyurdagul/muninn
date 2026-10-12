use anyhow::{anyhow, bail, Context, Result};
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{json, Value};

use crate::sse::SseParser;
use crate::types::{Message, Provider, Request, Role, ToolCall};

/// Any `/chat/completions` endpoint that speaks the OpenAI wire format
/// (OpenRouter, OpenAI, vLLM, Ollama, LM Studio, ...).
pub struct OpenAiCompatible {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
    model: String,
}

impl OpenAiCompatible {
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

    fn body(&self, request: &Request<'_>) -> Value {
        let mut messages = vec![json!({ "role": "system", "content": request.system })];
        messages.extend(request.messages.iter().map(wire_message));
        let mut body = json!({ "model": self.model, "messages": messages, "stream": true });
        if !request.tools.is_empty() {
            body["tools"] = request
                .tools
                .iter()
                .map(|t| {
                    json!({
                        "type": "function",
                        "function": { "name": t.name, "description": t.description, "parameters": t.parameters },
                    })
                })
                .collect();
        }
        body
    }
}

fn wire_message(message: &Message) -> Value {
    match message.role {
        Role::User => json!({ "role": "user", "content": message.content }),
        Role::Tool => json!({
            "role": "tool",
            "tool_call_id": message.tool_call_id,
            "content": message.content,
        }),
        Role::Assistant => {
            let mut m = json!({ "role": "assistant", "content": message.content });
            if !message.tool_calls.is_empty() {
                m["tool_calls"] = message
                    .tool_calls
                    .iter()
                    .map(|c| {
                        json!({
                            "id": c.id,
                            "type": "function",
                            "function": { "name": c.name, "arguments": c.arguments },
                        })
                    })
                    .collect();
            }
            m
        }
    }
}

#[async_trait]
impl Provider for OpenAiCompatible {
    async fn complete(
        &self,
        request: Request<'_>,
        on_text: &mut (dyn for<'s> FnMut(&'s str) + Send),
    ) -> Result<Message> {
        let response = self
            .http
            .post(format!("{}/chat/completions", self.base_url))
            .bearer_auth(&self.api_key)
            .header("X-Title", "Muninn")
            .json(&self.body(&request))
            .send()
            .await
            .with_context(|| format!("cannot reach {}", self.base_url))?;
        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            bail!("model endpoint returned {status}: {}", truncate(&text, 1000));
        }

        let mut parser = SseParser::default();
        let mut reply = Accumulator::default();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            for payload in parser.push(&chunk.context("model stream interrupted")?) {
                if payload == "[DONE]" {
                    return reply.finish();
                }
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

/// Builds an assistant message out of streamed `chat.completion.chunk` events.
#[derive(Default)]
struct Accumulator {
    content: String,
    calls: Vec<ToolCall>,
}

impl Accumulator {
    /// Applies one event; returns the text delta it carried, if any.
    fn apply(&mut self, event: &Value) -> Result<Option<String>> {
        if let Some(error) = event.get("error") {
            let message = error.get("message").and_then(Value::as_str).unwrap_or("unknown error");
            return Err(anyhow!("model error: {message}"));
        }
        let Some(delta) = event.pointer("/choices/0/delta") else {
            return Ok(None);
        };
        for call in delta.get("tool_calls").and_then(Value::as_array).into_iter().flatten() {
            let index = call.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
            while self.calls.len() <= index {
                self.calls.push(ToolCall {
                    id: String::new(),
                    name: String::new(),
                    arguments: String::new(),
                });
            }
            let slot = &mut self.calls[index];
            if let Some(id) = call.get("id").and_then(Value::as_str) {
                if slot.id.is_empty() {
                    slot.id = id.to_string();
                }
            }
            if let Some(name) = call.pointer("/function/name").and_then(Value::as_str) {
                if slot.name.is_empty() {
                    slot.name = name.to_string();
                }
            }
            if let Some(args) = call.pointer("/function/arguments").and_then(Value::as_str) {
                slot.arguments.push_str(args);
            }
        }
        match delta.get("content").and_then(Value::as_str) {
            Some(text) if !text.is_empty() => {
                self.content.push_str(text);
                Ok(Some(text.to_string()))
            }
            _ => Ok(None),
        }
    }

    fn finish(self) -> Result<Message> {
        let mut message = Message::assistant(self.content);
        message.tool_calls = self
            .calls
            .into_iter()
            .enumerate()
            .filter(|(_, c)| !c.name.is_empty())
            .map(|(i, mut c)| {
                if c.id.is_empty() {
                    c.id = format!("call_{i}");
                }
                c
            })
            .collect();
        Ok(message)
    }
}

fn truncate(text: &str, max: usize) -> &str {
    match text.char_indices().nth(max) {
        Some((i, _)) => &text[..i],
        None => text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accumulates_text_and_interleaved_tool_call_fragments() {
        let mut acc = Accumulator::default();
        let events = [
            json!({"choices":[{"delta":{"content":"Hel"}}]}),
            json!({"choices":[{"delta":{"content":"lo"}}]}),
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"a","function":{"name":"read","arguments":"{\"pa"}}]}}]}),
            json!({"choices":[{"delta":{"tool_calls":[{"index":1,"function":{"name":"search","arguments":"{}"}}]}}]}),
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"th\":1}"}}]}}]}),
        ];
        let texts: Vec<_> = events.iter().filter_map(|e| acc.apply(e).unwrap()).collect();
        assert_eq!(texts, ["Hel", "lo"]);
        let message = acc.finish().unwrap();
        assert_eq!(message.content, "Hello");
        assert_eq!(message.tool_calls[0].arguments, "{\"path\":1}");
        assert_eq!(message.tool_calls[1].name, "search");
        assert_eq!(message.tool_calls[1].id, "call_1", "missing ids are synthesized");
    }

    #[test]
    fn surfaces_in_stream_errors() {
        let mut acc = Accumulator::default();
        let err = acc.apply(&json!({"error":{"message":"rate limited"}})).unwrap_err();
        assert!(err.to_string().contains("rate limited"));
    }
}
