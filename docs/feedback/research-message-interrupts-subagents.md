# Does a parent-chat message interrupt running subagents?

Source: coder/coder at commit d1597a583b, checked out read-only at https://github.com/coder/coder/tree/d1597a583b.
Reported symptom: "sending a message [to the parent chat] interrupts subagent threads that are currently running."

## Verdict

(c) It depends, but the server never cascades an interrupt from a parent chat to its children.
A plain send from the web UI queues the message and touches neither the parent's turn nor any child.
A user interrupt of the parent (Stop, Escape, Send now, or Enter on an empty composer with a queued message) stops only the parent's own turn.
Children stop only when the parent model, on its next turn, chooses to call `interrupt_agent` or `message_agent` with `interrupt: true`, and the tool descriptions encourage exactly that for corrections and changed scope.
The UI also renders the parent's cancelled `wait_agent` call as a failed wait, which can look like the subagent itself stopped.

## 1. Web UI send path

The composer builds the create-message request without a `busy_behavior` field: `content`, `model_config_id`, `reasoning_effort`, `mcp_server_ids`, and plan-mode fields only (https://github.com/coder/coder/blob/d1597a583b/site/src/pages/AgentsPage/components/ChatConversation/submitChatTurn.ts#L373-L379).
The request is posted unchanged to `POST /api/v2/chats/{id}/messages` (https://github.com/coder/coder/blob/d1597a583b/site/src/api/api.ts#L3363-L3372).
No file under https://github.com/coder/coder/blob/d1597a583b/site/src outside the generated types references `busy_behavior`; the type is declared at https://github.com/coder/coder/blob/d1597a583b/site/src/api/typesGenerated.ts#L2063 and 3965.
The composer's `isLoading` is wired to the send mutation's pending flag, not to the chat's running status (https://github.com/coder/coder/blob/d1597a583b/site/src/pages/AgentsPage/components/ChatPageContent.tsx#L818), so typing and sending while the parent runs is allowed and lands in the queue.

The UI has three affordances that interrupt the parent chat.
The Stop button calls `onInterrupt` (https://github.com/coder/coder/blob/d1597a583b/site/src/pages/AgentsPage/components/AgentChatInput.tsx#L1960-L1980), and Escape does the same while streaming (https://github.com/coder/coder/blob/d1597a583b/site/src/pages/AgentsPage/components/AgentChatInput.tsx#L1193-L1202).
Both reach `POST /api/v2/chats/{id}/interrupt` for the current chat ID only (https://github.com/coder/coder/blob/d1597a583b/site/src/api/api.ts#L3385-L3390, https://github.com/coder/coder/blob/d1597a583b/site/src/pages/AgentsPage/AgentChatPage.tsx#L313-L314 and 530-535).
The queued-message row has a "Send now" button that promotes the message (https://github.com/coder/coder/blob/d1597a583b/site/src/pages/AgentsPage/components/QueuedMessagesList.tsx#L225-L241), which calls `POST /api/v2/chats/{id}/queue/{queuedMessageId}/promote` (https://github.com/coder/coder/blob/d1597a583b/site/src/api/api.ts#L3429-L3434).
Pressing Enter with an empty composer while messages are queued also promotes the queue head (https://github.com/coder/coder/blob/d1597a583b/site/src/pages/AgentsPage/components/AgentChatInput.tsx#L1130-L1145).
None of these code paths enumerates or targets child chats.

## 2. Server send, promote, and interrupt paths

The HTTP handler defaults an empty `busy_behavior` to queue (https://github.com/coder/coder/blob/d1597a583b/coderd/exp_chats.go#L2841-L2846), and `Server.SendMessage` repeats that default (https://github.com/coder/coder/blob/d1597a583b/coderd/x/chatd/chatd.go#L1463-L1471).
In `chatstate.Tx.SendMessage`, a queue-mode send to a running chat (states R0 and R1) inserts a queued row and keeps the current status (https://github.com/coder/coder/blob/d1597a583b/coderd/x/chatd/chatstate/transitions.go#L480-L498).
That helper does not insert history or change status (https://github.com/coder/coder/blob/d1597a583b/coderd/x/chatd/chatstate/transitions.go#L573-L601).
The worker's runner cancels its active task only when history version, status, or archived state changes (https://github.com/coder/coder/blob/d1597a583b/coderd/x/chatd/runner.go#L146-L174), so a queued send does not even cancel the parent's own in-flight turn.
Only `busy_behavior=interrupt` moves a running chat to `interrupting` (https://github.com/coder/coder/blob/d1597a583b/coderd/x/chatd/chatstate/transitions.go#L481-L493), and the web UI never sends it.

`Tx.PromoteQueuedMessage` from R1 or I1 moves the target to the queue head and sets the chat to `interrupting` (https://github.com/coder/coder/blob/d1597a583b/coderd/x/chatd/chatstate/transitions.go#L927-L944).
`Server.InterruptChat` runs `Tx.Interrupt` against one chat ID (https://github.com/coder/coder/blob/d1597a583b/coderd/x/chatd/chatd.go#L2386-L2419), and `Tx.Interrupt` updates only that chat's execution state (https://github.com/coder/coder/blob/d1597a583b/coderd/x/chatd/chatstate/transitions.go#L1004-L1050).
On the worker, an `interrupting` status spawns an interrupt task for that chat's runner (https://github.com/coder/coder/blob/d1597a583b/coderd/x/chatd/runner.go#L189-L190), and `StartInterrupt` loads and finalizes only `input.ChatID` (https://github.com/coder/coder/blob/d1597a583b/coderd/x/chatd/tasks.go#L251-L300).
The post-interruption hook only clears the root chat's turn summary (https://github.com/coder/coder/blob/d1597a583b/coderd/x/chatd/generation_preparer.go#L920-L930).

The only family-wide operation is archive.
`ArchiveChat` and `UnarchiveChat` cascade over the family through `chatstate.SetFamilyArchived` (https://github.com/coder/coder/blob/d1597a583b/coderd/x/chatd/chatd.go#L2060-L2130, https://github.com/coder/coder/blob/d1597a583b/coderd/x/chatd/chatstate/family.go#L27-L80).
Archive is permitted only from idle or error states, and an active member causes a state conflict instead of an interrupt (https://github.com/coder/coder/blob/d1597a583b/coderd/x/chatd/chatd.go#L2063-L2066).
The auto-archive query cascades to children by `root_chat_id` (https://github.com/coder/coder/blob/d1597a583b/coderd/database/queries/chats.sql#L2935 and 2972).
A search for `cascad`, `GetChildChats`, and `descendant` across https://github.com/coder/coder/blob/d1597a583b/coderd/x/chatd found no interrupt, cancel, or stop path that iterates over children; the child-listing call at https://github.com/coder/coder/blob/d1597a583b/coderd/x/chatd/subagent.go#L869 serves `list_agents`.

## 3. Child chat runs and context

`spawn_agent` creates the child as its own chat row with `InitialStatus: running` through `chatstate.CreateChatWithID` (https://github.com/coder/coder/blob/d1597a583b/coderd/x/chatd/subagent.go#L1146-L1173).
The tool call's context is used only for that creation, for context hydration, and for the watch event (https://github.com/coder/coder/blob/d1597a583b/coderd/x/chatd/subagent.go#L1174-L1181); no goroutine started there carries it into the child's generation.
A worker acquires the child like any other runnable chat.
The runner context derives from the worker manager's context (https://github.com/coder/coder/blob/d1597a583b/coderd/x/chatd/runner_manager.go#L221), the worker context derives from the worker's start context (https://github.com/coder/coder/blob/d1597a583b/coderd/x/chatd/worker.go#L54), and each task context derives from the runner context (https://github.com/coder/coder/blob/d1597a583b/coderd/x/chatd/runner.go#L208).
Cancelling the parent's generation task therefore cannot cancel the child's run.

`wait_agent` blocks in `awaitSubagentCompletion` and returns `ctx.Err()` when the parent's context ends (https://github.com/coder/coder/blob/d1597a583b/coderd/x/chatd/subagent.go#L1279-L1288).
It does not interrupt the child on that exit, and its description says "A timeout does not stop the child; it still owns its task" (https://github.com/coder/coder/blob/d1597a583b/coderd/x/chatd/subagent.go#L588-L593).

## 4. Model behavior after the parent is interrupted or redirected

The root-only orchestration prompt tells the model to "Follow each tool's availability and lifecycle guidance to reuse agents and stop abandoned work" (https://github.com/coder/coder/blob/d1597a583b/coderd/x/chatd/prompt.go#L10-L16).
The `message_agent` description tells the model to "set interrupt to true for corrections, changed scope, or a handoff that returns the child's task to you, so its current work stops first" (https://github.com/coder/coder/blob/d1597a583b/coderd/x/chatd/subagent.go#L737-L749).
With `interrupt: true`, the tool sends with `SendMessageBusyBehaviorInterrupt` (https://github.com/coder/coder/blob/d1597a583b/coderd/x/chatd/subagent.go#L770-L779), which moves a running child to `interrupting`.
`interrupt_agent` calls `InterruptChat` on the child directly (https://github.com/coder/coder/blob/d1597a583b/coderd/x/chatd/subagent.go#L797-L838 and 1387-1410).
The parent prompt contains no instruction to interrupt children on every new user message.
Speculation: a follow-up user message that reads as a correction or change of direction is likely to lead the parent model to call one of these tools, which would stop children as a model decision, not a server cascade.

## 5. message_agent and how a user message reaches children

A user message to the parent is stored only on the parent chat; no server code copies or forwards it to children.
The only path from the parent to a child is the parent model calling `message_agent`, which goes through `sendSubagentMessage` with an explicit busy behavior (https://github.com/coder/coder/blob/d1597a583b/coderd/x/chatd/subagent.go#L1184-L1215).
Delegated children have the orchestration block stripped from their prompt and cannot call `message_agent` or `list_agents` (https://github.com/coder/coder/blob/d1597a583b/coderd/x/chatd/subagent.go#L1060-L1063).

## What the user is probably seeing

Scenario A, a plain send while the parent waits on a child: the message queues, the parent stays blocked in `wait_agent` for up to the default 5 minutes (https://github.com/coder/coder/blob/d1597a583b/coderd/x/chatd/subagent.go#L63), and nothing is interrupted.
Users may then press Send now or Stop to make the parent respond sooner.

Scenario B, Send now, Stop, Escape, or empty Enter: the parent goes to `interrupting`, its runner cancels the active generation task, and the in-flight `wait_agent` call ends.
The interruption commits a synthetic error result for the unanswered tool call, "tool call was interrupted before it produced a result" (https://github.com/coder/coder/blob/d1597a583b/coderd/x/chatd/message_conversion.go#L27 and 1032-1046).
Inference from the renderer: the subagent tool row then shows the error verb "Failed waiting for" with an error icon (https://github.com/coder/coder/blob/d1597a583b/site/src/pages/AgentsPage/components/ChatElements/tools/SubagentTool.tsx#L38-L43 and 132-138), which reads as the subagent having stopped even though the child chat is still `running`.

Scenario C, the parent's next turn: the parent model sees the user's new message and the interrupted wait, and may call `interrupt_agent` or `message_agent` with `interrupt: true` because the tool guidance asks it to for corrections and changed scope.
In this case the children really are interrupted, by the model's tool call.

To tell the scenarios apart, check the child chat's status and the parent transcript.
If the child is still `running`, the user saw Scenario B's rendering.
If the parent transcript shows an `interrupt_agent` or `message_agent` call with `interrupt: true` after the user's message, the user saw Scenario C.
