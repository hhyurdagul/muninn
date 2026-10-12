//! Detect coding agents and register `muninn mcp` with them.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Agent {
    ClaudeCode,
    Codex,
    Omp,
}

#[derive(Debug, Clone)]
pub struct Integration {
    pub agent: Agent,
    pub installed: bool,
}

impl Agent {
    pub const ALL: [Agent; 3] = [Agent::ClaudeCode, Agent::Codex, Agent::Omp];

    pub fn name(self) -> &'static str {
        match self {
            Agent::ClaudeCode => "Claude Code",
            Agent::Codex => "Codex",
            Agent::Omp => "omp",
        }
    }

    fn binary(self) -> &'static str {
        match self {
            Agent::ClaudeCode => "claude",
            Agent::Codex => "codex",
            Agent::Omp => "omp",
        }
    }

    fn config(self, home: &Path) -> PathBuf {
        match self {
            Agent::ClaudeCode => home.join(".claude.json"),
            Agent::Codex => home.join(".codex/config.toml"),
            Agent::Omp => home.join(".omp/agent/mcp.json"),
        }
    }

    fn is_installed(self, home: &Path) -> bool {
        let text = fs::read_to_string(self.config(home)).unwrap_or_default();
        match self {
            Agent::Codex => text.contains("[mcp_servers.muninn]"),
            _ => serde_json::from_str::<Value>(&text).is_ok_and(|v| v.pointer("/mcpServers/muninn").is_some()),
        }
    }
}

/// Agents whose executable is on `PATH`.
pub fn detect() -> Vec<Integration> {
    let home = dirs::home_dir().unwrap_or_default();
    Agent::ALL
        .into_iter()
        .filter(|a| which(a.binary()).is_some())
        .map(|agent| Integration { agent, installed: agent.is_installed(&home) })
        .collect()
}

/// Registers `<muninn> mcp` as a user-level MCP server named `muninn`.
pub fn install(agent: Agent) -> Result<()> {
    let exe = muninn_command()?;
    let exe = exe.to_string_lossy();
    match agent {
        Agent::ClaudeCode => run(agent.binary(), &["mcp", "add", "--scope", "user", "muninn", "--", &exe, "mcp"]),
        Agent::Codex => run(agent.binary(), &["mcp", "add", "muninn", "--", &exe, "mcp"]),
        Agent::Omp => {
            let home = dirs::home_dir().context("no home directory")?;
            add_json_server(&agent.config(&home), &exe)
        }
    }
}

fn run(program: &str, args: &[&str]) -> Result<()> {
    let output = Command::new(program).args(args).output().with_context(|| format!("cannot run {program}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("already exists") {
            return Ok(());
        }
        bail!("{program} {}: {}", args.join(" "), stderr.trim());
    }
    Ok(())
}

/// Adds `mcpServers.muninn` to a JSON config, keeping everything else.
fn add_json_server(file: &Path, exe: &str) -> Result<()> {
    let mut config: Value = match fs::read_to_string(file) {
        Ok(text) => serde_json::from_str(&text).with_context(|| format!("{} is not valid JSON", file.display()))?,
        Err(_) => json!({}),
    };
    let servers = config
        .as_object_mut()
        .context("config root is not an object")?
        .entry("mcpServers")
        .or_insert_with(|| json!({}));
    servers
        .as_object_mut()
        .context("mcpServers is not an object")?
        .insert("muninn".into(), json!({ "type": "stdio", "command": exe, "args": ["mcp"] }));
    if let Some(dir) = file.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::write(file, serde_json::to_string_pretty(&config)? + "\n")?;
    Ok(())
}

/// The `muninn` on PATH if there is one (stable across rebuilds), else this
/// executable.
fn muninn_command() -> Result<PathBuf> {
    match which("muninn") {
        Some(path) => Ok(path),
        None => std::env::current_exe().context("cannot locate the muninn executable"),
    }
}

fn which(binary: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(binary))
        .find(|p| p.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_install_preserves_existing_servers_and_is_detected() {
        let home = tempfile::tempdir().unwrap();
        let file = Agent::Omp.config(home.path());
        assert!(!Agent::Omp.is_installed(home.path()));
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, r#"{"mcpServers":{"other":{"command":"x"}},"keep":1}"#).unwrap();
        add_json_server(&file, "/bin/muninn").unwrap();
        let config: Value = serde_json::from_str(&fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(config["mcpServers"]["other"]["command"], "x");
        assert_eq!(config["mcpServers"]["muninn"]["args"][0], "mcp");
        assert_eq!(config["keep"], 1);
        assert!(Agent::Omp.is_installed(home.path()));
    }
}
