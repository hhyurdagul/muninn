use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use muninn_agent::{Agent, CancellationToken, Event, Stop};
use muninn_ai::{Message, Provider, Request};
use muninn_vault::{Journal, Vault};
use parking_lot::Mutex;
use serde_json::Value;

use crate::config::{Paths, Settings};
use crate::conversation::{automatic, cited_unread, Conversation, NoteEvent, Proposal};
use crate::prompt::system_prompt;
use crate::tools::{toolset, TurnContext};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Approve,
    /// Rejected, with optional feedback for the librarian.
    Reject(String),
}

/// Someone who can approve a change while the turn waits (a human at a TUI
/// or terminal).
#[async_trait]
pub trait Reviewer: Send + Sync {
    async fn review(&self, proposal: &Proposal) -> Decision;
}

/// How changes that need review get approved.
#[derive(Clone)]
pub enum Review {
    /// Ask now and wait for the answer.
    Interactive(Arc<dyn Reviewer>),
    /// Store the proposal; the caller approves in a later message (MCP).
    Deferred,
}

/// Live progress of a turn.
#[derive(Debug, Clone, PartialEq)]
pub enum Update {
    Text(String),
    Tool {
        name: String,
        detail: String,
    },
    Note(NoteEvent),
    /// The reply cited these notes from search matches alone, so the model
    /// was asked to read them and answer again. The answer that follows
    /// replaces the one before.
    Recheck(Vec<String>),
}

#[derive(Debug, Clone)]
pub struct TurnResult {
    pub reply: String,
    pub stop: Stop,
    /// Notes read, changed, or proposed during this turn.
    pub notes: Vec<NoteEvent>,
    /// Every change still awaiting approval in the conversation.
    pub pending: Vec<Proposal>,
}

/// The most a request may hold, in characters (about 75k–100k tokens), so
/// long conversations still fit models with a 128k-token context.
const CONTEXT_CHARS: usize = 300_000;

pub struct Librarian {
    provider: Arc<dyn Provider>,
    vault: Arc<Vault>,
    journal: Arc<Journal>,
    /// Where to re-read the ignore patterns before each turn.
    paths: Option<Paths>,
}

impl Librarian {
    pub fn new(provider: Arc<dyn Provider>, vault: Vault, journal: Journal) -> Self {
        Self { provider, vault: Arc::new(vault), journal: Arc::new(journal), paths: None }
    }

    /// A librarian that follows `settings`, re-reading their ignore patterns
    /// from `paths` before every turn so edits apply without a restart.
    pub fn from_settings(settings: &Settings, paths: &Paths) -> Result<Self> {
        let vault = settings.open_vault()?;
        let librarian = Self::new(crate::provider::connect(settings)?, vault, Journal::open(paths.journal_file()));
        Ok(Self { paths: Some(paths.clone()), ..librarian })
    }

    pub fn vault(&self) -> &Vault {
        &self.vault
    }

    pub fn journal(&self) -> &Journal {
        &self.journal
    }

    /// Runs one message through the librarian. The conversation is updated
    /// even when the turn fails or is cancelled, so it can be saved as is.
    pub async fn turn(
        &self,
        conversation: &mut Conversation,
        text: &str,
        review: Review,
        on_update: &mut (dyn FnMut(Update) + Send),
        cancel: &CancellationToken,
    ) -> Result<TurnResult> {
        anyhow::ensure!(
            conversation.vault == self.vault.root(),
            "conversation {} belongs to vault {}",
            conversation.id,
            conversation.vault.display()
        );
        if let Some(paths) = &self.paths {
            self.vault.set_ignore(&Settings::load(paths)?.ignore)?;
        }
        let system = system_prompt(&self.vault, conversation.surface, conversation.kind);
        let ctx = Arc::new(TurnContext {
            vault: self.vault.clone(),
            journal: self.journal.clone(),
            review,
            conversation_id: conversation.id.clone(),
            user_turn: conversation.user_turns() + 1,
            pending: Mutex::new(conversation.pending.clone()),
            notes: Mutex::new(Vec::new()),
        });
        let agent = Agent::new(self.provider.clone(), toolset(ctx.clone())).max_context(CONTEXT_CHARS);

        let mut reported = 0;
        let mut outcome = {
            let mut on_event = forward(&ctx, on_update, &mut reported);
            agent.run(&system, &mut conversation.messages, Message::user(text), &mut on_event, cancel).await
        };
        // Models cite notes they only saw as search matches even when told
        // not to (F9), so ask once to read them and answer again.
        let unread = match &outcome {
            Ok(o) if o.stop == Stop::Done => cited_unread(&o.reply, &ctx.notes.lock()),
            _ => Vec::new(),
        };
        if !unread.is_empty() {
            on_update(Update::Recheck(unread.clone()));
            let mut on_event = forward(&ctx, on_update, &mut reported);
            outcome = agent.run(&system, &mut conversation.messages, recheck(&unread), &mut on_event, cancel).await;
        }

        let notes = ctx.notes.lock().clone();
        conversation.pending = ctx.pending.lock().clone();
        conversation.notes.extend(notes.iter().cloned());
        conversation.updated = chrono::Local::now();
        if conversation.title.is_empty() {
            conversation.title = title(text);
        }
        let outcome = outcome?;
        Ok(TurnResult { reply: outcome.reply, stop: outcome.stop, notes, pending: conversation.pending.clone() })
    }
}

/// Forwards agent events as updates, adding the notes each tool touched.
fn forward<'a>(
    ctx: &'a TurnContext,
    on_update: &'a mut (dyn FnMut(Update) + Send),
    reported: &'a mut usize,
) -> impl FnMut(Event) + Send + 'a {
    move |event| match event {
        Event::Text(delta) => on_update(Update::Text(delta)),
        Event::ToolStart { name, args } => on_update(Update::Tool { name, detail: tool_detail(&args) }),
        Event::ToolEnd { .. } => {
            let notes = ctx.notes.lock();
            for note in &notes[*reported..] {
                on_update(Update::Note(note.clone()));
            }
            *reported = notes.len();
        }
    }
}

fn recheck(unread: &[String]) -> Message {
    let list = unread.iter().map(|p| format!("`{p}`")).collect::<Vec<_>>().join(", ");
    automatic(format!(
        "(Automatic check from Muninn, not from the user.) Your answer cites {list}, but you saw only search-match \
         lines from them. Read them with read_note now, then give your complete answer again, corrected if needed. \
         The user sees only this new answer, so do not refer to the earlier one or to this check."
    ))
}

/// One tiny completion to prove the endpoint, key, and model work.
pub async fn check_model(settings: &Settings) -> Result<String> {
    let provider = crate::provider::connect(settings)?;
    let messages = [Message::user("ping")];
    let request = Request { system: "Reply with the single word OK.", messages: &messages, tools: &[] };
    let reply = provider.complete(request, &mut |_| {}).await?;
    Ok(reply.content)
}

/// The most telling argument of a tool call (path, query, or change id), for display.
pub fn tool_detail(args: &Value) -> String {
    ["path", "query", "change_id"]
        .iter()
        .find_map(|k| args.get(*k).and_then(Value::as_str))
        .unwrap_or_default()
        .to_string()
}

fn title(text: &str) -> String {
    let line = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
    match line.char_indices().nth(60) {
        Some((i, _)) => format!("{}…", &line[..i]),
        None => line.to_string(),
    }
}
