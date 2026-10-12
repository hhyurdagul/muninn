//! A note vault as a sandbox: every path is vault-relative, hidden entries
//! (`.git`, `.obsidian`, ...) are off limits, and nothing escapes the root,
//! including through symlinks.

mod journal;

use std::fs;
use std::path::{Component, Path, PathBuf};

use anyhow::{bail, Context, Result};
use regex::RegexBuilder;
use walkdir::WalkDir;

pub use journal::{unified_diff, Entry, Journal, Undo};

const MAX_TEXT_BYTES: u64 = 4 * 1024 * 1024;

pub struct Vault {
    root: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub path: String,
    /// 1-based line number; 0 when the file name matched.
    pub line: usize,
    pub text: String,
}

impl Vault {
    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        let root = root.as_ref();
        let root = root.canonicalize().with_context(|| format!("vault {} does not exist", root.display()))?;
        if !root.is_dir() {
            bail!("vault {} is not a directory", root.display());
        }
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Maps a vault-relative path (or an absolute path inside the vault) to an
    /// absolute path, rejecting anything outside the vault or hidden.
    pub fn resolve(&self, path: &str) -> Result<PathBuf> {
        let raw = Path::new(path.trim());
        let rel = if raw.is_absolute() {
            raw.strip_prefix(&self.root).with_context(|| format!("{path} is outside the vault"))?
        } else {
            raw
        };
        let mut abs = self.root.clone();
        for component in rel.components() {
            match component {
                Component::Normal(name) if name.to_string_lossy().starts_with('.') => {
                    bail!("{path}: hidden files and folders are off limits")
                }
                Component::Normal(name) => abs.push(name),
                Component::CurDir => {}
                _ => bail!("{path}: path must stay inside the vault"),
            }
        }
        // Symlinks: the deepest existing ancestor must still be inside the vault.
        let existing = abs.ancestors().find(|p| p.exists()).unwrap_or(&self.root);
        if !existing.canonicalize()?.starts_with(&self.root) {
            bail!("{path} points outside the vault");
        }
        Ok(abs)
    }

    /// Vault-relative display form of an absolute path, with `/` separators.
    pub fn relative(&self, abs: &Path) -> String {
        let rel = abs.strip_prefix(&self.root).unwrap_or(abs);
        rel.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/")
    }

    /// Reads a text note; `None` when it does not exist.
    pub fn read(&self, path: &str) -> Result<Option<String>> {
        let abs = self.resolve(path)?;
        if !abs.exists() {
            return Ok(None);
        }
        if abs.is_dir() {
            bail!("{path} is a folder, not a note");
        }
        read_text(&abs).map(Some)
    }

    /// Creates, overwrites (`Some`) or deletes (`None`) a note. Parent
    /// folders are created as needed; prefer [`Journal::apply`] so the change
    /// can be undone.
    pub fn put(&self, path: &str, content: Option<&str>) -> Result<()> {
        let abs = self.resolve(path)?;
        if abs == self.root || abs.is_dir() {
            bail!("{path} is a folder, not a note");
        }
        match content {
            Some(text) => {
                if let Some(parent) = abs.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::write(&abs, text).with_context(|| format!("cannot write {path}"))
            }
            None if abs.exists() => fs::remove_file(&abs).with_context(|| format!("cannot delete {path}")),
            None => Ok(()),
        }
    }

    pub fn create_folder(&self, path: &str) -> Result<()> {
        let abs = self.resolve(path)?;
        fs::create_dir_all(&abs).with_context(|| format!("cannot create {path}"))
    }

    /// Indented tree of `dir` down to `depth` levels; deeper folders show a
    /// file count. At most `max_lines` lines.
    pub fn tree(&self, dir: &str, depth: usize, max_lines: usize) -> Result<String> {
        let base = self.resolve(dir)?;
        if !base.is_dir() {
            bail!("{dir} is not a folder");
        }
        let mut lines = Vec::new();
        let mut total = 0;
        self.tree_into(&base, 0, depth.max(1), max_lines, &mut lines, &mut total)?;
        if total > lines.len() {
            lines.push(format!("… {} more entries", total - lines.len()));
        }
        Ok(lines.join("\n"))
    }

    fn tree_into(
        &self,
        dir: &Path,
        level: usize,
        depth: usize,
        max: usize,
        out: &mut Vec<String>,
        total: &mut usize,
    ) -> Result<()> {
        let indent = "  ".repeat(level);
        for (path, is_dir) in visible_children(dir)? {
            *total += 1;
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            if is_dir {
                if level + 1 < depth {
                    if out.len() < max {
                        out.push(format!("{indent}{name}/"));
                    }
                    self.tree_into(&path, level + 1, depth, max, out, total)?;
                } else if out.len() < max {
                    let files = WalkDir::new(&path)
                        .into_iter()
                        .filter_entry(|e| !is_hidden(e.path()))
                        .filter_map(Result::ok)
                        .filter(|e| e.file_type().is_file())
                        .count();
                    out.push(format!("{indent}{name}/ ({files} files)"));
                }
            } else if out.len() < max {
                out.push(format!("{indent}{name}"));
            }
        }
        Ok(())
    }

    /// Case-insensitive search over file names and contents under `dir`.
    /// `query` is a regex when `regex` is set, else a literal.
    pub fn search(&self, query: &str, regex: bool, dir: &str, max: usize) -> Result<Vec<Hit>> {
        let pattern = if regex { query.to_string() } else { regex::escape(query) };
        let re = RegexBuilder::new(&pattern).case_insensitive(true).build().context("invalid regex")?;
        let base = self.resolve(dir)?;
        let mut hits = Vec::new();
        let walker = WalkDir::new(&base)
            .sort_by_file_name()
            .into_iter()
            .filter_entry(|e| e.path() == base || !is_hidden(e.path()));
        'files: for entry in walker.filter_map(Result::ok).filter(|e| e.file_type().is_file()) {
            let rel = self.relative(entry.path());
            if re.is_match(&rel) {
                hits.push(Hit { path: rel.clone(), line: 0, text: String::new() });
            }
            let Ok(text) = read_text(entry.path()) else { continue };
            for (i, line) in text.lines().enumerate() {
                if hits.len() >= max {
                    break 'files;
                }
                if re.is_match(line) {
                    hits.push(Hit { path: rel.clone(), line: i + 1, text: clip(line.trim(), 200) });
                }
            }
        }
        hits.truncate(max);
        Ok(hits)
    }
}

fn visible_children(dir: &Path) -> Result<Vec<(PathBuf, bool)>> {
    let mut children: Vec<(PathBuf, bool)> = fs::read_dir(dir)?
        .filter_map(Result::ok)
        .filter(|e| !is_hidden(&e.path()))
        .map(|e| {
            let path = e.path();
            let is_dir = path.is_dir();
            (path, is_dir)
        })
        .collect();
    children.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    Ok(children)
}

fn is_hidden(path: &Path) -> bool {
    path.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.'))
}

fn read_text(path: &Path) -> Result<String> {
    let meta = fs::metadata(path)?;
    if meta.len() > MAX_TEXT_BYTES {
        bail!("file is too large ({} bytes)", meta.len());
    }
    let bytes = fs::read(path)?;
    if bytes.contains(&0) {
        bail!("not a text file");
    }
    String::from_utf8(bytes).context("not a UTF-8 text file")
}

fn clip(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", &text[..i]),
        None => text.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vault() -> (tempfile::TempDir, Vault) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("inbox")).unwrap();
        fs::create_dir_all(root.join("projects/deep/er")).unwrap();
        fs::create_dir_all(root.join(".obsidian")).unwrap();
        fs::write(root.join("AGENTS.md"), "# Rules\nSave to inbox.\n").unwrap();
        fs::write(root.join("inbox/Idea.md"), "Raven memory\nsecond line\n").unwrap();
        fs::write(root.join("projects/deep/er/x.md"), "nothing\n").unwrap();
        fs::write(root.join(".obsidian/app.json"), "raven").unwrap();
        let vault = Vault::open(root).unwrap();
        (dir, vault)
    }

    #[test]
    fn resolve_rejects_escapes_and_hidden_paths() {
        let (_dir, vault) = vault();
        assert!(vault.resolve("inbox/new.md").is_ok());
        assert!(vault.resolve("./inbox/../../etc/passwd").is_err());
        assert!(vault.resolve("/etc/passwd").is_err());
        assert!(vault.resolve(".obsidian/app.json").is_err());
        let inside = vault.root().join("inbox/Idea.md");
        assert_eq!(vault.resolve(inside.to_str().unwrap()).unwrap(), inside);
    }

    #[cfg(unix)]
    #[test]
    fn resolve_rejects_symlinks_leading_outside() {
        let (_dir, vault) = vault();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), vault.root().join("link")).unwrap();
        assert!(vault.resolve("link/secret.md").is_err());
        assert!(vault.put("link/secret.md", Some("x")).is_err());
    }

    #[test]
    fn tree_lists_dirs_first_and_summarizes_beyond_depth() {
        let (_dir, vault) = vault();
        let tree = vault.tree("", 2, 100).unwrap();
        assert_eq!(tree, "inbox/\n  Idea.md\nprojects/\n  deep/ (1 files)\nAGENTS.md");
        let capped = vault.tree("", 2, 2).unwrap();
        assert!(capped.ends_with("… 3 more entries"), "{capped}");
    }

    #[test]
    fn search_matches_names_and_lines_case_insensitively_skipping_hidden() {
        let (_dir, vault) = vault();
        let hits = vault.search("raven", false, "", 50).unwrap();
        assert_eq!(hits, vec![Hit { path: "inbox/Idea.md".into(), line: 1, text: "Raven memory".into() }]);
        let by_name = vault.search("idea", false, "", 50).unwrap();
        assert_eq!(by_name[0].line, 0);
        assert_eq!(vault.search("^(second|nothing)", true, "", 1).unwrap().len(), 1);
        assert!(vault.search("(", true, "", 5).is_err());
    }

    #[test]
    fn put_creates_parents_and_deletes() {
        let (_dir, vault) = vault();
        vault.put("a/b/c.md", Some("hi")).unwrap();
        assert_eq!(vault.read("a/b/c.md").unwrap().as_deref(), Some("hi"));
        vault.put("a/b/c.md", None).unwrap();
        assert_eq!(vault.read("a/b/c.md").unwrap(), None);
        assert!(vault.put("inbox", Some("x")).is_err());
    }
}
