//! `muninn mcp`: one tool, `muninn`, that takes a message and an optional
//! conversation id. Questions and change approvals flow through follow-up
//! calls in the same conversation.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use muninn_core::{
    CancellationToken, Conversation, Kind, Librarian, NoteKind, Paths, Review, Settings, Stop, Store, Surface,
    TurnResult,
};
use muninn_mcp::{Handler, ServerInfo, ToolInfo, ToolResult};
use serde_json::{json, Value};
use tokio::sync::Mutex;

const INSTRUCTIONS: &str = "Muninn is the librarian of the user's knowledge vault (their notes). Use the \
`muninn` tool to save anything worth keeping, to ask about anything the user may have written down, or to \
change notes. Describe what you want in plain language; Muninn already knows the vault's structure and rules \
and decides where things go. Muninn may answer with a question or a change that needs approval: reply in the \
same conversation by passing back its conversation_id.";

const TOOL_DESCRIPTION: &str = "Talk to Muninn, the librarian of the user's knowledge vault. Send something \
to save (include the content and any context that helps file it), a question about the user's notes, or a \
request to change notes. Muninn files saves according to the vault's rules and answers with citations of the \
notes it used. It may ask you a question back or propose a change that needs your approval; continue by \
calling this tool again with the returned conversation_id. Whether you check with the user first is up to you.";

pub async fn serve() -> Result<()> {
    let info = ServerInfo {
        name: "muninn".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        instructions: Some(INSTRUCTIONS.into()),
    };
    let handler = Arc::new(MuninnTool { paths: Paths::from_env(), locks: Mutex::default() });
    muninn_mcp::serve_stdio(info, handler).await
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
        match result {
            Ok(text) => ToolResult { text, is_error: false },
            Err(err) => ToolResult { text: format!("Muninn failed: {err:#}"), is_error: true },
        }
    }
}

impl MuninnTool {
    async fn converse(&self, args: &Value) -> Result<String> {
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
        let result =
            librarian.turn(&mut conversation, message, Review::Deferred, &mut |_| {}, &CancellationToken::new()).await;
        store.save(&conversation)?;
        Ok(render(&conversation, &result?))
    }
}

fn render(conversation: &Conversation, result: &TurnResult) -> String {
    let mut out =
        if result.reply.trim().is_empty() { "(no reply)".to_string() } else { result.reply.trim().to_string() };
    if result.stop == Stop::StepLimit {
        out.push_str("\n\n(Muninn stopped after too many steps.)");
    }
    out.push_str("\n\n---");
    let mut read: Vec<&str> = Vec::new();
    for note in result.notes.iter().filter(|n| n.kind == NoteKind::Read) {
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
    for note in result.notes.iter().filter(|n| n.kind == NoteKind::Changed) {
        let id = note.id.as_deref().unwrap_or_default();
        out.push_str(&format!("\nChanged `{}` (undo with `muninn undo {id}`)", note.path));
    }
    for proposal in &result.pending {
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
