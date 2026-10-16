use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
    Tool,
}

/// One chat message. The system prompt is passed separately in [`Request`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub content: String,
    /// Tool calls requested by an assistant message.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    /// For `Role::Tool`: the call this message answers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// Data a provider needs back unchanged on later requests, keyed by
    /// provider (e.g. Anthropic's content blocks with their thinking).
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub extra: Value,
}

impl Message {
    pub fn user(content: impl Into<String>) -> Self {
        Self::new(Role::User, content.into())
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self::new(Role::Assistant, content.into())
    }

    pub fn tool_result(call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self { tool_call_id: Some(call_id.into()), ..Self::new(Role::Tool, content.into()) }
    }

    fn new(role: Role, content: String) -> Self {
        Self { role, content, tool_calls: Vec::new(), tool_call_id: None, extra: Value::Null }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    /// Raw JSON arguments as produced by the model (may be invalid JSON).
    pub arguments: String,
    /// Provider data to send back with the call unchanged (e.g. Gemini's
    /// thought signature in `extra_content`).
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub extra: Value,
}

/// A tool the model may call. `parameters` is a JSON Schema object.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

pub struct Request<'a> {
    pub system: &'a str,
    pub messages: &'a [Message],
    pub tools: &'a [ToolSpec],
}

/// A chat model. `complete` streams text deltas to `on_text` and returns the
/// full assistant message, including any tool calls.
#[async_trait]
pub trait Provider: Send + Sync {
    async fn complete(
        &self,
        request: Request<'_>,
        on_text: &mut (dyn for<'s> FnMut(&'s str) + Send),
    ) -> anyhow::Result<Message>;
}
