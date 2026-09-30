//! Slash commands available in M1.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    New,
    Model(Option<String>),
    Effort(Option<String>),
    Workspace(Option<String>),
    Organization(Option<String>),
    /// `None` toggles; `Some` turns plan mode on or off.
    PlanMode(Option<bool>),
    Compact,
    Clear,
    Copy(Option<usize>),
    Web,
    Mouse,
    Help,
    Quit,
}

#[derive(Debug)]
pub struct CommandInfo {
    pub name: &'static str,
    /// Other names that run the same command, each with its slash.
    pub aliases: &'static [&'static str],
    pub usage: &'static str,
    pub description: &'static str,
}

impl CommandInfo {
    /// The usage with any aliases after it, as the slash menu and `/help` show it.
    pub fn display_usage(&self) -> String {
        if self.aliases.is_empty() {
            self.usage.to_owned()
        } else {
            format!("{} ({})", self.usage, self.aliases.join(", "))
        }
    }
}

pub const COMMANDS: &[CommandInfo] = &[
    CommandInfo {
        name: "/new",
        aliases: &[],
        usage: "/new",
        description: "Start a new chat; the current one keeps running",
    },
    CommandInfo {
        name: "/model",
        aliases: &[],
        usage: "/model [name]",
        description: "Pick the model for the next message",
    },
    CommandInfo {
        name: "/effort",
        aliases: &[],
        usage: "/effort [level]",
        description: "Pick the reasoning effort for the next message",
    },
    CommandInfo {
        name: "/workspace",
        aliases: &[],
        usage: "/workspace [name|none]",
        description: "Attach or detach a workspace",
    },
    CommandInfo {
        name: "/organization",
        aliases: &["/org"],
        usage: "/organization [name]",
        description: "Choose the organization new chats go to",
    },
    CommandInfo {
        name: "/plan-mode",
        aliases: &[],
        usage: "/plan-mode [on|off]",
        description: "Toggle plan mode, or turn it on or off",
    },
    CommandInfo {
        name: "/compact",
        aliases: &[],
        usage: "/compact",
        description: "Summarize the conversation to free context",
    },
    CommandInfo {
        name: "/clear",
        aliases: &[],
        usage: "/clear",
        description: "Reset the model context and keep the transcript",
    },
    CommandInfo {
        name: "/copy",
        aliases: &[],
        usage: "/copy [n]",
        description: "Copy the last message, or its nth code block",
    },
    CommandInfo {
        name: "/web",
        aliases: &[],
        usage: "/web",
        description: "Open this chat in the Coder web UI",
    },
    CommandInfo {
        name: "/mouse",
        aliases: &[],
        usage: "/mouse",
        description: "Toggle mouse capture",
    },
    CommandInfo {
        name: "/help",
        aliases: &[],
        usage: "/help",
        description: "Show every command and key",
    },
    CommandInfo {
        name: "/quit",
        aliases: &["/exit"],
        usage: "/quit",
        description: "Exit scuttle",
    },
];

/// The command `name` (without its slash) stands for, following aliases.
fn canonical(name: &str) -> &str {
    COMMANDS
        .iter()
        .find(|c| c.aliases.iter().any(|a| a.strip_prefix('/') == Some(name)))
        .and_then(|c| c.name.strip_prefix('/'))
        .unwrap_or(name)
}

pub fn parse(input: &str) -> Result<Command, String> {
    let input = input.trim();
    let Some(rest) = input.strip_prefix('/') else {
        return Err("not a command".into());
    };
    let (name, arg) = match rest.split_once(char::is_whitespace) {
        Some((name, arg)) => (name, Some(arg.trim()).filter(|a| !a.is_empty())),
        None => (rest, None),
    };
    match canonical(name) {
        "new" => Ok(Command::New),
        "model" => Ok(Command::Model(arg.map(str::to_owned))),
        "effort" => Ok(Command::Effort(arg.map(str::to_owned))),
        "workspace" => Ok(Command::Workspace(arg.map(str::to_owned))),
        "organization" => Ok(Command::Organization(arg.map(str::to_owned))),
        "plan-mode" => match arg {
            None => Ok(Command::PlanMode(None)),
            Some(a) if a.eq_ignore_ascii_case("on") => Ok(Command::PlanMode(Some(true))),
            Some(a) if a.eq_ignore_ascii_case("off") => Ok(Command::PlanMode(Some(false))),
            Some(other) => Err(format!("/plan-mode takes on or off, got {other:?}")),
        },
        "compact" => Ok(Command::Compact),
        "clear" => Ok(Command::Clear),
        "copy" => match arg {
            None => Ok(Command::Copy(None)),
            Some(n) => n
                .parse()
                .map(|n| Command::Copy(Some(n)))
                .map_err(|_| format!("/copy takes a code block number, got {n:?}")),
        },
        "web" => Ok(Command::Web),
        "mouse" => Ok(Command::Mouse),
        "help" => Ok(Command::Help),
        "quit" => Ok(Command::Quit),
        other => Err(format!("unknown command /{other}; type /help")),
    }
}

/// Commands whose name or one of whose aliases starts with `prefix`.
pub fn completions(prefix: &str) -> Vec<&'static CommandInfo> {
    COMMANDS
        .iter()
        .filter(|c| c.name.starts_with(prefix) || c.aliases.iter().any(|a| a.starts_with(prefix)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_new() {
        assert_eq!(parse("/new"), Ok(Command::New));
    }

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
    fn parses_plan_mode() {
        assert_eq!(parse("/plan-mode"), Ok(Command::PlanMode(None)));
        assert_eq!(parse("/plan-mode on"), Ok(Command::PlanMode(Some(true))));
        assert_eq!(parse("/plan-mode OFF"), Ok(Command::PlanMode(Some(false))));
        assert!(parse("/plan-mode maybe").unwrap_err().contains("on or off"));
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

    #[test]
    fn aliases_parse_complete_and_display() {
        assert_eq!(parse("/exit"), Ok(Command::Quit));
        let names: Vec<_> = completions("/ex").iter().map(|c| c.name).collect();
        assert_eq!(names, vec!["/quit"]);
        let quit = COMMANDS.iter().find(|c| c.name == "/quit").unwrap();
        assert_eq!(quit.display_usage(), "/quit (/exit)");
        let model = COMMANDS.iter().find(|c| c.name == "/model").unwrap();
        assert_eq!(model.display_usage(), "/model [name]");
    }

    #[test]
    fn parses_organization_and_its_alias() {
        assert_eq!(parse("/organization"), Ok(Command::Organization(None)));
        assert_eq!(
            parse("/org coder"),
            Ok(Command::Organization(Some("coder".into())))
        );
        let names: Vec<_> = completions("/or").iter().map(|c| c.name).collect();
        assert_eq!(names, vec!["/organization"]);
    }

    #[test]
    fn parses_web() {
        assert_eq!(parse("/web"), Ok(Command::Web));
    }

    #[test]
    fn every_listed_command_parses() {
        for c in COMMANDS {
            assert!(
                parse(c.name).is_ok(),
                "{} is listed but does not parse",
                c.name
            );
        }
        let names: Vec<&str> = COMMANDS.iter().map(|c| c.name).collect();
        assert_eq!(
            names,
            [
                "/new",
                "/model",
                "/effort",
                "/workspace",
                "/organization",
                "/plan-mode",
                "/compact",
                "/clear",
                "/copy",
                "/web",
                "/mouse",
                "/help",
                "/quit"
            ]
        );
    }

    #[test]
    fn aliases_never_shadow_a_command() {
        for c in COMMANDS {
            for alias in c.aliases {
                assert!(alias.starts_with('/'), "{alias}");
                assert!(
                    COMMANDS.iter().all(|other| other.name != *alias),
                    "{alias} is also a command's name"
                );
            }
        }

        let all: Vec<&str> = COMMANDS
            .iter()
            .flat_map(|c| std::iter::once(c.name).chain(c.aliases.iter().copied()))
            .collect();
        let unique: std::collections::HashSet<&str> = all.iter().copied().collect();
        assert_eq!(
            all.len(),
            unique.len(),
            "duplicate command name or alias: {:?}",
            all.iter()
                .copied()
                .filter(|s| all.iter().filter(|o| *o == s).count() > 1)
                .collect::<std::collections::HashSet<_>>()
        );
    }
}
