//! The librarian's tools: list, read, search and write notes inside the vault,
//! nothing else. Writes go through the journal (undo) and, when review is
//! required, through the caller's approval.

use std::sync::Arc;

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use muninn_agent::Tool;
use muninn_ai::ToolSpec;
use muninn_vault::{Journal, Vault};
use parking_lot::Mutex;
use serde_json::{json, Value};

use crate::conversation::{NoteEvent, NoteKind, Proposal};
use crate::librarian::{Decision, Review};

/// State shared by the tools during one turn.
pub(crate) struct TurnContext {
    pub vault: Arc<Vault>,
    pub journal: Arc<Journal>,
    pub review: Review,
    pub conversation_id: String,
    pub user_turn: usize,
    pub pending: Mutex<Vec<Proposal>>,
    pub notes: Mutex<Vec<NoteEvent>>,
}

#[derive(Clone, Copy)]
enum Kind {
    List,
    Read,
    Search,
    Write,
    Edit,
    CreateFolder,
    ApplyPending,
    DiscardPending,
}

pub(crate) struct VaultTool {
    kind: Kind,
    ctx: Arc<TurnContext>,
}

/// The tool set for a turn; pending-change tools only exist when review is
/// deferred to a later message.
pub(crate) fn toolset(ctx: Arc<TurnContext>) -> Vec<Arc<dyn Tool>> {
    let mut kinds = vec![Kind::List, Kind::Read, Kind::Search, Kind::Write, Kind::Edit, Kind::CreateFolder];
    if matches!(ctx.review, Review::Deferred) {
        kinds.extend([Kind::ApplyPending, Kind::DiscardPending]);
    }
    kinds
        .into_iter()
        .map(|kind| Arc::new(VaultTool { kind, ctx: ctx.clone() }) as Arc<dyn Tool>)
        .collect()
}

fn spec(name: &str, description: &str, properties: Value, required: &[&str]) -> ToolSpec {
    ToolSpec {
        name: name.into(),
        description: description.into(),
        parameters: json!({ "type": "object", "properties": properties, "required": required }),
    }
}

const REVIEW_DOC: &str = "Set true when the vault rules require changes to be reviewed before they land.";

#[async_trait]
impl Tool for VaultTool {
    fn spec(&self) -> ToolSpec {
        match self.kind {
            Kind::List => spec(
                "list_notes",
                "Show the folder tree of the vault or of a folder in it.",
                json!({
                    "path": { "type": "string", "description": "Vault-relative folder; empty for the root." },
                    "depth": { "type": "integer", "description": "Levels to expand (default 2)." },
                }),
                &[],
            ),
            Kind::Read => spec(
                "read_note",
                "Read a note. Long notes are returned in windows of lines.",
                json!({
                    "path": { "type": "string", "description": "Vault-relative path." },
                    "offset": { "type": "integer", "description": "First line to return (1-based)." },
                    "limit": { "type": "integer", "description": "Maximum lines (default 2000)." },
                }),
                &["path"],
            ),
            Kind::Search => spec(
                "search_notes",
                "Case-insensitive search over note names and contents. Returns `path:line: text` matches.",
                json!({
                    "query": { "type": "string" },
                    "regex": { "type": "boolean", "description": "Treat query as a regular expression." },
                    "path": { "type": "string", "description": "Limit to this folder." },
                    "max_results": { "type": "integer", "description": "Default 50." },
                }),
                &["query"],
            ),
            Kind::Write => spec(
                "write_note",
                "Create a note or replace its whole content.",
                json!({
                    "path": { "type": "string", "description": "Vault-relative path, e.g. inbox/idea.md." },
                    "content": { "type": "string" },
                    "summary": { "type": "string", "description": "One line describing the change, for the change log." },
                    "review": { "type": "boolean", "description": REVIEW_DOC },
                }),
                &["path", "content", "summary"],
            ),
            Kind::Edit => spec(
                "edit_note",
                "Replace one exact, unique piece of text in an existing note (e.g. to append under a heading).",
                json!({
                    "path": { "type": "string" },
                    "old_text": { "type": "string", "description": "Text to replace; must occur exactly once." },
                    "new_text": { "type": "string" },
                    "summary": { "type": "string", "description": "One line describing the change, for the change log." },
                    "review": { "type": "boolean", "description": REVIEW_DOC },
                }),
                &["path", "old_text", "new_text", "summary"],
            ),
            Kind::CreateFolder => spec(
                "create_folder",
                "Create a folder (and missing parents).",
                json!({ "path": { "type": "string" } }),
                &["path"],
            ),
            Kind::ApplyPending => spec(
                "apply_pending_change",
                "Write a proposed change after the caller approved it in a later message.",
                json!({ "change_id": { "type": "string" } }),
                &["change_id"],
            ),
            Kind::DiscardPending => spec(
                "discard_pending_change",
                "Drop a proposed change the caller rejected or that you are replacing.",
                json!({ "change_id": { "type": "string" } }),
                &["change_id"],
            ),
        }
    }

    async fn call(&self, args: Value) -> Result<String> {
        let ctx = &self.ctx;
        match self.kind {
            Kind::List => {
                let depth = opt_u64(&args, "depth").unwrap_or(2).clamp(1, 6) as usize;
                let tree = ctx.vault.tree(opt_str(&args, "path").unwrap_or(""), depth, 400)?;
                Ok(if tree.is_empty() { "(empty)".into() } else { tree })
            }
            Kind::Read => {
                let path = str_arg(&args, "path")?;
                let content = ctx.vault.read(path)?.with_context(|| format!("no note at {path}"))?;
                ctx.record(path, NoteKind::Read, None);
                Ok(window(&content, opt_u64(&args, "offset"), opt_u64(&args, "limit")))
            }
            Kind::Search => {
                let max = opt_u64(&args, "max_results").unwrap_or(50).clamp(1, 500) as usize;
                let regex = args.get("regex").and_then(Value::as_bool).unwrap_or(false);
                let hits =
                    ctx.vault.search(str_arg(&args, "query")?, regex, opt_str(&args, "path").unwrap_or(""), max)?;
                if hits.is_empty() {
                    return Ok("No matches.".into());
                }
                let lines: Vec<String> = hits
                    .iter()
                    .map(|h| match h.line {
                        0 => format!("{} (name matches)", h.path),
                        n => format!("{}:{n}: {}", h.path, h.text),
                    })
                    .collect();
                Ok(lines.join("\n"))
            }
            Kind::Write => {
                let content = str_arg(&args, "content")?.to_string();
                self.save(str_arg(&args, "path")?, content, str_arg(&args, "summary")?, review_arg(&args))
                    .await
            }
            Kind::Edit => {
                let path = str_arg(&args, "path")?;
                let old = str_arg(&args, "old_text")?;
                if old.is_empty() {
                    bail!("old_text must not be empty");
                }
                let current = ctx.vault.read(path)?.with_context(|| format!("no note at {path}"))?;
                match current.matches(old).count() {
                    0 => bail!("old_text was not found in {path}; read the note and copy the text exactly"),
                    1 => {}
                    n => bail!("old_text occurs {n} times in {path}; include more surrounding text"),
                }
                let updated = current.replacen(old, str_arg(&args, "new_text")?, 1);
                self.save(path, updated, str_arg(&args, "summary")?, review_arg(&args)).await
            }
            Kind::CreateFolder => {
                let path = str_arg(&args, "path")?;
                ctx.vault.create_folder(path)?;
                Ok(format!("Created folder {path}."))
            }
            Kind::ApplyPending => {
                let id = str_arg(&args, "change_id")?;
                let proposal = ctx.take_pending(id)?;
                if ctx.user_turn <= proposal.user_turn {
                    ctx.pending.lock().push(proposal);
                    bail!("the caller has not replied since this change was proposed; end your turn and wait for approval");
                }
                if ctx.vault.read(&proposal.path)? != proposal.before {
                    bail!("{} changed since the proposal; read it again and propose a new change", proposal.path);
                }
                ctx.write(&proposal.path, proposal.after, &proposal.summary)
            }
            Kind::DiscardPending => {
                let proposal = ctx.take_pending(str_arg(&args, "change_id")?)?;
                Ok(format!("Discarded proposed change to {}.", proposal.path))
            }
        }
    }
}

impl VaultTool {
    /// Writes directly, or routes the change through review when asked to
    /// or when it touches an existing AGENTS.md (never edited unasked).
    async fn save(&self, path: &str, after: String, summary: &str, review: bool) -> Result<String> {
        let ctx = &self.ctx;
        let path = ctx.vault.relative(&ctx.vault.resolve(path)?);
        let before = ctx.vault.read(&path)?;
        if before.as_deref() == Some(after.as_str()) {
            return Ok(format!("{path} already has this content; nothing changed."));
        }
        let is_rules = before.is_some() && path.rsplit('/').next().is_some_and(|n| n.eq_ignore_ascii_case("AGENTS.md"));
        if !review && !is_rules {
            return ctx.write(&path, after, summary);
        }
        let proposal = Proposal {
            id: crate::new_id("p"),
            path: path.clone(),
            before,
            after,
            summary: summary.to_string(),
            user_turn: ctx.user_turn,
        };
        match &ctx.review {
            Review::Interactive(reviewer) => match reviewer.review(&proposal).await {
                Decision::Approve => Ok(format!("Approved. {}", ctx.write(&path, proposal.after, summary)?)),
                Decision::Reject(feedback) if feedback.trim().is_empty() => {
                    Ok("The user rejected this change; nothing was written.".into())
                }
                Decision::Reject(feedback) => {
                    Ok(format!("The user rejected this change; nothing was written. Their feedback: {feedback}"))
                }
            },
            Review::Deferred => {
                let id = proposal.id.clone();
                ctx.pending.lock().push(proposal);
                ctx.record(&path, NoteKind::Proposed, Some(id.clone()));
                Ok(format!(
                    "Proposed change {id} to {path} is pending and has NOT been written. The diff is shown to the \
                     caller with your reply. Ask them to approve it, end your turn, and call apply_pending_change \
                     only after they approve in a later message."
                ))
            }
        }
    }
}

impl TurnContext {
    fn record(&self, path: &str, kind: NoteKind, id: Option<String>) {
        let path = self.vault.resolve(path).map(|p| self.vault.relative(&p)).unwrap_or_else(|_| path.into());
        self.notes.lock().push(NoteEvent { path, kind, id });
    }

    fn write(&self, path: &str, content: String, summary: &str) -> Result<String> {
        let entry = self.journal.apply(&self.vault, path, Some(content), summary, Some(&self.conversation_id))?;
        self.record(&entry.path, NoteKind::Changed, Some(entry.id.clone()));
        Ok(format!("Saved {} (change {}).", entry.path, entry.id))
    }

    fn take_pending(&self, id: &str) -> Result<Proposal> {
        let mut pending = self.pending.lock();
        let index = pending
            .iter()
            .position(|p| p.id == id)
            .with_context(|| format!("no pending change {id}"))?;
        Ok(pending.remove(index))
    }
}

fn window(content: &str, offset: Option<u64>, limit: Option<u64>) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let start = offset.unwrap_or(1).max(1) as usize;
    let limit = limit.unwrap_or(2000).max(1) as usize;
    if start == 1 && lines.len() <= limit {
        return content.to_string();
    }
    if start > lines.len() {
        return format!("[the note has only {} lines]", lines.len());
    }
    let end = (start - 1 + limit).min(lines.len());
    let body = lines[start - 1..end].join("\n");
    format!("[lines {start}-{end} of {}; use offset to read more]\n{body}", lines.len())
}

fn str_arg<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    opt_str(args, key).with_context(|| format!("missing string argument `{key}`"))
}

fn opt_str<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str)
}

fn opt_u64(args: &Value, key: &str) -> Option<u64> {
    args.get(key).and_then(Value::as_u64)
}

fn review_arg(args: &Value) -> bool {
    args.get("review").and_then(Value::as_bool).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(review: Review, user_turn: usize) -> (tempfile::TempDir, Arc<TurnContext>) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("vault")).unwrap();
        std::fs::write(dir.path().join("vault/AGENTS.md"), "rules\n").unwrap();
        std::fs::write(dir.path().join("vault/a.md"), "one\ntwo\ntwo\n").unwrap();
        let ctx = TurnContext {
            vault: Arc::new(Vault::open(dir.path().join("vault")).unwrap()),
            journal: Arc::new(Journal::open(dir.path().join("changes.jsonl"))),
            review,
            conversation_id: "c1".into(),
            user_turn,
            pending: Mutex::new(Vec::new()),
            notes: Mutex::new(Vec::new()),
        };
        (dir, Arc::new(ctx))
    }

    fn tool(ctx: &Arc<TurnContext>, name: &str) -> Arc<dyn Tool> {
        toolset(ctx.clone()).into_iter().find(|t| t.spec().name == name).unwrap()
    }

    #[tokio::test]
    async fn edit_requires_a_unique_match() {
        let (_d, ctx) = ctx(Review::Deferred, 1);
        let edit = tool(&ctx, "edit_note");
        let err = edit.call(json!({"path":"a.md","old_text":"two","new_text":"2","summary":"s"})).await.unwrap_err();
        assert!(err.to_string().contains("occurs 2 times"));
        edit.call(json!({"path":"a.md","old_text":"one","new_text":"1","summary":"s"})).await.unwrap();
        assert_eq!(ctx.vault.read("a.md").unwrap().unwrap(), "1\ntwo\ntwo\n");
        assert_eq!(ctx.notes.lock()[0].kind, NoteKind::Changed);
    }

    #[tokio::test]
    async fn deferred_review_waits_for_a_later_user_turn() {
        let (_d, ctx) = ctx(Review::Deferred, 1);
        let out = tool(&ctx, "write_note")
            .call(json!({"path":"inbox/n.md","content":"hi","summary":"s","review":true}))
            .await
            .unwrap();
        assert!(out.contains("has NOT been written"));
        assert_eq!(ctx.vault.read("inbox/n.md").unwrap(), None);
        let id = ctx.pending.lock()[0].id.clone();
        let err = tool(&ctx, "apply_pending_change").call(json!({"change_id": id})).await.unwrap_err();
        assert!(err.to_string().contains("has not replied"));
        assert_eq!(ctx.pending.lock().len(), 1, "a refused apply keeps the proposal");

        // Next turn: the caller has replied.
        let next = TurnContext {
            vault: ctx.vault.clone(),
            journal: ctx.journal.clone(),
            review: Review::Deferred,
            conversation_id: "c1".into(),
            user_turn: 2,
            pending: Mutex::new(ctx.pending.lock().clone()),
            notes: Mutex::new(Vec::new()),
        };
        let next = Arc::new(next);
        tool(&next, "apply_pending_change").call(json!({"change_id": id})).await.unwrap();
        assert_eq!(next.vault.read("inbox/n.md").unwrap().as_deref(), Some("hi"));
        assert!(next.pending.lock().is_empty());
    }

    #[tokio::test]
    async fn existing_agents_md_is_never_written_without_review() {
        let (_d, ctx) = ctx(Review::Deferred, 1);
        let out = tool(&ctx, "write_note")
            .call(json!({"path":"AGENTS.md","content":"new rules","summary":"s"}))
            .await
            .unwrap();
        assert!(out.contains("pending"));
        assert_eq!(ctx.vault.read("AGENTS.md").unwrap().as_deref(), Some("rules\n"));
    }

    #[test]
    fn window_pages_long_notes() {
        let text = "a\nb\nc\nd";
        assert_eq!(window(text, None, None), text);
        assert_eq!(window(text, Some(2), Some(2)), "[lines 2-3 of 4; use offset to read more]\nb\nc");
        assert_eq!(window(text, Some(9), None), "[the note has only 4 lines]");
    }
}
