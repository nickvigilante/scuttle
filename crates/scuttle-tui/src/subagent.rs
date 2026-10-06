//! Labels for the subagent tools, worded as the web UI words them
//! (`site/src/pages/AgentsPage/components/ChatElements/tools/subagentDescriptor.ts`, and
//! `SUBAGENT_VERBS` in `SubagentTool.tsx`).

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};

use coder_sdk::types;
use scuttle_core::chat_list::ChatList;
use scuttle_core::live::LiveBlock;
use serde_json::{Map, Value};
use uuid::Uuid;

/// What a subagent tool does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Spawn,
    Wait,
    Message,
    Interrupt,
}

/// The subagent action tool `name` performs, or `None` for any other tool. `spawn_subagent`
/// and `close_agent` are legacy names that older chats still hold.
pub fn action(name: &str) -> Option<Action> {
    Some(match name {
        "spawn_agent" | "spawn_explore_agent" | "spawn_computer_use_agent" | "spawn_subagent" => {
            Action::Spawn
        }
        "wait_agent" => Action::Wait,
        "message_agent" => Action::Message,
        "interrupt_agent" | "close_agent" => Action::Interrupt,
        _ => return None,
    })
}

/// How far a call got, which picks its verb.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Running,
    Done,
    Failed,
}

impl Action {
    fn verb(self, phase: Phase) -> &'static str {
        match (self, phase) {
            (Action::Spawn, Phase::Running) => "Spawning",
            (Action::Spawn, Phase::Done) => "Spawned",
            (Action::Spawn, Phase::Failed) => "Failed to spawn",
            (Action::Wait, Phase::Running) => "Waiting for",
            (Action::Wait, Phase::Done) => "Waited for",
            (Action::Wait, Phase::Failed) => "Failed waiting for",
            (Action::Message, Phase::Running) => "Messaging",
            (Action::Message, Phase::Done) => "Messaged",
            (Action::Message, Phase::Failed) => "Failed to message",
            (Action::Interrupt, Phase::Running) => "Interrupting",
            (Action::Interrupt, Phase::Done) => "Interrupted",
            (Action::Interrupt, Phase::Failed) => "Failed to interrupt",
        }
    }
}

/// `value` as an object, parsing a string that holds one, as the web UI's `parseArgs` does.
/// An object is borrowed, so only the string form is copied.
fn record(value: Option<&Value>) -> Option<Cow<'_, Map<String, Value>>> {
    match value? {
        Value::Object(map) => Some(Cow::Borrowed(map)),
        Value::String(s) => match serde_json::from_str::<Value>(s).ok()? {
            Value::Object(map) => Some(Cow::Owned(map)),
            _ => None,
        },
        _ => None,
    }
}

/// The trimmed, non-empty string `key` of `map`.
fn text(map: Option<&Map<String, Value>>, key: &str) -> Option<String> {
    map?.get(key)?
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// The title a call's result or arguments give, the result's first.
pub fn provided_title(args: Option<&Value>, result: Option<&Value>) -> Option<String> {
    let (args, result) = (record(args), record(result));
    text(result.as_deref(), "title").or_else(|| text(args.as_deref(), "title"))
}

/// The subagent a call's result or arguments name, the result's first.
pub fn chat_id(args: Option<&Value>, result: Option<&Value>) -> Option<Uuid> {
    let (args, result) = (record(args), record(result));
    text(result.as_deref(), "chat_id")
        .or_else(|| text(args.as_deref(), "chat_id"))
        .and_then(|id| Uuid::parse_str(&id).ok())
}

/// Subagent titles named anywhere in a transcript, by chat ID, so a `wait_agent` that names
/// only the chat finds the title its `spawn_agent` gave.
#[derive(Debug, Default)]
pub struct Titles {
    by_chat: HashMap<Uuid, String>,
}

impl Titles {
    /// Pairs every subagent call in `messages` and `live` with its result by call ID, and
    /// keeps each chat ID that a call and its result name together with a title.
    pub fn collect(messages: &[&types::CodersdkChatMessage], live: &[LiveBlock]) -> Titles {
        let mut call_args: BTreeMap<&str, &Value> = BTreeMap::new();
        let mut call_results: BTreeMap<&str, &Value> = BTreeMap::new();
        for part in messages.iter().flat_map(|m| m.content.iter()) {
            let Some(id) = part.tool_call_id.as_deref() else {
                continue;
            };
            if part.tool_name.as_deref().and_then(action).is_none() {
                continue;
            }
            match part.type_.as_ref().map(|t| t.as_str()) {
                Some("tool-call") => {
                    if let Some(args) = part.args.as_ref() {
                        call_args.insert(id, args);
                    }
                }
                Some("tool-result") => {
                    if let Some(result) = part.result.as_ref() {
                        call_results.insert(id, result);
                    }
                }
                _ => {}
            }
        }
        for block in live {
            match block {
                LiveBlock::ToolCall {
                    id,
                    name,
                    args: Some(args),
                    ..
                } if action(name).is_some() => {
                    call_args.insert(id, args);
                }
                LiveBlock::ToolResult {
                    id,
                    name,
                    result: Some(result),
                    ..
                } if action(name).is_some() => {
                    call_results.insert(id, result);
                }
                _ => {}
            }
        }
        let mut by_chat = HashMap::new();
        for id in call_args.keys().chain(call_results.keys()) {
            let (args, result) = (call_args.get(id).copied(), call_results.get(id).copied());
            if let (Some(chat), Some(title)) = (chat_id(args, result), provided_title(args, result))
            {
                by_chat.insert(chat, title);
            }
        }
        Titles { by_chat }
    }
}

/// The error chatd records as the result of a call its turn was interrupted before answering
/// (`interruptedToolResultErrorMessage` in `coderd/x/chatd/message_conversion.go`).
const INTERRUPTED_ERROR: &str = "tool call was interrupted before it produced a result";

/// Whether subagent tool `name` is a `wait_agent` whose turn was interrupted before it
/// answered: the parent stopped waiting, and the subagent may still be running.
pub fn stopped_waiting(name: &str, result: Option<&Value>, phase: Phase) -> bool {
    action(name) == Some(Action::Wait)
        && phase == Phase::Failed
        && text(record(result).as_deref(), "error").as_deref() == Some(INTERRUPTED_ERROR)
}

/// The verb and title for subagent tool `name`, or `None` for any other tool. The title is
/// the call's own, else the one another call in the transcript gave the same chat, else the
/// chat list's, else the first eight characters of the chat ID; a `None` title means nothing
/// names the subagent. A `wait_agent` whose turn was interrupted reads as stopped, not failed,
/// since the parent stopped waiting rather than the wait going wrong.
pub fn label(
    name: &str,
    args: Option<&Value>,
    result: Option<&Value>,
    phase: Phase,
    titles: &Titles,
    chats: Option<&ChatList>,
) -> Option<(&'static str, Option<String>)> {
    let action = action(name)?;
    let chat = chat_id(args, result);
    let title = provided_title(args, result)
        .or_else(|| chat.and_then(|c| titles.by_chat.get(&c).cloned()))
        .or_else(|| {
            let listed = chats?.find(chat?)?.title.as_deref()?.trim();
            (!listed.is_empty()).then(|| listed.to_owned())
        })
        .or_else(|| chat.map(|c| c.to_string()[..8].to_owned()))
        .map(|t| crate::markdown::drawable(&t).into_owned());
    let verb = if stopped_waiting(name, result, phase) {
        "Stopped waiting for"
    } else {
        action.verb(phase)
    };
    Some((verb, title))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn every_subagent_tool_has_an_action_and_other_tools_have_none() {
        for (name, wanted) in [
            ("spawn_agent", Action::Spawn),
            ("spawn_explore_agent", Action::Spawn),
            ("spawn_computer_use_agent", Action::Spawn),
            ("spawn_subagent", Action::Spawn),
            ("wait_agent", Action::Wait),
            ("message_agent", Action::Message),
            ("interrupt_agent", Action::Interrupt),
            ("close_agent", Action::Interrupt),
        ] {
            assert_eq!(action(name), Some(wanted), "{name}");
        }
        assert_eq!(action("execute"), None);
    }

    #[test]
    fn a_result_given_as_a_json_string_is_read_and_wins_over_the_args() {
        let id = Uuid::new_v4();
        let args = json!({"title": "From args", "chat_id": Uuid::new_v4()});
        let result = Value::String(json!({"title": "From result", "chat_id": id}).to_string());
        assert_eq!(
            provided_title(Some(&args), Some(&result)).as_deref(),
            Some("From result")
        );
        assert_eq!(chat_id(Some(&args), Some(&result)), Some(id));
        assert_eq!(provided_title(Some(&json!({"title": "  "})), None), None);
    }

    #[test]
    fn a_wait_cut_short_by_an_interrupt_reads_as_stopped_and_other_errors_as_failed() {
        let args = json!({"chat_id": Uuid::new_v4(), "title": "Fix CI"});
        let interrupted = json!({"error": "tool call was interrupted before it produced a result"});
        let label_for = |name: &str, result: &Value| {
            label(
                name,
                Some(&args),
                Some(result),
                Phase::Failed,
                &Titles::default(),
                None,
            )
            .map(|(verb, _)| verb)
        };
        assert_eq!(
            label_for("wait_agent", &interrupted),
            Some("Stopped waiting for")
        );
        assert_eq!(
            label_for("wait_agent", &Value::String(interrupted.to_string())),
            Some("Stopped waiting for")
        );
        assert_eq!(
            label_for("wait_agent", &json!({"error": "chat not found"})),
            Some("Failed waiting for")
        );
        assert_eq!(
            label_for("message_agent", &interrupted),
            Some("Failed to message")
        );
    }
}
