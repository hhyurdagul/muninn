---
type: project-idea
status: active
created: 2026-09-23
updated: 2026-10-12
---
# Muninn

Muninn makes your knowledge vault reachable from anywhere. You pin your vault once. After that, from any folder or any agent, you can save to it, ask it, or talk with it, without first going to the vault and getting an agent to understand it from scratch.

## The problem

To work with a vault today, you open an agent (Claude Code, omp, …) inside the vault folder. That breaks down when you are working somewhere else:

- **Saving.** Say you are in another project and want to keep something. There are two options, and both are friction:
  - Tell the current agent to do it. It has to go to the vault and work out its structure, contents, and rules before it can make the change.
  - Have the agent write a prompt. Then you move to the vault folder, start a fresh agent, and give it the prompt.
- **Asking.** Getting something out of the vault takes the same detour.
- **Talking.** A longer conversation about your knowledge means going to the vault folder and starting an agent there first.

## What Muninn does

It removes those steps:

- **Save from anywhere.** Hand over what should be kept. Muninn puts it in the right place according to the vault's own structure and rules.
- **Ask from anywhere.** Get an answer or material from the vault back to where you are.
- **Talk from anywhere.** Running `muninn` in any folder opens a TUI conversation that is already in your vault, like Claude Code or Codex.

There are three ways in:

- **CLI:** one prompt in, one result out. It has no chat mode.
- **MCP:** a conversation with the agent you are already working with. Muninn knows it is the librarian and acts on knowledge. Either side can ask questions: Muninn can ask the calling agent for context, or anything else it needs, before it acts.
- **TUI:** a conversation for you, like Claude Code or Codex.

## Decided

- **Muninn is its own agent.** It has its own agent loop and model access. It does not start Claude Code, omp, or another existing agent.
- **Saves land directly by default.** A save is just an agent call: Muninn writes into the vault following its rules. If the vault's rules say changes need review, Muninn shows the proposed change to the caller and writes it only after the caller approves. Muninn always does the writing itself.
- **Humans talk through the TUI; agents talk through MCP.** The CLI takes one prompt per call and has no chat mode.
- **Rules live in the vault, not in Muninn.** The vault's rules are its `AGENTS.md`. Muninn's own settings hold only the vault path, the model, and credentials.

## Onboarding

Running `muninn` for the first time opens the TUI and starts onboarding. `muninn init` restarts it. Onboarding begins with two plain setup steps, the vault path and the API key, because the agent cannot talk until a model is connected. From there it is a conversation with Muninn, and the rules get worked out in that conversation.

**Every setup**

1. Pick the vault path. It is stored in Muninn's settings, outside the vault.
2. Connect the model with an API key and make one test call.
3. Detect installed agents (Claude Code, Codex, omp, …) and offer to add `muninn mcp` to their MCP settings.
4. Ask whether saves should land directly or be reviewed first, and record the answer in `AGENTS.md`.

**Existing vault**

- Muninn reads the vault first: `AGENTS.md`, folder layout, frontmatter, link style, and languages. It tells you what it understood, and you correct it in the conversation.
- If `AGENTS.md` exists, Muninn uses it as it is. It may suggest changes but never edits the file without asking.
- If there is no `AGENTS.md`, you and Muninn work one out together, describing the vault's existing conventions, and Muninn writes it. Muninn never imposes a new structure.

**From zero**

- Muninn creates the minimal shape: an inbox and an `AGENTS.md`. Structure grows from there; when a pattern appears, Muninn suggests a folder.

**Later:** structure presets, imports from other tools, and a calibration run with sample saves before going live.

## MVP

CLI, MCP, and TUI, with one pinned vault, usable from any folder.

- Running `muninn` with no arguments opens the TUI conversation in the vault.
- The CLI takes one-shot prompts.
- MCP supports conversation, with questions going both ways between Muninn and the calling agent.
- MCP runs over stdio. The calling agent starts `muninn mcp` as a child process and keeps it running for its session, so no daemon is needed.

**Build notes**

- **Language:** Rust, shipped as one executable that starts fast.
- **Model:** any OpenAI-compatible endpoint, reached with an API key. OpenRouter is the likely first choice.
- **Vault access:** Muninn can read, search, and write notes inside the vault, and nothing else. It has no shell. When a vault rule requires running a command, Muninn says so instead of pretending the rule is satisfied.
- **MCP surface:** one tool that takes a message and an optional conversation ID. Muninn decides whether the message is a save, a question, or something else.
- **Review:** this happens inside the conversation, and the caller is the one who approves. Muninn sends the proposed change back to the calling agent and writes it once the agent approves in the same conversation. Whether the agent checks with its human first is up to the agent. In the TUI you approve in the conversation. In the CLI, the caller is whoever ran the command, so Muninn asks for confirmation on the terminal.
- **Undo:** every change is logged outside the vault with its before and after content. `muninn undo` reverts a change, but reports a conflict instead if the note has been edited since.
- **Conversations:** TUI and MCP conversations are saved outside the vault and can be continued later. The CLI stays one-shot.
- **TUI:** simple, but not an empty box. It has an input field, answers that stream in, the notes Muninn read or changed, the approve step for changes that need review, and a list of past conversations to continue.

**Done when**

- From another repo, Claude Code saves something through MCP, and it lands in the right place according to the vault's rules.
- Claude Code asks a question and gets an answer that cites the notes it came from.
- Muninn asks the calling agent a question back, and the conversation finishes correctly.
- One-shot CLI prompts work from any folder.
- Running `muninn` from any folder opens a conversation in the vault.
- Onboarding works in the TUI on Hasan's real vault and on an empty folder.
- A TUI conversation can be closed and continued later.

## Progress

### 2026-10-12 04:30 — MVP ready for real-model testing

**Built.** A Cargo workspace of small, independent crates plus one binary (see `README.md`):

- `muninn-ai`: chat types, a `Provider` trait, and a streaming OpenAI-compatible client. A scripted mock server sits behind the `mock` feature.
- `muninn-agent`: a generic tool-calling loop with events and cancellation. The transcript stays valid on errors and cancels.
- `muninn-vault`: sandboxed vault access (tree, read, search, write; no hidden paths, no escaping through symlinks) and the change journal with undo and conflict detection.
- `muninn-mcp`: a tools-only MCP server over stdio.
- `muninn-tui`: ratatui building blocks (terminal guard, input editor, chat log, diff view, picker).
- `muninn-core`: the librarian. Settings, saved conversations, vault tools, prompts, and the review flow. Each turn reads `AGENTS.md` fresh. Changes to an existing `AGENTS.md` always go through review.
- `muninn`: the binary. `muninn` (TUI + onboarding), `muninn init`, `muninn ask`, `muninn mcp`, `muninn log`, `muninn undo`.

**Review flow.** The model sets `review=true` when the vault rules require it.

- TUI: an approve dialog showing the diff.
- CLI: a y/N prompt on `/dev/tty`.
- MCP: the change is stored as pending, and its diff is returned to the caller. It is written only through `apply_pending_change`, which code refuses to run until the caller has sent a later message.

**Verified.**

- 36 automated tests (`cargo test --workspace`, also run by `nix build`). The end-to-end tests drive the real binary against a mock model: a CLI save from another folder, log and undo, an undo conflict, the MCP review flow (nothing is written before approval), and an MCP question back that cites the notes it read.
- Real Claude Code (`claude -p` from another folder, Muninn connected to a mock model):
  - a save landed in `inbox/` as the rules say;
  - Muninn asked Claude a question, Claude answered with the same `conversation_id`, and the save finished.
- TUI driven in tmux:
  - onboarding on an empty folder: setup steps, omp MCP registration, a conversation that created `inbox/` and `AGENTS.md`, and the approve dialog for the change to `AGENTS.md`;
  - quit, relaunch, and continue the conversation from Ctrl+O;
  - the CLI terminal confirmation.
- Read-only check on HasanVault: tree, search, and the hidden-path refusal all behave correctly. Nothing was written.
- Release binary starts in about 20 ms.

**Needs Hasan (real model and key).**

- Onboarding on HasanVault.
- Answer quality and placement with a real model.
- Registering with the real Claude Code, Codex, and omp configs.

**Known limits.**

- Long conversations are not trimmed to fit the model's context.
- A TUI turn on a conversation started over MCP cannot apply that conversation's pending MCP proposals.
