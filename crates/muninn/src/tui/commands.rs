//! Slash commands typed into the chat input.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Help,
    New,
    Resume,
    Log,
    Undo,
    Init,
    Quit,
}

pub struct Command {
    pub name: &'static str,
    pub args: &'static str,
    pub about: &'static str,
    pub action: Action,
}

pub const COMMANDS: &[Command] = &[
    Command { name: "help", args: "", about: "Commands and keyboard shortcuts", action: Action::Help },
    Command { name: "new", args: "", about: "Start a new conversation", action: Action::New },
    Command { name: "resume", args: "", about: "Continue an earlier conversation", action: Action::Resume },
    Command { name: "log", args: "", about: "Recent changes Muninn made to the vault", action: Action::Log },
    Command {
        name: "undo",
        args: "[id]",
        about: "Revert the latest change, or the one with this id",
        action: Action::Undo,
    },
    Command { name: "init", args: "", about: "Redo setup: vault, model, agents, vault rules", action: Action::Init },
    Command { name: "quit", args: "", about: "Leave Muninn (also /exit)", action: Action::Quit },
];

/// Commands matching a `/name` that is still being typed; empty otherwise.
pub fn completions(input: &str) -> Vec<&'static Command> {
    match input.strip_prefix('/') {
        Some(prefix) if !prefix.contains(char::is_whitespace) => {
            COMMANDS.iter().filter(|c| c.name.starts_with(prefix)).collect()
        }
        _ => Vec::new(),
    }
}

pub enum Parsed<'a> {
    Run(Action, &'a str),
    Unknown(&'a str),
}

/// Reads `/name args`. `None` means the input is a message, including ones
/// that merely start with a path such as `/home/me/notes`.
pub fn parse(input: &str) -> Option<Parsed<'_>> {
    let rest = input.trim().strip_prefix('/')?;
    let (name, args) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphabetic()) {
        return None;
    }
    let lookup = if name == "exit" { "quit" } else { name };
    Some(match COMMANDS.iter().find(|c| c.name == lookup) {
        Some(command) => Parsed::Run(command.action, args.trim()),
        None => Parsed::Unknown(name),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_are_told_apart_from_messages() {
        assert!(matches!(parse("/undo  abc "), Some(Parsed::Run(Action::Undo, "abc"))));
        assert!(matches!(parse("/exit"), Some(Parsed::Run(Action::Quit, ""))));
        assert!(matches!(parse("/nope"), Some(Parsed::Unknown("nope"))));
        assert!(parse("/home/me/notes is my vault").is_none());
        assert!(parse("hello /new").is_none());
        let names: Vec<_> = completions("/n").iter().map(|c| c.name).collect();
        assert_eq!(names, ["new"]);
        assert!(completions("/new ").is_empty(), "arguments close the list");
    }
}
