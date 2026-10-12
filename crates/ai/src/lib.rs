//! Provider-agnostic chat types and a streaming OpenAI-compatible client.
//!
//! ```no_run
//! # async fn demo() -> anyhow::Result<()> {
//! use muninn_ai::{Message, OpenAiCompatible, Provider, Request};
//! let provider = OpenAiCompatible::new("https://openrouter.ai/api/v1", "sk-...", "openai/gpt-4o-mini");
//! let messages = [Message::user("Say hi")];
//! let reply = provider
//!     .complete(Request { system: "Be brief.", messages: &messages, tools: &[] }, &mut |d| print!("{d}"))
//!     .await?;
//! # Ok(()) }
//! ```

mod openai;
mod sse;
mod types;

pub use openai::OpenAiCompatible;
pub use types::{Message, Provider, Request, Role, ToolCall, ToolSpec};
