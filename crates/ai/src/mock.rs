//! A scripted OpenAI-compatible server for tests and offline demos.
//! Each `/chat/completions` request is answered by streaming one assistant
//! message chosen by a handler that sees the request body; `GET /models`
//! lists two models. A reply whose `extra` holds `{"mock_status": 400}` is
//! answered with that HTTP error instead.

use std::sync::Arc;

use parking_lot::Mutex;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::types::Message;

type Handler = dyn Fn(&Value) -> Message + Send + Sync;

pub struct MockServer {
    /// Base URL to pass as the endpoint, e.g. `http://127.0.0.1:1234/v1`.
    pub url: String,
    requests: Arc<Mutex<Vec<Value>>>,
}

impl MockServer {
    /// Replies with `script` in order, then with a fixed text once exhausted.
    pub async fn scripted(script: Vec<Message>) -> std::io::Result<Self> {
        Self::scripted_on(0, script).await
    }

    /// Like [`MockServer::scripted`] on a fixed port (0 picks a free one).
    pub async fn scripted_on(port: u16, script: Vec<Message>) -> std::io::Result<Self> {
        let queue = Mutex::new(std::collections::VecDeque::from(script));
        let next =
            move |_: &Value| queue.lock().pop_front().unwrap_or_else(|| Message::assistant("(script exhausted)"));
        Self::start_on(port, next).await
    }

    /// Replies with whatever `handler` returns for each request body.
    pub async fn start(handler: impl Fn(&Value) -> Message + Send + Sync + 'static) -> std::io::Result<Self> {
        Self::start_on(0, handler).await
    }

    pub async fn start_on(
        port: u16,
        handler: impl Fn(&Value) -> Message + Send + Sync + 'static,
    ) -> std::io::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", port)).await?;
        let url = format!("http://{}/v1", listener.local_addr()?);
        let requests = Arc::new(Mutex::new(Vec::new()));
        let handler: Arc<Handler> = Arc::new(handler);
        let log = requests.clone();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let (handler, log) = (handler.clone(), log.clone());
                tokio::spawn(async move {
                    let _ = serve(stream, handler.as_ref(), &log).await;
                });
            }
        });
        Ok(Self { url, requests })
    }

    /// Bodies of all requests received so far.
    pub fn requests(&self) -> Vec<Value> {
        self.requests.lock().clone()
    }
}

async fn serve(mut stream: TcpStream, handler: &Handler, log: &Mutex<Vec<Value>>) -> std::io::Result<()> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 8192];
    let header_end = loop {
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            return Ok(());
        }
        buffer.extend_from_slice(&chunk[..n]);
        if let Some(i) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
    };
    let headers = String::from_utf8_lossy(&buffer[..header_end]).to_lowercase();
    if headers.starts_with("get ") && headers.lines().next().is_some_and(|l| l.contains("/models")) {
        let body = json!({ "data": [{ "id": "mock" }, { "id": "mock-large" }] }).to_string();
        let head = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            body.len()
        );
        stream.write_all(format!("{head}{body}").as_bytes()).await?;
        return stream.shutdown().await;
    }
    let length: usize = headers
        .lines()
        .find_map(|l| l.strip_prefix("content-length:"))
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(0);
    while buffer.len() < header_end + length {
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..n]);
    }
    let body: Value = serde_json::from_slice(&buffer[header_end..]).unwrap_or(Value::Null);
    let reply = handler(&body);
    log.lock().push(body);
    if let Some(status) = reply.extra.get("mock_status").and_then(Value::as_u64) {
        let body = json!({ "error": { "message": "mock failure" } }).to_string();
        let head = format!("HTTP/1.1 {status} Mock\r\ncontent-length: {}\r\nconnection: close\r\n\r\n", body.len());
        stream.write_all(format!("{head}{body}").as_bytes()).await?;
        return stream.shutdown().await;
    }

    let mut out = String::from("HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n");
    for event in events(&reply) {
        out.push_str(&format!("data: {event}\n\n"));
    }
    out.push_str("data: [DONE]\n\n");
    stream.write_all(out.as_bytes()).await?;
    stream.shutdown().await
}

/// Splits a message into realistic streaming chunks.
fn events(message: &Message) -> Vec<Value> {
    let delta = |d: Value| json!({ "choices": [{ "index": 0, "delta": d }] });
    let chars: Vec<char> = message.content.chars().collect();
    let mut events: Vec<Value> =
        chars.chunks(8).map(|c| delta(json!({ "content": c.iter().collect::<String>() }))).collect();
    for (index, call) in message.tool_calls.iter().enumerate() {
        let split = call.arguments.char_indices().nth(call.arguments.chars().count() / 2).map_or(0, |(i, _)| i);
        let (head, tail) = call.arguments.split_at(split);
        events.push(delta(json!({ "tool_calls": [{
            "index": index, "id": call.id, "type": "function",
            "function": { "name": call.name, "arguments": head },
        }] })));
        events.push(delta(json!({ "tool_calls": [{ "index": index, "function": { "arguments": tail } }] })));
    }
    events
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{OpenAiCompatible, Provider, Request, ToolCall};

    #[tokio::test]
    async fn roundtrips_text_and_tool_calls_through_the_real_client() {
        let mut call = Message::assistant("Let me look… ğ");
        call.tool_calls = vec![ToolCall {
            id: "t1".into(),
            name: "read".into(),
            arguments: r#"{"path":"çay.md"}"#.into(),
            ..Default::default()
        }];
        let server = MockServer::scripted(vec![call.clone()]).await.unwrap();
        let provider = OpenAiCompatible::new(&server.url, "key", "model");
        let messages = [Message::user("hi")];
        let mut streamed = String::new();
        let reply = provider
            .complete(Request { system: "sys", messages: &messages, tools: &[] }, &mut |d| streamed.push_str(d))
            .await
            .unwrap();
        assert_eq!(reply, call);
        assert_eq!(streamed, call.content);
        let request = &server.requests()[0];
        assert_eq!(request["messages"][0]["content"], "sys");
        assert_eq!(request["model"], "model");
    }
}
