//! The slash menu: scuttle's commands, then the user's personal skills, then the open chat's
//! workspace skills, with the labels and triggers that keep built-in names intact.

use crate::commands::COMMANDS;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skill {
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuKind {
    Command,
    Personal,
    Workspace,
    /// A value for a command's argument, such as a workspace name after `/workspace `.
    Argument,
    /// A dim line, such as "Loading skills…", that is never inserted.
    Note,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuEntry {
    pub label: String,
    /// What Tab puts in the composer.
    pub insert: String,
    pub description: String,
    pub kind: MenuKind,
    /// The names a typed prefix matches, each with its slash.
    pub names: Vec<String>,
}

/// The commands whose argument the slash menu completes, by name; `App::argument_menu` gives
/// each one's entries.
pub const ARGUMENT_COMMANDS: &[&str] = &["/workspace"];

/// A command line whose argument the slash menu completes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArgumentQuery<'t> {
    /// The command's name, such as `/workspace`, whichever of its names was typed.
    pub command: &'static str,
    /// The command as typed, such as `/ws`.
    pub typed: &'t str,
    /// The argument typed so far, possibly empty.
    pub partial: &'t str,
}

/// The argument being typed in `text`: a command from `ARGUMENT_COMMANDS`, by its name or an
/// alias, then one space, then at most one word with nothing after it. A space after the
/// word, a second line, or a longer first word, such as `/workspacex`, is no query.
pub fn argument_query(text: &str) -> Option<ArgumentQuery<'_>> {
    let (typed, partial) = text.split_once(' ')?;
    if partial.contains(char::is_whitespace) {
        return None;
    }
    let command = COMMANDS
        .iter()
        .filter(|c| ARGUMENT_COMMANDS.contains(&c.name))
        .find(|c| c.name == typed || c.aliases.contains(&typed))?;
    Some(ArgumentQuery {
        command: command.name,
        typed,
        partial,
    })
}

/// An entry that completes a command's argument with `name`, described by `description`.
pub fn argument(name: &str, description: &str) -> MenuEntry {
    MenuEntry {
        label: name.to_owned(),
        insert: name.to_owned(),
        description: description.to_owned(),
        kind: MenuKind::Argument,
        names: vec![name.to_owned()],
    }
}

/// A dim line, such as "Loading skills…", that is never inserted.
pub fn note_entry(text: &str) -> MenuEntry {
    MenuEntry {
        label: text.to_owned(),
        insert: String::new(),
        description: String::new(),
        kind: MenuKind::Note,
        names: vec![],
    }
}

/// The whole menu. `note` is a status line for the skill groups, such as a load failure.
/// Without a `username`, a personal skill that shadows a command is labeled by its trigger.
pub fn menu(
    personal: &[Skill],
    workspace: &[Skill],
    username: Option<&str>,
    note: Option<&str>,
) -> Vec<MenuEntry> {
    let builtin = |name: &str| {
        COMMANDS.iter().any(|c| {
            c.name.strip_prefix('/') == Some(name)
                || c.aliases.iter().any(|a| a.strip_prefix('/') == Some(name))
        })
    };
    let in_list = |list: &[Skill], name: &str| list.iter().any(|s| s.name == name);
    let mut entries: Vec<MenuEntry> = COMMANDS
        .iter()
        .map(|c| MenuEntry {
            label: c.display_usage(),
            insert: format!("{} ", c.name),
            description: c.description.to_owned(),
            kind: MenuKind::Command,
            names: std::iter::once(c.name)
                .chain(c.aliases.iter().copied())
                .map(str::to_owned)
                .collect(),
        })
        .collect();
    for s in personal {
        let shadows = builtin(&s.name);
        let label = match (shadows, username) {
            (true, Some(user)) => format!("/{user}:{}", s.name),
            (true, None) => format!("/personal/{}", s.name),
            (false, _) => format!("/{}", s.name),
        };
        let trigger = if shadows || in_list(workspace, &s.name) {
            format!("/personal/{}", s.name)
        } else {
            format!("/{}", s.name)
        };
        entries.push(MenuEntry {
            names: vec![label.clone(), trigger.clone()],
            label,
            insert: format!("{trigger} "),
            description: s.description.clone(),
            kind: MenuKind::Personal,
        });
    }
    for s in workspace {
        let trigger = if builtin(&s.name) || in_list(personal, &s.name) {
            format!("/workspace/{}", s.name)
        } else {
            format!("/{}", s.name)
        };
        entries.push(MenuEntry {
            names: vec![trigger.clone()],
            label: trigger.clone(),
            insert: format!("{trigger} "),
            description: s.description.clone(),
            kind: MenuKind::Workspace,
        });
    }
    if let Some(note) = note {
        entries.push(note_entry(note));
    }
    entries
}

/// The entries a typed `prefix` matches; the note shows while only `/` is typed. An entry
/// named exactly `prefix` comes first, so a skill named `plan` wins `/plan` over
/// `/plan-mode`; the rest keep the menu's order.
pub fn matches<'m>(menu: &'m [MenuEntry], prefix: &str) -> Vec<&'m MenuEntry> {
    ranked(menu, prefix, prefix == "/")
}

/// The argument entries a typed `partial` matches, as `matches` matches commands: by prefix,
/// an exact name first, the rest in their order. Every note line shows, so a list that is
/// still loading says so whatever is typed.
pub fn argument_matches<'m>(entries: &'m [MenuEntry], partial: &str) -> Vec<&'m MenuEntry> {
    ranked(entries, partial, true)
}

/// The entries with a name `prefix` starts, an exact name first and the rest in their order,
/// and the note lines when `notes` is set.
fn ranked<'m>(menu: &'m [MenuEntry], prefix: &str, notes: bool) -> Vec<&'m MenuEntry> {
    let mut found: Vec<&MenuEntry> = menu
        .iter()
        .filter(|e| match e.kind {
            MenuKind::Note => notes,
            _ => e.names.iter().any(|n| n.starts_with(prefix)),
        })
        .collect();
    // A stable sort, so prefix matches keep their order after the exact one.
    found.sort_by_key(|e| !e.names.iter().any(|n| n == prefix));
    found
}

/// The message to send when `text` starts with a skill's label or trigger: the trigger in its
/// place. `None` when the first word names no skill.
pub fn rewrite(text: &str, menu: &[MenuEntry]) -> Option<String> {
    let (first, rest) = match text.split_once(char::is_whitespace) {
        Some((first, rest)) => (first, Some(rest)),
        None => (text, None),
    };
    let skill = menu
        .iter()
        .filter(|e| matches!(e.kind, MenuKind::Personal | MenuKind::Workspace))
        .find(|e| e.names.iter().any(|n| n == first))?;
    let trigger = skill.insert.trim_end();
    Some(match rest {
        Some(rest) => format!("{trigger} {rest}"),
        None => trigger.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn skill(name: &str) -> Skill {
        Skill {
            name: name.into(),
            description: format!("the {name} skill"),
        }
    }

    fn entry<'m>(menu: &'m [MenuEntry], label: &str) -> &'m MenuEntry {
        menu.iter()
            .find(|e| e.label == label)
            .unwrap_or_else(|| panic!("no {label}"))
    }

    #[test]
    fn a_personal_skill_named_like_a_command_gets_a_qualified_label() {
        let menu = menu(
            &[skill("new"), skill("deploy"), skill("review")],
            &[skill("deploy"), skill("lint"), skill("model")],
            Some("nick"),
            None,
        );
        assert_eq!(entry(&menu, "/new").kind, MenuKind::Command);
        let shadow = entry(&menu, "/nick:new");
        assert_eq!(
            (shadow.insert.as_str(), &shadow.kind),
            ("/personal/new ", &MenuKind::Personal)
        );
        assert_eq!(
            entry(&menu, "/deploy").insert,
            "/personal/deploy ",
            "a personal skill that shares a workspace skill's name"
        );
        assert_eq!(entry(&menu, "/review").insert, "/review ");
        assert_eq!(entry(&menu, "/workspace/deploy").kind, MenuKind::Workspace);
        assert_eq!(entry(&menu, "/workspace/model").insert, "/workspace/model ");
        assert_eq!(entry(&menu, "/lint").insert, "/lint ");
        assert_eq!(
            rewrite("/nick:new ship it", &menu).as_deref(),
            Some("/personal/new ship it")
        );
        assert_eq!(
            rewrite("/personal/new", &menu).as_deref(),
            Some("/personal/new")
        );
        assert_eq!(rewrite("/lint", &menu).as_deref(), Some("/lint"));
        assert_eq!(rewrite("/nope", &menu), None);
    }

    #[test]
    fn matching_finds_commands_by_alias_and_skills_by_label_or_trigger() {
        let menu = menu(&[skill("new")], &[], Some("nick"), Some("Loading skills…"));
        let labels = |p: &str| {
            matches(&menu, p)
                .iter()
                .map(|e| e.label.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(labels("/ex"), ["/quit (/exit)"]);
        let commands = |p: &str| {
            matches(&menu, p)
                .iter()
                .filter(|e| e.kind == MenuKind::Command)
                .map(|e| e.insert.trim_end().to_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            commands("/c"),
            ["/chats", "/info", "/compact", "/clear", "/copy"],
            "the alias /chat-info starts with /c"
        );
        assert_eq!(labels("/nick"), ["/nick:new"]);
        assert_eq!(labels("/personal/"), ["/nick:new"]);
        assert!(
            labels("/").iter().any(|l| l == "Loading skills…"),
            "the note shows with every match"
        );
        assert!(!labels("/q").iter().any(|l| l == "Loading skills…"));
    }

    #[test]
    fn an_exact_name_is_listed_first_and_prefix_matches_keep_their_order() {
        let menu = menu(&[skill("plan"), skill("newsletter")], &[], None, None);
        let labels = |p: &str| {
            matches(&menu, p)
                .iter()
                .map(|e| e.label.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            labels("/plan"),
            ["/plan", "/plan-mode [on|off]"],
            "a skill that prefixes a command"
        );
        assert_eq!(
            labels("/new"),
            ["/new", "/newsletter"],
            "a command that prefixes a skill"
        );
        assert_eq!(labels("/pla"), ["/plan-mode [on|off]", "/plan"]);
    }

    #[test]
    fn an_argument_query_needs_a_listed_command_a_space_and_one_word() {
        fn query(text: &str) -> Option<(&'static str, &str, &str)> {
            argument_query(text).map(|q| (q.command, q.typed, q.partial))
        }
        assert_eq!(query("/workspace "), Some(("/workspace", "/workspace", "")));
        assert_eq!(query("/ws dev-2"), Some(("/workspace", "/ws", "dev-2")));
        assert_eq!(query("/workspace"), None, "no space yet: the command menu");
        assert_eq!(query("/workspacex dev"), None);
        assert_eq!(
            query("/workspace/deploy x"),
            None,
            "a workspace skill's trigger"
        );
        assert_eq!(query("/workspace dev "), None, "a space after the name");
        assert_eq!(query("/workspace dev\nmore"), None);
        assert_eq!(
            query("/model gpt"),
            None,
            "only /workspace completes its argument"
        );
    }

    #[test]
    fn argument_matches_go_by_prefix_with_an_exact_name_first_and_every_note() {
        let entries = vec![
            argument("dev-20", ""),
            argument("build3", ""),
            argument("dev-2", ""),
            argument("none", "Detach the workspace"),
            note_entry("Loading workspaces…"),
        ];
        let labels = |p: &str| {
            argument_matches(&entries, p)
                .iter()
                .map(|e| e.label.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            labels(""),
            ["dev-20", "build3", "dev-2", "none", "Loading workspaces…"]
        );
        assert_eq!(labels("dev-2"), ["dev-2", "dev-20", "Loading workspaces…"]);
        assert_eq!(labels("build3"), ["build3", "Loading workspaces…"]);
        assert_eq!(labels("zzz"), ["Loading workspaces…"]);
        assert!(
            argument_matches(&entries[..4], "zzz").is_empty(),
            "no match and no note: no menu"
        );
        let dev = argument("dev-2", "running · Docker");
        assert_eq!(
            (dev.insert.as_str(), dev.names.as_slice(), &dev.kind),
            (
                "dev-2",
                ["dev-2".to_owned()].as_slice(),
                &MenuKind::Argument
            )
        );
    }
}
