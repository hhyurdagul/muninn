//! A tools-only MCP server over newline-delimited JSON-RPC (the stdio
//! transport). Implement [`Handler`] and call [`serve_stdio`].

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::Mutex;
use tokio::task::JoinSet;

const PROTOCOL_VERSIONS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];

pub struct ServerInfo {
    pub name: String,
    pub version: String,
    /// Shown to the client model as guidance for using this server.
    pub instructions: Option<String>,
}

pub struct ToolInfo {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

pub struct ToolResult {
    pub text: String,
    pub is_error: bool,
}

#[async_trait]
pub trait Handler: Send + Sync + 'static {
    fn tools(&self) -> Vec<ToolInfo>;
    async fn call(&self, name: &str, args: Value) -> ToolResult;
}

pub async fn serve_stdio(info: ServerInfo, handler: Arc<dyn Handler>) -> Result<()> {
    serve(info, handler, BufReader::new(tokio::io::stdin()), tokio::io::stdout()).await
}

/// Serves until `reader` reaches EOF, then waits for in-flight tool calls.
/// Tool calls run concurrently.
pub async fn serve<R, W>(info: ServerInfo, handler: Arc<dyn Handler>, reader: R, writer: W) -> Result<()>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let writer = Arc::new(Mutex::new(writer));
    let mut calls = JoinSet::new();
    let mut lines = reader.lines();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            send(&writer, error(Value::Null, -32700, "parse error")).await?;
            continue;
        };
        let (Some(method), Some(id)) = (message.get("method").and_then(Value::as_str), message.get("id")) else {
            continue; // notification or response: nothing to answer
        };
        let id = id.clone();
        let params = message.get("params").cloned().unwrap_or(Value::Null);
        let reply = match method {
            "initialize" => ok(id, initialize(&info, &params)),
            "ping" => ok(id, json!({})),
            "tools/list" => ok(id, json!({ "tools": tool_list(handler.as_ref()) })),
            "tools/call" => {
                let (handler, writer) = (handler.clone(), writer.clone());
                calls.spawn(async move {
                    let name = params.get("name").and_then(Value::as_str).unwrap_or_default();
                    let args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
                    let result = handler.call(name, args).await;
                    let body = json!({
                        "content": [{ "type": "text", "text": result.text }],
                        "isError": result.is_error,
                    });
                    send(&writer, ok(id, body)).await
                });
                continue;
            }
            other => error(id, -32601, &format!("method not found: {other}")),
        };
        send(&writer, reply).await?;
    }
    while let Some(result) = calls.join_next().await {
        result??;
    }
    Ok(())
}

fn initialize(info: &ServerInfo, params: &Value) -> Value {
    let requested = params.get("protocolVersion").and_then(Value::as_str).unwrap_or_default();
    let version = PROTOCOL_VERSIONS.iter().find(|v| **v == requested).unwrap_or(&PROTOCOL_VERSIONS[0]);
    let mut result = json!({
        "protocolVersion": version,
        "capabilities": { "tools": { "listChanged": false } },
        "serverInfo": { "name": info.name, "version": info.version },
    });
    if let Some(instructions) = &info.instructions {
        result["instructions"] = json!(instructions);
    }
    result
}

fn tool_list(handler: &dyn Handler) -> Vec<Value> {
    handler
        .tools()
        .into_iter()
        .map(|t| json!({ "name": t.name, "description": t.description, "inputSchema": t.input_schema }))
        .collect()
}

fn ok(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

async fn send<W: AsyncWrite + Unpin>(writer: &Mutex<W>, message: Value) -> Result<()> {
    let mut writer = writer.lock().await;
    writer.write_all(format!("{message}\n").as_bytes()).await?;
    writer.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Upper;

    #[async_trait]
    impl Handler for Upper {
        fn tools(&self) -> Vec<ToolInfo> {
            vec![ToolInfo { name: "upper".into(), description: "d".into(), input_schema: json!({"type":"object"}) }]
        }
        async fn call(&self, name: &str, args: Value) -> ToolResult {
            let text = args["text"].as_str().unwrap_or_default().to_uppercase();
            ToolResult { text: format!("{name}:{text}"), is_error: text.is_empty() }
        }
    }

    async fn roundtrip(input: &str) -> Vec<Value> {
        let info = ServerInfo { name: "t".into(), version: "1".into(), instructions: Some("hi".into()) };
        let (client, server) = tokio::io::duplex(1 << 16);
        let (read_half, write_half) = tokio::io::split(server);
        let mut client = client;
        client.write_all(input.as_bytes()).await.unwrap();
        client.shutdown().await.unwrap();
        // The server reads from `client`'s writes and writes back on the same pipe.
        let reader = BufReader::new(read_half);
        serve(info, Arc::new(Upper), reader, write_half).await.unwrap();
        let mut out = String::new();
        tokio::io::AsyncReadExt::read_to_string(&mut client, &mut out).await.unwrap();
        let mut replies: Vec<Value> = out.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        replies.sort_by_key(|r| r["id"].as_i64());
        replies
    }

    #[tokio::test]
    async fn speaks_the_tools_protocol() {
        let input = [
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26"}}"#,
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"upper","arguments":{"text":"ok"}}}"#,
            r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"upper","arguments":{}}}"#,
            r#"{"jsonrpc":"2.0","id":5,"method":"resources/list"}"#,
        ]
        .join("\n");
        let replies = roundtrip(&format!("{input}\n")).await;
        assert_eq!(replies.len(), 5, "notifications get no reply");
        assert_eq!(replies[0]["result"]["protocolVersion"], "2025-03-26");
        assert_eq!(replies[0]["result"]["instructions"], "hi");
        assert_eq!(replies[1]["result"]["tools"][0]["inputSchema"]["type"], "object");
        assert_eq!(replies[2]["result"]["content"][0]["text"], "upper:OK");
        assert_eq!(replies[2]["result"]["isError"], false);
        assert_eq!(replies[3]["result"]["isError"], true);
        assert_eq!(replies[4]["error"]["code"], -32601);
    }

    #[tokio::test]
    async fn unknown_protocol_version_gets_latest() {
        let replies = roundtrip("{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"1999\"}}\n").await;
        assert_eq!(replies[0]["result"]["protocolVersion"], PROTOCOL_VERSIONS[0]);
    }
}
