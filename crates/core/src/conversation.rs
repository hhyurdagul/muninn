use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Local};
use muninn_ai::{Message, Role};
use serde::{Deserialize, Serialize};

/// Where a conversation is happening; it shapes the prompt and review flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Surface {
    Cli,
    Mcp,
    Tui,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    #[default]
    Chat,
    /// First-run conversation that works out the vault's rules.
    Onboarding,
}

/// A change waiting for the caller's approval.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Proposal {
    pub id: String,
    pub path: String,
    pub before: Option<String>,
    pub after: String,
    pub summary: String,
    /// Number of user messages when proposed; approval must come later.
    pub user_turn: usize,
}

impl Proposal {
    pub fn diff(&self) -> String {
        muninn_vault::unified_diff(&self.path, self.before.as_deref(), Some(&self.after))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NoteKind {
    Read,
    Changed,
    Proposed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoteEvent {
    pub path: String,
    pub kind: NoteKind,
    /// Change-log id for `Changed`, proposal id for `Proposed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Conversation {
    pub id: String,
    pub title: String,
    pub surface: Surface,
    #[serde(default)]
    pub kind: Kind,
    pub vault: PathBuf,
    pub created: DateTime<Local>,
    pub updated: DateTime<Local>,
    pub messages: Vec<Message>,
    #[serde(default)]
    pub pending: Vec<Proposal>,
    /// Notes read or changed, in order, for display.
    #[serde(default)]
    pub notes: Vec<NoteEvent>,
}

impl Conversation {
    pub fn new(surface: Surface, kind: Kind, vault: &Path) -> Self {
        let now = Local::now();
        Self {
            id: crate::new_id("c"),
            title: String::new(),
            surface,
            kind,
            vault: vault.to_path_buf(),
            created: now,
            updated: now,
            messages: Vec::new(),
            pending: Vec::new(),
            notes: Vec::new(),
        }
    }

    pub fn user_turns(&self) -> usize {
        self.messages.iter().filter(|m| m.role == Role::User).count()
    }
}

/// Conversations as JSON files in one directory.
pub struct Store {
    dir: PathBuf,
}

impl Store {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub fn save(&self, conversation: &Conversation) -> Result<()> {
        fs::create_dir_all(&self.dir)?;
        let file = self.file(&conversation.id)?;
        let tmp = file.with_extension("json.tmp");
        fs::write(&tmp, serde_json::to_vec_pretty(conversation)?)?;
        fs::rename(&tmp, &file)?;
        Ok(())
    }

    pub fn load(&self, id: &str) -> Result<Conversation> {
        let file = self.file(id)?;
        let bytes = fs::read(&file).with_context(|| format!("no conversation with id {id}"))?;
        serde_json::from_slice(&bytes).with_context(|| format!("corrupt conversation {}", file.display()))
    }

    /// Conversations in `vault`, most recently updated first.
    pub fn list(&self, vault: &Path) -> Result<Vec<Conversation>> {
        if !self.dir.exists() {
            return Ok(Vec::new());
        }
        let mut all: Vec<Conversation> = fs::read_dir(&self.dir)?
            .filter_map(Result::ok)
            .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
            .filter_map(|e| serde_json::from_slice(&fs::read(e.path()).ok()?).ok())
            .filter(|c: &Conversation| c.vault == vault)
            .collect();
        all.sort_by(|a, b| b.updated.cmp(&a.updated));
        Ok(all)
    }

    fn file(&self, id: &str) -> Result<PathBuf> {
        anyhow::ensure!(
            !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'),
            "invalid conversation id {id:?}"
        );
        Ok(self.dir.join(format!("{id}.json")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_lists_by_vault_newest_first_and_rejects_bad_ids() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path());
        let mut old = Conversation::new(Surface::Tui, Kind::Chat, Path::new("/v"));
        old.updated = Local::now() - chrono::Duration::hours(1);
        let new = Conversation::new(Surface::Mcp, Kind::Chat, Path::new("/v"));
        let other = Conversation::new(Surface::Tui, Kind::Chat, Path::new("/other"));
        for c in [&old, &new, &other] {
            store.save(c).unwrap();
        }
        let ids: Vec<_> = store.list(Path::new("/v")).unwrap().into_iter().map(|c| c.id).collect();
        assert_eq!(ids, [new.id.clone(), old.id]);
        assert_eq!(store.load(&new.id).unwrap().surface, Surface::Mcp);
        assert!(store.load("../etc/passwd").is_err());
    }
}
