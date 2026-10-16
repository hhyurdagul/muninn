//! One-shot commands: `ask`, `memory`, `model`, `undo`, `log`.

use std::collections::HashSet;
use std::io::{BufRead, IsTerminal, Read, Write};
use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use muninn_core::{
    check_model, list_models, CancellationToken, Conversation, Decision, Kind, Librarian, NoteKind, Paths, Proposal,
    ProviderKind, Review, Reviewer, Settings, Stop, Surface, Update,
};
use muninn_vault::{Entry, Journal, Undo, Vault};

const DIM: &str = "\x1b[2m";
const RESET: &str = "\x1b[0m";

pub async fn ask(words: Vec<String>) -> Result<i32> {
    let prompt = if words.is_empty() {
        let mut text = String::new();
        std::io::stdin().read_to_string(&mut text)?;
        text
    } else {
        words.join(" ")
    };
    anyhow::ensure!(!prompt.trim().is_empty(), "nothing to ask; usage: muninn ask \"...\"");
    let paths = Paths::from_env();
    let settings = Settings::load(&paths)?;
    anyhow::ensure!(settings.is_complete(), "Muninn is not set up yet; run `muninn` to onboard");
    let librarian = Librarian::from_settings(&settings, &paths)?;
    let mut conversation = Conversation::new(Surface::Cli, Kind::Chat, librarian.vault().root());

    // On a terminal, stream everything; when piped, print only the final answer.
    let live = std::io::stdout().is_terminal();
    let mut mid_line = false;
    let mut on_update = |update: Update| match update {
        Update::Text(delta) if live => {
            print!("{delta}");
            let _ = std::io::stdout().flush();
            mid_line = !delta.ends_with('\n');
        }
        Update::Tool { name, detail } => {
            if std::mem::take(&mut mid_line) {
                println!();
            }
            eprintln!("{DIM}· {name} {detail}{RESET}");
        }
        Update::Note(note) if note.kind == NoteKind::Changed => {
            eprintln!("{DIM}✎ {} (undo: muninn undo {}){RESET}", note.path, note.id.unwrap_or_default());
        }
        _ => {}
    };
    let review = Review::Interactive(Arc::new(TerminalReviewer));
    let result = librarian.turn(&mut conversation, &prompt, review, &mut on_update, &CancellationToken::new()).await?;
    if live {
        if mid_line {
            println!();
        }
    } else {
        println!("{}", result.reply);
    }
    if result.stop == Stop::StepLimit {
        eprintln!("muninn: stopped after too many steps");
        return Ok(1);
    }
    Ok(0)
}

/// Shows the diff and asks on the controlling terminal, so it works even
/// when stdin/stdout are piped.
struct TerminalReviewer;

#[async_trait]
impl Reviewer for TerminalReviewer {
    async fn review(&self, proposal: &Proposal) -> Decision {
        let proposal = proposal.clone();
        tokio::task::spawn_blocking(move || confirm(&proposal))
            .await
            .unwrap_or_else(|e| Err(anyhow::anyhow!(e)))
            .unwrap_or_else(|e| Decision::Reject(format!("could not ask for confirmation: {e:#}")))
    }
}

fn confirm(proposal: &Proposal) -> Result<Decision> {
    let mut tty =
        std::fs::OpenOptions::new().read(true).write(true).open("/dev/tty").context("no terminal to confirm on")?;
    writeln!(tty, "\n{} ({})", proposal.path, proposal.summary)?;
    for line in proposal.diff().lines() {
        let color = match line.chars().next() {
            Some('+') if !line.starts_with("+++") => "\x1b[32m",
            Some('-') if !line.starts_with("---") => "\x1b[31m",
            Some('@') => "\x1b[36m",
            _ => "",
        };
        writeln!(tty, "{color}{line}{RESET}")?;
    }
    write!(tty, "Apply this change? [y/N, or type feedback] ")?;
    tty.flush()?;
    let mut answer = String::new();
    std::io::BufReader::new(tty).read_line(&mut answer)?;
    Ok(match answer.trim() {
        "y" | "Y" | "yes" => Decision::Approve,
        "" | "n" | "N" | "no" => Decision::Reject(String::new()),
        feedback => Decision::Reject(feedback.to_string()),
    })
}

fn open_vault() -> Result<(Vault, Journal)> {
    let paths = Paths::from_env();
    let settings = Settings::load(&paths)?;
    Ok((settings.open_vault()?, Journal::open(paths.journal_file())))
}

/// Shows the current model, lists a provider's models, or switches to one
/// after a successful test call.
pub async fn model(provider: Option<String>, model: Option<String>, endpoint: Option<String>) -> Result<i32> {
    let paths = Paths::from_env();
    let mut settings = Settings::load(&paths)?;
    let Some(provider) = provider else {
        println!("{} · {}  ({})", settings.provider.id(), settings.model, settings.endpoint());
        return Ok(0);
    };
    let kind = ProviderKind::from_id(&provider).with_context(|| {
        let ids: Vec<_> = ProviderKind::ALL.iter().map(|k| k.id()).collect();
        format!("unknown provider `{provider}`; choose one of {}", ids.join(", "))
    })?;
    settings.provider = kind;
    if let Some(endpoint) = endpoint {
        settings.base_url = endpoint.trim_end_matches('/').to_string();
    }
    let entered = if kind.needs_key() && settings.api_key().is_none() {
        anyhow::ensure!(
            std::io::stdin().is_terminal(),
            "no API key for {}; set {} or run this in a terminal to enter one",
            kind.name(),
            kind.key_env().unwrap_or("MUNINN_API_KEY")
        );
        let hint = kind.key_url().map(|url| format!(" (get one at {url})")).unwrap_or_default();
        let key = read_secret(&format!("{} API key{hint}: ", kind.name()))?;
        anyhow::ensure!(!key.is_empty(), "no key entered; nothing changed");
        settings.set_key(kind, &key);
        Some(key)
    } else {
        None
    };
    let Some(model) = model else {
        for id in list_models(&settings).await? {
            println!("{id}");
        }
        // The key works: keep it, without switching providers.
        if let Some(key) = entered {
            let mut stored = Settings::load(&paths)?;
            stored.set_key(kind, &key);
            stored.save(&paths)?;
            eprintln!("Saved the {} key.", kind.name());
        }
        return Ok(0);
    };
    settings.model = model;
    check_model(&settings).await.context("test call failed; settings unchanged")?;
    settings.save(&paths)?;
    println!("Now using {} · {}", kind.id(), settings.model);
    Ok(0)
}

/// Reads a line from the terminal without showing it.
fn read_secret(prompt: &str) -> Result<String> {
    use muninn_tui::ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
    use muninn_tui::ratatui::crossterm::terminal;

    let mut tty = std::fs::OpenOptions::new().write(true).open("/dev/tty").context("no terminal")?;
    write!(tty, "{prompt}")?;
    tty.flush()?;
    terminal::enable_raw_mode()?;
    let mut secret = String::new();
    let result = loop {
        match event::read() {
            Ok(Event::Key(key)) if key.kind == KeyEventKind::Press => match key.code {
                KeyCode::Enter => break Ok(()),
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    break Err(anyhow::anyhow!("cancelled"))
                }
                KeyCode::Char(c) => {
                    secret.push(c);
                    let _ = write!(tty, "•");
                }
                KeyCode::Backspace if secret.pop().is_some() => {
                    let _ = write!(tty, "\x08 \x08");
                }
                _ => {}
            },
            Ok(Event::Paste(text)) => secret.push_str(&text),
            Ok(_) => {}
            Err(e) => break Err(e.into()),
        }
        let _ = tty.flush();
    };
    terminal::disable_raw_mode()?;
    writeln!(tty, "\r")?;
    result.map(|()| secret.trim().to_string())
}

/// Prints MEMORY.md (nothing when it is empty), e.g. for a session-start hook.
pub fn memory() -> Result<i32> {
    let (vault, _) = open_vault()?;
    if let Some(memory) = muninn_core::memory(&vault) {
        println!("{memory}");
    }
    Ok(0)
}

pub fn undo(id: Option<&str>) -> Result<i32> {
    let (vault, journal) = open_vault()?;
    let undo = journal.undo(&vault, id)?;
    match undo {
        Undo::Reverted { .. } => {
            println!("{}", describe_undo(&undo));
            Ok(0)
        }
        Undo::Conflict { .. } => {
            eprintln!("{}", describe_undo(&undo));
            Ok(2)
        }
    }
}

/// What an undo did, in one line.
pub(crate) fn describe_undo(undo: &Undo) -> String {
    match undo {
        Undo::Reverted { original, .. } => format!("Reverted {} [{}] {}", original.path, original.id, original.summary),
        Undo::Conflict { original } => {
            format!("Conflict: {} was edited after change {}; nothing was reverted.", original.path, original.id)
        }
    }
}

/// Changes Muninn made to `vault`, newest first, each with whether it was undone.
pub(crate) fn changes(vault: &Vault, journal: &Journal) -> Result<Vec<(Entry, bool)>> {
    let entries: Vec<_> = journal.entries()?.into_iter().filter(|e| e.vault == vault.root()).collect();
    let reverted: HashSet<String> = entries.iter().filter_map(|e| e.reverts.clone()).collect();
    Ok(entries
        .into_iter()
        .rev()
        .map(|e| {
            let undone = reverted.contains(&e.id);
            (e, undone)
        })
        .collect())
}

pub fn log(limit: usize) -> Result<i32> {
    let (vault, journal) = open_vault()?;
    let changes = changes(&vault, &journal)?;
    if changes.is_empty() {
        println!("No changes yet.");
    }
    for (entry, undone) in changes.iter().take(limit) {
        let mark = if *undone { " (undone)" } else { "" };
        println!("{}  {}  {}  {}{mark}", entry.id, entry.time.format("%Y-%m-%d %H:%M"), entry.path, entry.summary);
    }
    Ok(0)
}
