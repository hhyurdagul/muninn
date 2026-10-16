//! Drives the real `muninn` binary against a scripted model endpoint.

use std::fs;
use std::path::PathBuf;
use std::process::Stdio;

use muninn_ai::mock::MockServer;
use muninn_ai::{Message, ToolCall};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{ChildStdin, ChildStdout, Command};

const RULES: &str = "# Vault rules\n- Save ideas to `ideas/` as kebab-case .md files.\n- Saves land directly.\n";

struct Env {
    dir: tempfile::TempDir,
}

impl Env {
    fn new(url: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for sub in ["vault/projects", "elsewhere", "config", "data"] {
            fs::create_dir_all(root.join(sub)).unwrap();
        }
        fs::write(root.join("vault/AGENTS.md"), RULES).unwrap();
        fs::write(root.join("vault/projects/muninn.md"), "Decision: store conversations as JSON files.\n").unwrap();
        let config = format!(
            "vault = {:?}\nbase_url = {url:?}\nmodel = \"mock\"\napi_key = \"test\"\n",
            root.join("vault").display().to_string()
        );
        fs::write(root.join("config/config.toml"), config).unwrap();
        Self { dir }
    }

    fn vault(&self, rel: &str) -> PathBuf {
        self.dir.path().join("vault").join(rel)
    }

    fn muninn(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_muninn"));
        cmd.args(args)
            .current_dir(self.dir.path().join("elsewhere"))
            .env("MUNINN_CONFIG_DIR", self.dir.path().join("config"))
            .env("MUNINN_DATA_DIR", self.dir.path().join("data"))
            .env("HOME", self.dir.path())
            .env_remove("MUNINN_API_KEY")
            .env_remove("OPENROUTER_API_KEY")
            .env_remove("OPENAI_API_KEY")
            .env_remove("GEMINI_API_KEY")
            .env_remove("ANTHROPIC_API_KEY")
            .stdin(Stdio::null());
        cmd
    }

    async fn run(&self, args: &[&str]) -> (i32, String, String) {
        let out = self.muninn(args).output().await.unwrap();
        let text = |b: Vec<u8>| String::from_utf8(b).unwrap();
        (out.status.code().unwrap_or(-1), text(out.stdout), text(out.stderr))
    }
}

fn call(name: &str, args: Value) -> Message {
    let mut message = Message::assistant("");
    message.tool_calls = vec![ToolCall {
        id: format!("call-{name}"),
        name: name.into(),
        arguments: args.to_string(),
        ..Default::default()
    }];
    message
}

/// Role and content of the newest message in a chat request.
fn last(body: &Value) -> (String, String) {
    let message = body["messages"].as_array().and_then(|m| m.last()).cloned().unwrap_or_default();
    (message["role"].as_str().unwrap_or("").into(), message["content"].as_str().unwrap_or("").into())
}

#[tokio::test(flavor = "multi_thread")]
async fn cli_saves_from_another_folder_and_undo_reverts() {
    let server = MockServer::scripted(vec![
        call(
            "write_note",
            json!({"path":"ideas/ravens-remember.md","content":"Ravens remember faces.\n","summary":"Add raven idea"}),
        ),
        Message::assistant("Saved to `ideas/ravens-remember.md`."),
    ])
    .await
    .unwrap();
    let env = Env::new(&server.url);

    let (code, stdout, stderr) = env.run(&["ask", "save", "this:", "ravens remember faces"]).await;
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(stdout, "Saved to `ideas/ravens-remember.md`.\n", "piped output is only the answer");
    assert!(stderr.contains("write_note ideas/ravens-remember.md"), "{stderr}");
    assert_eq!(fs::read_to_string(env.vault("ideas/ravens-remember.md")).unwrap(), "Ravens remember faces.\n");

    let first = &server.requests()[0];
    let system = first["messages"][0]["content"].as_str().unwrap();
    assert!(system.contains("Save ideas to `ideas/`"), "vault rules are in the prompt");
    assert!(system.contains("one-shot terminal command"));
    assert_eq!(last(first), ("user".into(), "save this: ravens remember faces".into()));

    let (_, log, _) = env.run(&["log"]).await;
    assert!(log.contains("ideas/ravens-remember.md  Add raven idea"), "{log}");

    let (code, out, _) = env.run(&["undo"]).await;
    assert_eq!(code, 0);
    assert!(out.starts_with("Reverted ideas/ravens-remember.md"), "{out}");
    assert!(!env.vault("ideas/ravens-remember.md").exists());
    let (code, _, err) = env.run(&["undo"]).await;
    assert_eq!((code, err.trim()), (1, "muninn: nothing to undo"));
}

#[tokio::test(flavor = "multi_thread")]
async fn undo_reports_conflict_when_the_note_was_edited() {
    let server = MockServer::scripted(vec![
        call("edit_note", json!({"path":"projects/muninn.md","old_text":"JSON files","new_text":"JSON files in the data dir","summary":"Clarify"})),
        Message::assistant("Updated."),
    ])
    .await
    .unwrap();
    let env = Env::new(&server.url);
    assert_eq!(env.run(&["ask", "clarify the storage decision"]).await.0, 0);
    fs::write(env.vault("projects/muninn.md"), "edited by hand\n").unwrap();
    let (code, _, err) = env.run(&["undo"]).await;
    assert_eq!(code, 2);
    assert!(err.contains("Conflict: projects/muninn.md was edited"), "{err}");
    assert_eq!(fs::read_to_string(env.vault("projects/muninn.md")).unwrap(), "edited by hand\n");
}

#[tokio::test(flavor = "multi_thread")]
async fn muninn_rules_hidden_files_and_ignore_settings() {
    let server = MockServer::scripted(vec![
        call("list_notes", json!({"hidden": true, "depth": 3})),
        call("read_note", json!({"path": ".agents/skills/sync/SKILL.md"})),
        call("read_note", json!({"path": "private/diary.md"})),
        call("write_note", json!({"path": ".agents/new.md", "content": "x", "summary": "s"})),
        Message::assistant("Done."),
    ])
    .await
    .unwrap();
    let env = Env::new(&server.url);
    for (path, text) in [
        ("MUNINN.md", "- Muninn saves ideas to `inbox/`.\n"),
        (".agents/skills/sync/SKILL.md", "Sync weekly.\n"),
        (".git/config", "token\n"),
        ("private/diary.md", "secret\n"),
    ] {
        fs::create_dir_all(env.vault(path).parent().unwrap()).unwrap();
        fs::write(env.vault(path), text).unwrap();
    }
    let config = env.dir.path().join("config/config.toml");
    let settings = fs::read_to_string(&config).unwrap() + "ignore = [\".git/\", \"private/\"]\n";
    fs::write(&config, settings).unwrap();

    let (code, _, stderr) = env.run(&["ask", "look around"]).await;
    assert_eq!(code, 0, "{stderr}");
    let requests = server.requests();
    let system = requests[0]["messages"][0]["content"].as_str().unwrap();
    assert!(system.contains("Save ideas to `ideas/`") && system.contains("Muninn saves ideas to `inbox/`"));
    assert!(
        system.find("Save ideas to").unwrap() < system.find("Muninn saves ideas").unwrap(),
        "MUNINN.md comes after AGENTS.md"
    );
    let results: Vec<String> = requests[4]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["role"] == "tool")
        .map(|m| m["content"].as_str().unwrap().to_string())
        .collect();
    assert!(results[0].contains(".agents/") && !results[0].contains(".git") && !results[0].contains("private"));
    assert_eq!(results[1], "Sync weekly.\n", "hidden files are readable");
    assert!(results[2].contains("excluded by Muninn's ignore settings"), "{}", results[2]);
    assert!(results[3].contains("read-only"), "{}", results[3]);
    assert!(!env.vault(".agents/new.md").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn cli_reports_missing_setup() {
    let env = Env::new("http://127.0.0.1:9/v1");
    fs::remove_file(env.dir.path().join("config/config.toml")).unwrap();
    let (code, _, err) = env.run(&["ask", "hello"]).await;
    assert_eq!(code, 1);
    assert!(err.contains("not set up yet"), "{err}");
}

struct Mcp {
    stdin: ChildStdin,
    stdout: Lines<BufReader<ChildStdout>>,
    next_id: i64,
    instructions: String,
    _child: tokio::process::Child,
}

impl Mcp {
    async fn start(env: &Env) -> Self {
        let mut child = env.muninn(&["mcp"]).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
        let mut mcp = Self {
            stdin: child.stdin.take().unwrap(),
            stdout: BufReader::new(child.stdout.take().unwrap()).lines(),
            next_id: 0,
            instructions: String::new(),
            _child: child,
        };
        let init = mcp
            .request(
                "initialize",
                json!({"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"0"}}),
            )
            .await;
        assert_eq!(init["result"]["serverInfo"]["name"], "muninn");
        mcp.instructions = init["result"]["instructions"].as_str().unwrap_or_default().to_string();
        mcp.stdin.write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n").await.unwrap();
        mcp
    }

    async fn request(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let message = json!({"jsonrpc":"2.0","id":self.next_id,"method":method,"params":params});
        self.stdin.write_all(format!("{message}\n").as_bytes()).await.unwrap();
        loop {
            let line = self.stdout.next_line().await.unwrap().expect("server closed");
            let reply: Value = serde_json::from_str(&line).unwrap();
            if reply["id"] == self.next_id {
                return reply;
            }
        }
    }

    /// Calls the `muninn` tool; returns (text, conversation_id).
    async fn say(&mut self, message: &str, conversation: Option<&str>) -> (String, String) {
        let mut args = json!({ "message": message });
        if let Some(id) = conversation {
            args["conversation_id"] = json!(id);
        }
        let reply = self.request("tools/call", json!({"name":"muninn","arguments":args})).await;
        assert_eq!(reply["result"]["isError"], false, "{reply}");
        let text = reply["result"]["content"][0]["text"].as_str().unwrap().to_string();
        let id = text.rsplit("conversation_id: ").next().unwrap().trim().to_string();
        (text, id)
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn mcp_review_flow_writes_only_after_the_caller_approves() {
    let server = MockServer::start(|body| {
        let (role, content) = last(body);
        let text = body.to_string();
        match (role.as_str(), content.as_str()) {
            ("user", c) if c.contains("SQLite") => call(
                "write_note",
                json!({"path":"ideas/use-sqlite.md","content":"Use SQLite.\n","summary":"Record decision","review":true}),
            ),
            ("tool", c) if c.contains("pending") => Message::assistant("I proposed `ideas/use-sqlite.md`. Approve?"),
            ("user", "Approved.") => {
                let id = text.split("Proposed change ").nth(1).unwrap().split(' ').next().unwrap();
                call("apply_pending_change", json!({ "change_id": id }))
            }
            _ => Message::assistant("Saved `ideas/use-sqlite.md`."),
        }
    })
    .await
    .unwrap();
    let env = Env::new(&server.url);
    let mut mcp = Mcp::start(&env).await;

    let tools = mcp.request("tools/list", json!({})).await;
    assert_eq!(tools["result"]["tools"].as_array().unwrap().len(), 1);
    assert_eq!(tools["result"]["tools"][0]["inputSchema"]["required"][0], "message");

    let (text, id) = mcp.say("Save this decision: we use SQLite", None).await;
    assert!(text.contains("Pending change p"), "{text}");
    assert!(text.contains("+Use SQLite."), "diff is shown: {text}");
    assert!(!env.vault("ideas/use-sqlite.md").exists(), "nothing written before approval");
    let system = server.requests()[0]["messages"][0]["content"].as_str().unwrap().to_string();
    assert!(system.contains("called over MCP"));

    let (text, same) = mcp.say("Approved.", Some(&id)).await;
    assert_eq!(same, id);
    assert!(text.contains("Changed `ideas/use-sqlite.md`"), "{text}");
    assert!(!text.contains("Pending change"), "{text}");
    assert_eq!(fs::read_to_string(env.vault("ideas/use-sqlite.md")).unwrap(), "Use SQLite.\n");
}

#[tokio::test(flavor = "multi_thread")]
async fn mcp_conversation_continues_after_muninn_asks_back_and_cites_notes() {
    let server = MockServer::start(|body| {
        let (role, content) = last(body);
        match (role.as_str(), content.as_str()) {
            ("user", c) if c.contains("storage") => Message::assistant("Which project do you mean?"),
            ("user", "Muninn") => call("read_note", json!({"path":"projects/muninn.md"})),
            ("tool", c) => Message::assistant(format!("You decided: {} (source: `projects/muninn.md`)", c.trim())),
            _ => Message::assistant("?"),
        }
    })
    .await
    .unwrap();
    let env = Env::new(&server.url);
    let mut mcp = Mcp::start(&env).await;

    let (question, id) = mcp.say("What did I decide about storage?", None).await;
    assert!(question.starts_with("Which project do you mean?"), "{question}");
    let (answer, _) = mcp.say("Muninn", Some(&id)).await;
    assert!(answer.contains("store conversations as JSON files"), "{answer}");
    assert!(answer.contains("Notes read: `projects/muninn.md`"), "{answer}");

    let second_turn = &server.requests()[1];
    let roles: Vec<&str> =
        second_turn["messages"].as_array().unwrap().iter().map(|m| m["role"].as_str().unwrap()).collect();
    assert_eq!(roles, ["system", "user", "assistant", "user"], "history was restored");

    let saved: Value =
        serde_json::from_slice(&fs::read(env.dir.path().join(format!("data/conversations/{id}.json"))).unwrap())
            .unwrap();
    assert_eq!(saved["surface"], "mcp");
}

#[tokio::test(flavor = "multi_thread")]
async fn agents_get_memory_at_startup_and_only_a_reviewed_change_updates_it() {
    let server = MockServer::scripted(vec![
        call("edit_note", json!({"path":"MEMORY.md","old_text":"- Prefers Rust","new_text":"- Prefers Rust\n- Uses Nix flakes","summary":"Remember Nix"})),
        Message::assistant("I proposed adding that you use Nix flakes."),
    ])
    .await
    .unwrap();
    let env = Env::new(&server.url);
    fs::write(env.vault("MEMORY.md"), "# About the user\n- Prefers Rust\n").unwrap();

    let (code, out, _) = env.run(&["memory"]).await;
    assert_eq!((code, out.as_str()), (0, "# About the user\n- Prefers Rust\n"));

    let mut mcp = Mcp::start(&env).await;
    assert!(mcp.instructions.contains("- Prefers Rust"), "{}", mcp.instructions);
    let (text, _) = mcp.say("Remember that I use Nix flakes", None).await;
    assert!(text.contains("Pending change"), "{text}");
    assert_eq!(fs::read_to_string(env.vault("MEMORY.md")).unwrap(), "# About the user\n- Prefers Rust\n");
}

#[tokio::test(flavor = "multi_thread")]
async fn model_switches_only_after_a_test_call_and_keeps_keys_per_provider() {
    let server = MockServer::scripted(vec![Message::assistant("OK")]).await.unwrap();
    let env = Env::new(&server.url);
    let config = || fs::read_to_string(env.dir.path().join("config/config.toml")).unwrap();

    let (code, out, _) = env.run(&["model"]).await;
    assert_eq!(code, 0);
    assert!(out.starts_with("custom · mock"), "single-key settings migrate to `custom`: {out}");

    let (code, _, err) = env.run(&["model", "anthropic", "claude-sonnet-5-5"]).await;
    assert_eq!(code, 1);
    assert!(err.contains("no API key for Anthropic"), "{err}");
    assert!(config().contains("model = \"mock\""), "a failed switch changes nothing");

    let (code, out, err) = env.run(&["model", "custom", "mock-2", "--endpoint", &server.url]).await;
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("custom · mock-2"));
    assert!(config().contains("model = \"mock-2\""));
    assert!(config().contains("custom = \"test\""), "the key stays with its provider");
    assert_eq!(server.requests().last().unwrap()["model"], "mock-2");
}

#[tokio::test(flavor = "multi_thread")]
async fn mcp_failure_partway_reports_what_was_already_written() {
    let server = MockServer::start(|body| match body["messages"].as_array().map_or(0, Vec::len) {
        // System prompt and user message: save a note first.
        0..=2 => call("write_note", json!({"path":"ideas/a.md","content":"A\n","summary":"Add a"})),
        _ => Message { extra: json!({"mock_status": 400}), ..Message::assistant("") },
    })
    .await
    .unwrap();
    let env = Env::new(&server.url);
    let mut mcp = Mcp::start(&env).await;
    let reply =
        mcp.request("tools/call", json!({"name":"muninn","arguments":{"message":"Save idea a, then more"}})).await;
    assert_eq!(reply["result"]["isError"], true);
    let text = reply["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("Muninn failed") && text.contains("400"), "{text}");
    assert!(text.contains("Changed `ideas/a.md` (undo with `muninn undo "), "{text}");
    assert!(text.contains("conversation_id: c"), "{text}");
    assert_eq!(fs::read_to_string(env.vault("ideas/a.md")).unwrap(), "A\n");
}

#[tokio::test(flavor = "multi_thread")]
async fn mcp_footer_names_notes_cited_from_search_matches_only() {
    let server = MockServer::scripted(vec![
        call("search_notes", json!({"query":"conversations"})),
        Message::assistant("You store conversations as JSON (`projects/muninn.md`)."),
    ])
    .await
    .unwrap();
    let env = Env::new(&server.url);
    let mut mcp = Mcp::start(&env).await;
    let (text, _) = mcp.say("How do I store conversations?", None).await;
    assert!(text.contains("Cited from search matches only (not read in full): `projects/muninn.md`"), "{text}");
    assert!(!text.contains("Notes read"), "{text}");
}
