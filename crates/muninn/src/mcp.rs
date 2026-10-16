//! `muninn mcp`: one tool, `muninn`, that takes a message and an optional
//! conversation id. Questions and change approvals flow through follow-up
//! calls in the same conversation.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use muninn_core::{
    CancellationToken, Conversation, Kind, Librarian, NoteEvent, NoteKind, Paths, Review, Settings, Stop, Store,
    Surface, TurnResult,
};
use muninn_mcp::{Handler, ServerInfo, ToolInfo, ToolResult};
use serde_json::{json, Value};
use tokio::sync::Mutex;

const INSTRUCTIONS: &str = "Muninn is the librarian of the user's knowledge vault (their notes). Use the \
`muninn` tool to save anything worth keeping, to ask about anything the user may have written down, or to \
change notes. Describe what you want in plain language; Muninn already knows the vault's structure and rules \
and decides where things go. Muninn may answer with a question or a change that needs approval: reply in the \
same conversation by passing back its conversation_id. Ask Muninn to remember lasting facts about the user \
(preferences, projects, decisions); it keeps the memory below up to date.";

/// Claude Code keeps this many characters of a server's instructions.
const INSTRUCTIONS_LIMIT: usize = 4096;

const TOOL_DESCRIPTION: &str = "Talk to Muninn, the librarian of the user's knowledge vault. Send something \
to save (include the content and any context that helps file it), a question about the user's notes, or a \
request to change notes. Muninn files saves according to the vault's rules and answers with citations of the \
notes it used. It may ask you a question back or propose a change that needs your approval; continue by \
calling this tool again with the returned conversation_id. Whether you check with the user first is up to you.";

pub async fn serve() -> Result<()> {
    let paths = Paths::from_env();
    let info = ServerInfo {
        name: "muninn".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        instructions: Some(instructions(&paths)),
    };
    let handler = Arc::new(MuninnTool { paths, locks: Mutex::default() });
    muninn_mcp::serve_stdio(info, handler).await
}

/// The server instructions, with the vault's MEMORY.md so that agents know
/// the user from the start of a session.
fn instructions(paths: &Paths) -> String {
    let memory = Settings::load(paths).ok().and_then(|s| s.open_vault().ok()).and_then(|v| muninn_core::memory(&v));
    let Some(memory) = memory else { return INSTRUCTIONS.into() };
    let text = format!("{INSTRUCTIONS}\n\n# What Muninn remembers about the user (MEMORY.md)\n{memory}");
    if text.chars().count() <= INSTRUCTIONS_LIMIT {
        return text;
    }
    let cut = "\n…(cut; ask Muninn for the rest)";
    let keep: String = text.chars().take(INSTRUCTIONS_LIMIT - cut.chars().count()).collect();
    keep + cut
}

struct MuninnTool {
    paths: Paths,
    /// One turn at a time per conversation.
    locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
}

#[async_trait]
impl Handler for MuninnTool {
    fn tools(&self) -> Vec<ToolInfo> {
        vec![ToolInfo {
            name: "muninn".into(),
            description: TOOL_DESCRIPTION.into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "message": { "type": "string", "description": "What you want saved, asked, or changed, or your reply to Muninn." },
                    "conversation_id": { "type": "string", "description": "Continue an earlier conversation with Muninn." },
                },
                "required": ["message"],
            }),
        }]
    }

    async fn call(&self, name: &str, args: Value) -> ToolResult {
        let result = match name {
            "muninn" => self.converse(&args).await,
            other => Err(anyhow::anyhow!("unknown tool {other}")),
        };
        result.unwrap_or_else(|err| ToolResult { text: format!("Muninn failed: {err:#}"), is_error: true })
    }
}

impl MuninnTool {
    async fn converse(&self, args: &Value) -> Result<ToolResult> {
        let message = args.get("message").and_then(Value::as_str).context("`message` is required")?;
        let settings = Settings::load(&self.paths)?;
        anyhow::ensure!(
            settings.is_complete(),
            "Muninn is not set up yet. Ask the user to run `muninn` in a terminal to onboard."
        );
        let librarian = Librarian::from_settings(&settings, &self.paths)?;
        let store = Store::new(self.paths.conversations_dir());
        let id = args.get("conversation_id").and_then(Value::as_str).filter(|s| !s.is_empty());

        let lock = match id {
            Some(id) => self.locks.lock().await.entry(id.to_string()).or_default().clone(),
            None => Arc::default(),
        };
        let _guard = lock.lock().await;
        let mut conversation = match id {
            Some(id) => store.load(id)?,
            None => Conversation::new(Surface::Mcp, Kind::Chat, librarian.vault().root()),
        };
        let before = conversation.notes.len();
        let result =
            librarian.turn(&mut conversation, message, Review::Deferred, &mut |_| {}, &CancellationToken::new()).await;
        store.save(&conversation)?;
        Ok(match result {
            Ok(result) => ToolResult { text: render(&conversation, &result), is_error: false },
            Err(err) => {
                ToolResult { text: failure(&conversation, &conversation.notes[before..], &err), is_error: true }
            }
        })
    }
}

fn render(conversation: &Conversation, result: &TurnResult) -> String {
    let mut out =
        if result.reply.trim().is_empty() { "(no reply)".to_string() } else { result.reply.trim().to_string() };
    if result.stop == Stop::StepLimit {
        out.push_str("\n\n(Muninn stopped after too many steps.)");
    }
    out + &footer(conversation, &result.notes)
}

/// A turn that broke off: the error, plus whatever it already did, so the
/// caller knows what landed and can continue the conversation.
fn failure(conversation: &Conversation, notes: &[NoteEvent], err: &anyhow::Error) -> String {
    let mut out = format!("Muninn failed: {err:#}");
    if notes.iter().any(|n| n.kind != NoteKind::Read) {
        out.push_str("\n\nThe turn stopped partway. The changes listed below were already made.");
    }
    out.push_str(&footer(conversation, notes));
    out.push_str("\nReply in this conversation to let Muninn pick up where it stopped.");
    out
}

/// Notes read and changed, pending proposals, and the conversation id.
fn footer(conversation: &Conversation, notes: &[NoteEvent]) -> String {
    let mut out = String::from("\n\n---");
    let mut read: Vec<&str> = Vec::new();
    for note in notes.iter().filter(|n| n.kind == NoteKind::Read) {
        if !read.contains(&note.path.as_str()) {
            read.push(&note.path);
        }
    }
    if !read.is_empty() {
        out.push_str(&format!(
            "\nNotes read: {}",
            read.iter().map(|p| format!("`{p}`")).collect::<Vec<_>>().join(", ")
        ));
    }
    for note in notes.iter().filter(|n| n.kind == NoteKind::Changed) {
        let id = note.id.as_deref().unwrap_or_default();
        out.push_str(&format!("\nChanged `{}` (undo with `muninn undo {id}`)", note.path));
    }
    for proposal in &conversation.pending {
        out.push_str(&format!(
            "\n\nPending change {} to `{}`: {}\n```diff\n{}```\nNot written yet. Reply in this conversation to approve or reject it.",
            proposal.id,
            proposal.path,
            proposal.summary,
            proposal.diff()
        ));
    }
    out.push_str(&format!("\n\nconversation_id: {}", conversation.id));
    out
}
