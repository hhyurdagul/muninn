# Muninn

Your knowledge vault, reachable from anywhere. Pin the vault once; then save to it, ask it, or talk with it from any folder or any coding agent. The design is in [Muninn.md](Muninn.md).

## Use

```sh
muninn                      # TUI conversation in your vault (onboarding on first run)
muninn init                 # restart onboarding
muninn ask "save: ravens remember faces"     # one prompt in, one result out
echo "what did I decide about storage?" | muninn ask
muninn memory               # print MEMORY.md, the short memory agents load at startup
muninn log                  # changes Muninn made
muninn undo [change-id]     # revert the latest (or a given) change
muninn mcp                  # MCP server over stdio, started by your agent
```

Onboarding asks for the vault folder and an OpenAI-compatible endpoint, model, and API key (OpenRouter by default), makes one test call, offers to register `muninn mcp` with Claude Code, Codex, and omp, and then works out the vault's rules in conversation. That includes whether Muninn should get rules of its own.

| Where | Path |
|---|---|
| Settings (vault, endpoint, model, key, ignore patterns) | `~/.config/muninn/config.toml` (`MUNINN_CONFIG_DIR`) |
| Conversations, change log | `~/.local/share/muninn/` (`MUNINN_DATA_DIR`) |
| API key override | `MUNINN_API_KEY` |

Muninn reads, searches, and writes notes inside the vault only, never outside the root. It has no shell. When the vault's rules require review, changes are shown first: in the TUI as an approve dialog, in `muninn ask` as a terminal prompt, and over MCP as a pending diff that the calling agent approves by replying in the same conversation (`conversation_id`).

### Vault rules

`AGENTS.md` at the vault root holds the rules every agent in the vault follows. If Muninn needs rules that other agents shouldn't follow (or the other way round), put those in `MUNINN.md` at the root. Muninn follows both, and `MUNINN.md` wins where they disagree. Onboarding asks whether you want the split. Muninn changes an existing `AGENTS.md` or root `MUNINN.md` only after review.

### Memory

`MEMORY.md` at the vault root is the short memory about you that every agent gets when it starts: who you are, what you're working on, standing preferences, and recent decisions. `muninn mcp` puts it in its MCP server instructions, so Claude Code and other MCP clients see it at the start of every session without calling a tool. Agents that don't read server instructions can load it with `muninn memory` from a session-start hook.

Muninn keeps the file up to date: ask it to remember something, or let it notice when a save changes one of these facts. Every change to `MEMORY.md` goes through review, because a wrong line would mislead every agent that loads it. The file is capped at 3,000 characters so that it fits in the 4,096 characters Claude Code keeps of server instructions. Details belong in notes, which `MEMORY.md` links to.

### Hidden files and ignore patterns

Hidden files and folders (`.obsidian`, `.agents`, …) are read-only for Muninn. They are left out of its listings and searches unless it asks for them (`list_notes` with `hidden=true`, or a search inside a named hidden folder), but it can read them by path. Muninn never writes them.

`ignore` in `config.toml` takes gitignore-style patterns, relative to the vault root, for paths Muninn can't list, search, read, or write. Muninn re-reads them before every turn, so edits apply without a restart:

```toml
ignore = [".git/", ".stversions/", "private/", "*.key", "!private/shared.md"]
```

The default is `[".git/"]`.

### In the TUI

Muninn's replies render as Markdown: headings, emphasis, lists and task lists, quotes and `> [!NOTE]` alerts, code blocks, tables, links, `[[wikilinks]]`, footnotes, and `$math$` (shown as source).

| Command | What it does |
|---|---|
| `/help` | Commands and keyboard shortcuts |
| `/new` | Start a new conversation (Ctrl+N) |
| `/resume` | Continue an earlier conversation (Ctrl+O) |
| `/log` | Recent changes Muninn made to the vault |
| `/undo [id]` | Revert the latest change, or the one with this id |
| `/init` | Redo setup and onboarding |
| `/quit`, `/exit` | Leave (Ctrl+C) |

Typing `/` opens the command list (↑↓ select, Tab complete, Enter run). Enter sends; Ctrl+Enter, Shift+Enter, Alt+Enter, or Ctrl+J insert a new line. Ctrl/Alt+←→ move by word, Ctrl/Alt+Backspace and Ctrl+W delete a word, Ctrl+Delete and Alt+D delete the next word, Ctrl+U and Ctrl+K delete to the line's start or end, and Ctrl+Home/End jump to the message's start or end. On macOS, Cmd+←→, Cmd+↑↓, and Cmd+Backspace work too. Ctrl+Enter, Shift+Enter, and Cmd need a terminal with the kitty keyboard protocol (kitty, Ghostty, WezTerm, foot, iTerm2); Alt+Enter and Ctrl+J work everywhere.

### Add to an agent by hand

```sh
claude mcp add --scope user muninn -- muninn mcp
codex mcp add muninn -- muninn mcp
```

## Develop

Everything runs through the flake:

```sh
nix develop                 # rustc, cargo, clippy, rustfmt, rust-analyzer, tmux
cargo test --workspace
cargo clippy --workspace --all-targets --all-features
nix build                   # release binary in ./result/bin/muninn (runs the tests)
nix run . -- ask "hello"
```

Try it offline against a scripted model:

```sh
cargo run -p muninn-ai --features mock --example mock_server -- script.json 18431
# then use http://127.0.0.1:18431/v1 as the endpoint (any key)
```

### Crates

Each crate stands alone; only `muninn-core` and the binary know what Muninn is.

| Crate | What it is | Depends on |
|---|---|---|
| `muninn-ai` | Chat types, `Provider` trait, streaming OpenAI-compatible client, mock server (`mock` feature) | — |
| `muninn-agent` | Tool-calling agent loop with events and cancellation | `muninn-ai` |
| `muninn-vault` | Sandboxed vault access (tree, read, search, write) and the undoable change journal | — |
| `muninn-mcp` | Tools-only MCP server over stdio JSON-RPC | — |
| `muninn-tui` | ratatui pieces: terminal guard, input editor, chat log, Markdown renderer, diff view, picker | — |
| `muninn-core` | The librarian: settings, conversations, vault tools, prompts, review flow | ai, agent, vault |
| `muninn` | The binary: CLI, MCP surface, TUI app, onboarding, agent integration | core, mcp, tui, vault |

```mermaid
graph LR
  muninn --> core[muninn-core]
  muninn --> mcp[muninn-mcp]
  muninn --> tui[muninn-tui]
  core --> agent[muninn-agent]
  core --> vault[muninn-vault]
  muninn --> vault
  agent --> ai[muninn-ai]
  core --> ai
```
