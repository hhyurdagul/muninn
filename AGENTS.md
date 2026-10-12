# AGENTS.md

Guide for agents (and humans) continuing work on Muninn. Read this first, then `Muninn.md` (the product spec, plus a dated progress log at the bottom) and `README.md` (usage).

## What this is

Muninn is a librarian agent for one pinned knowledge vault (a folder of Markdown notes). You can reach it from any folder in three ways:

- `muninn` is a TUI chat for humans. On the first run it starts onboarding.
- `muninn ask "…"` is a one-shot CLI command with no chat mode.
- `muninn mcp` is an MCP server over stdio, used by coding agents. It exposes one tool, `muninn`, which takes a `message` and an optional `conversation_id`.

Muninn runs its own agent loop against any OpenAI-compatible endpoint, with OpenRouter as the default. It never shells out.

## Ground rules

- **Use the flake for everything dev-related.** Run commands as `nix develop -c <cmd>`, or work inside `nix develop`. Add new tools to `devShells.default` in `flake.nix`, never to the host. Flakes only see files git tracks, so `git add` new files before running `nix build` or `nix develop`.
- **Keep crates decoupled.** The library crates must stay usable outside Muninn (see the dependency rules below). Domain logic goes in `muninn-core`; UI and wiring go in the `muninn` binary.
- **Write concise, readable code.** Prefer boring designs, use no speculative abstraction, and delete dead code. Use `anyhow` for errors and `parking_lot` locks in sync code; use `tokio::sync` when a guard is held across `.await`. rustfmt is set to `max_width = 120` (`rustfmt.toml`).
- **Commit with git at every commitable step**, with a short imperative message. **Never add commit attribution**: no `Co-Authored-By` and no "generated with" lines.
- **Keep the progress log current.** After meaningful work, append a dated entry (`### YYYY-MM-DD HH:MM — title`) under `## Progress` in `Muninn.md`: what was built, what was verified, what needs Hasan, and known limits.
- **Treat Hasan's real vault (`~/HasanVault`) as read-only** unless he asks otherwise. Smoke-test against temporary folders.

## Commands

```sh
nix develop -c cargo test --workspace                                   # all tests
nix develop -c cargo clippy --workspace --all-targets --all-features    # must be warning-free
nix develop -c cargo fmt --all
nix build                       # release binary at ./result/bin/muninn; runs the test suite too
nix develop -c cargo run -p muninn-ai --features mock --example mock_server -- script.json 18431
```

When `Cargo.lock` changes, `nix build` picks it up automatically through `cargoLock.lockFile`.

## Architecture

```mermaid
graph LR
  muninn --> core[muninn-core]
  muninn --> mcp[muninn-mcp]
  muninn --> tui[muninn-tui]
  muninn --> vault
  core --> agent[muninn-agent]
  core --> vault[muninn-vault]
  core --> ai[muninn-ai]
  agent --> ai
```

| Crate | Files | Responsibility | Must not know about |
|---|---|---|---|
| `muninn-ai` | `types.rs` (`Message`, `ToolCall`, `ToolSpec`, `Provider`), `openai.rs` (streaming client), `sse.rs`, `mock.rs` (feature `mock`) | LLM wire protocol | tools, vaults, Muninn |
| `muninn-agent` | `lib.rs` (`Agent`, `Tool`, `Event`, `Stop`) | tool-calling loop, cancellation, valid transcripts | vaults, UI |
| `muninn-vault` | `lib.rs` (`Vault`), `journal.rs` (`Journal`, `Undo`, `unified_diff`) | sandboxed file access, change log, undo | AI, UI |
| `muninn-mcp` | `lib.rs` (`Handler`, `serve`, `serve_stdio`) | MCP JSON-RPC (tools only) | Muninn |
| `muninn-tui` | `input.rs`, `chat_log.rs`, `markdown.rs` (pulldown-cmark → styled lines), `diff.rs`, `picker.rs`, `terminal.rs`, `wrap.rs` | generic ratatui widgets | Muninn |
| `muninn-core` | `config.rs` (`Settings`, `Paths`), `conversation.rs` (`Conversation`, `Store`, `Proposal`, `NoteEvent`), `tools.rs` (vault tools), `prompt.rs`, `librarian.rs` (`Librarian::turn`, `Review`, `Reviewer`, `Update`) | the librarian, independent of any UI | terminals, MCP |
| `muninn` | `main.rs` (clap), `cli.rs`, `mcp.rs`, `integrations.rs`, `tui/{mod,chat,commands,setup}.rs` | surfaces and onboarding | — |

### One turn

1. A surface builds a `Conversation` (`Surface::Cli|Mcp|Tui`, `Kind::Chat|Onboarding`). The TUI and MCP load existing conversations from `Store`.
2. It calls `Librarian::turn(conv, text, review, on_update, cancel)`.
3. `prompt::system_prompt` is rebuilt every turn. It contains the role text, the surface-specific review text, the current `AGENTS.md`, and the vault tree. For onboarding, it adds the onboarding section.
4. `tools::toolset` creates the tools over a shared `TurnContext` (vault, journal, review mode, pending proposals, note events). `Agent::run` loops until the model replies without calling any tools.
5. The surface renders `Update`s (`Text`, `Tool`, `Note`) as they stream in. Afterwards it saves the conversation; the CLI does not save.

### Settings and data

- `Paths::from_env` decides where files live. Settings are in `~/.config/muninn/config.toml`, with mode 0600 because the file holds the key; `MUNINN_CONFIG_DIR` overrides the location.
- Conversations are in `~/.local/share/muninn/conversations/<id>.json` and the change log is in `changes.jsonl`; `MUNINN_DATA_DIR` overrides the location.
- `MUNINN_API_KEY` overrides the stored key.

## Invariants (do not break; tests cover most)

- **Vault sandbox.** `Vault::resolve` rejects `..`, absolute paths outside the root, any hidden component (`.git`, `.obsidian`, …), and symlinks that lead outside the vault. All file access goes through `Vault`.
- **Every note write goes through `Journal::apply`**, so it can be undone. `Journal::undo` reports `Conflict` instead of overwriting a note that was edited afterwards.
- **Changes to an existing `AGENTS.md` always go through review**, whatever the model asks for (`VaultTool::save` in `core/src/tools.rs`).
- **Review modes.**
  - TUI and CLI use `Review::Interactive`: the tool waits while a `Reviewer` asks a human (a channel to the TUI dialog, or `/dev/tty` in the CLI).
  - MCP uses `Review::Deferred`: the proposal is stored on the conversation, and `apply_pending_change` refuses to run until a later user turn exists (`Proposal.user_turn`). The diff is appended to the MCP reply by `muninn/src/mcp.rs::render`.
- **Valid transcripts.** `Agent::run` answers every tool call, including on errors and cancellation. Tool errors go back to the model as `Error: …`.
- **Rules live in the vault.** Muninn's settings hold only the vault path, endpoint, model, and key. Never store vault conventions in Muninn's settings.
- **No shell.** If a vault rule needs a command, Muninn says so instead of running anything.

## Testing

- Unit tests sit next to the code (`#[cfg(test)]`). End-to-end tests are in `crates/muninn/tests/e2e.rs`. They run the real binary against `muninn_ai::mock::MockServer`, with temporary vault, config, data, and `HOME` folders.
  - `MockServer::scripted(vec![...])` replays fixed assistant messages.
  - `MockServer::start(|request_body| ...)` decides each reply from the request, for example to echo a generated proposal id.
- Test behavior that users or callers can see: boundaries, refusals, persistence, protocol. Don't write tests that only check wiring or wording.
- For TUI smoke tests, use tmux from the dev shell against the mock example. Run in an isolated environment (`HOME`, `MUNINN_CONFIG_DIR`, `MUNINN_DATA_DIR`) and set `PATH` without the real `claude`, `codex`, or `omp`, so onboarding can't edit real agent configs:
  ```sh
  tmux -L t new-session -d -s m -x 120 -y 36 "env HOME=/tmp/x/home MUNINN_CONFIG_DIR=/tmp/x/cfg MUNINN_DATA_DIR=/tmp/x/data muninn"
  tmux -L t send-keys -t m 'text' Enter; tmux -L t capture-pane -p -t m
  ```
- Claude Code smoke test: `claude -p "…use the muninn tool…" --mcp-config cfg.json --strict-mcp-config --allowedTools mcp__muninn__muninn`. Point `cfg.json` at the binary, and use `env` to set `MUNINN_CONFIG_DIR` to a config whose `base_url` is the mock server.

## Common changes

- **New vault tool:** add a `Kind` variant in `core/src/tools.rs`, its `spec()` entry, and its `call()` branch. Register it in `toolset`. Writes must go through `VaultTool::save` or `TurnContext::write`. Mention the tool in `prompt.rs` if the model needs guidance.
- **New provider or wire format:** implement `muninn_ai::Provider` in `muninn-ai`, then choose it in `Librarian::from_settings`. Add any new settings field to `Settings` (with a serde default) and to onboarding (`tui/setup.rs`).
- **New coding-agent integration:** add an `Agent` variant in `muninn/src/integrations.rs` (`name`, `binary`, `config`, `is_installed`, and its arm in `install`). Prefer the agent's own CLI (`<agent> mcp add`) over editing its config files.
- **Prompt behavior:** edit `core/src/prompt.rs`. Surface-specific instructions go in `surface_text`, and the onboarding flow in `ONBOARDING`.
- **TUI:** generic widgets belong in `muninn-tui`. Screens and state belong in `muninn/src/tui/`. Background work reports back to the UI loop through `AppEvent` on the `Tx` channel.
- **Slash command:** add an `Action` variant and a `COMMANDS` entry in `muninn/src/tui/commands.rs`, then its arm in `Chat::run` (`tui/chat.rs`). The `/help` table and the completion list come from `COMMANDS`. Commands that touch the conversation or the vault must refuse while a turn runs.

## Backlog / known gaps

- Not yet verified with a real model:
  - onboarding on `~/HasanVault`;
  - answer quality and where saves land;
  - registering with the real Claude Code, Codex, and omp configs.
- Long conversations are never trimmed or summarized to fit the context window.
- A TUI turn on a conversation started over MCP has no `apply_pending_change` (the review mode is interactive), so pending MCP proposals stay stuck there.
- The CLI cannot confirm changes without a terminal (`/dev/tty`). In that case changes needing review are rejected.
- Later items from the spec: structure presets, imports from other tools, and a calibration run before going live.
