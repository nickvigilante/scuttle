# scuttle: M2 design

**Status:** draft for review.
This amends milestone M2 in [the design spec](2026-09-28-scuttle-design.md) and was drafted autonomously from the approved spec, the M1 daily-use feedback, the M1 and M1.5 review ledgers, and read-only research.
No plan or code starts until the author approves it.

**Baseline:** scuttle branch `m1-polish` at `b59ce84` (M1.5 including its fix wave), `coder-sdk` at `2cdac1e`, and coder/coder `main` at `d1597a583b`, which is also the ref the pinned SDK was generated from.
Citations are `path:line` in coder/coder unless another repo is named.
Anything marked "unconfirmed" could not be verified against source.

**Related:** [M1 daily-use feedback](../feedback/2026-09-29-m1-daily-use.md) on branch `m1-polish`, an internal ticket.

## Design rule

The author confirmed the rule for M2 and later milestones: a minimal but customizable interface built on slash commands.
Each command reaches one part of the Coder API.
scuttle does not mirror the web UI screen for screen, but it covers most of the web UI's features.
Every M2 feature below is therefore a command that opens one overlay or runs one action, and none of them adds a permanent panel.

## 1. Goal and scope

### Goal

M2 makes scuttle the author's only Coder Agents client for a full working day.
Section 10 of the spec defines M2 as navigation and chat features, and the success criterion (two consecutive weeks of daily use) starts after M2.
The daily-use feedback added inspection commands (`/info`, `/workspace` details, `/git`, `/mcp`) that the author reaches for many times a day, so they join M2.

### In M2

| Feature                          | Source                               | Notes                                                                 |
|----------------------------------|--------------------------------------|-----------------------------------------------------------------------|
| `/chats` with watch updates      | Spec section 10, feedback M2 table   | Adds search, filters, unread, archived, and subagent nesting.         |
| `/subagents`                     | Feedback M2 table                    | Popup with a live preview; Enter opens full screen.                   |
| Chat switching                   | Spec section 10, M1 ledger           | The shared mechanism behind `/chats`, `/subagents`, and `/new`.       |
| `/title`                         | Spec section 10                      | Set, or propose and confirm.                                          |
| `/queue`                         | Spec section 10                      | List, delete, and promote queued messages.                            |
| Plan-mode questions and the plan | Spec section 10, feedback item 10    | `ask_user_question` menus and "Implement the plan"; the toggle shipped in M1.5. |
| Attachments                      | Spec section 10                      | `/attach <path>` and `@path`.                                         |
| `/model` table                   | Feedback M2 table                    | Grouped by provider, with context limits.                             |
| Skills in the slash menu         | Feedback M2 table, spec open question 3 | Listing and inserting only; editing stays in M4.                   |
| `/info`                          | Feedback M2 table                    | Chat details including cost.                                          |
| `/workspace` details             | Feedback M2 table                    | A table without a workspace, details with one.                        |
| `/git`                           | Feedback M2 table                    | Branch, PR, repo, provider, and the diff in the user's pager.        |
| `/mcp`                           | Feedback M2 table                    | The chat's MCP servers and what scuttle can tell about their health.  |
| Deferred fixes kept in M2        | Section 17                           | Mostly small, and most touch code M2 rewrites anyway.                 |

### Moved out of M2

| Item                                        | Moves to | Why                                                                                                   |
|---------------------------------------------|----------|-------------------------------------------------------------------------------------------------------|
| The `coder` theme source                    | M4       | It is not navigation, and M4 already holds `/theme`, the `custom` palette, and the color-blind themes the feedback's "Later" list ties to it. Grouping them builds the palette work once. |
| Cost in the footer                          | M3       | `/info` shows cost in M2; footer fields belong to the M3 `/statusline` work.                           |
| Skill editing (`/settings` skills section)  | M4       | M2 only lists and inserts skills.                                                                      |
| Notice-queue and narrow-footer minors       | M3       | They are footer behavior, which M3 redesigns.                                                          |

`/new` shipped in M1.5, so M2 only changes how it shares code with chat switching (section 4).
The plan-mode toggle also shipped in M1.5 as `/plan-mode`; M2 adds only the question menus and the plan action.

## 2. `/chats`

### Behavior

`/chats` opens a full-height overlay listing the user's chats, newest activity first, with pinned chats at the top.
Each root chat is one row, and its subagents nest one level below it, collapsed by default.
Typing filters the list with a fuzzy match on the title.
The last row is always "Search all chats for “<query>”", which runs the server's full-text search across titles, PR titles, and message bodies.

Each row shows, from left to right:

- A status marker: an animated spinner while `running` or `interrupting`, `?` for `requires_action`, `!` for `error`, and a blank for `waiting`.
- An unread dot when the chat has an assistant message the user has not seen.
- The title, truncated to fit.
- A subagent count such as `+3` when the chat has children, and the marker of the most active child when the row is collapsed.
- `archived` in the dim style when the chat is archived.
- The relative time since `updated_at`.

```text
 Chats   all | active | unread | archived                      /chats
 > fix watch
   ⠋ • Fix the flaky watch reconnect test             +2    4m
        ⠋ explore: find every watch caller                  4m
          review: check the diff                            9m
       Draft the M2 design                                  1h
       Old reconnect spike                   archived       3d
   Search all chats for “fix watch”
```

### Command, aliases, and keys

- `/chats` opens the overlay, with `/resume` as an alias for Claude Code muscle memory.
- `/chats <query>` opens it with the filter already typed.
- The default key is Ctrl+R (decision in section 18).

| Key          | Action                                                                  |
|--------------|-------------------------------------------------------------------------|
| Typing       | Fuzzy filter on titles.                                                 |
| Up, Down     | Move the selection.                                                     |
| Right, Left  | Expand or collapse the selected chat's subagents.                       |
| Enter        | Open the selected chat or subagent, or run the server search row.       |
| Tab          | Cycle the filter: all, active, unread, archived.                        |
| Ctrl+A       | Archive or unarchive the selected root chat, with a confirmation.       |
| Ctrl+E       | Rename the selected chat inline.                                        |
| Ctrl+P       | Pin or unpin the selected root chat.                                    |
| Ctrl+U       | Mark the selected chat unread or read.                                  |
| Esc          | Close the overlay.                                                      |

The overlay owns the keyboard while open, so these bindings never reach the composer's text widget.

### Filters

- **All** shows unarchived chats, which is the server default: the search parser starts from `archived:false` and owned chats only (`coderd/searchquery/search.go:523-525`).
- **Active** shows chats whose own status, or any child's status, is `running`, `interrupting`, or `requires_action`.
- **Unread** shows chats with `has_unread`.
- **Archived** refetches with `q=archived:true`, because the default list never returns archived chats.

Active and Unread filter the loaded list locally, so they respond instantly and include watch updates.

### API

- The list is `GET /api/v2/chats` (`coderd/chat_routes.go:35`, handler `coderd/exp_chats.go:423`) with `limit`, `offset`, and `q`.
  The query language supports `archived:`, `has_unread:`, `status:`, `title:`, and `search:` (`coderd/exp_chats.go:416`).
- The response is root chats only (`coderd/database/queries/chats.sql:767`, inside `GetChats` at `:554`), each with its subagents embedded in `children`, filtered by the same archive filter (`coderd/exp_chats.go:519-535`, `codersdk/chats.go:188-193`).
- Fields used: `title`, `status`, `has_unread` (`codersdk/chats.go:174`), `archived` (`:164`), `pin_order` (`:167`), `parent_chat_id` and `root_chat_id` (`:149-150`), `updated_at`, and `diff_status` (`:161`).
- Archive, unarchive, rename, pin, and read state all use `PATCH /api/v2/chats/{chat}` with `archived`, `title`, `pin_order`, or `read` (`codersdk/chats.go:710-738`).
  Archive state can change only on a root chat (`coderd/exp_chats.go:2470-2475`), and a child chat cannot be pinned (`:2543-2545`).
  The web UI blocks archiving while the chat or any child is running and leaves the server's conflict response as the backstop (`site/src/pages/AgentsPage/components/ChatActionsMenuItems.tsx:26-43`); scuttle does the same.
- Updates come from the watch socket (section 4).
- `coder-sdk` already has `list_chats` (generated) and `watch_chats` (`unofficial-coder-sdk-rs/crates/coder-sdk/src/stream.rs:136`).

### Paging and search

scuttle loads 50 chats at a time and loads the next page when the selection reaches the last loaded row.
The server's default page size and maximum were not checked and are unconfirmed.
The "Search all chats" row sends `q=search:"<query>"` and shows the results in the same overlay until the filter text changes.

### Unread

- Opening a chat's stream marks it read on the server, on connect and on disconnect (`coderd/exp_chats.go:3298-3308`, `:3415-3416`).
- The watch payload's `has_unread` is not used for chats already in the list.
  The web UI instead marks a chat unread when a fresh `status_change` arrives for a chat that is not open (`site/src/api/queries/chats.ts:658-661`), and clears the flag locally for the open chat (`site/src/pages/AgentsPage/AgentsPageLayout.tsx:556-566`).
  scuttle follows the web UI, because the payload's accuracy for unread was not confirmed.
- Ctrl+U sends `read: false` or `read: true`.
  Marking the open chat unread does not persist, because its stream marks it read again (`codersdk/chats.go:725-731`).

### States

- **Loading:** the overlay opens at once with the last loaded list, or with "Loading chats…" before the first load.
- **Empty:** "No chats yet. Type a message to start one."
- **No matches:** "No chats match."
- **Error:** the server's message on one line, with "Press Tab to retry"; the rest of scuttle keeps working.
- **Watch disconnected:** the header shows "live updates paused" while the watch socket reconnects.

### Core, runtime, and TUI

- `scuttle-core` gains a `chat_list` module holding the root chats, their children, the paging cursor, and the merge rules for watch events (section 4).
  It also owns the filter state and the fuzzy ranking, so both are unit-tested without a terminal.
- The runtime runs the list fetch, the search fetch, the PATCH actions, and the long-lived watch task.
- `scuttle-tui` draws the overlay and maps keys to `Msg` values.
- The fuzzy matcher is a crate choice for the plan; `nucleo-matcher` is the likely candidate and its version is unconfirmed.

## 3. `/subagents`

### Behavior

`/subagents` opens a popup over the transcript listing the open chat's subagents with their status, title, and last activity.
The selected subagent's transcript streams live in the lower part of the popup, so the author can watch it work without leaving the parent.
Enter opens the selected subagent full screen as the open chat, where the author can read, send messages, interrupt, and use every chat command.
Returning to the parent is `/parent` (alias `/back`), or Esc while the subagent is idle and the composer is empty (decision in section 18).
When the open chat is itself a subagent, `/subagents` lists its siblings, and the popup title names the parent.

### Command, aliases, and keys

- `/subagents` opens the popup; no alias.
- In the popup: Up and Down select, Enter opens full screen, PageUp and PageDown scroll the preview, and Esc closes.
- `/parent` (alias `/back`) opens the parent of the open subagent.

### API

- The children come from the open chat's single `GET /api/v2/chats/{chat}`, which embeds `children` (`codersdk/chats.go:188-193`), and from the list cache, which the watch keeps current.
  Subagents are at most one level deep (`codersdk/chats.go:188-193`).
- The preview opens a second `GET /api/v2/chats/{chat}/stream` for the selected child (`coderd/chat_routes.go:99`).
  This marks the child read, which matches the author looking at it.
- New subagents arrive as watch `created` events with a `parent_chat_id` (`coderd/x/chatd/subagent.go:1180`), and the web UI adds them under their parent the same way (`site/src/pages/AgentsPage/AgentsPageLayout.tsx:628-649`).
- The server accepts `POST /chats/{chat}/messages` on a child chat and rejects only `inline_mcp_servers` there (`coderd/exp_chats.go:2798-2810`).
  Whether chatd treats a user message to a running subagent the way the parent expects is unconfirmed; the web UI opens a subagent as an ordinary chat page.
- Cost for a subagent is the whole tree's cost (`coderd/exp_chats.go:1790-1823`), which `/info` states.

### States

- **Loading:** the list shows at once from the cached `children`; the preview shows "Connecting…" until its snapshot arrives.
- **Empty:** "This chat has no subagents."
- **Error:** a preview stream failure shows its error in the preview area and retries with the normal backoff while the popup stays open.

### Core, runtime, and TUI

- `scuttle-core` holds a second, preview-only `Transcript` and reuses the reducer unchanged.
  Preview stream events arrive tagged `ForPreview { chat, generation }` and are dropped unless both match the current preview (section 4).
- The runtime gains a second stream slot for the preview, with its own generation, closed when the popup closes or the selection changes.
- `scuttle-tui` draws the popup with the existing transcript renderer at the popup's width.
- Opening a subagent full screen is the same chat switch as `/chats` (section 4), and it closes the preview stream first.

## 4. Chat switching

`/chats`, `/subagents`, `/parent`, and `/new` all change the open chat.
M1.5 built `/new` as a one-off; M2 turns it into one operation with two entry points: open an existing chat, or open a blank one.

### Open a chat

1. Refuse while a chat creation is in flight, with the M1.5 notice "Wait for this chat to finish starting".
   A creation takes about a second, and its reply would otherwise open the chat the author just left.
1. Close the main stream and bump the stream generation (below).
1. Reset per-chat state exactly as `/new` does today: the transcript, the wait, the reconnect counter, the stream error, the plan mode, and the chosen workspace.
1. Also reset the chosen model and effort, so the next message uses the opened chat's `last_model_config_id` and `last_reasoning_effort`.
   Today `ChatLoaded` keeps an earlier `/model` choice (`selected_model.or(chat.last_model_config_id)`), which is right at launch but wrong when switching.
1. Keep the composer text, as `/new` does.
1. Emit `ClearView`, then `LoadChat(id)`, and record `loading = Some(id)`.
1. Mark the chat read in the local list.

A second switch while a load is in flight is allowed and supersedes the first.
`ChatLoaded` and `ChatLoadFailed` apply only when their chat ID equals `loading`, so a slow reply for an abandoned chat is dropped.
Both already carry the chat ID, so no new tag is needed.

Leaving a running chat never interrupts it; the server keeps it running, and the list shows it as active through the watch socket.

### The stream generation

M1 tags stream events with `ForChat { chat }` and silences replaced tasks with an atomic generation inside the runtime (`crates/scuttle-tui/src/runtime.rs`, `StreamSender`).
That is enough for chat A to chat B, but not for A to B and back to A: an event from the first stream on A that is already in the channel carries the right chat ID and would apply to the second visit.

M2 moves the generation into `scuttle-core`:

- The core keeps a `stream_generation` counter and increments it on every `OpenStream`, `ReconnectAfter`, and `CloseStream`.
- `Effect::OpenStream` and `Effect::ReconnectAfter` carry the generation.
- The runtime tags every stream message `ForStream { chat, generation, msg }` instead of `ForChat`.
- The core applies a stream message only when both the chat and the generation are current.

REST replies keep `ForChat`, because a late reply about the same chat is still true on a later visit.
The runtime keeps its own atomic as well, so an aborted task stops sending promptly; the core's check is the one that guarantees correctness and is unit-tested.
The preview stream uses the same scheme with its own counter and a `ForPreview` tag.

### The watch socket

- The runtime starts one watch task at launch with `watch_chats` and keeps it for the life of the process.
- It reconnects with the same backoff as the chat stream, and `coder-sdk` already ends a silent socket after 45 seconds (`crates/coder-sdk/src/stream.rs`, `STREAM_IDLE_TIMEOUT`).
- The socket sends no snapshot and has no resume cursor (`coder-sdk` `watch_chats` doc comment), so every connect and reconnect also refetches the first page of `GET /chats`, as the web UI does on open (`site/src/pages/AgentsPage/AgentsPageLayout.tsx:690-694`).
- Events arrive as `Msg::Watch(WatchEvent)`, untagged, because they are about many chats.

The chat list merges each event by kind, following the web UI's `mergeWatchedChatSummary` (`site/src/api/queries/chats.ts:595-661`):

| Kind                  | Effect on the list                                                                                                        |
|-----------------------|---------------------------------------------------------------------------------------------------------------------------|
| `status_change`       | Take `status` when the payload's `updated_at` is not older than the cached one; mark unread if the chat is not open.      |
| `title_change`        | Take `title` regardless of `updated_at`, because title generation can publish an older snapshot.                          |
| `summary_change`      | Take `last_turn_summary` only.                                                                                            |
| `chat_summary_change` | Take `summary` only.                                                                                                      |
| `diff_status_change`  | Take `diff_status` regardless of `updated_at`; if it is the open chat, refresh `/git` data.                               |
| `context_dirty`       | Merge the `context` flags; if it is the open chat, refetch it so skills and `/info` see new resources.                    |
| `created`             | Add a root chat at the top, or add a child under its parent; also fires on unarchive (`coderd/x/chatd/chatd.go:2093`).     |
| `deleted`             | Mark the chat archived; it fires once per family member on archive (`coderd/x/chatd/chatd.go:2079`, `coderd/x/chatd/auto_archive.go:151`). |
| `action_required`     | Mark the chat as needing action.                                                                                          |

For the open chat, the watch updates the chat record (title, diff status, and the archived flag), but never the transcript or the status, which the chat stream owns.
When the open chat is archived, the composer shows "This chat is archived. Ctrl+A in /chats unarchives it." and does not send, matching the web UI's read-only archived view (`site/src/pages/AgentsPage/AgentChatPageView.tsx:889`).

### `/new`

`/new` becomes "open a blank chat" through the same reset path.
Its behavior does not change: it closes the stream, keeps the composer text and the chosen model, and leaves a running agent running.

### Plan mode after a switch

`PATCH /chats/{chat}` with `plan_mode` writes the database and returns `204` without publishing a watch event (`coderd/exp_chats.go:2657-2683`).
So the watch socket cannot deliver plan-mode changes, which the M1.5 ledger assumed it would.
Section 17 describes the replacement.

## 5. `/title`

### Behavior

- `/title <text>` renames the open chat.
- `/title` alone asks the server for a proposed title, puts it in an inline editor over the composer, and saves on Enter or cancels on Esc.

### API

- Rename: `PATCH /api/v2/chats/{chat}` with `title` (`codersdk/chats.go:711`), which publishes `title_change` (`coderd/exp_chats.go:2282-2295`).
- Propose: `POST /api/v2/chats/{chat}/title/propose` (`coderd/chat_routes.go:87`, `coderd/exp_chats.go:3722-3732`), which returns `ProposeChatTitleResponse { title }` and does not save it.
  The handler maps a manual title timeout to `504` (`coderd/exp_chats.go:187`).

### States

- **Loading:** "Proposing a title…" in the editor until the reply.
- **Error:** the server's message as a notice; the old title stays.
- **No chat:** "Start a chat first."

### Core, runtime, and TUI

The core owns the editor text and the pending request; the runtime runs both calls; the TUI draws a one-line editor.

## 6. `/queue`

### Behavior

Queued messages already render dimmed below the live turn (`crates/scuttle-tui/src/transcript_view.rs`).
`/queue` opens an overlay listing them in order, where Enter promotes the selected message to run next and Delete removes it.

### API

- The list is the stream's `queue_update` event, which the reducer already keeps in `transcript.queued`.
- Delete: `DELETE /api/v2/chats/{chat}/queue/{queuedMessage}` (`coderd/chat_routes.go:94-95`, `coderd/exp_chats.go:3153`).
- Promote: `POST /api/v2/chats/{chat}/queue/{queuedMessage}/promote` (`coderd/chat_routes.go:96`, `coderd/exp_chats.go:3214`), which returns `202` (`coderd/exp_chats.go:3293`).
- The next `queue_update` is authoritative, so the overlay never edits the list itself.

### States

- **Empty:** "Nothing is queued."
- **Error:** the server's message; `409` for promote reads "The chat has no queued messages to promote." from the server.

### Core, runtime, and TUI

The core maps the selection to a queued message ID and emits the effect; the runtime calls the endpoint; the TUI draws the list.

## 7. Plan-mode questions and the plan

### Behavior

- When the agent calls `ask_user_question`, the turn ends and scuttle shows its questions as a menu above the composer.
- Each question lists its options with their descriptions, plus "Other", which opens a one-line text field.
- Several questions are answered one after another, and Enter on the last one sends the answers.
- When the agent calls `propose_plan`, the rendered plan ends with an "Implement the plan" action (Ctrl+Enter, or `/implement`).

### Answer and plan formats

- One question sends the chosen label, or `Other: <text>`.
- Several questions send one `N. <header>: <answer>` line each, using `Question N` when a header is empty (`site/src/pages/AgentsPage/components/ChatElements/tools/AskUserQuestionTool.tsx:48`, `:108-120`).
- "Implement the plan" sends the literal text "Implement the plan." with `plan_mode: ""` in the same request (`site/src/pages/AgentsPage/AgentChatPage.tsx:682-688`).
  The spec said it sends the plan's own text; the web UI does not, so scuttle follows the web UI.

### API

- The tool is `ask_user_question` with `questions[].header`, `question`, and `options[].label` and `description` (`coderd/x/chatd/chattool/askuserquestion.go:14-35`).
- The menu appears when the last durable assistant message ends with an `ask_user_question` call and no user message follows it.
- Answers and the plan action go through the existing `SendMessage` effect, which already accepts a plan-mode change.

### States

- **Answered elsewhere:** if a user message arrives from another client, the menu closes.
- **Send failure:** the answers go back into the composer as text, like any failed send.

### Core, runtime, and TUI

The core detects a pending question from the transcript and owns the answer state; the TUI draws the menu; no new runtime effect is needed.

## 8. Attachments

### Behavior

- `/attach <path>` uploads a local file and shows it as a chip above the composer; the next message carries it.
- `@path` at a word boundary does the same, with path completion on Tab.
- Backspace on an empty composer removes the last chip.
- An image is described by name and size, since the terminal cannot show it.

### API

- Upload: `POST /api/v2/chats/files?organization=<org>` with the raw bytes and a `Content-Disposition` header carrying the file name (`coderd/exp_chats.go:6423-6436`), returning `{ id }` (`codersdk/chats.go:818-820`).
- Accepted types are PNG, JPEG, GIF, WebP, SVG, plain text, Markdown, CSV, JSON, and PDF, with a `413` above 10 MiB (`coderd/exp_chats.go:6427`, `:6434`).
- The message part is `{ "type": "file", "file_id": <id> }` (`codersdk/chats.go:617-620`).
- The generated `upload_chat_file` takes the organization, the disposition, and the body (`unofficial-coder-sdk-rs/crates/coder-api-gen/src/generated.rs:17522`).
- The spec's limit of 50 attachments per chat could not be found in the handler and is unconfirmed.

### States

- **Uploading:** the chip shows a spinner, and sending waits for every upload.
- **Rejected:** a type or size check fails locally, before upload, with the reason.
- **Error:** the server's message on the chip; Backspace removes it.

### Core, runtime, and TUI

The core owns the chip list and the checks; the runtime reads the file and uploads it; the TUI draws chips and completes paths.
File reads happen in the runtime, so the core never touches the filesystem.

## 9. `/model`

### Behavior

`/model` opens a table grouped by provider, with a fuzzy filter across the model name, display name, and provider name.

```text
 Model                                                     context
 > son
 Anthropic
   Claude Sonnet                        current            200k
   Claude Sonnet (extended)             default            1M
 OpenAI                                 needs your API key
   ...
```

- Each row shows the display name, a `current` or `default` tag, and the context window.
- A provider that cannot be used shows its reason in the group header, and its models are listed but not selectable.
- No icons.
- `/model <name>` still picks by name without opening the table.

### API

- `GET /api/v2/organizations/{organization}/chats/models` (`coderd/chat_routes.go:171-172`, `coderd/exp_chats.go:7706-7714`) returns `OrganizationChatModelsResponse` with `models` and `providers` (`codersdk/chats.go:2355-2359`), which M1 already fetches and partly discards.
- Grouping joins `models[].ai_provider_id` (`codersdk/chats.go:1431`) to `providers[].id`, and the header uses `providers[].display_name` (`codersdk/chats.go:2338-2351`).
- The context window is `context_limit` (`codersdk/chats.go:1436`), shown as `200k` or `1M`.
- An unusable provider has `available: false` and an `unavailable_reason` of `missing_api_key`, `fetch_failed`, or `user_api_key_required` (`codersdk/chats.go:873-880`).
- `unsupported_providers` lists configured providers the harness cannot use (`codersdk/chats.go:884-889`), shown as a dim footer line.

### States

The M1.5 models states carry over: "Loading models…", the empty-organization guidance from feedback item 15, and the retry on failure.

### Core, runtime, and TUI

- The core keeps the providers alongside the models, and computes the grouped, ranked rows.
- The TUI replaces the flat model picker with a Ratatui `Table`.

## 10. Skills in the slash menu

### Behavior

- The `/` menu lists three groups: scuttle's commands, the user's personal skills, and the open chat's workspace skills.
- Every entry shows its description.
- Selecting a skill inserts its trigger into the composer as message text for the agent, as the web UI does.
- A built-in command keeps its plain name.
- A personal skill whose name matches a built-in command appears as `/<username>:<name>`, as the author decided.

### The trigger text the agent sees

The server tells the model that a skill listed as `personal/<name>` or `workspace/<name>` must be passed to `read_skill` by that qualified alias (`coderd/x/chatd/chattool/skill.go:113-118`).
It resolves a bare name or `<source>/<name>`, and reports a bare name that matches both sources as ambiguous (`coderd/x/skills/skills.go:190-225`).
It does not understand `<username>:<name>`.

So scuttle separates the menu label from the inserted text:

- The menu shows `/<username>:<name>` for a personal skill that collides with a built-in, and `/<name>` otherwise.
- Typing `/<username>:<name>` is accepted and rewritten on send.
- The inserted and sent text is `/personal/<name>` when the name collides with a built-in or with a workspace skill, and `/<name>` otherwise.
- A workspace skill that collides with anything uses `/workspace/<name>`, as the web UI does (`site/src/pages/AgentsPage/components/ChatMessageInput/SkillsTriggerMenu.tsx:45-57`).

This needs the author's confirmation (section 18).

### Precedence

- The web UI hides a built-in command when any skill shares its name (`site/src/pages/AgentsPage/components/ChatMessageInput/ChatMessageInput.tsx:689-705`), and its submit path defers to the skill (`site/src/pages/AgentsPage/components/ChatConversation/submitChatTurn.ts:117-134`).
- scuttle reverses that: built-ins win, and the skill stays reachable by its qualified form.
- This retires the spec's `/skill <name>` fallback.

### API

- Personal skills: `GET /api/experimental/users/{user}/skills` (`coderd/coderd.go:1343-1349`), returning `UserSkillMetadata { name, description }` (`codersdk/userskills.go:14-20`).
- Workspace skills: the open chat's `context.resources` entries with `kind: "skill"` and `status: "ok"`, which only the single-chat GET populates (`codersdk/chats.go:199-251`), deduplicated first-wins as the web UI does (`site/src/pages/AgentsPage/components/ChatPageContent.tsx:88-109`).
- The username comes from `GET /api/v2/users/me`, which M2 also uses for the welcome screen (section 17).

### States

- **Loading:** built-in commands show at once; skill groups show "Loading skills…".
- **Error:** the skill groups show one dim line; the experimental API never blocks commands (spec section 8).

### Core, runtime, and TUI

- `commands.rs` gains a merged menu model that takes the built-ins, the personal skills, the workspace skills, and the username.
  It computes the labels, the collisions, and the sent text, all testable in the core.
- The runtime fetches skills once at startup and again after `/new` or a switch into a chat with a workspace.

## 11. `/info`

### Behavior

`/info` (alias `/chat-info`) opens a read-only panel for the open chat.

| Field                   | Source                                                                                                    |
|-------------------------|-----------------------------------------------------------------------------------------------------------|
| Title and ID            | `title`, `id`.                                                                                            |
| Summary                 | `summary`, else `last_turn_summary` (`codersdk/chats.go:157-160`).                                         |
| Parent                  | For a subagent, the parent's title, from `parent_chat_id`.                                                |
| Organization and owner  | `organization_id` resolved to a name, `owner_username`.                                                   |
| Model and effort        | `last_model_config_id` through the models list, `last_reasoning_effort`.                                  |
| Plan mode               | `plan_mode`.                                                                                              |
| Workspace               | `workspace_id` resolved to a name, or "none".                                                             |
| Created and updated     | `created_at`, `updated_at`, as local time and relative time.                                              |
| Context                 | Tokens against `context_limit`, computed as the footer already does (`crates/scuttle-core/src/usage.rs`). |
| Cost                    | `total_cost_micros`, `request_count`, and `unpriced_request_count` (`codersdk/chats.go:2047-2052`).        |
| Changes                 | PR number or "branch", with additions and deletions, from `diff_status`.                                  |
| Warnings                | `warnings` (`codersdk/chats.go:182`), when present.                                                       |

- Cost comes from `GET /api/v2/chats/{chat}/cost` (`coderd/chat_routes.go:77`).
  It covers the whole chat tree, so a subagent reports its root's total (`coderd/exp_chats.go:1790-1823`), and the panel says so, as the web UI does (`site/src/pages/AgentsPage/exp/chatBoard/ChatInfo.tsx:180-185`).
- Cost comes from AI Gateway data with its own retention, 60 days by default (`coderd/exp_chats.go:1798-1802`).
- The web UI shows cost only when the `aibridge` feature is visible (`site/src/pages/AgentsPage/exp/chatBoard/ChatInfo.tsx:115`); scuttle hides the row on `404` or `403`, per spec section 8.
- Unpriced requests add "Excludes unpriced usage from N requests." (`site/src/pages/AgentsPage/exp/chatBoard/ChatInfo.tsx:186-191`).

### States

- **Loading:** fields from the cached chat show at once; cost shows `…` until it loads.
- **Error:** a failed field shows "unavailable" and the rest of the panel stays.
- **No chat:** "Start a chat first."

### Core, runtime, and TUI

The runtime fetches the chat and the cost when the panel opens; the core formats micros and times; the TUI draws a two-column list.
Cost refetches on each `status_change` for the chat's tree while the panel is open.

## 12. `/workspace`

### Without a workspace

- `/workspace` shows the M1 picker grown into a table: name, template display name, status, and last used.
- It lists workspaces in the chat's organization, sorted by last used, with a fuzzy filter.
- Enter attaches the selection, and `none` detaches, as in M1.
- The query stays `owner:me organization:<org>` (`crates/scuttle-tui/src/runtime.rs`, `FetchWorkspaces`), because the chat runs with its owner's credentials.
  The author's words were "workspaces in the organization"; listing other users' workspaces is a decision in section 18.

### With a workspace

`/workspace` shows the attached workspace's details with actions:

| Line or action     | Source                                                                                                                                                   |
|--------------------|----------------------------------------------------------------------------------------------------------------------------------------------------------|
| Name and owner     | `name`, `owner_name` (`codersdk/workspaces.go:38`, `:53`).                                                                                               |
| Template           | `template_display_name`, else `template_name` (`codersdk/workspaces.go:43-44`), and `outdated`.                                                          |
| Status and health  | `latest_build` status and `health` (`codersdk/workspaces.go:50`, `:68`).                                                                                  |
| Agent              | The chat's `agent_id` (`codersdk/chats.go:148`) resolved in `latest_build.resources[].agents[]`.                                                         |
| Copy SSH command   | `ssh <agent>.<workspace>.<owner>.<hostname_suffix>`, the web UI's format (`site/src/pages/AgentsPage/AgentChatPage.tsx:585-588`), with the suffix from `GET /api/v2/deployment/ssh` (`coderd/coderd.go:1410`, `codersdk/deployment.go:5605-5613`). Falls back to `coder ssh <owner>/<workspace>` when there is no suffix. |
| Open in web        | `<deployment>/@<owner>/<workspace>` (`site/src/router.tsx:700`, `site/src/modules/workspaces/WorkspaceMoreActions/WorkspaceMoreActions.tsx:159`), using the `/web` opener and its SSH copy fallback. |
| Detach             | `PATCH /chats/{chat}` with an empty `workspace_id`, as `/workspace none` does today.                                                                     |
| Switch             | Opens the table from the section above.                                                                                                                  |

- `/workspace <name>` and `/workspace none` keep working unchanged, and the alias `/ws` is new.
- Workspace details come from `GET /api/v2/workspaces/{workspace}` (generated `get_workspace_metadata_by_id`, `generated.rs:32407`).
- The SSH config comes from the generated `ssh_config` (`generated.rs:19469`).

### States

- **Loading:** "Loading workspaces…".
- **Empty:** "You have no workspaces in <org>. The agent can create one."
- **Error:** "Workspaces failed to load: <message>. /workspace retries." This fixes M1 review Minor 8, where a failed load reads as "No workspace named …".

### Core, runtime, and TUI

- The core gains a `WorkspacesState` with loading, loaded, and failed, like `ModelsState`.
- `WorkspaceRef` grows the template, status, and last-used fields.
- The runtime fetches details and the SSH config.
- The clipboard copy stays in the TUI, as `/web` does.

## 13. `/git`

### Behavior

`/git` opens a panel for the open chat's changes.

| Line or action | Source                                                                                                         |
|----------------|----------------------------------------------------------------------------------------------------------------|
| Repository     | `remote_origin` from the diff contents.                                                                        |
| Provider       | `provider` from the diff contents, such as `github`.                                                           |
| Branch         | `branch`, and `head_branch` and `base_branch` from the diff status.                                            |
| Pull request   | `#<pr_number>`, title, state, draft, approved, and changes requested, from the diff status.                    |
| Size           | Additions, deletions, changed files, and commits.                                                              |
| Open PR        | Opens `pull_request_url` or the status `url` with the `/web` opener, with its copy fallback.                   |
| View diff      | Hands the diff to the user's pager (below).                                                                    |
| Local changes  | Uncommitted changes in the workspace, per repository (below).                                                  |

- `/diff` is an alias that goes straight to "View diff".
- The panel updates on the watch's `diff_status_change` for the open chat.

### API

- Status: the chat's `diff_status` (`codersdk/chats.go:1724-1744`), which list, get, and watch payloads all carry.
- Contents: `GET /api/v2/chats/{chat}/diff` (`coderd/chat_routes.go:88`, `coderd/exp_chats.go:3781-3791`), returning `provider`, `remote_origin`, `branch`, `pull_request_url`, and `diff` (`codersdk/chats.go:1747-1754`).
  Without a git access token for the provider, it returns the metadata with an empty diff (`coderd/exp_chats.go:4093`), and scuttle says "Link your <provider> account in Coder to see the diff."
- Local changes: the `GET /api/v2/chats/{chat}/stream/git` WebSocket (`coderd/chat_routes.go:101`, `coderd/exp_chats.go:1993-2003`) sends `WorkspaceAgentGitServerMessage` values with per-repository `branch`, `remote_origin`, and `unified_diff` (`codersdk/workspaceagents.go:749-766`).
  It returns `400` with fixed messages when the chat has no workspace (`codersdk/chats.go:1756-1772`).
  M2 opens it only while the `/git` panel is open, and `coder-sdk` needs a small typed wrapper for it like `watch_chats`.

### The pager

- The pager is `$GIT_PAGER`, else `git config core.pager` run in the current directory, else `$PAGER`, else `less -R`.
  That is git's own order, so delta applies when git uses it.
- scuttle hands the terminal over as it does for `$EDITOR`: it leaves the alternate screen, disables mouse capture, pipes the diff to the pager's stdin, waits, and restores.
- delta reads a unified diff on stdin, so no temporary file is needed.

### States

- **No changes:** "No git changes for this chat yet."
- **No workspace:** the status and PR still show, and local changes read "Attach a workspace to see local changes."
- **Error:** the server's message per section; the other sections stay.

### Core, runtime, and TUI

- The core holds the diff status and contents.
- The runtime fetches the contents and runs the git-watch socket.
- The TUI owns the pager handoff next to the `$EDITOR` handoff, because both take over the terminal.

## 14. `/mcp`

### Behavior

`/mcp` lists the MCP servers the chat uses, in three groups, with what scuttle can tell about each.

| Group                     | Source                                                                                                            | Health scuttle can show                                                                                                           |
|---------------------------|-------------------------------------------------------------------------------------------------------------------|-----------------------------------------------------------------------------------------------------------------------------------|
| Organization servers      | `mcp_server_ids` (`codersdk/chats.go:168`) joined to `GET /api/v2/organizations/{organization}/mcp-servers` (`coderd/chat_routes.go:147-148`, `coderd/mcp.go:144-155`). | `enabled`, `availability`, and `auth_connected` (`codersdk/mcp.go:86-103`), which shows an OAuth2 server that needs reconnecting. |
| Inline servers            | `inline_mcp_servers` on the single-chat GET (`codersdk/chats.go:184-187`).                                         | None beyond its slug and URL.                                                                                                     |
| Workspace servers         | `context.resources` with `kind: "mcp_server"` (`codersdk/chats.go:217-251`).                                       | `status` and `error` from the last context snapshot, and the tool list.                                                            |

- Each row shows the display name or slug, the URL when the API returns it, and the tool allow and deny lists when set.
- An organization server that is `default_off` and not selected shows as "off"; `force_on` servers cannot be turned off (`site/src/pages/AgentsPage/utils/mcpSelection.ts:7-21`).
- As the last, cuttable M2 task, Space toggles an organization server for the next message.
  There is no PATCH for the selection, so the toggle rides on `mcp_server_ids` in the next `POST /chats/{chat}/messages` (`codersdk/chats.go:766`), as the web UI does.

### Can scuttle detect a silent disconnect?

Mostly no, and this is a server-side gap.

- chatd reconnects to every selected MCP server on each generation step (`coderd/x/chatd/active_turn_debug.go:181-187`).
- A server that fails to connect is skipped and only logged (`coderd/x/chatd/mcpclient/mcpclient.go:217-223`, `:376-383`), so the agent simply has fewer tools.
- The per-step connect outcomes (connected, error, timeout, no tools) are recorded only in the chat's debug run summary under `mcp_connect` (`coderd/x/chatd/active_turn_debug.go:143-170`).
  That is exposed through `GET /api/experimental/chats/{chat}/debug/runs` (`coderd/chat_routes.go:127-129`, summary map at `codersdk/chats.go:1227`) only when debug logging is on for the user.
- Nothing in the chat, stream, or watch APIs reports a server's connection state.

What M2 does:

- Show `auth_connected: false` as "needs reconnecting in the web UI", which covers expired OAuth2 tokens.
- Show a workspace server's non-ok `status` and `error`.
- When debug logging is on (`GET /api/v2/chats/config/user-debug-logging`, `coderd/chat_routes.go:53`), read the latest debug run and show each server's last connect outcome.
  Otherwise show "Connection health is not reported by the server."

The server change and a Linear ticket title are in section 19.

### States

- **Empty:** "This chat uses no MCP servers."
- **Error:** the server's message per group; other groups stay.

### Core, runtime, and TUI

The core joins the three sources into rows; the runtime fetches the organization's servers and, when allowed, the debug run; the TUI draws the grouped list.

## 15. The `coder` theme source (moved to M4)

The spec's M2 list includes the `coder` theme source.
This draft recommends moving it to M4 (section 1), where `/theme` and the palettes live.
The research is recorded here so M4 does not repeat it.

- `GET /api/v2/users/{user}/appearance` (`coderd/coderd.go:1691`) returns `theme_preference`, `theme_mode` (`single` or `sync`), `theme_light`, and `theme_dark` (`codersdk/users.go:244-256`).
- The six theme names are `light`, `light-protan-deuter`, `light-tritan`, `dark`, `dark-protan-deuter`, and `dark-tritan` (`codersdk/users.go:272`, `:277`).

## 16. Shared pieces

- **Signed-in user:** the runtime fetches `GET /api/v2/users/me` at startup for the welcome screen, the skill labels, and the owner checks.
- **Table overlay:** `/chats`, `/model`, `/workspace`, `/queue`, `/mcp`, and `/subagents` share one overlay widget with a filter line, grouped rows, and a status line for loading, empty, and error text.
  The M1 `PickerState` becomes this widget.
- **`/help`:** lists every new command with its aliases, as feedback item 8 requires.

## 17. Deferred items

### Items deferred to M2

| Item                                                                                  | Origin                                         | Decision | Reason                                                                                                                                                  |
|---------------------------------------------------------------------------------------|------------------------------------------------|----------|---------------------------------------------------------------------------------------------------------------------------------------------------------|
| Channel-level stream staleness when switching chats                                   | M1 Task 14, M1 final review                    | Keep     | The core-owned stream generation in section 4 is the fix, and chat switching needs it.                                                                  |
| The view drifts while scrolled up                                                     | M1 Task 14                                     | Keep     | Subagent previews and chat switching make scrolled-up reading common; the fix anchors the scroll offset to a message ID.                                |
| No retry of the organization lookup                                                   | M1 Task 14                                     | Keep     | The M1.5 fix wave (F7) covers `scuttle <chat-id>`; M2 retries the lookup on `/new` and `/organization` when it failed at startup.                      |
| `main.rs` calls `server_version()` outside `runtime.rs`                               | M1 Task 14                                     | Later    | No user impact; move it when the startup sequence next changes.                                                                                         |
| The wheel recalls history when the mouse is off                                       | M1 final review Minor 3                        | Keep     | Small: turn off alternate scroll mode (`CSI ? 1007 l`) while capture is off, and restore it on exit.                                                    |
| The welcome screen never shows the signed-in user                                     | M1 final review Minor 5                        | Keep     | `GET /users/me` is needed anyway for skill labels (section 16).                                                                                         |
| A failed workspace load reads as "No workspace named …"                               | M1 final review Minor 8                        | Keep     | Part of the `/workspace` table (section 12).                                                                                                            |
| Loading history older than 200 messages                                               | M1 final review, declined items                | Keep     | Chat switching opens long chats; load older messages with `before_id` (`coderd/exp_chats.go:1688`) on scroll to the top. Cuttable.                    |
| A durable tool result whose call is outside the loaded window renders nothing         | M1 Task 10                                     | Keep     | Fixed by older-history loading; otherwise render the orphan result with its tool name.                                                                  |
| `"\r\n"` in a notice becomes two spaces; the notice queue can replay stale notices     | M1 final fix wave                              | M3       | Footer behavior, which M3 redesigns with `/statusline`.                                                                                                  |
| The `update_queued` drain loop has no cap                                              | M1 final fix wave                              | Keep     | The watch socket adds steady traffic; cap the drain so a burst cannot starve drawing.                                                                    |
| The WebSocket upgrade has only a connect timeout                                       | M1 final fix wave                              | Keep     | M2 runs up to four sockets (stream, preview, watch, git); add an upgrade timeout in `coder-sdk`.                                                         |
| The watchdog reason says "0 seconds" in tests                                         | M1 final fix wave                              | Keep     | Fold into the same `coder-sdk` change.                                                                                                                   |
| The reconnect counter resets on the first healthy event after a reopen                | M1 Task 8                                      | Keep     | The watch task reuses the reconnect logic, so fix it once for both.                                                                                      |
| Overlapping plan-mode requests can leave the footer out of sync                        | M1.5 Task 8                                    | Keep, changed | The planned fix assumed the watch socket reports plan mode; it does not (section 4). Serialize plan-mode requests, send only the latest wanted state, and `GET /chats/{chat}` after each PATCH to adopt the server's value. |
| The footer drops the connection status before "plan mode" at narrow widths            | M1.5 Task 8                                    | M3       | Footer layout belongs to `/statusline`.                                                                                                                  |
| No runtime test for `CloseStream` during a delayed `ReconnectAfter`                   | M1.5 Task 11                                   | Keep     | Chat switching exercises exactly this path; add the test with the stream generation change.                                                            |
| The activity label reads "Working…" while a tool runs                                 | M1.5 final review Minor 7                      | Keep     | Confirmed below.                                                                                                                                        |

The tool label check against chatd confirms the bug.
chatd decides to run tools from unresolved tool calls in the durable history (`coderd/x/chatd/generation.go:206-220`), so the assistant message with its tool calls is persisted before the tools run.
That durable message clears the live turn, and each tool result is published only after its tool finishes (`coderd/x/chatd/chatloop/chatloop.go:539-568`).
The fix is the one the review proposed: while running with an empty live turn, name the tool calls in the last durable assistant message that have no result yet.

### Other deferred minors

These were recorded as deferred without a milestone.
None blocks M2, so they stay in the backlog unless a task touches the same code.

| Item                                                                                          | Origin         | Decision |
|-----------------------------------------------------------------------------------------------|----------------|----------|
| An `http://` URL matches a key stored for `https://` on the same host; `:0443` misses the lookup | M1 Task 1   | Later; CLI parity, SDK-side. |
| `args_raw` reparsed on every delta; duplicated find-or-insert                                  | M1 Task 4      | Later; performance only. |
| Boundary search order; density collapses "preview" and "auto"                                  | M1 Task 6      | Later. |
| Links and images show only text; no highlight cache; unpadded tables                           | M1 Task 9      | Later; revisit if switching between long chats is slow. |
| A blank line before consecutive tools; ANSI stripping edge cases; id-less pairing              | M1 Task 10     | Later. |
| Footer `fit()` newline and grapheme handling                                                   | M1 Task 12     | M3, with the footer. |
| OSC 52 raw-byte limit, nested tmux, test naming                                                | M1 Task 13     | Later. |
| Fixed PTY timeouts; a panic in `spawn()` leaks `HOME`                                          | M1 Task 15     | Later; test harness only. |
| A non-string `organization` value fails the config load                                        | M1.5 Task 1    | Later. |
| Boundary tests for the padded layout and the gutter                                            | M1.5 Tasks 2, 3 | Later. |
| The echo never reaching the stream leaves only a status to end the wait                        | M1.5 Task 4    | Re-check after M2; the watch's `status_change` may end the wait on its own. |
| Double blank row around the rule; no `NO_COLOR`                                                | M1.5 Task 5    | `NO_COLOR` to M4 with themes; the rest later. |
| Missing `/effort` and `/web` tests; side-effecting closure in the footer                       | M1.5 Tasks 7, 10, 12 | Later. |

## 18. Decisions

Each decision has a recommendation and its tradeoff.
The four the brief names come first.

### Ctrl+R for `/chats`

- **Recommendation:** keep Ctrl+R, as the spec's keybinding table has it.
- **Why:** it reads as "recent" and matches shell history search, which is close to what `/chats` does.
- **Tradeoff:** `ratatui-textarea` 0.9.2 binds Ctrl+R to redo (`src/textarea.rs:593-598`), so the composer loses redo; undo (Ctrl+U) stays.
  Ctrl+T is unbound in the widget and is the alternative if redo matters.
  Either stays remappable when `/keybindings` lands in M4.
- Ctrl+K, the web UI's search shortcut (`site/src/pages/AgentsPage/hooks/useAgentsPageKeybindings.ts:34`), is the widget's delete-to-end-of-line (`src/textarea.rs:344-348`), so it is not a candidate.

### Command and skill precedence

- **Decided by the author:** built-in commands keep their names, and a colliding personal skill appears as `/<username>:<name>`.
- **Needs confirmation:** the server cannot resolve `<username>:<name>` (section 10), so scuttle shows that label but sends `/personal/<name>`.
- **Tradeoff:** the transcript then shows `/personal/<name>`, not the label the author typed.
  The alternative, sending `/<username>:<name>` literally, relies on the model guessing the mapping, which `read_skill` would reject.

### Diff viewing

- **Recommendation:** `/git` and `/diff` hand the diff to the user's pager, resolved in git's order, so delta works with no scuttle configuration.
- **Tradeoff:** the diff leaves the scuttle screen while it is open, and there is no inline commenting.
  An in-app diff view with syntax highlighting would keep context, but it duplicates delta, and the spec lists the old TUI's diff drawer as a non-goal.
  This resolves spec open question 6.

### `client_type`

- **Finding:** `client_type` is recorded and reported, but nothing branches on it.
  The server defaults it to `api` (`coderd/exp_chats.go:1452-1459`), subagents inherit the parent's value (`coderd/x/chatd/subagent.go:1164`), telemetry reports it (`coderd/telemetry/telemetry.go:2102`), and the web UI sends `ui` (`site/src/pages/AgentsPage/AgentCreatePage.tsx:242`).
  No query filters on it.
- **Recommendation:** keep sending nothing, which records `api`.
- **Tradeoff:** Coder's telemetry counts scuttle chats as API chats, which is accurate, since scuttle is not the web UI.
  Sending `ui` would inflate web UI usage numbers and would gain nothing today.
  This resolves spec open question 2.

### Scope: move the `coder` theme source to M4

- **Recommendation:** move it (section 1).
- **Tradeoff:** the author keeps the `terminal` theme through M2 and M3.

### The subagent preview

- **Recommendation:** a second, live chat stream for the selected subagent.
- **Tradeoff:** the runtime holds two streams, and the preview marks the child read.
  The cheaper alternative is a REST snapshot of recent messages refreshed on watch events, but `status_change` fires at turn boundaries, not per step (`coderd/x/chatd/generation.go:1391`), so that preview would look frozen while the subagent works.

### Returning from a subagent

- **Recommendation:** `/parent` (alias `/back`), plus Esc when the subagent is idle and the composer is empty.
- **Tradeoff:** Esc keeps its interrupt meaning while the subagent runs, so the same key does two things depending on state.
  A dedicated key would be clearer but costs another binding.

### Which workspaces `/workspace` lists

- **Recommendation:** keep `owner:me` within the chat's organization.
- **Tradeoff:** the author's words were "workspaces in the organization", which could mean every workspace they can see.
  Attaching someone else's workspace needs SSH permission on it (`coderd/exp_chats.go:4446`, in `validateChatWorkspaceSelection`), so listing them would mostly show rows that fail to attach.

### `/mcp` toggling in M2

- **Recommendation:** include toggling for the next message as the last `/mcp` task, and cut it if M2 runs long.
- **Tradeoff:** without it, changing MCP servers still needs the web UI.

## 19. Risks and unknowns

### Needs a server change

- **MCP connection state.** No chat, stream, or watch API reports whether an MCP server connected on the last step (section 14).
  Suggested Linear ticket for the Coder Agents team: "Report per-server MCP connection state on chats so clients can warn before tools go missing".
  A minimal version adds the latest connect outcome per server to the single-chat GET and publishes a watch event when it changes.
- **Plan-mode changes are not published.** A `PATCH` with `plan_mode` publishes nothing (`coderd/exp_chats.go:2657-2683`), so other clients learn about it only from the next snapshot.
  scuttle works around it with a GET after each PATCH.
  Suggested ticket: "Publish a chat watch event when plan mode changes".

### Unknowns

- Whether a user message to a running subagent behaves well in chatd (section 3).
- Whether `UpdateChatPlanModeByID` bumps `updated_at`, which decides whether a later watch payload's `plan_mode` can be trusted over a local change.
- Whether the watch payload's `has_unread` is accurate; scuttle follows the web UI's local rule instead (section 2).
- The default and maximum page size of `GET /chats`.
- The 50-attachment limit per chat from the spec.
- `GET /chats/{chat}/stream/parts` exists (`coderd/chat_routes.go:100`) but is hidden from the API docs (`coderd/exp_chats.go:9045`), so scuttle does not use it.

### Risks

- **Experimental skills API.** Personal skills live under `/api/experimental` and may change; failures stay inside the skill groups (spec section 8).
- **Socket count.** Up to four sockets (chat, preview, watch, git) each need reconnect logic and idle timeouts; the SDK's `frames` helper already gives every socket the same timeout.
- **Watch gaps.** Events during a watch reconnect are lost; the refetch on reconnect covers the first page only, so a chat further down can stay stale until it is scrolled to.
- **Chat list size.** Fuzzy filtering is local to loaded pages; the "Search all chats" row covers the rest.
- **Scope.** M2 has thirteen features; the cuttable tasks are marked in the task order.

## 20. Proposed task order

Each task ends with passing tests and the author trying it, as M1 and M1.5 did.

1. `coder-sdk`: WebSocket upgrade timeout, the watchdog message, a typed `watch_chat_git` stream, and a pin bump in scuttle.
1. Core-owned stream generation with `ForStream` tags, the `CloseStream` during `ReconnectAfter` test, and the reconnect counter fix.
1. The chat switch operation: `open_chat` and `open_blank` sharing one reset, `loading`-guarded load replies, and the model and effort reset.
1. `GET /users/me` at startup, the welcome screen user, and the organization lookup retry.
1. The watch task and the `chat_list` module with the merge rules, the refetch on connect, and the drain cap.
1. The shared table overlay, replacing `PickerState`.
1. `/chats`: list, fuzzy filter, filters, paging, unread, open, and the Ctrl+R binding.
1. `/chats` actions: archive, rename, pin, and read state.
1. `/chats` server search row.
1. The running-tool activity label.
1. `/subagents` with the preview stream, `/parent`, and the Esc rule.
1. `/model` grouped table.
1. `/title`.
1. `/queue`.
1. Plan-mode questions, "Implement the plan", and serialized plan-mode requests with the GET refresh.
1. Attachments: `/attach`, `@path`, chips, and upload.
1. Skills in the slash menu with the collision labels.
1. `/info`.
1. `/workspace` table, details, SSH copy, open in web, and `WorkspacesState`.
1. `/git` panel and the pager handoff.
1. `/mcp` list and health.
1. `/mcp` toggling for the next message (cuttable).
1. Older-history loading and orphan tool results (cuttable).
1. Scroll anchoring while scrolled up, the wheel with the mouse off, and `/help` for every new command.
