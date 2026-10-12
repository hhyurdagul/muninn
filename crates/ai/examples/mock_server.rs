//! Serve a scripted OpenAI-compatible endpoint for offline demos:
//!
//! ```sh
//! cargo run -p muninn-ai --features mock --example mock_server -- script.json [port]
//! ```
//!
//! `script.json` is an array of assistant messages, answered in order:
//! `[{"role":"assistant","content":"Hi"}, {"role":"assistant","tool_calls":[{"id":"1","name":"list_notes","arguments":"{}"}]}]`

use muninn_ai::mock::MockServer;
use muninn_ai::Message;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let script_path = args.next().ok_or_else(|| anyhow::anyhow!("usage: mock_server <script.json> [port]"))?;
    let script: Vec<Message> = serde_json::from_str(&std::fs::read_to_string(script_path)?)?;
    let port = args.next().map(|p| p.parse()).transpose()?.unwrap_or(0);
    let server = MockServer::scripted_on(port, script).await?;
    println!("{}", server.url);
    tokio::signal::ctrl_c().await?;
    Ok(())
}
