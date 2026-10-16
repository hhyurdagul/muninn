//! Provider-agnostic chat types and streaming clients for OpenAI-compatible
//! endpoints and Anthropic's Messages API.
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

mod anthropic;
#[cfg(any(test, feature = "mock"))]
pub mod mock;
mod openai;
mod retry;
mod sse;
mod types;

pub use anthropic::{Anthropic, ANTHROPIC_BASE_URL};
pub use openai::OpenAiCompatible;
pub use types::{Message, Provider, Request, Role, ToolCall, ToolSpec};
