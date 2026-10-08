//! Telling the user when a chat finishes, fails, or needs an answer: the watch socket's status
//! changes become alerts, which the UI shows as a toast or a desktop notification.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use coder_sdk::{ChatStatus, types};
use uuid::Uuid;

/// How a chat's turn ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The chat went from running to waiting. A plan-mode question also ends this way, since
    /// the watch payload carries no messages to tell the two apart.
    Finished,
    /// The chat waits on the user: its status is `requires_action`.
    NeedsAnswer,
    /// The chat stopped on an error.
    Failed,
}

/// One chat's turn ending, for the UI to tell the user about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatAlert {
    pub chat_id: Uuid,
    /// The chat's title as the screen may show it, or "Untitled".
    pub title: String,
    pub outcome: Outcome,
    /// Whether the chat was the open one when the alert was made.
    pub open: bool,
}

/// The last status seen for each root chat, so a watch event that changes it becomes an alert.
/// The record is its own, since the chat list drops events for chats on no loaded page.
#[derive(Debug, Default)]
pub struct Detector {
    last: HashMap<Uuid, (ChatStatus, Option<DateTime<Utc>>)>,
}

impl Detector {
    /// Records `chat`'s status without alerting, as from a list page, so a refetch after the
    /// watch reconnects never alerts.
    pub fn seed(&mut self, chat: &types::CodersdkChat) {
        self.record(chat);
    }

    /// The alert a watch event of `kind` for `chat` raises, if any. `open` is the open chat.
    /// The first status seen for a chat is no transition, subagents never alert, and an event
    /// older than the one recorded is ignored.
    pub fn observe(
        &mut self,
        kind: &str,
        chat: &types::CodersdkChat,
        open: Option<Uuid>,
    ) -> Option<ChatAlert> {
        if kind != "status_change" && kind != "action_required" {
            return None;
        }
        let (id, previous, status) = self.record(chat)?;
        let outcome = match (previous?, status) {
            (before, now) if before == now => return None,
            (ChatStatus::Running, ChatStatus::Waiting) => Outcome::Finished,
            (_, ChatStatus::RequiresAction) => Outcome::NeedsAnswer,
            (_, ChatStatus::Error) => Outcome::Failed,
            _ => return None,
        };
        let title = chat
            .title
            .as_deref()
            .map(crate::files::display_name)
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| "Untitled".into());
        Some(ChatAlert {
            chat_id: id,
            title,
            outcome,
            open: open == Some(id),
        })
    }

    /// Records `chat`'s status, returning its id, the status it replaced, and the new one, or
    /// `None` when the chat is a subagent, lacks an id or a status, or is older than the record.
    fn record(
        &mut self,
        chat: &types::CodersdkChat,
    ) -> Option<(Uuid, Option<ChatStatus>, ChatStatus)> {
        if chat.parent_chat_id.is_some() {
            return None;
        }
        let id = chat.id?;
        let status = ChatStatus::parse(chat.status.as_ref()?.as_str());
        let at = chat.updated_at;
        if let Some((_, Some(seen))) = self.last.get(&id)
            && at.is_some_and(|at| at < *seen)
        {
            return None;
        }
        let previous = self.last.insert(id, (status.clone(), at)).map(|(s, _)| s);
        Some((id, previous, status))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chat(id: Uuid, status: &str, at: &str) -> types::CodersdkChat {
        types::CodersdkChat {
            id: Some(id),
            title: Some("Fix the swagger annotations".into()),
            status: Some(types::CodersdkChatStatus(status.into())),
            updated_at: Some(at.parse().unwrap()),
            ..Default::default()
        }
    }

    const T1: &str = "2026-10-08T10:00:00Z";
    const T2: &str = "2026-10-08T10:01:00Z";
    const T3: &str = "2026-10-08T10:02:00Z";

    fn outcome(d: &mut Detector, kind: &str, c: &types::CodersdkChat) -> Option<Outcome> {
        d.observe(kind, c, None).map(|a| a.outcome)
    }

    #[test]
    fn the_first_status_seen_for_a_chat_is_no_transition() {
        let mut d = Detector::default();
        let id = Uuid::new_v4();
        assert_eq!(
            outcome(&mut d, "status_change", &chat(id, "waiting", T1)),
            None
        );
        assert_eq!(
            outcome(&mut d, "status_change", &chat(id, "error", T2)),
            Some(Outcome::Failed)
        );
    }

    #[test]
    fn running_to_waiting_is_finished() {
        let mut d = Detector::default();
        let id = Uuid::new_v4();
        assert_eq!(
            outcome(&mut d, "status_change", &chat(id, "running", T1)),
            None
        );
        let alert = d
            .observe("status_change", &chat(id, "waiting", T2), None)
            .expect("an alert");
        assert_eq!(
            alert,
            ChatAlert {
                chat_id: id,
                title: "Fix the swagger annotations".into(),
                outcome: Outcome::Finished,
                open: false,
            }
        );
        assert_eq!(
            outcome(&mut d, "status_change", &chat(id, "waiting", T3)),
            None,
            "a repeated status is no transition"
        );
    }

    #[test]
    fn interrupting_to_waiting_is_not_finished() {
        let mut d = Detector::default();
        let id = Uuid::new_v4();
        outcome(&mut d, "status_change", &chat(id, "interrupting", T1));
        assert_eq!(
            outcome(&mut d, "status_change", &chat(id, "waiting", T2)),
            None
        );
    }

    #[test]
    fn requires_action_and_action_required_need_an_answer_once() {
        let mut d = Detector::default();
        let id = Uuid::new_v4();
        outcome(&mut d, "status_change", &chat(id, "running", T1));
        assert_eq!(
            outcome(&mut d, "status_change", &chat(id, "requires_action", T2)),
            Some(Outcome::NeedsAnswer)
        );
        assert_eq!(
            outcome(&mut d, "action_required", &chat(id, "requires_action", T2)),
            None,
            "the action_required that follows is the same transition"
        );
        let other = Uuid::new_v4();
        outcome(&mut d, "status_change", &chat(other, "running", T1));
        assert_eq!(
            outcome(
                &mut d,
                "action_required",
                &chat(other, "requires_action", T2)
            ),
            Some(Outcome::NeedsAnswer),
            "action_required alone is enough"
        );
    }

    #[test]
    fn error_is_failed() {
        let mut d = Detector::default();
        let id = Uuid::new_v4();
        outcome(&mut d, "status_change", &chat(id, "running", T1));
        assert_eq!(
            outcome(&mut d, "status_change", &chat(id, "error", T2)),
            Some(Outcome::Failed)
        );
    }

    #[test]
    fn subagents_never_alert() {
        let mut d = Detector::default();
        let id = Uuid::new_v4();
        let sub = |status, at| types::CodersdkChat {
            parent_chat_id: Some(Uuid::new_v4()),
            ..chat(id, status, at)
        };
        assert_eq!(outcome(&mut d, "status_change", &sub("running", T1)), None);
        assert_eq!(outcome(&mut d, "status_change", &sub("waiting", T2)), None);
        assert_eq!(outcome(&mut d, "status_change", &sub("error", T3)), None);
    }

    #[test]
    fn other_event_kinds_and_stale_events_never_alert() {
        let mut d = Detector::default();
        let id = Uuid::new_v4();
        outcome(&mut d, "status_change", &chat(id, "running", T2));
        assert_eq!(
            outcome(&mut d, "title_change", &chat(id, "waiting", T3)),
            None
        );
        assert_eq!(
            outcome(&mut d, "status_change", &chat(id, "waiting", T1)),
            None,
            "an event older than the record is ignored"
        );
        assert_eq!(
            outcome(&mut d, "status_change", &chat(id, "waiting", T3)),
            Some(Outcome::Finished)
        );
    }

    #[test]
    fn a_seeded_status_counts_as_seen_but_seeding_never_alerts() {
        let mut d = Detector::default();
        let id = Uuid::new_v4();
        d.seed(&chat(id, "running", T1));
        d.seed(&chat(id, "running", T1));
        assert_eq!(
            outcome(&mut d, "status_change", &chat(id, "waiting", T2)),
            Some(Outcome::Finished)
        );
        d.seed(&chat(id, "running", T3));
        assert_eq!(
            outcome(&mut d, "status_change", &chat(id, "running", T3)),
            None
        );
    }

    #[test]
    fn the_open_chat_is_marked_and_titles_are_cleaned() {
        let mut d = Detector::default();
        let id = Uuid::new_v4();
        let mut c = chat(id, "running", T1);
        c.title = Some("  a\u{202e}b\x1b ".into());
        d.observe("status_change", &c, Some(id));
        c.status = Some(types::CodersdkChatStatus("waiting".into()));
        c.updated_at = Some(T2.parse().unwrap());
        let alert = d.observe("status_change", &c, Some(id)).expect("an alert");
        assert!(alert.open);
        assert_eq!(alert.title, "ab");
        c.title = None;
        c.status = Some(types::CodersdkChatStatus("error".into()));
        assert_eq!(
            d.observe("status_change", &c, None)
                .map(|a| (a.title, a.open)),
            Some(("Untitled".into(), false))
        );
    }
}
