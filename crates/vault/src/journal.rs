//! Append-only log of every note change, with before/after content, so any
//! change can be reverted later unless the note was edited since.

use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};

use crate::Vault;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    pub time: DateTime<Local>,
    pub vault: PathBuf,
    pub path: String,
    /// `None`: the note did not exist.
    pub before: Option<String>,
    /// `None`: the note was deleted.
    pub after: Option<String>,
    pub summary: String,
    /// Free-form origin tag, e.g. a conversation id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    /// Set when this entry undoes another entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reverts: Option<String>,
}

#[derive(Debug)]
pub enum Undo {
    Reverted { original: Entry, undo: Entry },
    /// The note changed after the entry was made; nothing was touched.
    Conflict { original: Entry },
}

pub struct Journal {
    file: PathBuf,
}

impl Journal {
    pub fn open(file: impl Into<PathBuf>) -> Self {
        Self { file: file.into() }
    }

    /// Writes `after` to `path` (deleting on `None`) and records the change.
    pub fn apply(
        &self,
        vault: &Vault,
        path: &str,
        after: Option<String>,
        summary: &str,
        origin: Option<&str>,
    ) -> Result<Entry> {
        let abs = vault.resolve(path)?;
        let entry = Entry {
            id: new_id(),
            time: Local::now(),
            vault: vault.root().to_path_buf(),
            path: vault.relative(&abs),
            before: vault.read(path)?,
            after,
            summary: summary.to_string(),
            origin: origin.map(str::to_string),
            reverts: None,
        };
        self.commit(vault, entry)
    }

    fn commit(&self, vault: &Vault, entry: Entry) -> Result<Entry> {
        vault.put(&entry.path, entry.after.as_deref())?;
        if let Some(dir) = self.file.parent() {
            fs::create_dir_all(dir)?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.file)
            .with_context(|| format!("cannot open change log {}", self.file.display()))?;
        writeln!(file, "{}", serde_json::to_string(&entry)?)?;
        Ok(entry)
    }

    /// All entries, oldest first.
    pub fn entries(&self) -> Result<Vec<Entry>> {
        if !self.file.exists() {
            return Ok(Vec::new());
        }
        fs::read_to_string(&self.file)?
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).context("corrupt change log line"))
            .collect()
    }

    /// Reverts entry `id`, or the latest not-yet-reverted change in `vault`.
    pub fn undo(&self, vault: &Vault, id: Option<&str>) -> Result<Undo> {
        let entries = self.entries()?;
        let reverted: HashSet<&str> = entries.iter().filter_map(|e| e.reverts.as_deref()).collect();
        let original = match id {
            Some(id) => entries
                .iter()
                .find(|e| e.id == id)
                .with_context(|| format!("no change with id {id}"))?,
            None => entries
                .iter()
                .rev()
                .find(|e| e.vault == vault.root() && e.reverts.is_none() && !reverted.contains(e.id.as_str()))
                .context("nothing to undo")?,
        };
        if original.vault != vault.root() {
            bail!("change {} belongs to vault {}", original.id, original.vault.display());
        }
        if reverted.contains(original.id.as_str()) {
            bail!("change {} was already undone", original.id);
        }
        if vault.read(&original.path)? != original.after {
            return Ok(Undo::Conflict { original: original.clone() });
        }
        let undo = self.commit(
            vault,
            Entry {
                id: new_id(),
                time: Local::now(),
                vault: original.vault.clone(),
                path: original.path.clone(),
                before: original.after.clone(),
                after: original.before.clone(),
                summary: format!("undo: {}", original.summary),
                origin: None,
                reverts: Some(original.id.clone()),
            },
        )?;
        Ok(Undo::Reverted { original: original.clone(), undo })
    }

    pub fn path(&self) -> &Path {
        &self.file
    }
}

fn new_id() -> String {
    use std::sync::atomic::{AtomicU32, Ordering};
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let now = Local::now();
    let salt = (now.timestamp_subsec_nanos() ^ std::process::id().rotate_left(16))
        .wrapping_add(COUNTER.fetch_add(1, Ordering::Relaxed));
    format!("{}-{:04x}", now.format("%y%m%d%H%M%S"), salt & 0xffff)
}

/// Unified diff of a note change, for showing proposals to humans and agents.
pub fn unified_diff(path: &str, before: Option<&str>, after: Option<&str>) -> String {
    let diff = similar::TextDiff::from_lines(before.unwrap_or(""), after.unwrap_or(""));
    let old = if before.is_some() { format!("a/{path}") } else { "/dev/null".into() };
    let new = if after.is_some() { format!("b/{path}") } else { "/dev/null".into() };
    diff.unified_diff().context_radius(3).header(&old, &new).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (tempfile::TempDir, Vault, Journal) {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("vault")).unwrap();
        let vault = Vault::open(dir.path().join("vault")).unwrap();
        let journal = Journal::open(dir.path().join("data/changes.jsonl"));
        (dir, vault, journal)
    }

    #[test]
    fn undo_restores_before_and_latest_skips_reverted() {
        let (_d, vault, journal) = setup();
        let created = journal.apply(&vault, "n.md", Some("v1".into()), "create", Some("c1")).unwrap();
        journal.apply(&vault, "n.md", Some("v2".into()), "edit", None).unwrap();

        let Undo::Reverted { undo, .. } = journal.undo(&vault, None).unwrap() else { panic!() };
        assert_eq!(vault.read("n.md").unwrap().as_deref(), Some("v1"));
        assert_eq!(undo.summary, "undo: edit");

        let Undo::Reverted { original, .. } = journal.undo(&vault, None).unwrap() else { panic!() };
        assert_eq!(original.id, created.id);
        assert_eq!(vault.read("n.md").unwrap(), None, "undoing a creation deletes the note");
        assert!(journal.undo(&vault, None).unwrap_err().to_string().contains("nothing to undo"));
        assert!(journal.undo(&vault, Some(&created.id)).is_err(), "double undo");
    }

    #[test]
    fn undo_reports_conflict_when_note_edited_since() {
        let (_d, vault, journal) = setup();
        let entry = journal.apply(&vault, "n.md", Some("v1".into()), "create", None).unwrap();
        vault.put("n.md", Some("hand edit")).unwrap();
        assert!(matches!(journal.undo(&vault, Some(&entry.id)).unwrap(), Undo::Conflict { .. }));
        assert_eq!(vault.read("n.md").unwrap().as_deref(), Some("hand edit"));
    }

    #[test]
    fn undo_of_an_undo_redoes() {
        let (_d, vault, journal) = setup();
        journal.apply(&vault, "n.md", Some("v1".into()), "create", None).unwrap();
        let Undo::Reverted { undo, .. } = journal.undo(&vault, None).unwrap() else { panic!() };
        journal.undo(&vault, Some(&undo.id)).unwrap();
        assert_eq!(vault.read("n.md").unwrap().as_deref(), Some("v1"));
    }

    #[test]
    fn diff_marks_new_files() {
        let diff = unified_diff("n.md", None, Some("hello\n"));
        assert!(diff.starts_with("--- /dev/null\n+++ b/n.md\n"), "{diff}");
        assert!(diff.contains("+hello"));
    }
}
