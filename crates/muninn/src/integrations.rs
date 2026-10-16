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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Missing,
    /// Registered with this muninn.
    Current,
    /// Registered with another command, often an older build.
    Stale(String),
}

#[derive(Debug, Clone)]
pub struct Integration {
    pub agent: Agent,
    pub status: Status,
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

    /// The user-level file that lists the agent's MCP servers, honouring the
    /// variable that moves the agent's config directory.
    fn config(self, home: &Path) -> PathBuf {
        let (var, default, file) = match self {
            Agent::ClaudeCode => ("CLAUDE_CONFIG_DIR", home.to_path_buf(), ".claude.json"),
            Agent::Codex => ("CODEX_HOME", home.join(".codex"), "config.toml"),
            Agent::Omp => ("PI_CODING_AGENT_DIR", home.join(".omp/agent"), "mcp.json"),
        };
        std::env::var_os(var).filter(|v| !v.is_empty()).map(PathBuf::from).unwrap_or(default).join(file)
    }

    /// The command registered as the `muninn` server, if any.
    fn registered(self, home: &Path) -> Option<String> {
        let text = fs::read_to_string(self.config(home)).ok()?;
        let command = match self {
            Agent::Codex => {
                text.parse::<toml::Table>().ok()?.get("mcp_servers")?.get("muninn")?.get("command")?.clone()
            }
            _ => {
                return serde_json::from_str::<Value>(&text)
                    .ok()?
                    .pointer("/mcpServers/muninn/command")?
                    .as_str()
                    .map(Into::into)
            }
        };
        command.as_str().map(Into::into)
    }

    fn status(self, home: &Path, current: &str) -> Status {
        match self.registered(home) {
            None => Status::Missing,
            Some(command) if command == current => Status::Current,
            Some(command) => Status::Stale(command),
        }
    }
}

/// Agents whose executable is on `PATH`.
pub fn detect() -> Vec<Integration> {
    let home = dirs::home_dir().unwrap_or_default();
    let current = muninn_command().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
    Agent::ALL
        .into_iter()
        .filter(|a| which(a.binary()).is_some())
        .map(|agent| Integration { agent, status: agent.status(&home, &current) })
        .collect()
}

/// Registers `<muninn> mcp` as a user-level MCP server named `muninn`,
/// replacing an earlier registration.
pub fn install(agent: Agent) -> Result<()> {
    let exe = muninn_command()?;
    let exe = exe.to_string_lossy();
    match agent {
        Agent::ClaudeCode => {
            let _ = run(agent.binary(), &["mcp", "remove", "--scope", "user", "muninn"]);
            run(agent.binary(), &["mcp", "add", "--scope", "user", "muninn", "--", &exe, "mcp"])
        }
        Agent::Codex => {
            let _ = run(agent.binary(), &["mcp", "remove", "muninn"]);
            run(agent.binary(), &["mcp", "add", "muninn", "--", &exe, "mcp"])
        }
        Agent::Omp => {
            let home = dirs::home_dir().context("no home directory")?;
            add_json_server(&agent.config(&home), &exe)
        }
    }
}

fn run(program: &str, args: &[&str]) -> Result<()> {
    let output = Command::new(program).args(args).output().with_context(|| format!("cannot run {program}"))?;
    if !output.status.success() {
        bail!("{program} {}: {}", args.join(" "), String::from_utf8_lossy(&output.stderr).trim());
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

/// The `muninn` on PATH if there is one, else the path this one was started
/// by. Symlinks are kept, so `result/bin/muninn` or a profile link still
/// works after a rebuild, unlike the `/nix/store` path it resolves to.
fn muninn_command() -> Result<PathBuf> {
    if let Some(path) = which("muninn") {
        return Ok(path);
    }
    match std::env::args_os().next().map(PathBuf::from) {
        Some(arg) if arg.components().count() > 1 => Ok(std::path::absolute(arg)?),
        _ => std::env::current_exe().context("cannot locate the muninn executable"),
    }
}

fn which(binary: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?).map(|dir| dir.join(binary)).find(|p| p.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_install_preserves_existing_servers_and_is_detected() {
        let home = tempfile::tempdir().unwrap();
        let file = Agent::Omp.config(home.path());
        assert_eq!(Agent::Omp.status(home.path(), "/bin/muninn"), Status::Missing);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, r#"{"mcpServers":{"other":{"command":"x"}},"keep":1}"#).unwrap();
        add_json_server(&file, "/bin/muninn").unwrap();
        let config: Value = serde_json::from_str(&fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(config["mcpServers"]["other"]["command"], "x");
        assert_eq!(config["mcpServers"]["muninn"]["args"][0], "mcp");
        assert_eq!(config["keep"], 1);
        assert_eq!(Agent::Omp.status(home.path(), "/bin/muninn"), Status::Current);
        assert_eq!(Agent::Omp.status(home.path(), "/new/muninn"), Status::Stale("/bin/muninn".into()));
    }

    #[test]
    fn reads_the_command_codex_registered() {
        let home = tempfile::tempdir().unwrap();
        let file = home.path().join(".codex/config.toml");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, "model = \"x\"\n\n[mcp_servers.muninn]\ncommand = \"/old/muninn\"\nargs = [\"mcp\"]\n")
            .unwrap();
        assert_eq!(Agent::Codex.status(home.path(), "/old/muninn"), Status::Current);
        assert_eq!(Agent::Codex.status(home.path(), "/new/muninn"), Status::Stale("/old/muninn".into()));
    }
}
