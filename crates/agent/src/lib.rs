//! A small tool-calling agent loop: ask the model, run the tools it calls,
//! feed the results back, repeat until it answers without tools.

use std::borrow::Cow;
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use muninn_ai::{Message, Provider, Request, Role, ToolCall, ToolSpec};
use serde_json::Value;
pub use tokio_util::sync::CancellationToken;

/// A capability the model can invoke. Returning `Err` reports the error text
/// to the model, which can then recover.
#[async_trait]
pub trait Tool: Send + Sync {
    fn spec(&self) -> ToolSpec;
    async fn call(&self, args: Value) -> Result<String>;
}

/// Progress of a turn, delivered as it happens.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Text(String),
    ToolStart { name: String, args: Value },
    ToolEnd { name: String, output: String, is_error: bool },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stop {
    /// The model answered without calling tools.
    Done,
    Cancelled,
    /// The model kept calling tools past the step limit.
    StepLimit,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    /// Text of the final assistant message.
    pub reply: String,
    pub stop: Stop,
}

pub struct Agent {
    provider: Arc<dyn Provider>,
    tools: Vec<Arc<dyn Tool>>,
    max_steps: usize,
    max_context: Option<usize>,
}

impl Agent {
    pub fn new(provider: Arc<dyn Provider>, tools: Vec<Arc<dyn Tool>>) -> Self {
        Self { provider, tools, max_steps: 40, max_context: None }
    }

    pub fn max_steps(mut self, steps: usize) -> Self {
        self.max_steps = steps;
        self
    }

    /// Keeps each request, system prompt included, within about `chars`
    /// characters (see [`fit`]). The history itself is never shortened.
    pub fn max_context(mut self, chars: usize) -> Self {
        self.max_context = Some(chars);
        self
    }

    /// Runs one user turn. `history` receives the user message, every
    /// assistant message, and every tool result, and always stays a valid
    /// transcript (no tool call is left unanswered), even on error or cancel.
    pub async fn run(
        &self,
        system: &str,
        history: &mut Vec<Message>,
        user: Message,
        on_event: &mut (dyn FnMut(Event) + Send),
        cancel: &CancellationToken,
    ) -> Result<Outcome> {
        let specs: Vec<ToolSpec> = self.tools.iter().map(|t| t.spec()).collect();
        history.push(user);
        let mut reply = String::new();
        for _ in 0..self.max_steps {
            let messages = match self.max_context {
                Some(chars) => fit(history, chars.saturating_sub(system.len())),
                None => Cow::Borrowed(history.as_slice()),
            };
            let request = Request { system, messages: &messages, tools: &specs };
            let mut on_text = |delta: &str| on_event(Event::Text(delta.to_string()));
            let message = tokio::select! {
                biased;
                _ = cancel.cancelled() => return Ok(Outcome { reply, stop: Stop::Cancelled }),
                result = self.provider.complete(request, &mut on_text) => result?,
            };
            reply = message.content.clone();
            let calls = message.tool_calls.clone();
            history.push(message);
            if calls.is_empty() {
                return Ok(Outcome { reply, stop: Stop::Done });
            }
            for call in &calls {
                let output = if cancel.is_cancelled() {
                    Err(anyhow::anyhow!("cancelled by the user"))
                } else {
                    self.call_tool(call, on_event).await
                };
                let content = output.unwrap_or_else(|e| format!("Error: {e:#}"));
                history.push(Message::tool_result(&call.id, content));
            }
            if cancel.is_cancelled() {
                return Ok(Outcome { reply, stop: Stop::Cancelled });
            }
        }
        Ok(Outcome { reply, stop: Stop::StepLimit })
    }

    async fn call_tool(&self, call: &ToolCall, on_event: &mut (dyn FnMut(Event) + Send)) -> Result<String> {
        let args: Value = if call.arguments.trim().is_empty() {
            Value::Object(Default::default())
        } else {
            serde_json::from_str(&call.arguments).map_err(|e| anyhow::anyhow!("arguments are not valid JSON ({e})"))?
        };
        let tool = self
            .tools
            .iter()
            .find(|t| t.spec().name == call.name)
            .ok_or_else(|| anyhow::anyhow!("unknown tool `{}`", call.name))?;
        on_event(Event::ToolStart { name: call.name.clone(), args: args.clone() });
        let result = tool.call(args).await;
        let (output, is_error) = match &result {
            Ok(text) => (text.clone(), false),
            Err(e) => (format!("{e:#}"), true),
        };
        on_event(Event::ToolEnd { name: call.name.clone(), output, is_error });
        result
    }
}

/// Sent in place of a tool result from an earlier turn when the conversation
/// outgrows the context budget.
pub const OMITTED: &str = "[Earlier tool result omitted to fit the context window. Run the tool again if you need it.]";

/// The part of `history` to send so it fits in `budget` characters. Tool
/// results of earlier turns become [`OMITTED`] first; if that is not enough,
/// the oldest turns are dropped. The current turn (from the last user message)
/// is always sent whole, so the transcript stays valid.
pub fn fit(history: &[Message], budget: usize) -> Cow<'_, [Message]> {
    if history.iter().map(size).sum::<usize>() <= budget {
        return Cow::Borrowed(history);
    }
    let current = history.iter().rposition(|m| m.role == Role::User).unwrap_or(0);
    let mut messages = history.to_vec();
    for m in &mut messages[..current] {
        if m.role == Role::Tool && m.content.len() > OMITTED.len() {
            m.content = OMITTED.into();
        }
    }
    let mut total: usize = messages.iter().map(size).sum();
    let mut start = 0;
    while total > budget && start < current {
        let next = (start + 1..current).find(|&i| messages[i].role == Role::User).unwrap_or(current);
        total -= messages[start..next].iter().map(size).sum::<usize>();
        start = next;
    }
    messages.drain(..start);
    Cow::Owned(messages)
}

/// Rough size of a message on the wire, in characters.
fn size(m: &Message) -> usize {
    let extra = if m.extra.is_null() { 0 } else { m.extra.to_string().len() };
    m.content.len() + m.tool_calls.iter().map(|c| c.arguments.len()).sum::<usize>() + extra
}

#[cfg(test)]
mod tests {
    use super::*;
    use parking_lot::Mutex;

    /// Replays canned assistant messages.
    struct Script(Mutex<Vec<Message>>);

    #[async_trait]
    impl Provider for Script {
        async fn complete(&self, _: Request<'_>, on_text: &mut (dyn for<'s> FnMut(&'s str) + Send)) -> Result<Message> {
            let message = self.0.lock().remove(0);
            on_text(&message.content);
            Ok(message)
        }
    }

    struct Echo;

    #[async_trait]
    impl Tool for Echo {
        fn spec(&self) -> ToolSpec {
            ToolSpec { name: "echo".into(), description: String::new(), parameters: serde_json::json!({}) }
        }
        async fn call(&self, args: Value) -> Result<String> {
            match args.get("fail") {
                Some(_) => anyhow::bail!("boom"),
                None => Ok(args.to_string()),
            }
        }
    }

    fn calls(calls: &[(&str, &str)]) -> Message {
        let mut m = Message::assistant("");
        m.tool_calls = calls
            .iter()
            .enumerate()
            .map(|(i, (name, args))| ToolCall {
                id: format!("c{i}"),
                name: name.to_string(),
                arguments: args.to_string(),
                ..Default::default()
            })
            .collect();
        m
    }

    fn agent(script: Vec<Message>) -> Agent {
        Agent::new(Arc::new(Script(Mutex::new(script))), vec![Arc::new(Echo)])
    }

    #[tokio::test]
    async fn runs_tools_and_reports_errors_back_to_the_model() {
        let agent = agent(vec![
            calls(&[("echo", r#"{"x":1}"#), ("echo", r#"{"fail":true}"#), ("nope", "{}"), ("echo", "{bad")]),
            Message::assistant("done"),
        ]);
        let mut history = Vec::new();
        let mut events = Vec::new();
        let outcome = agent
            .run("sys", &mut history, Message::user("hi"), &mut |e| events.push(e), &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(outcome, Outcome { reply: "done".into(), stop: Stop::Done });
        let results: Vec<_> = history[2..6].iter().map(|m| m.content.as_str()).collect();
        assert_eq!(results[0], r#"{"x":1}"#);
        assert_eq!(results[1], "Error: boom");
        assert_eq!(results[2], "Error: unknown tool `nope`");
        assert!(results[3].starts_with("Error: arguments are not valid JSON"));
        assert!(events.contains(&Event::ToolEnd { name: "echo".into(), output: "boom".into(), is_error: true }));
        assert_eq!(events.last(), Some(&Event::Text("done".into())));
    }

    #[tokio::test]
    async fn stops_at_step_limit_with_a_valid_transcript() {
        let agent = agent(vec![calls(&[("echo", "{}")]), calls(&[("echo", "{}")])]).max_steps(2);
        let mut history = Vec::new();
        let outcome =
            agent.run("sys", &mut history, Message::user("hi"), &mut |_| {}, &CancellationToken::new()).await.unwrap();
        assert_eq!(outcome.stop, Stop::StepLimit);
        assert_eq!(history.last().unwrap().tool_call_id.as_deref(), Some("c0"));
    }

    #[test]
    fn fits_long_conversations_by_omitting_old_results_then_dropping_old_turns() {
        let big = "x".repeat(1000);
        let turn = |q: &str| vec![Message::user(q), calls(&[("echo", "{}")]), Message::tool_result("c0", &big)];
        let history: Vec<Message> = [turn("one"), turn("two"), turn("now")].concat();

        assert!(matches!(fit(&history, 10_000), Cow::Borrowed(_)), "fits as is");

        let sent = fit(&history, 1_500);
        assert_eq!(sent.len(), history.len(), "old results omitted, no turn dropped");
        assert_eq!((sent[2].content.as_str(), sent[5].content.as_str()), (OMITTED, OMITTED));
        assert_eq!(sent[8].content, big, "the current turn is sent whole");

        let sent = fit(&history, 1_150);
        assert_eq!(sent[0].content, "two", "starts at a user message");
        assert_eq!(sent.len(), 6);

        let sent = fit(&history, 10);
        assert_eq!((sent[0].content.as_str(), sent.len()), ("now", 3), "never cuts the current turn");
    }

    #[tokio::test]
    async fn cancelled_turn_answers_pending_tool_calls() {
        let agent = agent(vec![calls(&[("echo", "{}")])]);
        let cancel = CancellationToken::new();
        cancel.cancel();
        let mut history = Vec::new();
        let outcome = agent.run("sys", &mut history, Message::user("hi"), &mut |_| {}, &cancel).await.unwrap();
        assert_eq!(outcome.stop, Stop::Cancelled);
        assert_eq!(history.len(), 1, "cancel before the model replied keeps only the user message");
    }
}
