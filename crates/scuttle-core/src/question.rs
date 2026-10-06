//! Plan-mode questions from `ask_user_question`, and the plan from `propose_plan`.

use coder_sdk::types;
use serde::Deserialize;

use crate::transcript::Transcript;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    pub label: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Question {
    pub header: String,
    pub question: String,
    pub options: Vec<Choice>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingQuestion {
    pub call_id: String,
    pub questions: Vec<Question>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    Choice(String),
    Other(String),
}

#[derive(Deserialize)]
struct Args {
    #[serde(default)]
    questions: Vec<ArgQuestion>,
}

#[derive(Deserialize)]
struct ArgQuestion {
    #[serde(default)]
    header: String,
    #[serde(default)]
    question: String,
    #[serde(default)]
    options: Vec<ArgOption>,
}

#[derive(Deserialize)]
struct ArgOption {
    #[serde(default)]
    label: String,
    #[serde(default)]
    description: String,
}

fn part_type(p: &types::CodersdkChatMessagePart) -> Option<&str> {
    p.type_.as_ref().map(|t| t.as_str())
}

/// The last tool call of the newest assistant message, unless a user message follows it or
/// the call's tool failed. chatd ends a plan-mode turn only on a successful result
/// (`historyHasStopAfterToolResult`), and the web UI offers no answer for a failed call.
fn last_call(transcript: &Transcript) -> Option<&types::CodersdkChatMessagePart> {
    let mut failed: Vec<&str> = Vec::new();
    for m in transcript.messages().rev() {
        match m.role.as_ref().map(|r| r.as_str()) {
            Some("user") => return None,
            Some("assistant") => {
                return m
                    .content
                    .iter()
                    .rev()
                    .find(|p| part_type(p) == Some("tool-call"))
                    .filter(|p| {
                        p.tool_call_id
                            .as_deref()
                            .is_none_or(|id| !failed.contains(&id))
                    });
            }
            _ => failed.extend(
                m.content
                    .iter()
                    .filter(|p| part_type(p) == Some("tool-result") && p.is_error == Some(true))
                    .filter_map(|p| p.tool_call_id.as_deref()),
            ),
        }
    }
    None
}

/// The questions the agent is waiting on: the newest assistant message ends with an
/// `ask_user_question` call and no user message follows. Options without a label are dropped,
/// and so is one named "Other", since the menu always adds its own, as the web UI does.
pub fn pending(transcript: &Transcript) -> Option<PendingQuestion> {
    let call =
        last_call(transcript).filter(|p| p.tool_name.as_deref() == Some("ask_user_question"))?;
    let args: Args = serde_json::from_value(call.args.clone()?).ok()?;
    let questions: Vec<Question> = args
        .questions
        .into_iter()
        .map(|q| Question {
            header: q.header,
            question: q.question,
            options: q
                .options
                .into_iter()
                .filter(|o| {
                    let label = o.label.trim();
                    !label.is_empty() && !label.eq_ignore_ascii_case("other")
                })
                .map(|o| Choice {
                    label: o.label,
                    description: o.description,
                })
                .collect(),
        })
        .collect();
    (!questions.is_empty()).then(|| PendingQuestion {
        call_id: call.tool_call_id.clone().unwrap_or_default(),
        questions,
    })
}

/// The successful `propose_plan` results that hold a plan: inline `content`, or the stored
/// plan file's `file_id` that chatd returns (`chattool/proposeplan.go`). The web UI offers its
/// Implement button only for such a result (`ProposePlanTool.tsx`, `canImplementPlan`).
fn plan_results(transcript: &Transcript) -> impl Iterator<Item = &types::CodersdkChatMessagePart> {
    let filled = |v: Option<&serde_json::Value>| {
        v.and_then(|v| v.as_str())
            .is_some_and(|s| !s.trim().is_empty())
    };
    transcript
        .messages()
        .flat_map(|m| m.content.iter())
        .filter(|p| {
            part_type(p) == Some("tool-result")
                && p.tool_name.as_deref() == Some("propose_plan")
                && p.is_error != Some(true)
        })
        .filter(move |p| {
            p.result
                .as_ref()
                .is_some_and(|r| filled(r.get("content")) || filled(r.get("file_id")))
        })
}

/// Whether the newest assistant message ends with a `propose_plan` call whose plan arrived and
/// no user message follows it: the plan the hint and Ctrl+Enter offer.
pub fn plan_ready(transcript: &Transcript) -> bool {
    let Some(call) =
        last_call(transcript).filter(|p| p.tool_name.as_deref() == Some("propose_plan"))
    else {
        return false;
    };
    plan_results(transcript)
        .any(|r| r.tool_call_id.is_some() && r.tool_call_id == call.tool_call_id)
}

/// Whether any successful `propose_plan` in the chat holds a plan, which `/implement` accepts
/// as the web UI's button on each plan does.
pub fn plan_proposed(transcript: &Transcript) -> bool {
    plan_results(transcript).next().is_some()
}

fn one(answer: &Answer) -> String {
    match answer {
        Answer::Choice(label) => label.clone(),
        Answer::Other(text) => format!("Other: {}", text.trim()),
    }
}

/// The message the web UI sends for these answers.
pub fn answer_text(questions: &[Question], answers: &[Answer]) -> String {
    if let ([_], [answer]) = (questions, answers) {
        return one(answer);
    }
    questions
        .iter()
        .zip(answers)
        .enumerate()
        .map(|(i, (q, a))| {
            let header = if q.header.trim().is_empty() {
                format!("Question {}", i + 1)
            } else {
                q.header.clone()
            };
            format!("{}. {header}: {}", i + 1, one(a))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use serde_json::json;

    pub(crate) fn asked() -> serde_json::Value {
        json!([
            {"id": 1, "role": "user", "content": [{"type": "text", "text": "plan the M2 work"}]},
            {"id": 2, "role": "assistant", "content": [
                {"type": "text", "text": "Two questions first."},
                {"type": "tool-call", "tool_call_id": "q1", "tool_name": "ask_user_question", "args": {"questions": [
                    {"header": "Scope", "question": "Which crate?", "options": [
                        {"label": "core", "description": "state and rules"},
                        {"label": "tui", "description": "drawing and keys"},
                        {"label": " ", "description": "a blank option is dropped"}
                    ]},
                    {"header": "", "question": "Tests?", "options": [{"label": "yes", "description": ""}]}
                ]}}
            ]}
        ])
    }

    /// A `propose_plan` call and its result, as chatd stores them, starting at message `id`.
    pub(crate) fn proposed(id: i64) -> [serde_json::Value; 2] {
        let call = format!("p{id}");
        [
            json!({"id": id, "role": "assistant", "content": [
                {"type": "tool-call", "tool_call_id": call, "tool_name": "propose_plan", "args": {"path": "PLAN.md"}}
            ]}),
            json!({"id": id + 1, "role": "tool", "content": [
                {"type": "tool-result", "tool_call_id": call, "tool_name": "propose_plan",
                 "result": {"ok": true, "path": "PLAN.md", "kind": "plan", "file_id": "6f1c1b6e-8d4b-4c55-9a7e-1d2b3c4d5e6f"}}
            ]}),
        ]
    }

    fn transcript(messages: serde_json::Value) -> Transcript {
        let mut t = Transcript::default();
        t.load(serde_json::from_value(messages).unwrap());
        t
    }

    #[test]
    fn a_question_is_pending_until_a_user_message_follows() {
        let t = transcript(asked());
        let pending = pending(&t).unwrap();
        assert_eq!(pending.call_id, "q1");
        assert_eq!(pending.questions.len(), 2);
        assert_eq!(pending.questions[0].options.len(), 2);
        assert_eq!(pending.questions[0].options[1].label, "tui");
        let mut answered = asked();
        answered
            .as_array_mut()
            .unwrap()
            .push(json!({"id": 3, "role": "user", "content": [{"type": "text", "text": "core"}]}));
        assert_eq!(super::pending(&transcript(answered)), None);
        assert!(!plan_ready(&t));
    }

    #[test]
    fn answers_are_labels_or_other_text_and_several_are_numbered() {
        let t = transcript(asked());
        let questions = pending(&t).unwrap().questions;
        assert_eq!(
            answer_text(&questions[..1], &[Answer::Choice("tui".into())]),
            "tui"
        );
        assert_eq!(
            answer_text(&questions[..1], &[Answer::Other("  both ".into())]),
            "Other: both"
        );
        assert_eq!(
            answer_text(
                &questions,
                &[
                    Answer::Choice("tui".into()),
                    Answer::Other("unit tests only".into())
                ]
            ),
            "1. Scope: tui\n2. Question 2: Other: unit tests only"
        );
    }

    #[test]
    fn a_proposed_plan_is_ready_until_a_user_message_follows() {
        let t = transcript(json!(proposed(1)));
        assert!(
            plan_ready(&t),
            "the tool result after the call does not count as a reply"
        );
        assert_eq!(pending(&t), None);
    }

    #[test]
    fn an_option_named_other_is_dropped_since_the_menu_adds_its_own() {
        let t = transcript(json!([
            {"id": 1, "role": "assistant", "content": [
                {"type": "tool-call", "tool_call_id": "q1", "tool_name": "ask_user_question", "args": {"questions": [
                    {"header": "Scope", "question": "Which crate?", "options": [
                        {"label": "core", "description": "state"},
                        {"label": " OTHER ", "description": "something else"}
                    ]}
                ]}}
            ]}
        ]));
        let labels: Vec<String> = pending(&t).unwrap().questions[0]
            .options
            .iter()
            .map(|o| o.label.clone())
            .collect();
        assert_eq!(labels, ["core"]);
    }

    #[test]
    fn a_question_or_plan_whose_tool_failed_is_not_offered() {
        let failed = |name: &str| {
            transcript(json!([
                {"id": 1, "role": "assistant", "content": [
                    {"type": "tool-call", "tool_call_id": "c1", "tool_name": name, "args": {"questions": [
                        {"header": "Scope", "question": "Which crate?", "options": [{"label": "core", "description": "state"}]}
                    ]}}
                ]},
                {"id": 2, "role": "tool", "content": [
                    {"type": "tool-result", "tool_call_id": "c1", "tool_name": name, "result": "questions[0].options must contain 2-4 items", "is_error": true}
                ]}
            ]))
        };
        assert_eq!(pending(&failed("ask_user_question")), None);
        assert!(!plan_ready(&failed("propose_plan")));
    }

    #[test]
    fn a_plan_needs_content_and_implement_finds_one_anywhere_in_the_chat() {
        let empty = transcript(json!([
            {"id": 1, "role": "assistant", "content": [
                {"type": "tool-call", "tool_call_id": "p1", "tool_name": "propose_plan", "args": {}}
            ]},
            {"id": 2, "role": "tool", "content": [
                {"type": "tool-result", "tool_call_id": "p1", "tool_name": "propose_plan", "result": {"ok": true}}
            ]}
        ]));
        assert!(!plan_ready(&empty), "a plan without content offers nothing");
        assert!(!plan_proposed(&empty));
        let inline = transcript(json!([
            {"id": 1, "role": "assistant", "content": [
                {"type": "tool-call", "tool_call_id": "p1", "tool_name": "propose_plan", "args": {}}
            ]},
            {"id": 2, "role": "tool", "content": [
                {"type": "tool-result", "tool_call_id": "p1", "tool_name": "propose_plan", "result": {"content": "1. Do it"}}
            ]}
        ]));
        assert!(
            plan_ready(&inline),
            "inline content counts, as in the web UI"
        );
        let [call, result] = proposed(1);
        let earlier = transcript(json!([
            call,
            result,
            {"id": 3, "role": "user", "content": [{"type": "text", "text": "and tests?"}]},
            {"id": 4, "role": "assistant", "content": [{"type": "text", "text": "Sure."}]}
        ]));
        assert!(
            !plan_ready(&earlier),
            "the hint and Ctrl+Enter are for the latest turn only"
        );
        assert!(
            plan_proposed(&earlier),
            "/implement takes any successful plan"
        );
    }
}
