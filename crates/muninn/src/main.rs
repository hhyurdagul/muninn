mod cli;
mod integrations;
mod mcp;
mod tui;

use anyhow::Result;
use clap::{Parser, Subcommand};

/// Muninn: your knowledge vault, reachable from anywhere.
///
/// Run with no arguments to open a conversation in your vault.
#[derive(Parser)]
#[command(name = "muninn", version, about)]
struct Args {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// One prompt in, one result out: save to or ask the vault from any folder.
    /// Reads the prompt from stdin when none is given.
    Ask {
        /// The prompt; quote it or pass it as separate words.
        prompt: Vec<String>,
    },
    /// Print MEMORY.md, the short memory about you that agents load when they
    /// start (for agents that take it from a session-start hook).
    Memory,
    /// Serve MCP over stdio (your coding agent starts this).
    Mcp,
    /// Restart onboarding: vault path, model, agent integration, vault rules.
    Init,
    /// Revert a change Muninn made (the latest one by default).
    Undo {
        /// Change id, as shown by `muninn log`.
        id: Option<String>,
    },
    /// List recent changes Muninn made to the vault.
    Log {
        /// Number of changes to show.
        #[arg(short = 'n', long, default_value_t = 20)]
        limit: usize,
    },
}

#[tokio::main]
async fn main() {
    let result: Result<i32> = match Args::parse().command {
        None => tui::run(tui::Start::Default).await.map(|_| 0),
        Some(Command::Init) => tui::run(tui::Start::Onboarding).await.map(|_| 0),
        Some(Command::Ask { prompt }) => cli::ask(prompt).await,
        Some(Command::Memory) => cli::memory(),
        Some(Command::Mcp) => mcp::serve().await.map(|_| 0),
        Some(Command::Undo { id }) => cli::undo(id.as_deref()),
        Some(Command::Log { limit }) => cli::log(limit),
    };
    match result {
        Ok(code) => std::process::exit(code),
        Err(err) => {
            eprintln!("muninn: {err:#}");
            std::process::exit(1);
        }
    }
}
