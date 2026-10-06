//! Slash commands available in M1.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    New,
    Chats(Option<String>),
    Subagents,
    Parent,
    Model(Option<String>),
    Effort(Option<String>),
    Workspace(Option<String>),
    Organization(Option<String>),
    /// `None` toggles; `Some` turns plan mode on or off.
    PlanMode(Option<bool>),
    /// Leaves plan mode and asks the agent to implement its proposed plan.
    Implement,
    /// `None` proposes a title to confirm; `Some` renames directly.
    Title(Option<String>),
    Queue,
    /// Uploads the file at the path and attaches it to the next message.
    Attach(String),
    /// Lists the chat's files, to save or view one.
    Files,
    Info,
    Git,
    Diff,
    Mcp,
    Usage,
    Statusline,
    Compact,
    Clear,
    Copy(Option<usize>),
    Web,
    Mouse,
    Settings,
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
        name: "/chats",
        aliases: &["/resume"],
        usage: "/chats [query]",
        description: "Find and open a chat or subagent (Ctrl+R)",
    },
    CommandInfo {
        name: "/subagents",
        aliases: &[],
        usage: "/subagents",
        description: "Watch this chat's subagents live and open one",
    },
    CommandInfo {
        name: "/parent",
        aliases: &["/back"],
        usage: "/parent",
        description: "Return from a subagent to its parent (Esc while it is idle)",
    },
    CommandInfo {
        name: "/model",
        aliases: &[],
        usage: "/model [name]",
        description: "Pick the model for the next message, and set when each model compacts",
    },
    CommandInfo {
        name: "/effort",
        aliases: &[],
        usage: "/effort [level]",
        description: "Pick the reasoning effort for the next message",
    },
    CommandInfo {
        name: "/workspace",
        aliases: &["/ws"],
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
        name: "/implement",
        aliases: &[],
        usage: "/implement",
        description: "Leave plan mode and implement the proposed plan (Ctrl+Enter)",
    },
    CommandInfo {
        name: "/title",
        aliases: &[],
        usage: "/title [text]",
        description: "Rename this chat, or edit a proposed title",
    },
    CommandInfo {
        name: "/queue",
        aliases: &[],
        usage: "/queue",
        description: "Run a queued message next, or remove it",
    },
    CommandInfo {
        name: "/attach",
        aliases: &[],
        usage: "/attach <path>",
        description: "Attach a file to the next message (or type @path)",
    },
    CommandInfo {
        name: "/files",
        aliases: &[],
        usage: "/files",
        description: "List this chat's files, and save or view one",
    },
    CommandInfo {
        name: "/info",
        aliases: &["/chat-info"],
        usage: "/info",
        description: "Show this chat's details, context, and cost",
    },
    CommandInfo {
        name: "/git",
        aliases: &[],
        usage: "/git",
        description: "Show this chat's branch, pull request, and local changes",
    },
    CommandInfo {
        name: "/diff",
        aliases: &[],
        usage: "/diff",
        description: "Show this chat's diff in your git pager",
    },
    CommandInfo {
        name: "/mcp",
        aliases: &[],
        usage: "/mcp",
        description: "List the MCP servers for this chat, or for a new chat's first message, and turn them on or off",
    },
    CommandInfo {
        name: "/usage",
        aliases: &[],
        usage: "/usage",
        description: "Show your AI spend, workspace quota, and this chat's cost and context",
    },
    CommandInfo {
        name: "/statusline",
        aliases: &[],
        usage: "/statusline",
        description: "Choose the footer's fields, their order, and their warnings",
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
        name: "/settings",
        aliases: &[],
        usage: "/settings",
        description: "Edit config.toml in $EDITOR; scuttle applies it when you save and quit",
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
        "chats" => Ok(Command::Chats(arg.map(str::to_owned))),
        "subagents" => Ok(Command::Subagents),
        "parent" => Ok(Command::Parent),
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
        "implement" => Ok(Command::Implement),
        "title" => Ok(Command::Title(arg.map(str::to_owned))),
        "queue" => Ok(Command::Queue),
        "attach" => arg
            .map(|p| Command::Attach(p.to_owned()))
            .ok_or_else(|| "/attach takes a file path".to_owned()),
        "files" => Ok(Command::Files),
        "info" => Ok(Command::Info),
        "git" => Ok(Command::Git),
        "diff" => Ok(Command::Diff),
        "mcp" => Ok(Command::Mcp),
        "usage" => Ok(Command::Usage),
        "statusline" => Ok(Command::Statusline),
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
        "settings" => Ok(Command::Settings),
        "help" => Ok(Command::Help),
        "quit" => Ok(Command::Quit),
        other => Err(format!("unknown command /{other}; type /help")),
    }
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
    }

    #[test]
    fn parses_plan_mode() {
        assert_eq!(parse("/plan-mode"), Ok(Command::PlanMode(None)));
        assert_eq!(parse("/plan-mode on"), Ok(Command::PlanMode(Some(true))));
        assert_eq!(parse("/plan-mode OFF"), Ok(Command::PlanMode(Some(false))));
        assert!(parse("/plan-mode maybe").unwrap_err().contains("on or off"));
        assert_eq!(parse("/implement"), Ok(Command::Implement));
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
        assert_eq!(parse("/ws dev"), Ok(Command::Workspace(Some("dev".into()))));
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
    fn aliases_parse_and_display() {
        assert_eq!(parse("/exit"), Ok(Command::Quit));
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
    }

    #[test]
    fn parses_chats_and_its_alias() {
        assert_eq!(parse("/chats"), Ok(Command::Chats(None)));
        assert_eq!(
            parse("/resume fix watch"),
            Ok(Command::Chats(Some("fix watch".into())))
        );
    }

    #[test]
    fn parses_settings() {
        assert_eq!(parse("/settings"), Ok(Command::Settings));
    }

    #[test]
    fn parses_web() {
        assert_eq!(parse("/web"), Ok(Command::Web));
    }

    #[test]
    fn every_listed_command_parses() {
        for c in COMMANDS {
            // A command whose usage requires an argument, like `/attach <path>`, gets one.
            let input = if c.usage.contains('<') {
                format!("{} x", c.name)
            } else {
                c.name.to_owned()
            };
            assert!(
                parse(&input).is_ok(),
                "{} is listed but does not parse",
                c.name
            );
        }
        let names: Vec<&str> = COMMANDS.iter().map(|c| c.name).collect();
        assert_eq!(
            names,
            [
                "/new",
                "/chats",
                "/subagents",
                "/parent",
                "/model",
                "/effort",
                "/workspace",
                "/organization",
                "/plan-mode",
                "/implement",
                "/title",
                "/queue",
                "/attach",
                "/files",
                "/info",
                "/git",
                "/diff",
                "/mcp",
                "/usage",
                "/statusline",
                "/compact",
                "/clear",
                "/copy",
                "/web",
                "/mouse",
                "/settings",
                "/help",
                "/quit"
            ]
        );
    }

    #[test]
    fn parses_usage() {
        assert_eq!(parse("/usage"), Ok(Command::Usage));
    }

    #[test]
    fn parses_statusline() {
        assert_eq!(parse("/statusline"), Ok(Command::Statusline));
    }

    #[test]
    fn parses_attach() {
        assert_eq!(
            parse("/attach ~/shots/login page.png"),
            Ok(Command::Attach("~/shots/login page.png".into()))
        );
        assert!(parse("/attach").unwrap_err().contains("path"));
        assert_eq!(parse("/chat-info"), Ok(Command::Info));
        assert_eq!(parse("/git"), Ok(Command::Git));
        assert_eq!(parse("/diff"), Ok(Command::Diff));
        assert_eq!(parse("/mcp"), Ok(Command::Mcp));
    }

    #[test]
    fn parses_files() {
        assert_eq!(parse("/files"), Ok(Command::Files));
    }

    #[test]
    fn parses_title() {
        assert_eq!(parse("/title"), Ok(Command::Title(None)));
        assert_eq!(
            parse("/title Fix the watch test"),
            Ok(Command::Title(Some("Fix the watch test".into())))
        );
        assert_eq!(parse("/queue"), Ok(Command::Queue));
    }

    #[test]
    fn parses_subagents_and_parent_with_its_alias() {
        assert_eq!(parse("/subagents"), Ok(Command::Subagents));
        assert_eq!(parse("/parent"), Ok(Command::Parent));
        assert_eq!(parse("/back"), Ok(Command::Parent));
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
