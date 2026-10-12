//! Muninn's librarian, independent of any user interface: give it a
//! conversation and a message, get streamed updates and a result back.

mod config;
mod conversation;
mod librarian;
mod prompt;
mod tools;

pub use config::{Paths, Settings, DEFAULT_BASE_URL, DEFAULT_MODEL};
pub use conversation::{Conversation, Kind, NoteEvent, NoteKind, Proposal, Store, Surface};
pub use librarian::{check_model, Decision, Librarian, Review, Reviewer, TurnResult, Update};
pub use muninn_agent::{CancellationToken, Stop};

fn new_id(prefix: &str) -> String {
    use std::sync::atomic::{AtomicU32, Ordering};
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let now = chrono::Local::now();
    let salt = now.timestamp_subsec_nanos().wrapping_add(COUNTER.fetch_add(1, Ordering::Relaxed) * 7919);
    format!("{prefix}{}-{:04x}", now.format("%y%m%d%H%M%S"), salt & 0xffff)
}
