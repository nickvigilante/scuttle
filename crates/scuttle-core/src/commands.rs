//! Slash commands available in M1.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Model(Option<String>),
    Effort(Option<String>),
    Workspace(Option<String>),
    Compact,
    Clear,
    Copy(Option<usize>),
    Mouse,
    Help,
    Quit,
}

#[derive(Debug)]
pub struct CommandInfo {
    pub name: &'static str,
    pub usage: &'static str,
    pub description: &'static str,
}

pub const COMMANDS: &[CommandInfo] = &[
    CommandInfo {
        name: "/model",
        usage: "/model [name]",
        description: "Pick the model for the next message",
    },
    CommandInfo {
        name: "/effort",
        usage: "/effort [level]",
        description: "Pick the reasoning effort for the next message",
    },
    CommandInfo {
        name: "/workspace",
        usage: "/workspace [name|none]",
        description: "Attach or detach a workspace",
    },
    CommandInfo {
        name: "/compact",
        usage: "/compact",
        description: "Summarize the conversation to free context",
    },
    CommandInfo {
        name: "/clear",
        usage: "/clear",
        description: "Reset the model context and keep the transcript",
    },
    CommandInfo {
        name: "/copy",
        usage: "/copy [n]",
        description: "Copy the last message, or its nth code block",
    },
    CommandInfo {
        name: "/mouse",
        usage: "/mouse",
        description: "Toggle mouse capture",
    },
    CommandInfo {
        name: "/help",
        usage: "/help",
        description: "Show commands and keys",
    },
    CommandInfo {
        name: "/quit",
        usage: "/quit",
        description: "Exit scuttle",
    },
];

pub fn parse(input: &str) -> Result<Command, String> {
    let input = input.trim();
    let Some(rest) = input.strip_prefix('/') else {
        return Err("not a command".into());
    };
    let (name, arg) = match rest.split_once(char::is_whitespace) {
        Some((name, arg)) => (name, Some(arg.trim()).filter(|a| !a.is_empty())),
        None => (rest, None),
    };
    match name {
        "model" => Ok(Command::Model(arg.map(str::to_owned))),
        "effort" => Ok(Command::Effort(arg.map(str::to_owned))),
        "workspace" => Ok(Command::Workspace(arg.map(str::to_owned))),
        "compact" => Ok(Command::Compact),
        "clear" => Ok(Command::Clear),
        "copy" => match arg {
            None => Ok(Command::Copy(None)),
            Some(n) => n
                .parse()
                .map(|n| Command::Copy(Some(n)))
                .map_err(|_| format!("/copy takes a code block number, got {n:?}")),
        },
        "mouse" => Ok(Command::Mouse),
        "help" => Ok(Command::Help),
        "quit" => Ok(Command::Quit),
        other => Err(format!("unknown command /{other}; type /help")),
    }
}

pub fn completions(prefix: &str) -> Vec<&'static CommandInfo> {
    COMMANDS
        .iter()
        .filter(|c| c.name.starts_with(prefix))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_effort() {
        assert_eq!(
            parse("/effort high"),
            Ok(Command::Effort(Some("high".into())))
        );
        assert_eq!(parse("/effort"), Ok(Command::Effort(None)));
        assert!(completions("/e").iter().any(|c| c.name == "/effort"));
    }

    #[test]
    fn parses_commands_and_arguments() {
        assert_eq!(
            parse("/model  Claude Sonnet "),
            Ok(Command::Model(Some("Claude Sonnet".into())))
        );
        assert_eq!(parse("/model"), Ok(Command::Model(None)));
        assert_eq!(
            parse("/workspace none"),
            Ok(Command::Workspace(Some("none".into())))
        );
        assert_eq!(parse("/copy 2"), Ok(Command::Copy(Some(2))));
        assert_eq!(parse("/copy"), Ok(Command::Copy(None)));
        assert_eq!(parse("/compact"), Ok(Command::Compact));
        assert_eq!(parse("/quit"), Ok(Command::Quit));
    }

    #[test]
    fn rejects_unknown_and_malformed_commands() {
        assert!(parse("/nope").unwrap_err().contains("/help"));
        assert!(parse("/copy two").is_err());
        assert!(parse("hello").is_err());
    }

    #[test]
    fn completes_by_prefix() {
        let names: Vec<_> = completions("/c").iter().map(|c| c.name).collect();
        assert_eq!(names, vec!["/compact", "/clear", "/copy"]);
        assert!(completions("/zzz").is_empty());
    }
}
