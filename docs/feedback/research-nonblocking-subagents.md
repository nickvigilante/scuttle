# Research: non-blocking subagents in Coder Agents (chatd)

Question: while chatd subagents run, can the parent chat keep accepting and answering the user's messages, the way Claude Code's main thread does?
Source read at coder/coder `main` commit `d1597a583b` (2026-10-01), read-only.

## Short answer

Spawning is already non-blocking, but nothing tells the parent when a child finishes, and `wait_agent` blocks the parent's turn.
The parent is usable while children run only when its model has ended the turn without calling `wait_agent`.
In that case the parent has no way to learn that a child finished until the user sends another message.
When the parent is inside `wait_agent`, which the prompt tells it to use, the parent is `running` and user messages queue behind the wait, for up to 5 minutes per call.
I found no open GitHub issue that asks for this, but several open and recently closed PRs cover parts of it.

## What chatd does today

`spawn_agent` creates the child chat and returns `chat_id`, `title`, and `status` right away, without waiting for the child (https://github.com/coder/coder/blob/d1597a583b/coderd/x/chatd/subagent.go#L528-L550).
The child runs as its own chat, with `InitialStatus: running` (https://github.com/coder/coder/blob/d1597a583b/coderd/x/chatd/subagent.go#L1166).
The spawn description tells the parent to do only work that does not depend on the child, and to call `wait_agent` when its next step needs the child's result (https://github.com/coder/coder/blob/d1597a583b/coderd/x/chatd/subagent_catalog.go#L319-L327).
The root system prompt says "Use wait_agent to collect results needed for the task before claiming completion" (https://github.com/coder/coder/blob/d1597a583b/coderd/x/chatd/prompt.go#L13).
`wait_agent` is a blocking tool call that defaults to 5 minutes (https://github.com/coder/coder/blob/d1597a583b/coderd/x/chatd/subagent.go#L63 and https://github.com/coder/coder/blob/d1597a583b/coderd/x/chatd/subagent.go#L88).
It subscribes to the child's state channel and polls until the child leaves `running` or `interrupting`, the timer fires, or the context is cancelled (https://github.com/coder/coder/blob/d1597a583b/coderd/x/chatd/subagent.go#L1223-L1297 and https://github.com/coder/coder/blob/d1597a583b/coderd/x/chatd/subagent.go#L1437-L1438).
A timeout returns `timed_out: true` and leaves the child running (https://github.com/coder/coder/blob/d1597a583b/coderd/x/chatd/subagent.go#L586-L592 and https://github.com/coder/coder/blob/d1597a583b/coderd/x/chatd/subagent.go#L673-L690).
`message_agent` queues a message to a busy child, or interrupts the child first when `interrupt` is true (https://github.com/coder/coder/blob/d1597a583b/coderd/x/chatd/subagent.go#L736-L790).
`interrupt_agent` stops a child's current work and leaves it waiting (https://github.com/coder/coder/blob/d1597a583b/coderd/x/chatd/subagent.go#L798-L830).
`spawn_agent` has no background or async option, because spawning is already asynchronous.
`wait_agent` has no non-blocking "poll and return" mode other than a short `timeout_seconds`, and `list_agents` reports status.
I found no code that wakes an idle parent when a child finishes, and no code that injects a completion message into the parent.
The only path back into the parent is the parent calling `wait_agent` or `list_agents` during one of its own turns.

## What happens when the user messages a busy parent

While `wait_agent` runs, the parent chat's turn is still in progress, so the parent's status is `running`.
`SendMessage` takes a busy behavior of `queue` (the default) or `interrupt` (https://github.com/coder/coder/blob/d1597a583b/coderd/x/chatd/chatd.go#L1112-L1123 and https://github.com/coder/coder/blob/d1597a583b/coderd/x/chatd/chatd.go#L1452-L1471).
The HTTP handler maps the request's `busy_behavior` and defaults to `queue` (https://github.com/coder/coder/blob/d1597a583b/coderd/exp_chats.go#L2841-L2846).
With `queue`, the message waits in `chat_queued_messages` until the turn ends, which includes the end of the blocking `wait_agent` call.
With `interrupt`, chatd cancels the active generation, which also cancels `wait_agent` through its context (https://github.com/coder/coder/blob/d1597a583b/coderd/x/chatd/subagent.go#L1291-L1292), and then promotes the queued message.
`PromoteQueued`, the "send now" path for a queued message, moves a running chat to `interrupting` so the worker drains the in-flight generation before it promotes the message (https://github.com/coder/coder/blob/d1597a583b/coderd/x/chatd/chatd.go#L2158-L2167).
I found no code that cascades the parent's interrupt to children, so the children keep running, but the parent must call `wait_agent` again to collect them.
The result is that the user's only choices are to wait for the wait or to interrupt the parent's turn.

## Can the parent be used while children run?

Partly.
If the parent model spawns children and ends its turn without calling `wait_agent`, the parent goes idle and answers new user messages normally while the children run.
The parent does not learn that a child finished until a later turn calls `wait_agent` or `list_agents`, and a later turn starts only when the user sends a message.
The prompt pushes the model toward calling `wait_agent` in the same turn, which is the blocking case.
In that case user messages queue for up to 5 minutes per `wait_agent` call, and the model often calls `wait_agent` again after a timeout.
Interrupting gets the user's message answered but ends the parent's orchestration turn.

## Existing GitHub issues and PRs

No GitHub issue directly asks for "parent stays responsive while subagents run" or for background subagents.
The searches covered "subagent", "background agent", "wait_agent", "non-blocking", "parallel agents", "main thread", "async subagent", "spawn_agent", "steer", "notify parent", "parent chat", and "queued message".
The searches found these related PRs:

- https://github.com/coder/coder/pull/29784 (closed 2026-09-29, not merged): delivers `steer` and `interrupt` queued messages before the next model call within a turn. It states that "a `steer` still waits for long tool calls like `execute` or `wait_agent` to finish. Returning early from those is a follow-up." That follow-up is the core of this request.
- https://github.com/coder/coder/pull/29715 (open): direct agent messaging. Children can message their direct parent through `SendMessage` with sender identity, so a child could reach an idle parent before or after it finishes.
- https://github.com/coder/coder/pull/29617 (open, `chat-tree` experiment): `send_chat_message` delivers fire-and-forget messages between a parent and its named children, with `queue` or `interrupt` delivery.
- https://github.com/coder/coder/pull/29615 (open, `chat-tree` experiment): adds a chat tree root, named child chats, and a tree API, which is a user-addressable parent and child structure.
- https://github.com/coder/coder/pull/29383 (open, `chat-orchestrator` experiment): adds a persistent per-user orchestrator chat that spawns independent chats and reads their state.
- https://github.com/coder/coder/pull/29292 (open): keeps `wait_agent` waiting on a paused child.
- https://github.com/coder/coder/pull/28758 (open): refactor that extracts a subagent manager, which is a likely place for a background-completion feature.
- https://github.com/coder/coder/pull/29484 (open) and https://github.com/coder/coder/pull/29041 (merged): tune delegation and ownership guidance in the prompts.
- https://github.com/coder/coder/pull/27335 (merged): documents the 5-minute `wait_agent` default and that a timeout does not stop the child.
- https://github.com/coder/coder/pull/26673 (merged): reframed subagents as persistent workers that can be interrupted, and reports that about 23% of `wait_agent` calls time out.
- https://github.com/coder/coder/issues/19675 (closed): Tasks UX request to avoid auto-redirect to encourage a "background agent workflow". It concerns Coder Tasks, not chatd subagents.

## Claude Code comparison

The Claude Code `Agent` (Task) tool runs a subagent in the foreground by default, and a foreground subagent blocks the main thread until it returns.
Claude Code can also run subagents in the background, either on request or by backgrounding a running one, and the main conversation stays interactive.
When a background subagent finishes, Claude Code delivers a completion notification into the main conversation, which starts a new main-agent turn without user input.
The main agent can also continue or message a running subagent and stop it.
The two features that make this work are the completion notification that wakes the main agent and a main turn that is not held open by a wait.

## What a feature request should ask for

1. A completion notification: when a child reaches a terminal or idle state, chatd delivers a system-authored message to the parent, which starts a parent turn if the parent is idle and queues the message if it is busy.
2. Non-blocking collection: a way for the parent to end its turn while children own their tasks, with prompt guidance that prefers the notification over a long `wait_agent`.
3. A user message that interrupts the wait: a user message queued during `wait_agent` makes `wait_agent` return early without cancelling the whole turn, so the model can answer and resume waiting. This is the follow-up that https://github.com/coder/coder/pull/29784 named.
4. A clear status: the UI shows that the parent is idle but has children running, so users know they can talk to it.
5. Defined interaction with the existing queue and interrupt semantics, the `paused` status from https://github.com/coder/coder/pull/29292, and child-to-parent messages from https://github.com/coder/coder/pull/29715, so the features do not overlap.

The request should cite https://github.com/coder/coder/pull/29784 and https://github.com/coder/coder/pull/29715, which already cover parts of this work.
