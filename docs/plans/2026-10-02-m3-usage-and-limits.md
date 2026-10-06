# scuttle M3: Usage and Limits Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Show the user's AI spend, workspace quota, and chat cost in a footer they configure with `/statusline` and in a `/usage` panel, with opt-in threshold warnings, make scuttle exit cleanly on signals and without hanging, line up the `/chats` titles behind fixed pin and status columns, show each chat's pull request in `/chats`, fix five composer and activity row annoyances, and draw Nerd Font icons through one `icons` setting, with a tip that suggests a Nerd Font in text mode.

**Architecture:** `scuttle-core` gains the status line settings (`config.rs`), the limit model and its formatting (`usage.rs`), and the refresh state with core-owned generations (`app.rs`); `scuttle-tui` draws the configured footer, runs the minute timer from the main loop's existing deadline, fetches through the generated client in `runtime.rs`, and adds the `/statusline` editor and the `/usage` panel as table overlays.
The main loop catches SIGINT, SIGTERM, and SIGHUP outside a handoff and leaves through the same terminal restore as `/quit`, and the tokio runtime shuts down with a bounded wait.
One SDK task redacts the session token from `server_version` errors, and scuttle repins to it through a local `file://` URL.

**Tech Stack:** Rust 2024 on the toolchain in `rust-toolchain.toml`, ratatui 0.30, crossterm 0.29, tokio 1 (adding its `signal` feature), toml 0.9, toml_edit 0.23, `coder-sdk` and its generated `coder-api-gen` client, wiremock and portable-pty in tests.

**Spec:** `docs/specs/2026-09-28-scuttle-design.md`, sections 6 (usage and limits), 8 (degraded modes), and 10 (the M3 row), with the M3 items in `docs/specs/2026-09-30-scuttle-m2-design.md` section 1 ("Moved out of M2") and section 17 (deferred items marked M3).
It also covers three items parked for M3 during M2.5: a graceful-shutdown handler for external signals, a bounded exit, and token redaction for the SDK's `server_version` request.
It also covers feedback items 47 to 53 in `docs/feedback/2026-10-01-m2-daily-use.md`: the `/chats` markers misalign the titles (47), `/chats` does not show attached pull requests (48), the line being typed looks underlined and the composer lost its bottom rule (49), a large paste goes in as raw text (50), End and Cmd+Left and Cmd+Right do not act on the current line (51), the activity row repeats the transcript's animated `Thinking` and tool markers (52), and scuttle should lean on Nerd Font icons, with a tip that links to nerdfonts.com (53).
The icon audit behind item 53 is `.superpowers/sdd/2026-10-02-m3-usage-and-limits/nerd-font-audit.md`.

## Global Constraints

- scuttle work happens on branch `m3`, created from `m2-polish` at `9594b67`, in its own worktree.
- SDK work happens on branch `m3-sdk`, created from `m2-sdk` at `80a4635`, in `<unofficial-coder-sdk-rs checkout>/.worktrees/m3-sdk`.
- Never push, never commit on `main`, never add a remote, and stage only the files a task names; never `git add -A`, because the worktree holds uncommitted documents that are not part of any task.
- Pushing `m3-sdk` needs the author's approval and is not part of this plan; scuttle depends on it through `git = "file://<unofficial-coder-sdk-rs checkout>"` with `branch = "m3-sdk"`, and `Cargo.lock` pins the commit.
- Only Task 10 changes the `coder-sdk` dependency line and its `Cargo.lock` pin.
- No task adds a crate to either `Cargo.toml` or to `Cargo.lock`; Task 8 turns on tokio's `signal` feature, whose only dependency, `signal-hook-registry`, is already in `Cargo.lock` through tokio's `process` feature.
- `scuttle-core` has no terminal dependencies (no ratatui, crossterm, arboard, or tokio).
- API calls and process launches happen only in `crates/scuttle-tui/src/runtime.rs`, or in a terminal handoff the main loop in `crates/scuttle-tui/src/main.rs` runs with the input thread paused (the pager, and `$EDITOR` through `Tui::open_editor` and `Tui::edit_settings`); Task 10 moves the last exception, the startup `server_version` call, into the runtime.
- The session token is never printed, logged, rendered, or written anywhere by scuttle.
- The local config file never holds secrets; this milestone adds exactly the keys `statusline.fields`, `statusline.thresholds.context`, `statusline.thresholds.spend`, `statusline.thresholds.quota`, and the top-level `icons = "nerd" | "text"` (Task 19), none of which matches the secret-name check in `config.rs`.
- `icons` left out falls back to the `NERD_FONT` environment variable (`1`, `true`, or `yes` for nerd; `0`, `false`, or `no` for text), and with neither set scuttle draws text; `main` reads `NERD_FONT` once, and no test reads the real environment.
- Task 19 makes `chats.pin_icon` optional: a value in the file still wins over the icon set's default pin.
- Every config write goes through `write_config` in `crates/scuttle-core/src/config.rs`: a temp file with mode 0600 moved into place, refusing a file its owner made read-only.
- Every crate sets `publish = false`, edition 2024, and the toolchain pinned in `rust-toolchain.toml`.
- Commit messages use Conventional Commits and end with the trailer `Assisted-by: AI`; they never name an AI model or vendor.
- A commit scope is a crate name (`scuttle-core`, `scuttle-tui`, `coder-sdk`), or no scope when a commit spans crates.
- Markdown files use one sentence per line and no em dashes, en dashes, or spaced double hyphens as punctuation; the same rule applies to code comments and user-facing strings.
- No coworker names, Linear IDs, or absolute home-directory paths in any committed file.
- The existing insta snapshots must not change, except the two welcome snapshots Task 19 updates for the Nerd Font tip; if another one does, the task broke M1 rendering.
- New tests never sleep to let time pass: TUI tests pass explicit `Instant`s to `Tui::draw_at`, `Tui::sync_notice`, and `Tui::poll_usage`, and runtime and pty tests wait on a channel or the screen with a timeout.
- Run `cargo fmt --all` before each commit; every scuttle task ends green on `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo fmt --all --check`, and the SDK task ends green on the same three commands in the SDK worktree.
- M2's interfaces stay as they are: the core owns every request generation, replies keep their `Msg::ForChat`, `Msg::ForRefresh`, and `Msg::ForPlan` tagging, every terminal handoff pauses the input thread (`terminal::Input::pause` and `resume`), the TUI rebuilds `View` only when `view_revision` moved or the width or history reset changed, scroll anchoring stays in `keep_top` and `keep_marks`, and an effect that needs the terminal is answered by `Effect::runs_in_main`.
- M2.5's interfaces stay as they are: `Tui::new(config, config_path, theme, welcome, seed)`, a footer field a command changes reads as `/command: value` with the command in `Theme::brand` and the value dim, `Tui::config` follows what `config.toml` holds, and `/info` keeps `App::info_panel` and its `CostState`.
- Crate APIs in this plan were checked against the sources of ratatui 0.30.2, crossterm 0.29.0, tokio 1.53.1, toml 0.9.12, toml_edit 0.23.10, signal-hook 0.3.18, and the generated client on `m2-sdk`, but not compiled.
  When a signature differs in the resolved version, adapt the implementation and the test setup, keep every test assertion, and report the adaptation.

## Review Focus

- A session token that expires while scuttle runs: the minute timer must not add a "run `coder login`" notice every minute; one notice, then the refreshes stop (Task 3, `a_rejected_token_stops_the_limits_with_one_notice`).
- A quota reply still in flight when the user switches organization: the footer must never show the old organization's credits as the new one's (Task 3, `a_quota_reply_for_an_organization_left_behind_is_never_shown`).
- A narrow terminal while a hidden field is past its threshold: the warning must outlast every unwarned field except the status, and the line must never be wider than the screen (Task 6, `a_hidden_field_past_its_threshold_shows_highlighted_and_gives_way_last`).
- A hand-edited `config.toml` with a misspelled field, a repeated field, or a threshold of 0 or 150: a parse error that names it, or a repeated field kept once, never a silent default or a panic (Task 1, `a_misspelled_field_or_an_impossible_threshold_is_an_error` and `status_line_fields_and_thresholds_load_from_the_file`).
- A zero, missing, or enormous budget or quota: no division by zero or overflow, a zero budget reads as reached, and a quota of `-1` reads as none (Task 2, `percents_never_divide_by_zero_or_overflow` and `quota_texts_say_when_no_quota_applies`).

## Decisions this plan makes that the spec left open

- **`/statusline` in a TUI.** `/statusline` opens a table overlay that lists all eleven fields, the shown ones first in footer order, then the hidden ones; each row reads `[x] model` or `[ ] cost`, a dim description, and its warning.
  Space or Enter shows or hides the selected field, Alt+Up and Alt+Down move it, and Shift+Up and Shift+Down do the same for terminals that keep Alt for themselves; Up, Down, PageUp, and PageDown move the selection as in every other overlay, and Esc closes.
  These keys exist only while the overlay has the keyboard, so they cannot reach the composer, and no other overlay or `/help` entry gives them a meaning.
  Each change applies to the footer at once and is saved at once, as every other local setting is, through `config::set_statusline` and the atomic 0600 `write_config`; a failed save keeps the change for the session and shows the error, and `Tui::config` keeps what the file holds.
- **Default fields.** The spec names `model`, `context`, `spend`, and `status`, but M2.5's footer also shows `/effort`, `/workspace`, `/organization`, and `/plan-mode` when they apply, and `/statusline` must build on it.
  The default list is `model`, `effort`, `context`, `workspace`, `organization`, `plan-mode`, `spend`, `status`, so an upgrade changes the footer only by adding spend where the deployment has it.
  Each conditional field still shows only when it has a value, so the M2.5 footer tests keep their exact strings.
- **New field texts.** `spend $1.20/$50.00` (or `spend $1.20` with no budget), `cost $0.42`, `quota 3/10` (hidden when no quota applies), and `queue 2` (hidden when nothing is queued); none of them reads as `/command:`, because no command changes them.
- **Thresholds.** `[statusline.thresholds]` takes `context`, `spend`, and `quota` as whole percents from 1 to 100; a key left out means no warning, so the default is off.
  Cost and queue have no limit to measure against, so they take no threshold.
  A field at or past its threshold draws in `Theme::warn`, bold, and a field the list leaves out joins the end of the line while it is past its threshold.
  In `/statusline`, Left and Right step a warning through off, 50, 60, 70, 75, 80, 85, 90, 95, and 100; a hand-written value between steps moves to the next step in the pressed direction.
- **Narrow footers.** When the line is too long, whole fields give way in this order: workspace, organization, queue, cost, quota, effort, spend, context, model, plan mode.
  The status is never dropped, only cut once it is all that is left, which fixes the deferred bug where the connection status went before plan mode.
  A field past its threshold gives way only after every field that is not, and the remaining line is still cut between grapheme clusters.
- **Footer `fit` and notices.** A notice or field puts `\r\n`, `\n`, and a lone `\r` each as one space, a tab as four spaces as `wrap.rs` counts it, and drops other control characters, and the width that decides what drops is measured on that same text.
  A key press now clears the notices still waiting their turn along with the active one, and a waiting notice older than two notice lifetimes (10 seconds) is dropped unseen, so neither a key nor an expiry replays old notices; the startup notices still take turns.
- **`/usage` layout.** A full-height panel with `/info`'s label column, wrapping, and Up and Down scrolling (`label_rows` and `label_column` in `overlay.rs`), with the rows "AI spend", "Period", "Budget", "Chat cost", "Context", and "Workspace quota".
  A `404` hides the row, a `403` puts the server's message in the row once, a request in flight reads "Loading…", and any other failure reads "unavailable: …".
- **Refreshing.** The TUI owns the minute: `Tui::usage_deadline` joins the main loop's existing deadline, and `Tui::poll_usage` sends `Msg::RefreshLimits` when it passes, and the open chat's cost with `Msg::RefreshCost` when one is due.
  A turn that ends (the core counts the stream status leaving running) and a change of the organization in view make the next refresh due at once; a turn end or a chat switch makes the cost due.
  The core owns `limits_generation` for spend and quota and keeps using `cost_generation` for cost, so a reply to an earlier refresh is dropped, and a quota reply for another organization never shows.
  A handoff runs to completion inside one main-loop iteration, so no refresh can start while the pager or the editor has the terminal; one that came due during it runs right after.
  A `404` or `403` stops asking for that limit until restart, and a `401` stops both with one notice.
  Cost is fetched only while the footer lists `cost` or `/usage` is open.
- **Which organization and user.** Spend uses `me`, as the web UI does; quota uses `App::current_org`, the open chat's organization or else the one new chats go to, since that is where the agent creates workspaces.
- **Signals.** A tokio signal stream catches SIGINT, SIGTERM, and SIGHUP for the whole run; outside a handoff the main loop leaves through `terminal::finish`, which restores raw mode, the alternate screen, the mouse, mode 1007, keyboard enhancement, and the title stack, and exits with 128 plus the signal number (130, 143, 129).
  During a handoff the signal waits: SIGTERM and SIGHUP end scuttle once the pager or editor exits, and a SIGINT, which the child got too, is forgotten by renewing the SIGINT stream.
  SIGQUIT keeps M2.5's handling, so `handoff_signals` now covers only SIGQUIT.
- **Bounded exit.** `main` builds the tokio runtime itself and ends it with `Runtime::shutdown_timeout(500 ms)`, so a `spawn_blocking` read stuck on a stalled network mount is left behind instead of holding the process open.
- **Task split.** The settings model, the limit formatting, and the core state are three tasks, because each has its own test cycle and the footer, the timer, and both overlays consume them.
  The footer is two tasks: the text cutting and notice queue can be rejected apart from the field model and drop order.
  The four `main.rs` tasks (timer, signals, bounded exit, repin) run back to back, as do the two overlay tasks, which share `commands.rs` and `overlay.rs`.
- **Icons.** One top-level `icons = "nerd" | "text"` key drives every icon, with `NERD_FONT` deciding while the key is left out and text when neither is set, since scuttle cannot detect the font.
  The icon module is `crates/scuttle-tui/src/icons.rs`, because glyphs, slots, and styles are rendering, and the set in effect rides on `Theme::icons`, so no surface's signature changes.
  Each glyph takes a fixed two-cell slot, the glyph and a space, counted by `icons::slot` and never with `width_cjk`; the one-cell state markers and spinners stay, and a tool's kind glyph gets its own slot after its marker.
  The footer keeps its `/command:` labels, and only its connection and error notices take icons, so the `/statusline` and `/usage` legend glyphs the audit proposes are left out.
  Text mode keeps what scuttle draws today, 🔵 included, plus the welcome screen's Nerd Font tip and a `/help` Icons section.

## Spec corrections from the server

Section 8 of the spec assumed both limits return `403` when unlicensed and that cost is Enterprise-only; the server at `d1597a583b` differs, and this plan follows the server.

- The quota route has no feature gate: an Enterprise deployment without Template RBAC answers `200` with `budget: -1`, never `403`.
- The cost route is in the open-source build and has no gate: without AI Gateway data it answers `200` with zeros, and `404` means the chat is gone.
  scuttle keeps hiding cost on `403` and `404`, as `/info` already does.

## Server facts (coder/coder `d1597a583b`)

| Endpoint | Where it is registered | Response | Open-source build | Enterprise without the license |
|----------|------------------------|----------|-------------------|--------------------------------|
| `GET /api/v2/users/{user}/ai/spend` | Enterprise router, behind `RequireFeatureMW(FeatureAIBridge)` | `codersdk.UserAISpendStatus` | `404` "Route not found." | `403` "AI Gateway is a Premium feature. Contact sales!" |
| `GET /api/v2/organizations/{organization}/members/{user}/workspace-quota` | Enterprise router, no feature gate | `codersdk.WorkspaceQuota` | `404` "Route not found." | `200` with `budget: -1` |
| `GET /api/v2/chats/{chat}/cost` | Open-source chat routes, no gate | `codersdk.ChatCost` | `200` | `200` |

- The spend route and its gate: [enterprise/coderd/coderd.go#L725-L740](https://github.com/coder/coder/blob/d1597a583b/enterprise/coderd/coderd.go#L725-L740); the handler `userAISpendStatus` and its Swagger annotations: [enterprise/coderd/aibridge.go#L1030-L1038](https://github.com/coder/coder/blob/d1597a583b/enterprise/coderd/aibridge.go#L1030-L1038).
- The gate's `403` and its message: [enterprise/coderd/templates.go#L352-L367](https://github.com/coder/coder/blob/d1597a583b/enterprise/coderd/templates.go#L352-L367), with `FeatureAIBridge` humanized as "AI Gateway" in [codersdk/deployment.go#L254-L266](https://github.com/coder/coder/blob/d1597a583b/codersdk/deployment.go#L254-L266).
- The spend types: [codersdk/aibridge.go#L28-L80](https://github.com/coder/coder/blob/d1597a583b/codersdk/aibridge.go#L28-L80).
  `UserAISpendStatus` flattens to `user_id`, `effective_group_id`, `effective_budget` (`spend_limit_micros`, `limit_source`), `period_start` (inclusive), `period_end` (exclusive), and `current_spend_micros`.
  `limit_source` is `user_override` or `group`, and a `null` `effective_budget` means no budget applies, so spend is unlimited.
- The quota route: [enterprise/coderd/coderd.go#L520-L531](https://github.com/coder/coder/blob/d1597a583b/enterprise/coderd/coderd.go#L520-L531); the handler and the `-1` allowance without Template RBAC: [enterprise/coderd/workspacequota.go#L154-L176](https://github.com/coder/coder/blob/d1597a583b/enterprise/coderd/workspacequota.go#L154-L176); the type, `credits_consumed` and `budget`: [codersdk/workspaces.go#L718-L721](https://github.com/coder/coder/blob/d1597a583b/codersdk/workspaces.go#L718-L721).
- The cost route: [coderd/chat_routes.go#L77](https://github.com/coder/coder/blob/d1597a583b/coderd/chat_routes.go#L77); the handler, which reports the whole chat tree and `404` for a missing chat: [coderd/exp_chats.go#L1785-L1835](https://github.com/coder/coder/blob/d1597a583b/coderd/exp_chats.go#L1785-L1835); the type: [codersdk/chats.go#L2040-L2052](https://github.com/coder/coder/blob/d1597a583b/codersdk/chats.go#L2040-L2052).
- An unknown `/api/v2` route answers `404` "Route not found.": [coderd/coderd.go#L1385](https://github.com/coder/coder/blob/d1597a583b/coderd/coderd.go#L1385) and [coderd/httpapi/httpapi.go#L191-L195](https://github.com/coder/coder/blob/d1597a583b/coderd/httpapi/httpapi.go#L191-L195).
- `{user}` accepts `me`: [coderd/httpmw/userparam.go#L62](https://github.com/coder/coder/blob/d1597a583b/coderd/httpmw/userparam.go#L62).
- The web UI asks for spend only when AI Gateway is visible, refreshes quota every 60 seconds, and treats a zero budget as reached: [site/src/pages/AgentsPage/components/UsageIndicator.tsx#L55-L88](https://github.com/coder/coder/blob/d1597a583b/site/src/pages/AgentsPage/components/UsageIndicator.tsx#L55-L88) and [site/src/utils/budget.ts#L26-L37](https://github.com/coder/coder/blob/d1597a583b/site/src/utils/budget.ts#L26-L37).

## SDK facts (`m2-sdk` at `80a4635`)

- The generated client already has all three endpoints, reached through `coder_sdk::Client::api()`, so no SDK change is needed for them:
  - `get_user_ai_spend(&self, user: &str) -> Result<ResponseValue<types::CodersdkUserAiSpendStatus>, Error<()>>`
  - `get_workspace_quota_by_user(&self, organization: &Uuid, user: &str) -> Result<ResponseValue<types::CodersdkWorkspaceQuota>, Error<()>>`
  - `get_chat_cost(&self, chat: &Uuid) -> Result<ResponseValue<types::CodersdkChatCost>, Error<()>>`, which `/info` already uses
- Every field of those types is an `Option`; `CodersdkAiBudgetLimit` has `limit_source: Option<CodersdkAiBudgetLimitSource>` (a string newtype, `.0`) and `spend_limit_micros: Option<i64>`.
- `coder_sdk::Error::from_progenitor` turns a non-200 into `Error::Api { status, message, .. }`, and a `401` into `Error::Unauthorized`.
- `Client::server_version` (in `crates/coder-sdk/src/client.rs`) builds its own error with `Error::from_status`, while the other hand-built requests use `Client::error_from_status`, which redacts the token from the message, the detail, and every validation; Task S1 closes that gap.

## Execution order and dependencies

Tasks run in this order: S1, then Tasks 1 through 13, then Tasks 15 through 18, then Task 19, then Task 14.
Task 10 waits for S1, since it repins scuttle to the commit S1 makes.
Every other scuttle task depends only on the scuttle tasks before it in that order, as each task's Interfaces block says.
Task 14 runs last: it builds on Task 13's fixed pin and status columns and on Task 19's icon module, its `icons` key, and its `chat_cells`, so it keeps its number but follows Task 19.
Tasks 15, 16, and 17 all edit `composer.rs` and `Tui::key` or `Tui::draw_at`, so they run in that order: Task 16's composer tests count Task 15's two rules, and Task 17 adds its keys beside Task 16's Tab and Backspace arms.
Task 18 runs after Task 17, on Task 15's composer height.
Task 19 runs after Task 18, so it draws its chip icons in Task 16's `Tui::chip_lines` and leaves Task 18's activity row as it is.

## File structure

- `crates/scuttle-core/src/config.rs`: `StatusField`, `Thresholds`, `StatuslineConfig`, `FieldList`, `set_statusline`, and the `[statusline]` lines of `TEMPLATE` (Task 1).
- `crates/scuttle-core/src/usage.rs`: `LimitState`, `Limit`, `Refusal`, the percent and threshold helpers, and the footer and `/usage` texts (Task 2).
- `crates/scuttle-core/src/time.rs`: `until` (Task 2).
- `crates/scuttle-core/src/app.rs`: the limit and cost state, its messages and effects, the turn counter (Task 3), and the `/statusline` and `/usage` commands (Tasks 11 and 12).
- `crates/scuttle-core/src/panels.rs`: `usage_lines`, sharing `/info`'s cost and context text (Task 12).
- `crates/scuttle-core/src/commands.rs`: `/statusline` and `/usage` (Tasks 11 and 12).
- `crates/scuttle-tui/src/runtime.rs`: the spend and quota fetches (Task 4) and `Runtime::server_version` (Task 10).
- `crates/scuttle-tui/src/footer.rs`: one-line cutting (Task 5) and the configured fields with their drop order (Task 6).
- `crates/scuttle-tui/src/app.rs`: the notice queue (Task 5), the footer settings (Task 6), the refresh timer (Task 7), and the two overlays' plumbing (Tasks 11 and 12).
- `crates/scuttle-tui/src/terminal.rs`: `Shutdown` and the narrowed `handoff_signals` (Task 8).
- `crates/scuttle-tui/src/main.rs`: the timer wakeup (Task 7), the signal arm (Task 8), the bounded runtime (Task 9), and the version check through the runtime (Task 10).
- `crates/scuttle-tui/src/overlay.rs`: `StatuslineState` and `Overlay::Statusline` (Task 11), `Overlay::Usage` (Task 12), and the `/chats` pin and status columns (Task 13).
- `crates/scuttle-tui/src/help.rs`: the `/statusline` keys (Task 11) and the `/chats` markers (Task 13).
- `crates/scuttle-tui/tests/pty.rs`: the signal test (Task 8).
- `Cargo.toml` and `Cargo.lock`: tokio's `signal` feature (Task 8) and the `m3-sdk` pin (Task 10).
- `crates/scuttle-core/src/chat_list.rs`: `PrState`, `PrBadge`, `pr_badge`, and `ChatRow::pr` (Task 14).
- `crates/scuttle-tui/src/theme.rs`: the pull request state styles (Task 14).
- `crates/scuttle-tui/src/overlay.rs` and `crates/scuttle-tui/src/help.rs`: the `/chats` pull request column, the `/git` pull request glyph, and their `/help` sentences (Task 14).
- `crates/scuttle-tui/src/composer.rs`: the cleared cursor-line style and the two-rule height (Task 15), the paste snippets and their keys (Task 16), and the line keys and `at_line_end` (Task 17).
- `crates/scuttle-tui/src/app.rs`: the composer's bottom rule (Task 15), sending snippets (Task 16), End and `edit_key` (Task 17), and the activity row that gives way (Task 18).
- `crates/scuttle-core/src/attachments.rs`, core `app.rs`, and `runtime.rs`: the pasted chip, `Msg::AttachPaste`, `Effect::UploadText`, and its upload (Task 16).
- `crates/scuttle-tui/src/activity.rs`: `shows_activity` (Task 18).
- `crates/scuttle-tui/src/icons.rs`: new; `Icon`, `Slot`, `slot`, `style`, `lead`, `line_with`, `tool_icon`, and `NERD_FONTS_URL` (Task 19), and the pull request icons and `pr_icon` (Task 14).
- `crates/scuttle-core/src/config.rs`: `IconSet`, the top-level `icons` key, the optional `chats.pin_icon`, and their `TEMPLATE` lines (Task 19).
- `crates/scuttle-core/src/panels.rs`: `McpRow::on` and `McpRow::failed` (Task 19).
- `crates/scuttle-tui/src/theme.rs`: `Theme::colors`, `Theme::icons`, and `Theme::icon` (Task 19).
- `crates/scuttle-tui/src/app.rs` and `crates/scuttle-tui/src/main.rs`: `Tui::set_icon_env`, the live `icons` switch, the resolved pin, the chip icons, and the one `NERD_FONT` read (Task 19).
- `crates/scuttle-tui/src/transcript_view.rs`, `footer.rs`, `overlay.rs`, and `help.rs`: the icons on each surface, the welcome screen's Nerd Font tip and its link, and the `/help` Icons section (Task 19).
- `crates/scuttle-tui/src/snapshots/`: the two welcome snapshots, and `crates/scuttle-tui/tests/pty.rs`: `NERD_FONT` removed from the child's environment (Task 19).
- SDK `crates/coder-sdk/src/client.rs` and `crates/coder-sdk/tests/errors.rs` (Task S1).

---

### Task S1: Redact the session token from `server_version` errors (SDK)

**Files:**
- Modify: `crates/coder-sdk/src/client.rs` (`Client::server_version`)
- Test: `crates/coder-sdk/tests/errors.rs`

**Interfaces:**
- Consumes: `Client::error_from_status(&self, status: u16, body: &[u8]) -> Error` (existing, `pub(crate)`).
- Produces: `Client::server_version(&self) -> Result<String>` with an unchanged signature, whose `Error::Api` message, detail, and validation details never hold the session token.
  Task 10 pins scuttle to this commit.

All commands in this task run from `<unofficial-coder-sdk-rs checkout>/.worktrees/m3-sdk`.

- [ ] **Step 1: Create the worktree**

Run: `cd <unofficial-coder-sdk-rs checkout> && git worktree add .worktrees/m3-sdk -b m3-sdk m2-sdk && cd .worktrees/m3-sdk && git log --oneline -1`
Expected: `80a4635 fix(coder-sdk): redact the session token from every hand-built request's error`

- [ ] **Step 2: Write the failing test**

Append to `crates/coder-sdk/tests/errors.rs`:

```rust
#[tokio::test]
async fn a_refused_version_check_redacts_the_token() {
    let server = MockServer::start().await;
    let secret = "s3cr3t-session-token-do-not-leak";
    Mock::given(path("/api/v2/buildinfo"))
        .respond_with(ResponseTemplate::new(500).set_body_json(serde_json::json!({
            "message": format!("bad token {secret}"),
            "detail": format!("echoed {secret}"),
            "validations": [{"field": "token", "detail": format!("got {secret}")}],
        })))
        .mount(&server)
        .await;
    let client = Client::new(&Session {
        url: server.uri().parse().unwrap(),
        token: SecretString::from(secret),
    })
    .unwrap();
    let err = client.server_version().await.unwrap_err();
    let rendered = format!("{err} {err:?}");
    assert!(rendered.contains("[redacted]"), "{rendered}");
    assert!(!rendered.contains(secret), "{rendered}");
}
```

- [ ] **Step 3: Run the test to verify it fails**

Run: `cargo test -p coder-sdk --test errors a_refused_version_check_redacts_the_token`
Expected: FAIL, with the panic message showing `bad token s3cr3t-session-token-do-not-leak`.

- [ ] **Step 4: Redact the error**

In `crates/coder-sdk/src/client.rs`, replace the doc comment and the error line of `server_version`:

```rust
    /// The server's version string from `/api/v2/buildinfo`. A refusal's text has the session
    /// token redacted, as every other hand-built request's does.
    pub async fn server_version(&self) -> Result<String> {
```

and

```rust
        if status != 200 {
            return Err(self.error_from_status(status, &body));
        }
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p coder-sdk --test errors`
Expected: PASS, including `server_version_sends_token_and_parses_version` and `unauthorized_maps_to_unauthorized`.

Run: `cargo fmt --all && cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check`
Expected: all green.

- [ ] **Step 6: Commit**

```bash
git add crates/coder-sdk/src/client.rs crates/coder-sdk/tests/errors.rs
git commit -m "fix(coder-sdk): redact the session token from server_version errors" \
  -m "The buildinfo request built its error with Error::from_status, so a server or proxy that echoed the token in a refusal leaked it. It now uses Client::error_from_status, like the other hand-built requests." \
  -m "Assisted-by: AI"
```

---

### Task 1: Status line settings

**Files:**
- Modify: `crates/scuttle-core/src/config.rs` (new types after `ChatsConfig`, `LocalConfig`, `TEMPLATE`, `load_from_str`, a new `set_statusline` after `set_effort`, and the template test)
- Test: `crates/scuttle-core/src/config.rs` (`mod tests`)

**Interfaces:**
- Consumes: `ConfigError`, `edit_document`, and `write_config` (existing, `config.rs`).
- Produces:
  - `pub enum StatusField { Model, Effort, Context, Workspace, Organization, PlanMode, Spend, Status, Cost, Quota, Queue }` (`Copy`, `Eq`, `Hash`, `Deserialize` in kebab case, so `PlanMode` is `"plan-mode"`).
  - `StatusField::ALL: [StatusField; 11]`, `StatusField::DEFAULT: [StatusField; 8]`, `StatusField::name(self) -> &'static str`, `StatusField::description(self) -> &'static str`, `StatusField::takes_threshold(self) -> bool`.
  - `pub struct Thresholds { pub context: Option<u8>, pub spend: Option<u8>, pub quota: Option<u8> }` (`Copy`, `Default`), `Thresholds::get(&self, StatusField) -> Option<u8>`, `Thresholds::step(&mut self, StatusField, up: bool)`, and `pub const THRESHOLD_STEPS: [u8; 9]`.
  - `pub struct StatuslineConfig { pub fields: Vec<StatusField>, pub thresholds: Thresholds }` with `Default` giving `StatusField::DEFAULT`, and `LocalConfig::statusline: StatuslineConfig`.
  - `pub struct FieldList { pub rows: Vec<(StatusField, bool)> }` with `FieldList::new(&StatuslineConfig) -> FieldList`, `fields(&self) -> Vec<StatusField>`, `toggle(&mut self, at: usize)`, and `move_by(&mut self, at: usize, up: bool) -> usize`.
  - `pub fn set_statusline(path: &Path, statusline: &StatuslineConfig) -> Result<(), ConfigError>`.

- [ ] **Step 1: Write the failing tests**

Add to `mod tests` in `crates/scuttle-core/src/config.rs`:

```rust
    #[test]
    fn the_status_line_defaults_to_the_m2_5_footer_plus_spend() {
        let cfg = LocalConfig::default();
        assert_eq!(cfg.statusline.fields, StatusField::DEFAULT.to_vec());
        assert_eq!(cfg.statusline.thresholds, Thresholds::default());
        let names: Vec<&str> = StatusField::DEFAULT.iter().map(|f| f.name()).collect();
        assert_eq!(
            names,
            [
                "model",
                "effort",
                "context",
                "workspace",
                "organization",
                "plan-mode",
                "spend",
                "status"
            ]
        );
    }

    #[test]
    fn every_field_name_is_the_one_the_file_uses() {
        for field in StatusField::ALL {
            let cfg = load_from_str(&format!("[statusline]\nfields = [\"{}\"]\n", field.name()))
                .unwrap_or_else(|e| panic!("{}: {e}", field.name()));
            assert_eq!(cfg.statusline.fields, vec![field]);
            assert!(!field.description().is_empty());
        }
    }

    #[test]
    fn status_line_fields_and_thresholds_load_from_the_file() {
        let cfg = load_from_str(
            "[statusline]\nfields = [\"status\", \"cost\", \"status\", \"model\"]\n\n[statusline.thresholds]\nspend = 80\ncontext = 90\n",
        )
        .unwrap();
        assert_eq!(
            cfg.statusline.fields,
            vec![StatusField::Status, StatusField::Cost, StatusField::Model],
            "a repeated field keeps its first place"
        );
        assert_eq!(
            cfg.statusline.thresholds,
            Thresholds {
                context: Some(90),
                spend: Some(80),
                quota: None
            }
        );
        let empty = load_from_str("[statusline]\nfields = []\n").unwrap();
        assert!(empty.statusline.fields.is_empty(), "an empty list is allowed");
    }

    #[test]
    fn a_misspelled_field_or_an_impossible_threshold_is_an_error() {
        match load_from_str("[statusline]\nfields = [\"modle\"]\n") {
            Err(ConfigError::Parse(m)) => {
                assert!(m.starts_with("line ") && m.contains("modle"), "{m}");
            }
            other => panic!("{other:?}"),
        }
        for bad in [0, 101, 150] {
            assert_eq!(
                load_from_str(&format!("[statusline.thresholds]\nquota = {bad}\n")),
                Err(ConfigError::Parse(format!(
                    "statusline.thresholds.quota is {bad}; use a percent from 1 to 100, or leave it out for no warning"
                )))
            );
        }
        assert!(
            load_from_str("[statusline.thresholds]\nspend = 300\n").is_err(),
            "too big for a percent at all"
        );
    }

    #[test]
    fn a_threshold_steps_from_off_through_the_levels_and_back() {
        let mut t = Thresholds::default();
        t.step(StatusField::Spend, true);
        assert_eq!(t.spend, Some(50), "up from off is the lowest level");
        t.step(StatusField::Spend, true);
        assert_eq!(t.spend, Some(60));
        t.step(StatusField::Spend, false);
        t.step(StatusField::Spend, false);
        assert_eq!(t.spend, None, "below the lowest level is off");
        t.spend = Some(83);
        t.step(StatusField::Spend, true);
        assert_eq!(t.spend, Some(85), "a hand-written value moves to the next level up");
        t.spend = Some(83);
        t.step(StatusField::Spend, false);
        assert_eq!(t.spend, Some(80));
        t.spend = Some(100);
        t.step(StatusField::Spend, true);
        assert_eq!(t.spend, Some(100), "the top level stays");
        t.step(StatusField::Model, true);
        assert_eq!(
            t,
            Thresholds {
                spend: Some(100),
                ..Thresholds::default()
            },
            "a field without a limit takes no threshold"
        );
        assert_eq!(t.get(StatusField::Spend), Some(100));
        assert_eq!(t.get(StatusField::Cost), None);
    }

    #[test]
    fn the_field_list_holds_every_field_once_and_moves_and_toggles_rows() {
        let cfg = StatuslineConfig {
            fields: vec![StatusField::Status, StatusField::Model],
            thresholds: Thresholds::default(),
        };
        let mut list = FieldList::new(&cfg);
        assert_eq!(list.rows.len(), StatusField::ALL.len());
        assert_eq!(
            list.rows[..3],
            [
                (StatusField::Status, true),
                (StatusField::Model, true),
                (StatusField::Effort, false)
            ],
            "the shown fields first, in order, then the hidden ones"
        );
        assert_eq!(list.move_by(0, true), 0, "the first row cannot move up");
        assert_eq!(list.move_by(0, false), 1);
        assert_eq!(list.fields(), vec![StatusField::Model, StatusField::Status]);
        list.toggle(2);
        assert_eq!(
            list.fields(),
            vec![StatusField::Model, StatusField::Status, StatusField::Effort]
        );
        let last = list.rows.len() - 1;
        assert_eq!(list.move_by(last, false), last, "the last row cannot move down");
    }

    #[test]
    fn saving_the_status_line_keeps_the_rest_of_the_file_and_drops_warnings_turned_off() {
        let dir = std::env::temp_dir().join(format!("scuttle-statusline-{}", uuid::Uuid::new_v4()));
        let path = dir.join("config.toml");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&path, "# mine\nmouse = false\n\n[statusline]\nfields = [\"model\"]\n")
            .unwrap();
        let mut cfg = StatuslineConfig {
            fields: vec![StatusField::Spend, StatusField::Status],
            thresholds: Thresholds {
                spend: Some(80),
                ..Thresholds::default()
            },
        };
        set_statusline(&path, &cfg).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("# mine\nmouse = false\n"), "{text}");
        assert_eq!(load(&path).unwrap().statusline, cfg);
        assert!(!load(&path).unwrap().mouse, "other keys stay");
        cfg.thresholds.spend = None;
        set_statusline(&path, &cfg).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("thresholds"), "an empty table is removed:\n{text}");
        assert_eq!(load(&path).unwrap().statusline, cfg);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
```

In `the_template_holds_only_defaults_and_every_key_in_it_parses`, replace the comparison and the assertion after it with:

```rust
        assert_eq!(
            LocalConfig {
                welcome: WelcomeConfig::default(),
                density: BTreeMap::new(),
                statusline: StatuslineConfig {
                    thresholds: Thresholds::default(),
                    ..cfg.statusline.clone()
                },
                ..cfg.clone()
            },
            LocalConfig::default(),
            "every value in the template is its default"
        );
        assert_eq!(cfg.density.get("read_file"), Some(&Density::Summary));
        assert_eq!(
            cfg.statusline.thresholds,
            Thresholds {
                context: Some(80),
                spend: Some(80),
                quota: Some(90)
            },
            "the example thresholds parse"
        );
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p scuttle-core --lib config::tests`
Expected: FAIL to compile with "cannot find type `StatusField`" and "no field `statusline`".

- [ ] **Step 3: Add the types**

In `crates/scuttle-core/src/config.rs`, after `impl Default for ChatsConfig`, add:

```rust
/// A field of the status footer, as `statusline.fields` names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StatusField {
    Model,
    Effort,
    Context,
    Workspace,
    Organization,
    PlanMode,
    Spend,
    Status,
    Cost,
    Quota,
    Queue,
}

impl StatusField {
    /// Every field, in the order the `/statusline` editor lists the hidden ones.
    pub const ALL: [StatusField; 11] = [
        StatusField::Model,
        StatusField::Effort,
        StatusField::Context,
        StatusField::Workspace,
        StatusField::Organization,
        StatusField::PlanMode,
        StatusField::Spend,
        StatusField::Status,
        StatusField::Cost,
        StatusField::Quota,
        StatusField::Queue,
    ];

    /// The footer until `statusline.fields` says otherwise: the spec's `model`, `context`,
    /// `spend`, and `status`, with the fields the M2.5 footer already showed in their places.
    pub const DEFAULT: [StatusField; 8] = [
        StatusField::Model,
        StatusField::Effort,
        StatusField::Context,
        StatusField::Workspace,
        StatusField::Organization,
        StatusField::PlanMode,
        StatusField::Spend,
        StatusField::Status,
    ];

    /// The field's name in `config.toml`.
    pub fn name(self) -> &'static str {
        match self {
            StatusField::Model => "model",
            StatusField::Effort => "effort",
            StatusField::Context => "context",
            StatusField::Workspace => "workspace",
            StatusField::Organization => "organization",
            StatusField::PlanMode => "plan-mode",
            StatusField::Spend => "spend",
            StatusField::Status => "status",
            StatusField::Cost => "cost",
            StatusField::Quota => "quota",
            StatusField::Queue => "queue",
        }
    }

    /// What the field shows, for the `/statusline` editor.
    pub fn description(self) -> &'static str {
        match self {
            StatusField::Model => "The chat's model (/model)",
            StatusField::Effort => "The reasoning effort, when the model has levels (/effort)",
            StatusField::Context => "Context window used, of the model's limit",
            StatusField::Workspace => "The attached workspace (/workspace)",
            StatusField::Organization => {
                "The organization new chats go to, when you have several (/organization)"
            }
            StatusField::PlanMode => "Plan mode, while it is on (/plan-mode)",
            StatusField::Spend => "Your AI spend this period, of your budget",
            StatusField::Status => "The chat's status and the connection",
            StatusField::Cost => "This chat's cost, for its whole tree",
            StatusField::Quota => "Workspace credits used, of your quota",
            StatusField::Queue => "Messages waiting in the queue",
        }
    }

    /// Whether the field has a limit to measure against, so a threshold can warn on it.
    pub fn takes_threshold(self) -> bool {
        matches!(
            self,
            StatusField::Context | StatusField::Spend | StatusField::Quota
        )
    }
}

/// The warning levels the `/statusline` editor steps through with Left and Right, after off.
pub const THRESHOLD_STEPS: [u8; 9] = [50, 60, 70, 75, 80, 85, 90, 95, 100];

/// The `[statusline.thresholds]` table: the percent of its limit at which a field is
/// highlighted, even when the footer leaves it out. A key left out means no warning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(default)]
pub struct Thresholds {
    pub context: Option<u8>,
    pub spend: Option<u8>,
    pub quota: Option<u8>,
}

impl Thresholds {
    /// `field`'s threshold; a field without a limit has none.
    pub fn get(&self, field: StatusField) -> Option<u8> {
        match field {
            StatusField::Context => self.context,
            StatusField::Spend => self.spend,
            StatusField::Quota => self.quota,
            _ => None,
        }
    }

    fn slot(&mut self, field: StatusField) -> Option<&mut Option<u8>> {
        match field {
            StatusField::Context => Some(&mut self.context),
            StatusField::Spend => Some(&mut self.spend),
            StatusField::Quota => Some(&mut self.quota),
            _ => None,
        }
    }

    /// Moves `field`'s threshold one level: up from off to the lowest level, down from the
    /// lowest level to off, and from a value between levels to the next level that way.
    pub fn step(&mut self, field: StatusField, up: bool) {
        let Some(slot) = self.slot(field) else {
            return;
        };
        *slot = match (*slot, up) {
            (None, true) => Some(THRESHOLD_STEPS[0]),
            (None, false) => None,
            (Some(v), true) => Some(THRESHOLD_STEPS.iter().copied().find(|s| *s > v).unwrap_or(v)),
            (Some(v), false) => THRESHOLD_STEPS.iter().rev().copied().find(|s| *s < v),
        };
    }

    /// Each threshold by its key in the file.
    fn entries(&self) -> [(&'static str, Option<u8>); 3] {
        [
            ("context", self.context),
            ("spend", self.spend),
            ("quota", self.quota),
        ]
    }
}

/// The `[statusline]` table: which footer fields show, in what order, and their warnings.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct StatuslineConfig {
    pub fields: Vec<StatusField>,
    pub thresholds: Thresholds,
}

impl Default for StatuslineConfig {
    fn default() -> Self {
        StatuslineConfig {
            fields: StatusField::DEFAULT.to_vec(),
            thresholds: Thresholds::default(),
        }
    }
}

impl StatuslineConfig {
    /// Keeps a repeated field only at its first place, and refuses a threshold outside 1 to
    /// 100, which no limit can reach or which would always warn.
    fn normalize(&mut self) -> Result<(), ConfigError> {
        let mut seen = Vec::new();
        self.fields.retain(|f| {
            let first = !seen.contains(f);
            seen.push(*f);
            first
        });
        for (name, value) in self.thresholds.entries() {
            if let Some(v) = value
                && !(1..=100).contains(&v)
            {
                return Err(ConfigError::Parse(format!(
                    "statusline.thresholds.{name} is {v}; use a percent from 1 to 100, or leave it out for no warning"
                )));
            }
        }
        Ok(())
    }
}

/// The `/statusline` editor's rows: every field once, with whether the footer shows it. The
/// shown fields come first, in the footer's order, then the hidden ones in
/// `StatusField::ALL` order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldList {
    pub rows: Vec<(StatusField, bool)>,
}

impl FieldList {
    pub fn new(statusline: &StatuslineConfig) -> FieldList {
        let mut rows: Vec<(StatusField, bool)> =
            statusline.fields.iter().map(|f| (*f, true)).collect();
        rows.extend(
            StatusField::ALL
                .iter()
                .filter(|f| !statusline.fields.contains(f))
                .map(|f| (*f, false)),
        );
        FieldList { rows }
    }

    /// The shown fields, in order, as `statusline.fields` stores them.
    pub fn fields(&self) -> Vec<StatusField> {
        self.rows
            .iter()
            .filter(|(_, shown)| *shown)
            .map(|(f, _)| *f)
            .collect()
    }

    /// Shows or hides the field at row `at`.
    pub fn toggle(&mut self, at: usize) {
        if let Some(row) = self.rows.get_mut(at) {
            row.1 = !row.1;
        }
    }

    /// Moves row `at` one place up or down, and returns where it is now.
    pub fn move_by(&mut self, at: usize, up: bool) -> usize {
        let to = if up {
            at.checked_sub(1)
        } else {
            Some(at + 1).filter(|t| *t < self.rows.len())
        };
        match to {
            Some(to) if at < self.rows.len() => {
                self.rows.swap(at, to);
                to
            }
            _ => at,
        }
    }
}
```

In `pub struct LocalConfig`, after `pub chats: ChatsConfig,`, add:

```rust
    /// The footer's fields and their warnings, which `/statusline` edits.
    pub statusline: StatuslineConfig,
```

and in its `Default`, after `chats: ChatsConfig::default(),`, add `statusline: StatuslineConfig::default(),`.

Replace `load_from_str` with:

```rust
pub fn load_from_str(text: &str) -> Result<LocalConfig, ConfigError> {
    let table: toml::Table = text.parse().map_err(|e| parse_error(text, &e))?;
    check_secrets(&table, "")?;
    let mut config: LocalConfig = toml::from_str(text).map_err(|e| parse_error(text, &e))?;
    config.statusline.normalize()?;
    Ok(config)
}
```

After `set_effort`, add:

```rust
/// The standard table at `key` of the table `item`, made from an inline table there or
/// created empty, so a sub-table can be written inside it.
fn standard_table<'a>(item: &'a mut toml_edit::Item, key: &str) -> &'a mut toml_edit::Item {
    if !item.get(key).is_some_and(toml_edit::Item::is_table) {
        let inline = item
            .get(key)
            .and_then(toml_edit::Item::as_inline_table)
            .cloned();
        item[key] = toml_edit::Item::Table(
            inline
                .map(toml_edit::InlineTable::into_table)
                .unwrap_or_default(),
        );
    }
    &mut item[key]
}

/// Saves the footer's fields and warnings under `[statusline]`, keeping the rest of the
/// file. A warning that is off is removed, and so is a `[statusline.thresholds]` table it
/// leaves empty.
pub fn set_statusline(path: &Path, statusline: &StatuslineConfig) -> Result<(), ConfigError> {
    edit_document(path, |doc| {
        let section = standard_table(doc.as_item_mut(), "statusline");
        let fields: toml_edit::Array = statusline.fields.iter().map(|f| f.name()).collect();
        section["fields"] = toml_edit::value(fields);
        let thresholds = standard_table(section, "thresholds");
        for (name, value) in statusline.thresholds.entries() {
            match value {
                Some(v) => thresholds[name] = toml_edit::value(i64::from(v)),
                None => {
                    if let Some(table) = thresholds.as_table_mut() {
                        table.remove(name);
                    }
                }
            }
        }
        if thresholds.as_table().is_some_and(toml_edit::Table::is_empty)
            && let Some(table) = section.as_table_mut()
        {
            table.remove("thresholds");
        }
    })
}
```

The last `if` borrows `thresholds` and then `section`; if the borrow checker refuses it, compute `let empty = thresholds.as_table().is_some_and(toml_edit::Table::is_empty);` first and test `empty` instead.

In `TEMPLATE`, after the `# spinner = "random"` line and its blank line, insert:

```
# [statusline]
# The footer's fields, in order; /statusline edits this list. A field with nothing to show
# stays hidden, such as spend on a deployment without AI Gateway.
# Other fields: "cost" (this chat's cost), "quota" (workspace credits), and "queue".
# fields = ["model", "effort", "context", "workspace", "organization", "plan-mode", "spend", "status"]

# [statusline.thresholds]
# Highlight context, spend, or quota once it reaches this percent of its limit, even when
# the footer leaves the field out. Leave a key out for no warning.
# context = 80
# spend = 80
# quota = 90

```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p scuttle-core --lib config::tests`
Expected: PASS, including the existing `the_template_is_written_only_when_the_file_is_missing` and `a_parse_error_names_its_line`.

- [ ] **Step 5: Run the full checks**

Run: `cargo fmt --all && cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check`
Expected: all green.

- [ ] **Step 6: Commit**

```bash
git add crates/scuttle-core/src/config.rs
git commit -m "feat(scuttle-core): add the status line fields and warning thresholds to the config" \
  -m "[statusline] lists the footer's fields in order, and [statusline.thresholds] holds the percent at which context, spend, or quota is highlighted. A repeated field keeps its first place, and a threshold outside 1 to 100 is a parse error." \
  -m "Assisted-by: AI"
```

---

### Task 2: The limit model and its texts

**Files:**
- Modify: `crates/scuttle-core/src/usage.rs` (new items after `short_tokens`)
- Modify: `crates/scuttle-core/src/time.rs` (new `until` after `ago`)
- Test: `crates/scuttle-core/src/usage.rs` and `crates/scuttle-core/src/time.rs` (`mod tests`)

**Interfaces:**
- Consumes: `ContextUsage`, `format_cost_micros`, and `count` (existing, `usage.rs`); `time::relative` (existing).
- Produces:
  - `pub enum LimitState<T> { Unknown, Loaded(T), Absent, Refused(String), Failed(String) }` (`Debug`, `Clone`, `Default` = `Unknown`), with `loaded(&self) -> Option<&T>`, `refreshes(&self) -> bool`, and `refuse(&mut self, refusal: Refusal) -> bool` (true for a `401`).
  - `pub enum Limit { Spend, Quota }` (`Copy`, `Eq`).
  - `pub enum Refusal { Absent, Unlicensed(String), Unauthorized, Failed(String) }` (`Clone`, `Eq`).
  - `pub const UNAUTHORIZED: &str`.
  - `pub fn percent(used: i64, limit: i64) -> Option<i64>`, `pub fn context_percent(u: &ContextUsage) -> Option<i64>`, `pub fn spend_percent(s: &types::CodersdkUserAiSpendStatus) -> Option<i64>`, `pub fn quota_percent(q: &types::CodersdkWorkspaceQuota) -> Option<i64>`, `pub fn crossed(percent: Option<i64>, threshold: Option<u8>) -> bool`.
  - `pub fn spend_field(s: &types::CodersdkUserAiSpendStatus) -> String`, `pub fn quota_field(q: &types::CodersdkWorkspaceQuota) -> Option<String>`, `pub fn spend_summary(s: &types::CodersdkUserAiSpendStatus) -> String`, `pub fn budget_source(s: &types::CodersdkUserAiSpendStatus) -> Option<String>`, `pub fn quota_summary(q: &types::CodersdkWorkspaceQuota) -> String`.
  - `pub fn time::until(when_unix: i64, now_unix: i64) -> String`.

- [ ] **Step 1: Write the failing tests**

Add to `mod tests` in `crates/scuttle-core/src/usage.rs`:

```rust
    fn spend(spent: i64, limit: Option<i64>, source: &str) -> types::CodersdkUserAiSpendStatus {
        let budget = limit.map(|l| json!({"spend_limit_micros": l, "limit_source": source}));
        serde_json::from_value(json!({"current_spend_micros": spent, "effective_budget": budget}))
            .unwrap()
    }

    fn quota(used: i64, budget: i64) -> types::CodersdkWorkspaceQuota {
        serde_json::from_value(json!({"credits_consumed": used, "budget": budget})).unwrap()
    }

    #[test]
    fn percents_never_divide_by_zero_or_overflow() {
        assert_eq!(percent(1, 4), Some(25));
        assert_eq!(percent(5, 0), Some(100), "a zero limit leaves no room");
        assert_eq!(percent(0, 0), Some(100));
        assert_eq!(percent(3, -1), None, "a negative limit means none applies");
        assert_eq!(percent(i64::MAX, 1), Some(i64::MAX), "clamped, not wrapped");
        assert_eq!(percent(i64::MAX, i64::MAX), Some(100));
        let u = ContextUsage {
            used: 50,
            limit: Some(200),
        };
        assert_eq!(context_percent(&u), Some(25));
        let no_limit = ContextUsage {
            used: 50,
            limit: Some(0),
        };
        assert_eq!(context_percent(&no_limit), None, "the footer shows no percent then");
    }

    #[test]
    fn a_threshold_is_crossed_at_its_percent() {
        assert!(crossed(Some(80), Some(80)));
        assert!(crossed(Some(120), Some(80)));
        assert!(!crossed(Some(79), Some(80)));
        assert!(!crossed(Some(100), None), "no threshold never warns");
        assert!(!crossed(None, Some(1)), "no limit never warns");
    }

    #[test]
    fn spend_texts_name_the_budget_and_where_it_comes_from() {
        let group = spend(1_200_000, Some(50_000_000), "group");
        assert_eq!(spend_field(&group), "spend $1.20/$50.00");
        assert_eq!(spend_summary(&group), "$1.20 of $50.00 (2%)");
        assert_eq!(spend_percent(&group), Some(2));
        assert_eq!(budget_source(&group).as_deref(), Some("Your group's budget"));
        let mine = spend(1_200_000, Some(50_000_000), "user_override");
        assert_eq!(
            budget_source(&mine).as_deref(),
            Some("Set for you, in place of your group's budget")
        );
        let unlimited = spend(1_200_000, None, "group");
        assert_eq!(spend_field(&unlimited), "spend $1.20");
        assert_eq!(
            spend_summary(&unlimited),
            "$1.20 this period; no budget applies, so spend is unlimited"
        );
        assert_eq!(spend_percent(&unlimited), None);
        assert_eq!(budget_source(&unlimited), None);
        let zero = spend(0, Some(0), "group");
        assert_eq!(spend_percent(&zero), Some(100), "a zero budget is already reached");
        assert_eq!(spend_summary(&zero), "$0.00 of $0.00, limit reached");
        let over = spend(60_000_000, Some(50_000_000), "group");
        assert_eq!(spend_summary(&over), "$60.00 of $50.00, limit reached");
        let empty: types::CodersdkUserAiSpendStatus = serde_json::from_value(json!({})).unwrap();
        assert_eq!(spend_field(&empty), "spend $0.00", "missing fields read as zero");
    }

    #[test]
    fn quota_texts_say_when_no_quota_applies() {
        let some = quota(3, 10);
        assert_eq!(quota_field(&some).as_deref(), Some("quota 3/10"));
        assert_eq!(quota_summary(&some), "3 of 10 credits used (30%)");
        assert_eq!(quota_percent(&some), Some(30));
        let none = quota(3, -1);
        assert_eq!(quota_field(&none), None, "the footer hides a quota that does not apply");
        assert_eq!(quota_summary(&none), "No quota applies; your workspaces use 3 credits");
        assert_eq!(quota_percent(&none), None);
        let zero = quota(0, 0);
        assert_eq!(quota_field(&zero).as_deref(), Some("quota 0/0"));
        assert_eq!(quota_percent(&zero), Some(100));
    }

    #[test]
    fn a_limit_keeps_what_loaded_through_a_failure_and_stops_asking_after_a_refusal() {
        let mut state = LimitState::Loaded(7u8);
        assert!(!state.refuse(Refusal::Failed("HTTP 502".into())));
        assert_eq!(state.loaded(), Some(&7), "a failed refresh keeps the value");
        assert!(state.refuse(Refusal::Unauthorized), "a 401 says so");
        assert_eq!(state.loaded(), Some(&7));
        let mut unknown = LimitState::<u8>::Unknown;
        assert!(unknown.refreshes());
        unknown.refuse(Refusal::Unauthorized);
        assert!(matches!(&unknown, LimitState::Failed(m) if m == UNAUTHORIZED));
        unknown.refuse(Refusal::Absent);
        assert!(matches!(unknown, LimitState::Absent));
        assert!(!unknown.refreshes(), "an open-source deployment never grows the route");
        let mut refused = LimitState::<u8>::Unknown;
        refused.refuse(Refusal::Unlicensed("AI Gateway is a Premium feature. Contact sales!".into()));
        assert!(matches!(&refused, LimitState::Refused(m) if m.starts_with("AI Gateway")));
        assert!(!refused.refreshes());
        refused.refuse(Refusal::Failed("HTTP 502".into()));
        assert!(
            matches!(&refused, LimitState::Refused(_)),
            "a failure does not undo a known refusal"
        );
    }
```

Add to `mod tests` in `crates/scuttle-core/src/time.rs`:

```rust
    #[test]
    fn a_time_ahead_reads_as_how_long_until_it() {
        let now = 1_000_000;
        assert_eq!(until(now + 240, now), "in 4m");
        assert_eq!(until(now + 29 * 86_400 + 3600, now), "in 29d");
        assert_eq!(until(now + 20, now), "now");
        assert_eq!(until(now - 5, now), "now", "a time already past is now");
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p scuttle-core --lib -- usage::tests time::tests`
Expected: FAIL to compile with "cannot find function `percent`" and "cannot find function `until`".

- [ ] **Step 3: Write the implementation**

In `crates/scuttle-core/src/usage.rs`, replace the module doc comment with:

```rust
//! Context window usage, computed the way the web UI does, and the AI spend and workspace
//! quota limits as the footer and `/usage` show them.
```

After `short_tokens`, add:

```rust
/// What scuttle knows of one limit: the user's AI spend or their workspace quota.
#[derive(Debug, Clone, Default)]
pub enum LimitState<T> {
    /// Not fetched yet.
    #[default]
    Unknown,
    Loaded(T),
    /// The deployment has no such route (`404`, an open-source build), so it shows nowhere.
    Absent,
    /// The deployment refused it (`403`, unlicensed), with the server's message for `/usage`.
    Refused(String),
    /// The last request failed, and none had loaded before it.
    Failed(String),
}

impl<T> LimitState<T> {
    pub fn loaded(&self) -> Option<&T> {
        match self {
            LimitState::Loaded(v) => Some(v),
            _ => None,
        }
    }

    /// Whether a refresh asks for it again: not after a `404` or `403`, since neither changes
    /// while scuttle runs.
    pub fn refreshes(&self) -> bool {
        !matches!(self, LimitState::Absent | LimitState::Refused(_))
    }

    /// Applies a refused or failed request. A failure keeps a value that already loaded, and a
    /// `404` or `403` already known, since neither changes while scuttle runs.
    /// Returns whether the session token was rejected, which stops every refresh.
    pub fn refuse(&mut self, refusal: Refusal) -> bool {
        match refusal {
            Refusal::Absent => *self = LimitState::Absent,
            Refusal::Unlicensed(message) => *self = LimitState::Refused(message),
            Refusal::Unauthorized => {
                if matches!(self, LimitState::Unknown | LimitState::Failed(_)) {
                    *self = LimitState::Failed(UNAUTHORIZED.into());
                }
                return true;
            }
            Refusal::Failed(message) => {
                if matches!(self, LimitState::Unknown | LimitState::Failed(_)) {
                    *self = LimitState::Failed(message);
                }
            }
        }
        false
    }
}

/// Which limit a request was for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Limit {
    Spend,
    Quota,
}

/// Why a limit request failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// `404`: the deployment has no such route, as an open-source build does not.
    Absent,
    /// `403`: the deployment is not licensed for it, with the server's message.
    Unlicensed(String),
    /// `401`: the session token was rejected.
    Unauthorized,
    /// Anything else, with its message.
    Failed(String),
}

/// What a limit reads as after the session token was rejected.
pub const UNAUTHORIZED: &str = "the session token was rejected";

/// `used` as a whole percent of `limit`, rounded down: `None` for a negative limit, which means
/// none applies, and 100 for a zero limit, which leaves no room. Clamped, so it never overflows.
pub fn percent(used: i64, limit: i64) -> Option<i64> {
    match limit {
        ..0 => None,
        0 => Some(100),
        _ => {
            let p = i128::from(used) * 100 / i128::from(limit);
            Some(p.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64)
        }
    }
}

/// The context window's use as a percent, when the model reports a limit above zero.
pub fn context_percent(u: &ContextUsage) -> Option<i64> {
    u.limit
        .filter(|limit| *limit > 0)
        .and_then(|limit| percent(u.used, limit))
}

/// The spend as a percent of the effective budget; `None` when no budget applies.
pub fn spend_percent(s: &types::CodersdkUserAiSpendStatus) -> Option<i64> {
    let limit = s.effective_budget.as_ref()?.spend_limit_micros?;
    percent(s.current_spend_micros.unwrap_or(0), limit)
}

/// The credits used as a percent of the quota; `None` when no quota applies (`budget` -1).
pub fn quota_percent(q: &types::CodersdkWorkspaceQuota) -> Option<i64> {
    percent(q.credits_consumed.unwrap_or(0), q.budget?)
}

/// Whether `percent` reached `threshold`. No threshold, or nothing to measure, never warns.
pub fn crossed(percent: Option<i64>, threshold: Option<u8>) -> bool {
    matches!((percent, threshold), (Some(p), Some(t)) if p >= i64::from(t))
}

/// The spend limit, when a budget applies.
fn spend_limit(s: &types::CodersdkUserAiSpendStatus) -> Option<i64> {
    s.effective_budget.as_ref()?.spend_limit_micros
}

/// The footer's spend field: `spend $1.20/$50.00`, or `spend $1.20` when no budget applies.
pub fn spend_field(s: &types::CodersdkUserAiSpendStatus) -> String {
    let spent = format_cost_micros(s.current_spend_micros.unwrap_or(0));
    match spend_limit(s) {
        Some(limit) => format!("spend {spent}/{}", format_cost_micros(limit)),
        None => format!("spend {spent}"),
    }
}

/// `/usage`'s spend row: the amount against the budget, or that spend is unlimited.
pub fn spend_summary(s: &types::CodersdkUserAiSpendStatus) -> String {
    let used = s.current_spend_micros.unwrap_or(0);
    let spent = format_cost_micros(used);
    let Some(limit) = spend_limit(s) else {
        return format!("{spent} this period; no budget applies, so spend is unlimited");
    };
    // The gateway blocks a request once spend reaches the limit, so reaching it is the end.
    if used >= limit {
        format!("{spent} of {}, limit reached", format_cost_micros(limit))
    } else {
        format!(
            "{spent} of {} ({}%)",
            format_cost_micros(limit),
            percent(used, limit).unwrap_or(0)
        )
    }
}

/// Where the budget comes from, for `/usage`; `None` when no budget applies.
pub fn budget_source(s: &types::CodersdkUserAiSpendStatus) -> Option<String> {
    let budget = s.effective_budget.as_ref()?;
    Some(match budget.limit_source.as_ref().map(|l| l.0.as_str()) {
        Some("user_override") => "Set for you, in place of your group's budget".into(),
        Some("group") => "Your group's budget".into(),
        Some(other) => other.replace('_', " "),
        None => "unknown".into(),
    })
}

/// The footer's quota field, `quota 3/10`; `None` when no quota applies.
pub fn quota_field(q: &types::CodersdkWorkspaceQuota) -> Option<String> {
    let budget = q.budget.filter(|b| *b >= 0)?;
    Some(format!("quota {}/{budget}", q.credits_consumed.unwrap_or(0)))
}

/// `/usage`'s quota row.
pub fn quota_summary(q: &types::CodersdkWorkspaceQuota) -> String {
    let used = q.credits_consumed.unwrap_or(0);
    match q.budget.filter(|b| *b >= 0) {
        Some(budget) => format!(
            "{used} of {budget} credits used ({}%)",
            percent(used, budget).unwrap_or(0)
        ),
        None => format!(
            "No quota applies; your workspaces use {}",
            count(used, "credit")
        ),
    }
}
```

In `crates/scuttle-core/src/time.rs`, after `ago`, add:

```rust
/// `in 4m` until `when_unix`, or `now` once it is less than a minute away or past.
pub fn until(when_unix: i64, now_unix: i64) -> String {
    match relative(now_unix, when_unix).as_str() {
        "now" => "now".into(),
        short => format!("in {short}"),
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p scuttle-core --lib -- usage::tests time::tests`
Expected: PASS.

- [ ] **Step 5: Run the full checks**

Run: `cargo fmt --all && cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check`
Expected: all green.
Several new items have no caller until Task 3; they are `pub`, so `dead_code` does not fire.

- [ ] **Step 6: Commit**

```bash
git add crates/scuttle-core/src/usage.rs crates/scuttle-core/src/time.rs
git commit -m "feat(scuttle-core): model the spend and quota limits and their texts" \
  -m "LimitState records what is known of a limit, keeping a loaded value through a failed refresh and remembering a 404 or 403. Percents clamp instead of overflowing, a zero limit reads as reached, and a quota of -1 reads as none." \
  -m "Assisted-by: AI"
```

---

### Task 3: Spend, quota, and chat cost state in the core

**Files:**
- Modify: `crates/scuttle-core/src/app.rs` (imports, `Msg`, `Effect`, `App` fields, `App::update` arms for `CostLoaded`, `CostHidden`, `CostFailed`, and `Stream`, `reset_chat_state`, `apply_watch`, and new methods)
- Test: `crates/scuttle-core/src/app.rs` (`mod tests`)

**Interfaces:**
- Consumes: `LimitState`, `Limit`, `Refusal` (Task 2); `CostState`, `App::current_org`, `App::is_running`, and `Effect::FetchCost` (existing).
- Produces:
  - `Msg::RefreshLimits`, `Msg::RefreshCost`, `Msg::SpendLoaded { spend: Box<types::CodersdkUserAiSpendStatus>, generation: u64 }`, `Msg::QuotaLoaded { org: Uuid, quota: types::CodersdkWorkspaceQuota, generation: u64 }`, `Msg::LimitFailed { limit: Limit, refusal: Refusal, generation: u64 }`, `Msg::UsageClosed`.
  - `Effect::FetchSpend { generation: u64 }` and `Effect::FetchQuota { org: Uuid, generation: u64 }`, both answered unwrapped by the runtime (Task 4).
  - `App::spend(&self) -> &LimitState<Box<types::CodersdkUserAiSpendStatus>>`, `App::quota(&self) -> &LimitState<types::CodersdkWorkspaceQuota>` (`Unknown` while it belongs to another organization), `App::turns_ended(&self) -> u64`.
  - `pub chat_cost: Option<CostState>`, `pub cost_in_footer: bool` (the TUI sets it from `statusline.fields`), and `pub usage_open: bool` on `App`.
  - `pub const LIMITS_STOPPED: &str`.
  - Private `App::refresh_limits(&mut self) -> Vec<Effect>` and `App::fetch_chat_cost(&mut self) -> Vec<Effect>`, which Task 12's `/usage` command calls.

- [ ] **Step 1: Write the failing tests**

Add to `mod tests` in `crates/scuttle-core/src/app.rs`:

```rust
    fn spend_status(spent: i64, limit: Option<i64>) -> Box<types::CodersdkUserAiSpendStatus> {
        let budget = limit.map(|l| json!({"spend_limit_micros": l, "limit_source": "group"}));
        Box::new(
            serde_json::from_value(json!({"current_spend_micros": spent, "effective_budget": budget}))
                .unwrap(),
        )
    }

    fn quota_of(used: i64, budget: i64) -> types::CodersdkWorkspaceQuota {
        serde_json::from_value(json!({"credits_consumed": used, "budget": budget})).unwrap()
    }

    #[test]
    fn limits_refresh_together_and_only_the_latest_reply_applies() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let org = started(&mut app);
        assert_eq!(
            app.update(Msg::RefreshLimits),
            vec![
                Effect::FetchSpend { generation: 1 },
                Effect::FetchQuota { org, generation: 1 }
            ]
        );
        assert_eq!(
            app.update(Msg::RefreshLimits),
            vec![
                Effect::FetchSpend { generation: 2 },
                Effect::FetchQuota { org, generation: 2 }
            ]
        );
        app.update(Msg::SpendLoaded {
            spend: spend_status(1, Some(10)),
            generation: 1,
        });
        assert!(
            matches!(app.spend(), LimitState::Unknown),
            "a reply to an earlier refresh is dropped"
        );
        app.update(Msg::SpendLoaded {
            spend: spend_status(1_200_000, Some(50_000_000)),
            generation: 2,
        });
        assert_eq!(
            app.spend().loaded().and_then(|s| s.current_spend_micros),
            Some(1_200_000)
        );
        app.update(Msg::QuotaLoaded {
            org,
            quota: quota_of(3, 10),
            generation: 2,
        });
        assert_eq!(app.quota().loaded().and_then(|q| q.budget), Some(10));
        app.update(Msg::LimitFailed {
            limit: Limit::Spend,
            refusal: Refusal::Failed("HTTP 502".into()),
            generation: 2,
        });
        assert!(
            app.spend().loaded().is_some(),
            "a failed refresh keeps what loaded"
        );
    }

    #[test]
    fn a_missing_or_unlicensed_limit_is_not_asked_for_again() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        app.update(Msg::RefreshLimits);
        app.update(Msg::LimitFailed {
            limit: Limit::Spend,
            refusal: Refusal::Unlicensed("AI Gateway is a Premium feature. Contact sales!".into()),
            generation: 1,
        });
        app.update(Msg::LimitFailed {
            limit: Limit::Quota,
            refusal: Refusal::Absent,
            generation: 1,
        });
        assert!(matches!(app.spend(), LimitState::Refused(m) if m.starts_with("AI Gateway")));
        assert!(matches!(app.quota(), LimitState::Absent));
        assert!(app.update(Msg::RefreshLimits).is_empty());
        assert!(app.notices.is_empty(), "neither refusal is a notice");
    }

    #[test]
    fn a_rejected_token_stops_the_limits_with_one_notice() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        app.update(Msg::RefreshLimits);
        for limit in [Limit::Spend, Limit::Quota] {
            app.update(Msg::LimitFailed {
                limit,
                refusal: Refusal::Unauthorized,
                generation: 1,
            });
        }
        let stopped = app
            .notices
            .iter()
            .filter(|n| matches!(n, Notice::Error(m) if m == LIMITS_STOPPED))
            .count();
        assert_eq!(stopped, 1, "{:?}", app.notices);
        assert!(app.update(Msg::RefreshLimits).is_empty(), "refreshes stop");
        assert!(matches!(app.spend(), LimitState::Failed(m) if m == crate::usage::UNAUTHORIZED));
    }

    #[test]
    fn a_quota_reply_for_an_organization_left_behind_is_never_shown() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let (product, coder) = two_orgs(&mut app);
        assert!(
            app.update(Msg::RefreshLimits)
                .contains(&Effect::FetchQuota {
                    org: coder.id,
                    generation: 1
                })
        );
        app.update(Msg::OrganizationChosen(product.id));
        app.update(Msg::QuotaLoaded {
            org: coder.id,
            quota: quota_of(3, 10),
            generation: 1,
        });
        assert!(
            matches!(app.quota(), LimitState::Unknown),
            "Coder's credits are not Product's"
        );
        assert!(
            app.update(Msg::RefreshLimits)
                .contains(&Effect::FetchQuota {
                    org: product.id,
                    generation: 2
                })
        );
        assert!(matches!(app.quota(), LimitState::Unknown));
        app.update(Msg::QuotaLoaded {
            org: product.id,
            quota: quota_of(1, 5),
            generation: 2,
        });
        assert_eq!(app.quota().loaded().and_then(|q| q.budget), Some(5));
    }

    #[test]
    fn a_turn_ends_when_the_status_leaves_running() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(Uuid::new_v4()),
            messages: vec![],
        });
        app.update(status("waiting"));
        assert_eq!(app.turns_ended(), 0, "an idle chat ran no turn");
        app.update(status("running"));
        app.update(status("running"));
        assert_eq!(app.turns_ended(), 0);
        app.update(status("waiting"));
        assert_eq!(app.turns_ended(), 1);
        app.update(status("waiting"));
        assert_eq!(app.turns_ended(), 1, "a second idle status is no new turn");
        app.update(status("running"));
        app.update(status("error"));
        assert_eq!(app.turns_ended(), 2, "a failed turn ended too");
    }

    #[test]
    fn the_chat_cost_is_fetched_only_while_the_footer_or_usage_shows_it() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(id),
            messages: vec![],
        });
        assert!(app.update(Msg::RefreshCost).is_empty(), "nothing shows the cost");
        app.cost_in_footer = true;
        assert_eq!(
            app.update(Msg::RefreshCost),
            vec![Effect::FetchCost {
                chat: id,
                generation: 1
            }]
        );
        assert!(matches!(app.chat_cost, Some(CostState::Loading)));
        app.update(Msg::ForChat {
            chat: id,
            msg: Box::new(Msg::CostLoaded {
                cost: serde_json::from_value(json!({"total_cost_micros": 420000, "request_count": 2}))
                    .unwrap(),
                generation: 1,
            }),
        });
        assert!(
            matches!(&app.chat_cost, Some(CostState::Loaded(c)) if c.total_cost_micros == Some(420000))
        );
        assert!(app.info_panel.is_none(), "/info stays closed");
        app.update(Msg::ForChat {
            chat: id,
            msg: Box::new(Msg::CostHidden { generation: 1 }),
        });
        assert!(matches!(app.chat_cost, Some(CostState::Hidden)));
        assert!(
            app.update(Msg::RefreshCost).is_empty(),
            "a refused cost is not asked for again"
        );
        app.update(Msg::Command(Command::New));
        assert!(app.chat_cost.is_none(), "another chat has its own cost");
        app.cost_in_footer = false;
        app.usage_open = true;
        app.update(Msg::UsageClosed);
        assert!(!app.usage_open);
    }
```

The `status` helper (`fn status(s: &str) -> Msg`) and `two_orgs` already exist in this test module.
Add `use crate::usage::{Limit, LimitState, Refusal};` to the test module's imports if `use super::*;` does not bring them in.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p scuttle-core --lib -- limits_ a_missing_or_unlicensed a_rejected_token a_quota_reply a_turn_ends the_chat_cost_is_fetched`
Expected: FAIL to compile with "no variant named `RefreshLimits`".

- [ ] **Step 3: Add the messages, effects, and state**

At the top of `crates/scuttle-core/src/app.rs`, add `use crate::usage::{Limit, LimitState, Refusal};`.

In `pub enum Msg`, after `InfoClosed,`, add:

```rust
    /// Asks for the AI spend and the workspace quota again: from the UI's minute timer, after
    /// a turn ends, and when the organization in view changes.
    RefreshLimits,
    /// Asks for the open chat's cost, while the footer lists `cost` or `/usage` is open.
    RefreshCost,
    /// The runtime's reply to `Effect::FetchSpend`, with the refresh's `generation`.
    SpendLoaded {
        spend: Box<types::CodersdkUserAiSpendStatus>,
        generation: u64,
    },
    /// The runtime's reply to `Effect::FetchQuota` for `org`, with the refresh's `generation`.
    QuotaLoaded {
        org: Uuid,
        quota: types::CodersdkWorkspaceQuota,
        generation: u64,
    },
    /// A spend or quota request failed, with why.
    LimitFailed {
        limit: Limit,
        refusal: Refusal,
        generation: u64,
    },
    /// The `/usage` panel closed.
    UsageClosed,
```

In `pub enum Effect`, after `FetchCost { .. },`, add:

```rust
    /// Fetches the signed-in user's AI spend, answered by `Msg::SpendLoaded` or
    /// `Msg::LimitFailed` with `generation`.
    FetchSpend { generation: u64 },
    /// Fetches the signed-in user's workspace quota in `org`, answered by `Msg::QuotaLoaded`
    /// or `Msg::LimitFailed` with `generation`.
    FetchQuota { org: Uuid, generation: u64 },
```

After `pub const PROMOTE_QUEUED`, add:

```rust
/// The notice a rejected session token gives once, when it stops the spend and quota refreshes.
pub const LIMITS_STOPPED: &str = "Spend and quota stopped updating: the session token was rejected. Run `coder login`, then restart scuttle.";

/// What `App::quota` reads as while the quota on hand belongs to another organization.
static NO_QUOTA: LimitState<types::CodersdkWorkspaceQuota> = LimitState::Unknown;
```

In `pub struct App`, after `cost_generation: u64,`, add:

```rust
    /// The open chat's cost for the footer and `/usage`, fetched while either shows it.
    pub chat_cost: Option<CostState>,
    /// Whether the footer lists the `cost` field. The UI sets it from `statusline.fields`.
    pub cost_in_footer: bool,
    /// Set while `/usage` is open.
    pub usage_open: bool,
    /// The user's AI spend this period.
    spend: LimitState<Box<types::CodersdkUserAiSpendStatus>>,
    /// The user's workspace quota in `quota_org`.
    quota: LimitState<types::CodersdkWorkspaceQuota>,
    /// The organization `quota` was asked for.
    quota_org: Option<Uuid>,
    /// Bumped by every refresh of the limits, so a reply to an earlier one is dropped.
    limits_generation: u64,
    /// Set by a `401` on a limit request; the limits refresh no more until restart.
    limits_stopped: bool,
    /// The open chat's turns that ended: each time its status left running.
    turns_ended: u64,
```

- [ ] **Step 4: Apply the replies and count turns**

Replace the three cost arms in `App::update`:

```rust
            Msg::CostLoaded { cost, generation } => {
                if generation == self.cost_generation {
                    if self.info_panel.is_some() {
                        self.info_panel = Some(CostState::Loaded(cost.clone()));
                    }
                    self.chat_cost = Some(CostState::Loaded(cost));
                }
                vec![]
            }
            Msg::CostHidden { generation } => {
                if generation == self.cost_generation {
                    if self.info_panel.is_some() {
                        self.info_panel = Some(CostState::Hidden);
                    }
                    self.chat_cost = Some(CostState::Hidden);
                }
                vec![]
            }
            Msg::CostFailed {
                message,
                generation,
            } => {
                // A failed refetch keeps the total already shown.
                if generation == self.cost_generation {
                    if self
                        .info_panel
                        .as_ref()
                        .is_some_and(|c| !matches!(c, CostState::Loaded(_)))
                    {
                        self.info_panel = Some(CostState::Failed(message.clone()));
                    }
                    if !matches!(self.chat_cost, Some(CostState::Loaded(_))) {
                        self.chat_cost = Some(CostState::Failed(message));
                    }
                }
                vec![]
            }
```

Delete `fn is_current_cost`, which nothing calls any more.

After the `Msg::InfoClosed` arm, add:

```rust
            Msg::RefreshLimits => self.refresh_limits(),
            Msg::RefreshCost => {
                if self.cost_in_footer || self.usage_open {
                    self.fetch_chat_cost()
                } else {
                    vec![]
                }
            }
            Msg::SpendLoaded { spend, generation } => {
                if generation == self.limits_generation {
                    self.spend = LimitState::Loaded(spend);
                }
                vec![]
            }
            Msg::QuotaLoaded {
                org,
                quota,
                generation,
            } => {
                if generation == self.limits_generation && self.quota_org == Some(org) {
                    self.quota = LimitState::Loaded(quota);
                }
                vec![]
            }
            Msg::LimitFailed {
                limit,
                refusal,
                generation,
            } => {
                if generation != self.limits_generation {
                    return vec![];
                }
                let unauthorized = match limit {
                    Limit::Spend => self.spend.refuse(refusal),
                    Limit::Quota => self.quota.refuse(refusal),
                };
                if unauthorized && !self.limits_stopped {
                    self.limits_stopped = true;
                    self.error(LIMITS_STOPPED);
                }
                vec![]
            }
            Msg::UsageClosed => {
                self.usage_open = false;
                vec![]
            }
```

Replace the start of the `Msg::Stream(ev)` arm so it counts a turn that ends:

```rust
            Msg::Stream(ev) => {
                let was_running = self.is_running();
                let resets = self.transcript.history_resets();
                let applied = self.transcript.apply(&ev);
                // A turn ends when the status leaves running, interrupting, or requires
                // action, whatever it lands on; the UI refreshes the limits then.
                if was_running && !self.is_running() {
                    self.turns_ended += 1;
                }
```

keeping the rest of the arm (`if ev.kind == ...HistoryReset ...` and `self.applied_stream(ev, applied)`) as it is.

In `reset_chat_state`, after `self.info_panel = None;`, add `self.chat_cost = None;`.

In `apply_watch`, replace `&& self.info_panel.is_some()` with `&& (self.info_panel.is_some() || self.cost_in_footer || self.usage_open)`, and replace its comment's first sentence with: "Cost covers the whole chat tree, so any family member's turn changes it, for `/info`, `/usage`, and the footer."

- [ ] **Step 5: Add the refresh methods and accessors**

In `impl App`, after `fn current_mcp`, add:

```rust
    /// The user's AI spend this period.
    pub fn spend(&self) -> &LimitState<Box<types::CodersdkUserAiSpendStatus>> {
        &self.spend
    }

    /// The user's workspace quota in the organization in view; `Unknown` while the quota on
    /// hand was asked for another organization.
    pub fn quota(&self) -> &LimitState<types::CodersdkWorkspaceQuota> {
        if self.quota_org == self.current_org() {
            &self.quota
        } else {
            &NO_QUOTA
        }
    }

    /// How many turns of the open chat ended, so the UI can refresh the limits after each.
    pub fn turns_ended(&self) -> u64 {
        self.turns_ended
    }

    /// Asks for the AI spend and the quota of the organization in view under a new
    /// generation, so a reply to an earlier refresh is dropped. A limit the deployment lacks
    /// or does not license is not asked for again, and nothing is after a `401`.
    fn refresh_limits(&mut self) -> Vec<Effect> {
        if self.limits_stopped {
            return vec![];
        }
        self.limits_generation += 1;
        let generation = self.limits_generation;
        let mut effects = Vec::new();
        if self.spend.refreshes() {
            effects.push(Effect::FetchSpend { generation });
        }
        let org = self.current_org();
        if org != self.quota_org {
            // Another organization's credits say nothing about this one, but a 404 or 403
            // holds for the whole deployment.
            if matches!(self.quota, LimitState::Loaded(_) | LimitState::Failed(_)) {
                self.quota = LimitState::Unknown;
            }
            self.quota_org = org;
        }
        if let Some(org) = org
            && self.quota.refreshes()
        {
            effects.push(Effect::FetchQuota { org, generation });
        }
        effects
    }

    /// Asks for the open chat's cost under a new generation, unless the server refused it.
    fn fetch_chat_cost(&mut self) -> Vec<Effect> {
        let Some(chat) = self.chat_id else {
            return vec![];
        };
        if matches!(self.chat_cost, Some(CostState::Hidden)) {
            return vec![];
        }
        if self.chat_cost.is_none() {
            self.chat_cost = Some(CostState::Loading);
        }
        self.cost_generation += 1;
        vec![Effect::FetchCost {
            chat,
            generation: self.cost_generation,
        }]
    }
```

`crates/scuttle-tui/src/runtime.rs` matches `Effect` exhaustively, so until Task 4 adds its arms, add `| Effect::FetchSpend { .. } | Effect::FetchQuota { .. }` to the list of ignored effects at the end of `Runtime::run`, with no other change there; Task 4 replaces that.

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test -p scuttle-core --lib -- limits_ a_missing_or_unlicensed a_rejected_token a_quota_reply a_turn_ends the_chat_cost_is_fetched info_fetches_cost only_the_latest_cost_reply`
Expected: PASS, including the two existing `/info` cost tests.

- [ ] **Step 7: Run the full checks**

Run: `cargo fmt --all && cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check`
Expected: all green.

- [ ] **Step 8: Commit**

```bash
git add crates/scuttle-core/src/app.rs crates/scuttle-tui/src/runtime.rs
git commit -m "feat(scuttle-core): track the spend, quota, and chat cost behind core generations" \
  -m "A refresh asks for both limits under a new generation and drops replies to earlier ones and quota replies for another organization. A 404 or 403 stops asking for that limit, and a 401 stops both with one notice. The core counts the turns that end, and the open chat's cost is kept for the footer and /usage while either shows it." \
  -m "Assisted-by: AI"
```

---

### Task 4: Fetch the spend and quota in the runtime

**Files:**
- Modify: `crates/scuttle-tui/src/runtime.rs` (imports, a new `refusal` after `model_refused`, and two arms in `Runtime::run`)
- Test: `crates/scuttle-tui/src/runtime.rs` (`mod tests`)

**Interfaces:**
- Consumes: `Effect::FetchSpend { generation }`, `Effect::FetchQuota { org, generation }`, `Msg::SpendLoaded`, `Msg::QuotaLoaded`, `Msg::LimitFailed` (Task 3); `Limit`, `Refusal` (Task 2); `Client::api().get_user_ai_spend(&str)` and `get_workspace_quota_by_user(&Uuid, &str)` (generated).
- Produces: `fn refusal(e: coder_sdk::Error) -> Refusal` (private); the two effects answered with unwrapped messages, since neither belongs to a chat.

- [ ] **Step 1: Write the failing tests**

Add to `mod tests` in `crates/scuttle-tui/src/runtime.rs`:

```rust
    #[tokio::test]
    async fn spend_and_quota_load_for_me_with_the_refresh_generation() {
        let server = MockServer::start().await;
        let org = Uuid::new_v4();
        Mock::given(method("GET"))
            .and(path("/api/v2/users/me/ai/spend"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "current_spend_micros": 1_200_000,
                "effective_budget": {"spend_limit_micros": 50_000_000, "limit_source": "group"},
                "period_start": "2026-10-01T00:00:00Z", "period_end": "2026-11-01T00:00:00Z"
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!(
                "/api/v2/organizations/{org}/members/me/workspace-quota"
            )))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"credits_consumed": 3, "budget": 10})),
            )
            .mount(&server)
            .await;
        let (mut rt, mut rx) = runtime(&server.uri());
        rt.run(Effect::FetchSpend { generation: 4 });
        match next(&mut rx).await {
            Msg::SpendLoaded {
                spend,
                generation: 4,
            } => assert_eq!(spend.current_spend_micros, Some(1_200_000)),
            other => panic!("{other:?}"),
        }
        rt.run(Effect::FetchQuota { org, generation: 4 });
        match next(&mut rx).await {
            Msg::QuotaLoaded {
                org: got,
                quota,
                generation: 4,
            } => {
                assert_eq!(got, org);
                assert_eq!(quota.budget, Some(10));
            }
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn a_missing_unlicensed_or_rejected_limit_says_which() {
        let org = Uuid::new_v4();
        for (status, body, want) in [
            (404, "Route not found.", Refusal::Absent),
            (
                403,
                "AI Gateway is a Premium feature. Contact sales!",
                Refusal::Unlicensed("AI Gateway is a Premium feature. Contact sales!".into()),
            ),
            (401, "You must be logged in.", Refusal::Unauthorized),
            (500, "boom", Refusal::Failed("boom".into())),
        ] {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .respond_with(api_error(status, body))
                .mount(&server)
                .await;
            let (mut rt, mut rx) = runtime(&server.uri());
            rt.run(Effect::FetchSpend { generation: 1 });
            match next(&mut rx).await {
                Msg::LimitFailed {
                    limit: Limit::Spend,
                    refusal,
                    generation: 1,
                } => assert_eq!(refusal, want, "spend, HTTP {status}"),
                other => panic!("spend, HTTP {status}: {other:?}"),
            }
            rt.run(Effect::FetchQuota { org, generation: 2 });
            match next(&mut rx).await {
                Msg::LimitFailed {
                    limit: Limit::Quota,
                    refusal,
                    generation: 2,
                } => assert_eq!(refusal, want, "quota, HTTP {status}"),
                other => panic!("quota, HTTP {status}: {other:?}"),
            }
        }
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p scuttle-tui --bin scuttle -- spend_and_quota_load a_missing_unlicensed_or_rejected`
Expected: FAIL: the first times out waiting for a message, because Task 3 left both effects ignored, or the test does not compile because `Limit` and `Refusal` are not imported yet.

- [ ] **Step 3: Write the implementation**

Add `use scuttle_core::usage::{Limit, Refusal};` to the imports at the top of `crates/scuttle-tui/src/runtime.rs`.

After `fn model_refused`, add:

```rust
/// What a failed spend or quota request means for the limit: a `404` from a deployment
/// without the route, a `403` from one without the license, or a rejected session token.
fn refusal(e: coder_sdk::Error) -> Refusal {
    match e {
        coder_sdk::Error::Api { status: 404, .. } => Refusal::Absent,
        coder_sdk::Error::Api {
            status: 403,
            message,
            ..
        } => Refusal::Unlicensed(message),
        coder_sdk::Error::Unauthorized => Refusal::Unauthorized,
        other => Refusal::Failed(other.to_string()),
    }
}
```

In `Runtime::run`, remove `| Effect::FetchSpend { .. } | Effect::FetchQuota { .. }` from the ignored effects, and after the `Effect::FetchCost` arm add:

```rust
            // Neither limit belongs to a chat, so the replies go back unwrapped, tagged only
            // with the core's generation.
            Effect::FetchSpend { generation } => self.spawn(Box::pin(async move {
                match client.api().get_user_ai_spend("me").await {
                    Ok(r) => Msg::SpendLoaded {
                        spend: Box::new(r.into_inner()),
                        generation,
                    },
                    Err(e) => Msg::LimitFailed {
                        limit: Limit::Spend,
                        refusal: refusal(coder_sdk::Error::from_progenitor(e).await),
                        generation,
                    },
                }
            })),
            Effect::FetchQuota { org, generation } => self.spawn(Box::pin(async move {
                match client.api().get_workspace_quota_by_user(&org, "me").await {
                    Ok(r) => Msg::QuotaLoaded {
                        org,
                        quota: r.into_inner(),
                        generation,
                    },
                    Err(e) => Msg::LimitFailed {
                        limit: Limit::Quota,
                        refusal: refusal(coder_sdk::Error::from_progenitor(e).await),
                        generation,
                    },
                }
            })),
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p scuttle-tui --bin scuttle -- spend_and_quota_load a_missing_unlicensed_or_rejected cost_is_hidden`
Expected: PASS.

- [ ] **Step 5: Run the full checks**

Run: `cargo fmt --all && cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check`
Expected: all green.

- [ ] **Step 6: Commit**

```bash
git add crates/scuttle-tui/src/runtime.rs
git commit -m "feat(scuttle-tui): fetch the AI spend and workspace quota" \
  -m "Both use the generated client for the signed-in user. A 404 means the deployment has no such route, a 403 that it is not licensed, and a 401 that the token was rejected; the core decides what each does." \
  -m "Assisted-by: AI"
```

---

### Task 5: One-line footer text and a notice queue that never replays

**Files:**
- Modify: `crates/scuttle-tui/src/footer.rs` (`fit`, new `fit_cut` and `flat_width`, `fields_width`, `field_spans`)
- Modify: `crates/scuttle-tui/src/app.rs` (`Tui::unshown_notices`, `Tui::sync_notice`, `Tui::key`, and a new `NOTICE_STALE`)
- Test: `crates/scuttle-tui/src/footer.rs` and `crates/scuttle-tui/src/app.rs` (`mod tests`)

**Interfaces:**
- Consumes: `crate::wrap::cells_width` (existing).
- Produces:
  - `fn fit_cut(text: &str, width: usize) -> (String, bool)` and `fn flat_width(text: &str) -> usize` in `footer.rs` (private), which Task 6's field cutting uses.
  - `pub const NOTICE_STALE: Duration` in `app.rs` (twice `NOTICE_TTL`).
  - `Tui::unshown_notices: VecDeque<(usize, Instant)>`, each waiting notice with when it arrived.

- [ ] **Step 1: Write the failing tests**

In `mod tests` of `crates/scuttle-tui/src/footer.rs`, replace `notice_line_breaks_become_spaces` with:

```rust
    #[test]
    fn a_notice_reads_as_one_line_with_one_space_per_line_break() {
        let app = App::new(BusyBehavior::Queue, true);
        let notice = Notice::Error("first\nsecond\r\nthird\rfourth\tfifth\u{7}!".into());
        let t = text(&footer_line(
            &app,
            Some(&notice),
            &Theme::terminal(true),
            80,
        ));
        assert_eq!(t, "first second third fourth    fifth!");
    }

    #[test]
    fn a_cut_notice_never_exceeds_the_width_or_splits_a_cluster() {
        let app = App::new(BusyBehavior::Queue, true);
        let notice = Notice::Info("tab\there \u{1F469}\u{200D}\u{1F4BB} e\u{301}\r\nend".into());
        for width in 0..=30u16 {
            let t = text(&footer_line(
                &app,
                Some(&notice),
                &Theme::terminal(true),
                width,
            ));
            assert!(cells_width(&t) <= width as usize, "{width}: {t:?}");
            assert!(!t.contains(['\t', '\r', '\n']), "{width}: {t:?}");
            assert!(!t.ends_with('\u{200D}'), "{width}: {t:?}");
        }
    }

    #[test]
    fn a_status_with_a_line_break_is_measured_as_drawn() {
        let mut app = live_app("running");
        app.update(stream(json!({"type": "retry", "retry": {"attempt": 1, "error": "rate\r\nlimited"}})));
        let t = status_text(&app);
        assert!(t.ends_with("retrying (attempt 1): rate limited"), "{t}");
        let width = cells_width(&t) as u16;
        assert_eq!(
            text(&footer_line(&app, None, &Theme::terminal(true), width)),
            t,
            "the width that decides what drops counts the break as one column"
        );
    }
```

Add to `mod tests` in `crates/scuttle-tui/src/app.rs`, after `notices_that_were_never_shown_take_turns`:

```rust
    #[test]
    fn a_key_drops_the_notices_waiting_their_turn() {
        let mut t = tui();
        let now = Instant::now();
        t.core.notices.push(Notice::Info("old one".into()));
        t.core.notices.push(Notice::Info("old two".into()));
        t.sync_notice(now);
        assert_eq!(t.active_notice(), Some(&Notice::Info("old two".into())));
        t.handle(key(KeyCode::Char('x'), KeyModifiers::NONE));
        t.sync_notice(now);
        assert_eq!(
            t.active_notice(),
            None,
            "the waiting notice does not replay after the key"
        );
        assert_eq!(t.notice_deadline(), None);
    }

    #[test]
    fn a_notice_that_waited_two_lifetimes_is_dropped_unseen() {
        let mut t = tui();
        let now = Instant::now();
        for text in ["one", "two", "three"] {
            t.core.notices.push(Notice::Info(text.into()));
        }
        t.sync_notice(now);
        assert_eq!(t.active_notice(), Some(&Notice::Info("three".into())));
        t.sync_notice(now + NOTICE_TTL);
        assert_eq!(t.active_notice(), Some(&Notice::Info("one".into())));
        t.sync_notice(now + NOTICE_TTL * 2);
        assert_eq!(
            t.active_notice(),
            None,
            "two waited ten seconds and no longer describes the moment"
        );
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p scuttle-tui --bin scuttle -- footer::tests a_key_drops_the_notices a_notice_that_waited`
Expected: FAIL: `a_notice_reads_as_one_line...` shows `"first second  third fourth\tfifth\u{7}!"`, `a_key_drops_the_notices...` shows `Some(Info("old one"))`, and `a_notice_that_waited...` shows `Some(Info("two"))`.

- [ ] **Step 3: Cut text to one line in the footer**

In `crates/scuttle-tui/src/footer.rs`, replace `fn fit` with:

```rust
/// `text` on one line, cut to `width` columns between grapheme clusters, and whether anything
/// was cut. A line break (`\r\n`, `\n`, or a lone `\r`) becomes one space and a tab four
/// spaces, as `wrap.rs` counts a tab; other control characters are dropped, since the
/// terminal draws none of them.
fn fit_cut(text: &str, width: usize) -> (String, bool) {
    let mut flat = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                chars.next_if_eq(&'\n');
                flat.push(' ');
            }
            '\n' => flat.push(' '),
            '\t' => flat.push_str("    "),
            c if c.is_control() => {}
            c => flat.push(c),
        }
    }
    let mut used = 0;
    let mut out = String::new();
    for g in flat.graphemes(true) {
        let w = g.width();
        if used + w > width {
            return (out, true);
        }
        used += w;
        out.push_str(g);
    }
    (out, false)
}

/// `text` cut to one line of `width` columns; see [`fit_cut`].
fn fit(text: String, width: usize) -> String {
    fit_cut(&text, width).0
}

/// The columns `text` takes once [`fit_cut`] puts it on one line.
fn flat_width(text: &str) -> usize {
    cells_width(&fit_cut(text, usize::MAX).0)
}
```

Replace the body of `fields_width` with:

```rust
    let joined: Vec<String> = fields.iter().map(Field::text).collect();
    flat_width(&joined.join(" · "))
```

Replace the cutting loop at the end of `field_spans` (from `let mut left = width;` to `out`) with:

```rust
    let mut left = width;
    let mut out = Vec::new();
    for span in spans {
        let (text, cut) = fit_cut(&span.content, left);
        left -= cells_width(&text);
        if !text.is_empty() {
            out.push(Span::styled(text, span.style));
        }
        if cut {
            break;
        }
    }
    out
```

- [ ] **Step 4: Keep the notice queue from replaying old notices**

In `crates/scuttle-tui/src/app.rs`, after `NOTICE_TTL`, add:

```rust
/// How long a notice may wait for its turn in the footer before it is dropped unseen, since it
/// no longer describes the moment.
pub const NOTICE_STALE: Duration = Duration::from_secs(10);
```

In `pub struct Tui`, replace the `unshown_notices` field with:

```rust
    /// Indices into `core.notices` that arrived but were never active, oldest first, each with
    /// the draw that first saw it.
    unshown_notices: VecDeque<(usize, Instant)>,
```

Replace `sync_notice` with:

```rust
    /// Makes the newest notice pushed since the last call active, and expires an old one. Once
    /// no notice is active, the oldest notice that was never shown takes its turn, unless it
    /// waited `NOTICE_STALE`.
    pub fn sync_notice(&mut self, now: Instant) {
        let count = self.core.notices.len();
        if count > self.notices_seen {
            self.unshown_notices
                .extend((self.notices_seen..count).map(|i| (i, now)));
            self.active_notice = self.unshown_notices.pop_back().map(|(i, _)| (i, now));
        }
        self.notices_seen = count;
        if self
            .active_notice
            .is_some_and(|(_, since)| now.duration_since(since) >= NOTICE_TTL)
        {
            self.active_notice = None;
        }
        self.unshown_notices
            .retain(|(_, arrived)| now.duration_since(*arrived) < NOTICE_STALE);
        if self.active_notice.is_none() {
            self.active_notice = self.unshown_notices.pop_front().map(|(i, _)| (i, now));
        }
    }
```

In `fn key`, after `self.active_notice = None;`, add:

```rust
        // The user is acting now, so notices still waiting their turn are about the past.
        self.unshown_notices.clear();
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p scuttle-tui --bin scuttle -- footer::tests a_new_notice a_notice_expires the_newest_notice notices_that_were_never_shown a_key_drops_the_notices a_notice_that_waited`
Expected: PASS, including the existing `fit_never_splits_a_grapheme_cluster` and `notices_that_were_never_shown_take_turns`.

- [ ] **Step 6: Run the full checks**

Run: `cargo fmt --all && cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check`
Expected: all green.

- [ ] **Step 7: Commit**

```bash
git add crates/scuttle-tui/src/footer.rs crates/scuttle-tui/src/app.rs
git commit -m "fix(scuttle-tui): put footer text on one line and stop replaying old notices" \
  -m "A CRLF in a notice or a status became two spaces, a tab and other control characters were measured as nothing, and the width that decided what to drop was measured on different text. A key now clears the notices waiting their turn, and one that waited ten seconds is dropped unseen." \
  -m "Assisted-by: AI"
```

---

### Task 6: The configured footer fields and their drop order

**Files:**
- Modify: `crates/scuttle-tui/src/footer.rs` (imports, `Field`, `fields_width`, `field_spans`, a new `status_text`, `field_for`, `DROP_ORDER`, `drop_candidate`, `status_line`, and `footer_line`)
- Modify: `crates/scuttle-tui/src/app.rs` (imports, a new `Tui::statusline` field, `Tui::new`, and the footer call in `draw_at`)
- Test: `crates/scuttle-tui/src/footer.rs` and `crates/scuttle-tui/src/app.rs` (`mod tests`)

**Interfaces:**
- Consumes: `StatusField`, `StatuslineConfig`, `Thresholds` (Task 1); `usage::{context_percent, spend_percent, quota_percent, crossed, spend_field, quota_field, format_cost_micros}` (Task 2); `App::spend`, `App::quota`, `App::chat_cost` (Task 3); `fit_cut`, `flat_width` (Task 5).
- Produces:
  - `pub fn status_line(app: &App, notice: Option<&Notice>, statusline: &StatuslineConfig, theme: &Theme, width: u16) -> Line<'static>`.
  - `#[cfg(test)] pub fn footer_line(app: &App, notice: Option<&Notice>, theme: &Theme, width: u16) -> Line<'static>`, the same with the default settings, for the existing tests.
  - `Tui::statusline: StatuslineConfig` (private), the footer's settings in effect, which Task 11 changes; `Tui::new` sets `core.cost_in_footer` from it.

- [ ] **Step 1: Write the failing tests**

Add to `mod tests` in `crates/scuttle-tui/src/footer.rs`:

```rust
    use scuttle_core::app::Effect;

    fn listing(fields: Vec<StatusField>, thresholds: Thresholds) -> StatuslineConfig {
        StatuslineConfig { fields, thresholds }
    }

    /// `app` with its spend loaded as `spent` of `limit` micros, through a refresh of its own.
    fn with_spend(app: &mut App, spent: i64, limit: i64) {
        let generation = app
            .update(Msg::RefreshLimits)
            .iter()
            .find_map(|e| match e {
                Effect::FetchSpend { generation } => Some(*generation),
                _ => None,
            })
            .expect("a spend fetch");
        app.update(Msg::SpendLoaded {
            spend: Box::new(
                serde_json::from_value(json!({"current_spend_micros": spent,
                    "effective_budget": {"spend_limit_micros": limit, "limit_source": "group"}}))
                .unwrap(),
            ),
            generation,
        });
    }

    #[test]
    fn the_status_survives_narrow_widths() {
        let app = busy_footer_app();
        let theme = Theme::terminal(true);
        for (width, want) in [
            (38, "/model: Big · /plan-mode: on · running"),
            (24, "/plan-mode: on · running"),
            (7, "running"),
            (4, "runn"),
        ] {
            assert_eq!(text(&footer_line(&app, None, &theme, width)), want, "{width}");
        }
    }

    #[test]
    fn spend_shows_by_default_once_it_loads() {
        let mut app = busy_footer_app();
        assert!(!status_text(&app).contains("spend"), "nothing before it loads");
        with_spend(&mut app, 1_200_000, 50_000_000);
        assert_eq!(
            status_text(&app),
            "/model: Big · 12.0k/200.0k (6%) · /organization: Engineering Platform · /plan-mode: on · spend $1.20/$50.00 · running"
        );
    }

    #[test]
    fn the_listed_fields_show_in_their_order_with_cost_quota_and_queue_on_request() {
        let mut app = busy_footer_app();
        app.update(stream(json!({"type": "queue_update", "queued_messages": [{"id": 1, "content": []}, {"id": 2, "content": []}]})));
        app.chat_cost = Some(CostState::Loaded(
            serde_json::from_value(json!({"total_cost_micros": 420000})).unwrap(),
        ));
        app.update(Msg::RefreshLimits);
        app.update(Msg::QuotaLoaded {
            org: app.org_id.unwrap(),
            quota: serde_json::from_value(json!({"credits_consumed": 3, "budget": 10})).unwrap(),
            generation: 1,
        });
        let cfg = listing(
            vec![
                StatusField::Status,
                StatusField::Queue,
                StatusField::Cost,
                StatusField::Quota,
                StatusField::Model,
            ],
            Thresholds::default(),
        );
        let theme = Theme::terminal(true);
        assert_eq!(
            text(&status_line(&app, None, &cfg, &theme, 120)),
            "running · queue 2 · cost $0.42 · quota 3/10 · /model: Big"
        );
        app.update(Msg::QuotaLoaded {
            org: app.org_id.unwrap(),
            quota: serde_json::from_value(json!({"credits_consumed": 3, "budget": -1})).unwrap(),
            generation: 1,
        });
        assert_eq!(
            text(&status_line(&app, None, &cfg, &theme, 120)),
            "running · queue 2 · cost $0.42 · /model: Big",
            "no quota applies, so the field hides"
        );
    }

    #[test]
    fn a_hidden_field_past_its_threshold_shows_highlighted_and_gives_way_last() {
        let mut app = busy_footer_app();
        let theme = Theme::terminal(true);
        let cfg = listing(
            vec![StatusField::Model, StatusField::Status],
            Thresholds {
                spend: Some(80),
                ..Thresholds::default()
            },
        );
        with_spend(&mut app, 1_000_000, 10_000_000);
        assert_eq!(
            text(&status_line(&app, None, &cfg, &theme, 80)),
            "/model: Big · running",
            "below its threshold, a hidden field stays hidden"
        );
        with_spend(&mut app, 9_000_000, 10_000_000);
        let line = status_line(&app, None, &cfg, &theme, 80);
        assert_eq!(text(&line), "/model: Big · running · spend $9.00/$10.00");
        let spend = line
            .spans
            .iter()
            .find(|s| s.content.starts_with("spend"))
            .unwrap();
        assert_eq!(spend.style, theme.warn.add_modifier(Modifier::BOLD));
        assert_eq!(
            text(&status_line(&app, None, &cfg, &theme, 28)),
            "running · spend $9.00/$10.00",
            "the model gives way before the warning"
        );
        for width in 0..=60u16 {
            let t = text(&status_line(&app, None, &cfg, &theme, width));
            assert!(cells_width(&t) <= width as usize, "{width}: {t:?}");
        }
    }

    #[test]
    fn context_past_its_threshold_is_highlighted_in_place() {
        let app = busy_footer_app();
        let theme = Theme::terminal(true);
        let mut cfg = StatuslineConfig::default();
        cfg.thresholds.context = Some(5);
        let line = status_line(&app, None, &cfg, &theme, 120);
        let context = line
            .spans
            .iter()
            .find(|s| s.content == "12.0k/200.0k (6%)")
            .unwrap();
        assert_eq!(context.style, theme.warn.add_modifier(Modifier::BOLD));
        cfg.thresholds.context = Some(7);
        let line = status_line(&app, None, &cfg, &theme, 120);
        let context = line
            .spans
            .iter()
            .find(|s| s.content == "12.0k/200.0k (6%)")
            .unwrap();
        assert_eq!(context.style, theme.dim, "6% is short of 7%");
    }
```

Add to `mod tests` in `crates/scuttle-tui/src/app.rs`:

```rust
    #[test]
    fn the_footer_shows_the_configured_fields_in_their_order() {
        let config = LocalConfig {
            statusline: StatuslineConfig {
                fields: vec![StatusField::Status, StatusField::Model, StatusField::Cost],
                ..StatuslineConfig::default()
            },
            ..LocalConfig::default()
        };
        let mut t = Tui::new(
            config,
            None,
            Theme::terminal(true),
            Welcome {
                url: "https://x".into(),
                user: String::new(),
                art: vec![],
                show: true,
            },
            0,
        );
        assert!(t.core.cost_in_footer, "the core fetches cost for the footer");
        t.update(Msg::ModelsLoaded(vec![serde_json::from_value(json!({"id": uuid::Uuid::new_v4(), "display_name": "Big", "is_default": true, "enabled": true, "reasoning_efforts": []})).unwrap()]));
        let shown = screen(&mut t, 60, 10);
        assert!(shown.contains("new chat · /model: Big"), "{shown}");
        assert!(!tui().core.cost_in_footer, "the default footer shows no cost");
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p scuttle-tui --bin scuttle -- footer::tests the_footer_shows_the_configured_fields`
Expected: FAIL to compile with "cannot find function `status_line`" and "no field `statusline`".

- [ ] **Step 3: Build the footer from the configured fields**

In `crates/scuttle-tui/src/footer.rs`, replace the module doc comment and the imports with:

```rust
//! The one-line status footer: the fields `[statusline]` lists, in its order, or the given
//! notice. A field a command changes reads as `/command: value`, and a field past its warning
//! threshold is highlighted, even when the list leaves it out.

use coder_sdk::ChatStatus;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use scuttle_core::app::{App, Connection, CostState, Notice};
use scuttle_core::config::{StatusField, StatuslineConfig, Thresholds};
use scuttle_core::transcript::RetryInfo;
use scuttle_core::usage::{self, context_usage, format_tokens};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::theme::Theme;
use crate::wrap::cells_width;
```

Replace everything from `/// One footer field` through the end of `pub fn footer_line` (keeping `fit_cut`, `fit`, `flat_width`, and `retry_status` above it) with:

```rust
/// One footer field: the slash command that changes it, if any, its value, whether the value
/// is a warning, and whether the field reached its threshold.
struct Field {
    command: Option<&'static str>,
    value: String,
    warn: bool,
    alarm: bool,
}

impl Field {
    fn command(command: &'static str, value: String) -> Field {
        Field {
            command: Some(command),
            value,
            warn: false,
            alarm: false,
        }
    }

    /// A command field whose value is a warning, such as a model the chat can no longer use.
    fn warning(command: &'static str, value: String) -> Field {
        Field {
            warn: true,
            ..Field::command(command, value)
        }
    }

    fn plain(value: String) -> Field {
        Field {
            command: None,
            value,
            warn: false,
            alarm: false,
        }
    }

    /// The field, highlighted when `crossed` says it reached its threshold.
    fn alarm(mut self, crossed: bool) -> Field {
        self.alarm = crossed;
        self
    }

    /// The field as it reads, such as `/effort: high`.
    fn text(&self) -> String {
        match self.command {
            Some(command) => format!("{command}: {}", self.value),
            None => self.value.clone(),
        }
    }
}

/// The connection, a provider retry, or the chat's status.
fn connection_status(app: &App) -> String {
    match app.connection {
        Connection::Reconnecting { attempt } => match app.last_stream_error.as_deref() {
            Some(error) => format!("reconnecting (attempt {attempt}): {error}"),
            None => format!("reconnecting (attempt {attempt})"),
        },
        Connection::Connecting => "connecting".into(),
        Connection::Idle => "new chat".into(),
        Connection::Live => match (&app.transcript.retry, &app.transcript.status) {
            (Some(retry), _) => retry_status(retry),
            (None, Some(ChatStatus::RequiresAction)) => "action required".into(),
            (None, Some(s)) => s.as_str().replace('_', " "),
            (None, None) => "ready".into(),
        },
    }
}

/// Field `name` as `app` has it now, or `None` while it has nothing to show. A field at or
/// past its threshold in `thresholds` is marked as an alarm.
fn field_for(app: &App, name: StatusField, thresholds: &Thresholds) -> Option<Field> {
    match name {
        // A model the chat can no longer use says so until another is picked.
        StatusField::Model => match app.unavailable_model() {
            Some(gone) => Some(Field::warning(
                "/model",
                match gone.name {
                    Some(name) => format!("{name} (unavailable)"),
                    None => "unavailable".to_owned(),
                },
            )),
            None => app.model_name().map(|name| Field::command("/model", name)),
        },
        StatusField::Effort => app.effort().map(|effort| Field::command("/effort", effort)),
        StatusField::Context => context_usage(app.transcript.messages()).map(|u| {
            let percent = usage::context_percent(&u);
            let text = match (u.limit, percent) {
                (Some(limit), Some(p)) => {
                    format!("{}/{} ({p}%)", format_tokens(u.used), format_tokens(limit))
                }
                _ => format_tokens(u.used),
            };
            Field::plain(text).alarm(usage::crossed(percent, thresholds.context))
        }),
        StatusField::Workspace => app
            .workspace_name()
            .map(|name| Field::command("/workspace", name)),
        StatusField::Organization => (app.organizations.len() > 1)
            .then(|| Field::command("/organization", app.org_label(app.current_org()))),
        StatusField::PlanMode => app
            .plan_mode
            .then(|| Field::command("/plan-mode", "on".into())),
        StatusField::Status => Some(Field::plain(connection_status(app))),
        StatusField::Cost => match app.chat_cost.as_ref() {
            Some(CostState::Loaded(cost)) => Some(Field::plain(format!(
                "cost {}",
                usage::format_cost_micros(cost.total_cost_micros.unwrap_or(0))
            ))),
            _ => None,
        },
        StatusField::Spend => app.spend().loaded().map(|spend| {
            Field::plain(usage::spend_field(spend))
                .alarm(usage::crossed(usage::spend_percent(spend), thresholds.spend))
        }),
        StatusField::Quota => app.quota().loaded().and_then(|quota| {
            let text = usage::quota_field(quota)?;
            Some(
                Field::plain(text)
                    .alarm(usage::crossed(usage::quota_percent(quota), thresholds.quota)),
            )
        }),
        StatusField::Queue => {
            let queued = app.transcript.queued.len();
            (queued > 0).then(|| Field::plain(format!("queue {queued}")))
        }
    }
}

/// The order whole fields give way when the line is too long, first to last. The status is
/// never dropped, only cut once nothing else is left.
const DROP_ORDER: [StatusField; 10] = [
    StatusField::Workspace,
    StatusField::Organization,
    StatusField::Queue,
    StatusField::Cost,
    StatusField::Quota,
    StatusField::Effort,
    StatusField::Spend,
    StatusField::Context,
    StatusField::Model,
    StatusField::PlanMode,
];

/// The field to drop next: the first in `DROP_ORDER`, taking a field past its threshold only
/// after every field that is not, and never the status.
fn drop_candidate(fields: &[(StatusField, Field)]) -> Option<usize> {
    fields
        .iter()
        .enumerate()
        .filter(|(_, (name, _))| *name != StatusField::Status)
        .min_by_key(|(_, (name, field))| (field.alarm, DROP_ORDER.iter().position(|d| d == name)))
        .map(|(i, _)| i)
}

/// The columns `fields` take, joined with ` · `.
fn fields_width(fields: &[(StatusField, Field)]) -> usize {
    let joined: Vec<String> = fields.iter().map(|(_, f)| f.text()).collect();
    flat_width(&joined.join(" · "))
}

/// `fields` as spans cut to `width` columns: each command in the brand accent, its value dim
/// or, for a warning, in the warning color, the separators and the plain fields dim, and a
/// field past its threshold in the warning color, bold.
fn field_spans(fields: &[(StatusField, Field)], theme: &Theme, width: usize) -> Vec<Span<'static>> {
    let alarm = theme.warn.add_modifier(Modifier::BOLD);
    let mut spans = Vec::new();
    for (i, (_, field)) in fields.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" · ", theme.dim));
        }
        match field.command {
            Some(command) => {
                spans.push(Span::styled(
                    command,
                    if field.alarm { alarm } else { theme.brand },
                ));
                let style = match (field.alarm, field.warn) {
                    (true, _) => alarm,
                    (false, true) => theme.warn,
                    (false, false) => theme.dim,
                };
                spans.push(Span::styled(format!(": {}", field.value), style));
            }
            None => spans.push(Span::styled(
                field.value.clone(),
                if field.alarm { alarm } else { theme.dim },
            )),
        }
    }
    let mut left = width;
    let mut out = Vec::new();
    for span in spans {
        let (text, cut) = fit_cut(&span.content, left);
        left -= cells_width(&text);
        if !text.is_empty() {
            out.push(Span::styled(text, span.style));
        }
        if cut {
            break;
        }
    }
    out
}

/// Renders the footer: `notice` in place of the fields when it is `Some`, else the fields
/// `statusline` lists, in its order, then any field it leaves out that reached its threshold.
/// The caller decides which notice (if any) is active, since `App::notices` only grows.
pub fn status_line(
    app: &App,
    notice: Option<&Notice>,
    statusline: &StatuslineConfig,
    theme: &Theme,
    width: u16,
) -> Line<'static> {
    let width = width as usize;
    if let Some(notice) = notice {
        let (text, style) = match notice {
            Notice::Info(t) => (t.clone(), theme.dim),
            Notice::Error(t) => (t.clone(), theme.error),
        };
        return Line::from(Span::styled(fit(text, width), style));
    }
    let thresholds = &statusline.thresholds;
    let mut fields: Vec<(StatusField, Field)> = statusline
        .fields
        .iter()
        .filter_map(|&name| field_for(app, name, thresholds).map(|f| (name, f)))
        .collect();
    fields.extend(
        StatusField::ALL
            .into_iter()
            .filter(|name| !statusline.fields.contains(name))
            .filter_map(|name| {
                field_for(app, name, thresholds)
                    .filter(|f| f.alarm)
                    .map(|f| (name, f))
            }),
    );
    while fields_width(&fields) > width {
        let Some(i) = drop_candidate(&fields) else {
            break;
        };
        fields.remove(i);
    }
    Line::from(field_spans(&fields, theme, width))
}

/// [`status_line`] with the default settings, as the footer was before `/statusline`.
#[cfg(test)]
pub fn footer_line(app: &App, notice: Option<&Notice>, theme: &Theme, width: u16) -> Line<'static> {
    status_line(app, notice, &StatuslineConfig::default(), theme, width)
}
```

`UnicodeWidthStr` stays imported for `g.width()` in `fit_cut` and for the existing tests.

- [ ] **Step 4: Give the TUI its footer settings**

In `crates/scuttle-tui/src/app.rs`, replace `use crate::footer::footer_line;` with `use crate::footer::status_line;`, and replace `use scuttle_core::config::{self, LocalConfig};` with `use scuttle_core::config::{self, LocalConfig, StatusField, StatuslineConfig};`.

In `pub struct Tui`, after `config_path: Option<PathBuf>,`, add:

```rust
    /// The footer's fields and warnings in effect: what `config.toml` holds, or a change
    /// `/statusline` made that could not be saved.
    statusline: StatuslineConfig,
```

In `Tui::new`, after `core.saved_efforts = config.efforts.clone();`, add:

```rust
        core.cost_in_footer = config.statusline.fields.contains(&StatusField::Cost);
        let statusline = config.statusline.clone();
```

and in the `Tui { .. }` literal, after `config_path,`, add `statusline,`.

In `draw_at`, replace the footer's `Paragraph::new(footer_line(...))` with:

```rust
            Paragraph::new(status_line(
                &self.core,
                self.active_notice(),
                &self.statusline,
                &self.theme,
                footer.width,
            )),
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p scuttle-tui --bin scuttle -- footer::tests the_footer_shows_the_configured_fields`
Expected: PASS, including every M2.5 footer test (`the_attached_workspace_shows_before_the_organization`, `the_workspace_gives_way_before_the_organization`, `commands_are_in_the_brand_accent_and_values_are_dim`, and `the_organization_gives_way_to_the_status_when_space_is_short`) with their strings unchanged.

- [ ] **Step 6: Run the full checks**

Run: `cargo fmt --all && cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check`
Expected: all green, with no insta snapshot changed.

- [ ] **Step 7: Commit**

```bash
git add crates/scuttle-tui/src/footer.rs crates/scuttle-tui/src/app.rs
git commit -m "feat(scuttle-tui): draw the footer from the status line settings" \
  -m "The footer shows the listed fields in their order, adds spend, cost, quota, and queue, and highlights a field past its threshold, joining the end of the line when the list leaves it out. When the line is too long, whole fields give way in a fixed order, a warned field last and the status never, so the connection status no longer goes before plan mode." \
  -m "Assisted-by: AI"
```

---

### Task 7: Refresh the limits every minute and after each turn

**Files:**
- Modify: `crates/scuttle-tui/src/app.rs` (a new `LIMITS_EVERY`, `Tui` fields `limits_due` and `cost_due`, `Tui::new`, `Tui::update`, and new `usage_deadline` and `poll_usage`)
- Modify: `crates/scuttle-tui/src/main.rs` (the deadline and the timer arm of the main loop)
- Test: `crates/scuttle-tui/src/app.rs` (`mod tests`)

**Interfaces:**
- Consumes: `Msg::RefreshLimits`, `Msg::RefreshCost`, `App::turns_ended`, `App::current_org` (Task 3).
- Produces:
  - `pub const LIMITS_EVERY: Duration` (60 seconds).
  - `Tui::usage_deadline(&self) -> Instant` and `Tui::poll_usage(&mut self, now: Instant) -> Vec<Effect>`, which the main loop calls on every timer wakeup.
  - `Tui::cost_due: bool` (private), which Task 11 sets when the footer starts showing cost.

- [ ] **Step 1: Write the failing tests**

Add to `mod tests` in `crates/scuttle-tui/src/app.rs`:

```rust
    fn fetches_spend(effects: &[Effect]) -> bool {
        effects
            .iter()
            .any(|e| matches!(e, Effect::FetchSpend { .. }))
    }

    #[test]
    fn limits_refresh_at_once_then_every_minute() {
        let mut t = info_tui();
        let start = t.epoch;
        assert!(t.usage_deadline() <= start, "the first refresh is due at startup");
        let effects = t.poll_usage(start);
        assert!(
            effects.contains(&Effect::FetchSpend { generation: 1 }),
            "{effects:?}"
        );
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, Effect::FetchQuota { generation: 1, .. })),
            "{effects:?}"
        );
        assert_eq!(t.usage_deadline(), start + LIMITS_EVERY);
        assert!(
            t.poll_usage(start + LIMITS_EVERY - Duration::from_secs(1))
                .is_empty(),
            "not due yet"
        );
        let effects = t.poll_usage(start + LIMITS_EVERY);
        assert!(
            effects.contains(&Effect::FetchSpend { generation: 2 }),
            "{effects:?}"
        );
    }

    #[test]
    fn a_finished_turn_refreshes_the_limits_at_the_next_wakeup() {
        let mut t = info_tui();
        let start = t.epoch;
        t.poll_usage(start);
        t.update(status("running"));
        assert_eq!(
            t.usage_deadline(),
            start + LIMITS_EVERY,
            "a turn in progress changes nothing"
        );
        t.update(status("waiting"));
        let later = start + Duration::from_secs(10);
        assert!(t.usage_deadline() <= later, "the turn ended");
        assert!(fetches_spend(&t.poll_usage(later)));
        assert_eq!(t.usage_deadline(), later + LIMITS_EVERY);
    }

    #[test]
    fn choosing_another_organization_refreshes_its_quota_at_the_next_wakeup() {
        let mut t = tui();
        let org = |name: &str| scuttle_core::app::OrgRef {
            id: uuid::Uuid::new_v4(),
            name: name.into(),
            display_name: name.into(),
            is_default: false,
            can_create_chats: true,
        };
        let (coder, product) = (org("coder"), org("product"));
        t.update(Msg::OrganizationsLoaded(vec![coder.clone(), product.clone()]));
        t.update(Msg::Started {
            org_id: coder.id,
            open_chat: None,
        });
        let start = t.epoch;
        t.poll_usage(start);
        t.update(Msg::OrganizationChosen(product.id));
        let later = start + Duration::from_secs(5);
        assert!(t.usage_deadline() <= later);
        let effects = t.poll_usage(later);
        assert!(
            effects.contains(&Effect::FetchQuota {
                org: product.id,
                generation: 2
            }),
            "{effects:?}"
        );
    }

    #[test]
    fn the_chat_cost_is_fetched_after_a_turn_only_while_the_footer_shows_it() {
        let mut t = info_tui();
        let start = t.epoch;
        t.poll_usage(start);
        t.update(status("running"));
        t.update(status("waiting"));
        let effects = t.poll_usage(start);
        assert!(fetches_spend(&effects));
        assert!(
            !effects
                .iter()
                .any(|e| matches!(e, Effect::FetchCost { .. })),
            "the default footer shows no cost: {effects:?}"
        );
        t.core.cost_in_footer = true;
        t.update(status("running"));
        t.update(status("waiting"));
        let effects = t.poll_usage(start);
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, Effect::FetchCost { .. })),
            "{effects:?}"
        );
    }
```

`status` and `info_tui` already exist in this test module; `info_tui` starts the core and loads a chat in its organization.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p scuttle-tui --bin scuttle -- limits_refresh_at_once a_finished_turn_refreshes choosing_another_organization_refreshes the_chat_cost_is_fetched_after`
Expected: FAIL to compile with "no method named `poll_usage`".

- [ ] **Step 3: Write the implementation**

In `crates/scuttle-tui/src/app.rs`, after `COPY_TTL`, add:

```rust
/// How often the AI spend and the workspace quota refresh while scuttle runs, as the web UI's
/// quota does.
pub const LIMITS_EVERY: Duration = Duration::from_secs(60);
```

In `pub struct Tui`, after `epoch_unix: i64,`, add:

```rust
    /// When the AI spend and the workspace quota refresh next. A turn that ends or a change of
    /// organization sets it to `epoch`, which has passed, so the next wakeup refreshes them.
    limits_due: Instant,
    /// Set when the open chat's cost should be asked for at the next wakeup: after a turn, a
    /// chat switch, or the footer starting to show cost. The core decides whether to ask.
    cost_due: bool,
```

In `Tui::new`, before `Tui {`, add `let epoch = Instant::now();`; in the literal, replace `epoch: Instant::now(),` with `epoch,`, and after `epoch_unix: now_unix(),` add:

```rust
            limits_due: epoch,
            cost_due: false,
```

In `Tui::update`, after `let open = self.core.chat_id;`, add:

```rust
        let turns = self.core.turns_ended();
        let org = self.core.current_org();
```

and just before the final `effects` of `Tui::update`, add:

```rust
        // A finished turn spent money and may have used credits, and the quota belongs to an
        // organization, so the limits refresh at the next wakeup instead of waiting out the
        // minute.
        if self.core.turns_ended() != turns {
            self.limits_due = self.epoch;
            self.cost_due = true;
        }
        if self.core.current_org() != org {
            self.limits_due = self.epoch;
        }
        if self.core.chat_id != open {
            self.cost_due = true;
        }
```

After `pub fn tick`, add:

```rust
    /// When the next refresh of the limits or the chat's cost is due, for the main loop's
    /// deadline.
    pub fn usage_deadline(&self) -> Instant {
        if self.cost_due {
            self.epoch
        } else {
            self.limits_due
        }
    }

    /// Runs the refreshes due at `now`. The main loop calls this from its timer wakeup, which
    /// cannot fire while a handoff has the terminal, since the loop runs each handoff to the
    /// end before it waits again.
    pub fn poll_usage(&mut self, now: Instant) -> Vec<Effect> {
        let mut effects = Vec::new();
        if now >= self.limits_due {
            self.limits_due = now + LIMITS_EVERY;
            effects.extend(self.update(Msg::RefreshLimits));
        }
        if std::mem::take(&mut self.cost_due) {
            effects.extend(self.update(Msg::RefreshCost));
        }
        effects
    }
```

In `crates/scuttle-tui/src/main.rs`, replace

```rust
        let deadline = earliest(
            tui.notice_deadline(),
            tui.animation_deadline(std::time::Instant::now()),
        );
```

with

```rust
        let deadline = earliest(
            earliest(
                tui.notice_deadline(),
                tui.animation_deadline(std::time::Instant::now()),
            ),
            Some(tui.usage_deadline()),
        );
```

and replace the timer arm

```rust
                () = sleep_until(deadline) => {
                    tui.tick();
                    continue;
                }
```

with

```rust
                () = sleep_until(deadline) => {
                    tui.tick();
                    // Handoffs run to the end above, so a refresh never starts during one;
                    // one that came due meanwhile runs now.
                    pending = tui.poll_usage(std::time::Instant::now());
                    continue;
                }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p scuttle-tui --bin scuttle -- limits_refresh_at_once a_finished_turn_refreshes choosing_another_organization_refreshes the_chat_cost_is_fetched_after earliest_picks`
Expected: PASS.

- [ ] **Step 5: Run the full checks**

Run: `cargo fmt --all && cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check`
Expected: all green, including the pty tests, whose fake server answers the new spend and quota requests with wiremock's default `404`, which hides both.

- [ ] **Step 6: Commit**

```bash
git add crates/scuttle-tui/src/app.rs crates/scuttle-tui/src/main.rs
git commit -m "feat(scuttle-tui): refresh the spend and quota every minute and after each turn" \
  -m "The TUI keeps the minute and joins it to the main loop's deadline, so no refresh starts while the pager or the editor has the terminal. A turn that ends or a change of organization makes the next one due at once, and the chat's cost is asked for after a turn or a chat switch while the footer shows it." \
  -m "Assisted-by: AI"
```

---

### Task 8: Exit cleanly on SIGINT, SIGTERM, and SIGHUP

**Files:**
- Modify: `Cargo.toml` (tokio features)
- Modify: `crates/scuttle-tui/src/terminal.rs` (a new `Shutdown`, and `handoff_signals` with its doc comments)
- Modify: `crates/scuttle-tui/src/main.rs` (install `Shutdown`, a signal arm in the main loop, and a renewal after each handoff)
- Test: `crates/scuttle-tui/tests/pty.rs`

**Interfaces:**
- Consumes: `terminal::finish` (existing), which every exit from the main loop already runs.
- Produces: `pub struct terminal::Shutdown` with `install() -> std::io::Result<Shutdown>` and `async fn recv(&mut self) -> u8` (the exit code: 130, 143, or 129); a SIGINT during a handoff is ignored through the handoff flag, so there is no `forget_interrupts`.

- [ ] **Step 1: Write the failing test**

Add to `crates/scuttle-tui/tests/pty.rs`:

```rust
#[tokio::test(flavor = "multi_thread")]
async fn a_signal_outside_a_handoff_restores_the_terminal_and_exits() {
    for (signal, code) in [("TERM", 143), ("HUP", 129), ("INT", 130)] {
        let server = fake_coder().await;
        let mut s = spawn(
            &format!("signal-{signal}"),
            &[
                ("CODER_URL", server.uri()),
                ("CODER_SESSION_TOKEN", "test-token-not-real".into()),
            ],
        );
        s.wait_for("Signed in as nick");
        let pid = s.child.process_id().expect("scuttle has a process id");
        let sent = std::process::Command::new("kill")
            .arg(format!("-{signal}"))
            .arg(pid.to_string())
            .status()
            .unwrap();
        assert!(sent.success(), "kill -{signal}");
        assert_eq!(s.exit_code(), code, "SIG{signal} exits with 128 plus its number");
        let raw = String::from_utf8_lossy(&s.raw()).to_string();
        let tail = &raw[raw
            .rfind("\x1b[?1049h")
            .expect("scuttle entered the alternate screen")..];
        assert!(tail.contains("\x1b[?1049l"), "SIG{signal}: alternate screen left");
        assert!(
            tail.contains("\x1b[?1000l") || tail.contains("\x1b[?1006l"),
            "SIG{signal}: mouse capture off"
        );
        assert!(tail.contains("\x1b[?2004l"), "SIG{signal}: bracketed paste off");
        assert!(tail.contains("\x1b[?1007h"), "SIG{signal}: alternate scroll mode back");
        assert!(tail.contains("\x1b[23;0t"), "SIG{signal}: the saved title is back");
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p scuttle-tui --test pty a_signal_outside_a_handoff`
Expected: FAIL: SIGTERM ends scuttle with the default action, so `exit_code` reports the signal instead of 143, or the tail lacks `\x1b[?1049l`.

- [ ] **Step 3: Catch the signals**

In the root `Cargo.toml`, add `"signal"` to the workspace `tokio` features:

```toml
tokio = { version = "1", features = ["macros", "rt-multi-thread", "sync", "time", "process", "signal"] }
```

Run: `cargo build && git diff --stat Cargo.lock`
Expected: the build succeeds and `Cargo.lock` is unchanged; if it shows a change, it must add no `[[package]]`.

In `crates/scuttle-tui/src/terminal.rs`, replace the doc comment of `pub struct HandoffSignals(());` with:

```rust
/// Keeps scuttle alive through Ctrl+\ while a program it handed the terminal to runs, as
/// `system(3)` does. Leaving raw mode turns the terminal's signal keys back on, and they
/// signal the whole foreground process group, scuttle included. Dropping it restores the
/// default action, so Ctrl+\ outside a handoff, which raw mode delivers as a key, is unchanged.
/// Ctrl+C sends SIGINT, which [`Shutdown`] catches for the whole run.
///
/// The signal is caught, not ignored: a caught signal resets to its default in the child at
/// exec, while an ignored one would stay ignored in the pager or editor.
```

In `handoff_signals`, replace `for signal in [signal_hook::consts::SIGINT, signal_hook::consts::SIGQUIT] {` with `for signal in [signal_hook::consts::SIGQUIT] {`.

After `impl Drop for HandoffSignals`, add:

```rust
/// The signals that end scuttle: SIGINT, SIGTERM, and SIGHUP, caught for the whole run, so
/// the main loop leaves through `finish` and the terminal gets every mode and the window
/// title back. While a handoff runs, the loop is busy with it, so a signal waits until the
/// pager or editor exits.
#[cfg(unix)]
pub struct Shutdown {
    interrupt: tokio::signal::unix::Signal,
    terminate: tokio::signal::unix::Signal,
    hangup: tokio::signal::unix::Signal,
}

#[cfg(unix)]
impl Shutdown {
    /// Starts catching the signals. It must run inside the tokio runtime.
    pub fn install() -> std::io::Result<Shutdown> {
        use tokio::signal::unix::{SignalKind, signal};
        Ok(Shutdown {
            interrupt: signal(SignalKind::interrupt())?,
            terminate: signal(SignalKind::terminate())?,
            hangup: signal(SignalKind::hangup())?,
        })
    }

    /// Waits for the next signal and returns the exit code it asks for: 128 plus its number,
    /// as a shell reports a process the signal ended.
    pub async fn recv(&mut self) -> u8 {
        tokio::select! {
            _ = self.interrupt.recv() => 130,
            _ = self.terminate.recv() => 143,
            _ = self.hangup.recv() => 129,
        }
    }

    /// Forgets a SIGINT that arrived during a handoff: Ctrl+C there was for the pager or the
    /// editor, which got it too. A new stream sees only signals from now on.
    pub fn forget_interrupts(&mut self) {
        if let Ok(fresh) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
        {
            self.interrupt = fresh;
        }
    }
}

/// Elsewhere scuttle has no signals to catch, so `recv` never returns.
#[cfg(not(unix))]
pub struct Shutdown;

#[cfg(not(unix))]
impl Shutdown {
    pub fn install() -> std::io::Result<Shutdown> {
        Ok(Shutdown)
    }

    pub async fn recv(&mut self) -> u8 {
        std::future::pending().await
    }

    pub fn forget_interrupts(&mut self) {}
}
```

- [ ] **Step 4: Leave the main loop on a signal**

In `crates/scuttle-tui/src/main.rs`, just before `let mut term = match terminal::enter(local.mouse) {`, add:

```rust
    // Installed before the terminal changes, so a signal from here on leaves through
    // `terminal::finish`, which restores it.
    let mut shutdown = match terminal::Shutdown::install() {
        Ok(shutdown) => shutdown,
        Err(e) => {
            eprintln!("scuttle: could not watch for signals: {e}");
            return ExitCode::FAILURE;
        }
    };
```

After each of the three `input.resume();` lines (the editor request, `Effect::Page`, and `Effect::EditSettings`), add:

```rust
            shutdown.forget_interrupts();
```

indented to match.
In the `tokio::select!` of the main loop, after the `Some(msg) = rx.recv() => { .. }` arm, add:

```rust
                code = shutdown.recv() => break ExitCode::from(code),
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p scuttle-tui --test pty -- a_signal_outside_a_handoff ctrl_c_in_the_pager_leaves_scuttle_running exit_restores_terminal_modes`
Expected: PASS; `ctrl_c_in_the_pager_leaves_scuttle_running` now passes because the SIGINT during the pager is forgotten, where before the conditional default handler skipped it.

- [ ] **Step 6: Run the full checks**

Run: `cargo fmt --all && cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check`
Expected: all green.

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml crates/scuttle-tui/src/terminal.rs crates/scuttle-tui/src/main.rs crates/scuttle-tui/tests/pty.rs
git commit -m "fix(scuttle-tui): restore the terminal when a signal ends scuttle" \
  -m "SIGINT, SIGTERM, and SIGHUP outside a pager or editor handoff ended scuttle without restoring raw mode, the alternate screen, the mouse, mode 1007, or the window title. They are now caught for the whole run, and the main loop leaves through the usual restore with 128 plus the signal number. A SIGINT during a handoff belongs to the child and is forgotten." \
  -m "Assisted-by: AI"
```

---

### Task 9: Bound the wait for background work on exit

**Files:**
- Modify: `crates/scuttle-tui/src/main.rs` (`main` becomes `run`, with a new `main`, `run_bounded`, and `SHUTDOWN_GRACE`)
- Test: `crates/scuttle-tui/src/main.rs` (`mod tests`)

**Interfaces:**
- Consumes: nothing new.
- Produces: `fn run_bounded(runtime: tokio::runtime::Runtime, app: impl std::future::Future<Output = ExitCode>, grace: std::time::Duration) -> ExitCode` and `const SHUTDOWN_GRACE: std::time::Duration` (500 ms), both private to `main.rs`; `async fn run() -> ExitCode` holds what `main` did.

- [ ] **Step 1: Write the failing test**

Add to `mod tests` in `crates/scuttle-tui/src/main.rs`:

```rust
    #[test]
    fn exiting_leaves_a_stuck_blocking_read_behind() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        // Nothing is sent until the runtime is gone, like a read on a stalled network mount.
        let (release, stuck) = std::sync::mpsc::channel::<()>();
        let (started_tx, started) = std::sync::mpsc::channel::<()>();
        let began = std::time::Instant::now();
        let code = run_bounded(
            runtime,
            async move {
                tokio::task::spawn_blocking(move || {
                    let _ = started_tx.send(());
                    let _ = stuck.recv();
                });
                started.recv().expect("the blocking read started");
                ExitCode::SUCCESS
            },
            std::time::Duration::from_millis(100),
        );
        assert_eq!(code, ExitCode::SUCCESS);
        assert!(
            began.elapsed() < std::time::Duration::from_secs(5),
            "exit waited {:?} for the stuck read",
            began.elapsed()
        );
        drop(release);
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p scuttle-tui --bin scuttle -- exiting_leaves_a_stuck_blocking_read_behind`
Expected: FAIL to compile with "cannot find function `run_bounded`".

- [ ] **Step 3: Write the implementation**

In `crates/scuttle-tui/src/main.rs`, replace

```rust
#[tokio::main]
async fn main() -> ExitCode {
```

with

```rust
/// How long exiting waits for work still running in the background, such as an attachment
/// read stuck on a stalled network mount, before leaving it behind.
const SHUTDOWN_GRACE: std::time::Duration = std::time::Duration::from_millis(500);

/// Runs `app` on `runtime`, then shuts the runtime down, waiting at most `grace` for its
/// tasks. Dropping a runtime instead waits for every `spawn_blocking` task, however long it
/// takes, so quitting could hang on a read that never returns.
fn run_bounded(
    runtime: tokio::runtime::Runtime,
    app: impl std::future::Future<Output = ExitCode>,
    grace: std::time::Duration,
) -> ExitCode {
    let code = runtime.block_on(app);
    runtime.shutdown_timeout(grace);
    code
}

fn main() -> ExitCode {
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => {
            eprintln!("scuttle: could not start: {e}");
            return ExitCode::FAILURE;
        }
    };
    run_bounded(runtime, run(), SHUTDOWN_GRACE)
}

/// scuttle, from reading the arguments to restoring the terminal; `main` runs it on a runtime
/// whose shutdown is bounded.
async fn run() -> ExitCode {
```

The body that follows is the old `main`'s, unchanged.
If `ExitCode` has no `PartialEq` in the pinned toolchain, compare `format!("{code:?}")` with `format!("{:?}", ExitCode::SUCCESS)` in the test instead.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p scuttle-tui --bin scuttle -- exiting_leaves_a_stuck_blocking_read_behind`
Expected: PASS within a second.

Run: `cargo test -p scuttle-tui --test pty`
Expected: PASS; every pty test still exits with its expected code.

- [ ] **Step 5: Run the full checks**

Run: `cargo fmt --all && cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check`
Expected: all green.

- [ ] **Step 6: Commit**

```bash
git add crates/scuttle-tui/src/main.rs
git commit -m "fix(scuttle-tui): never let quitting hang on a stuck file read" \
  -m "The runtime that #[tokio::main] dropped at exit waited for every spawn_blocking task, so an attachment read on a stalled network mount kept scuttle from exiting. main now builds the runtime and shuts it down with a 500 ms bound." \
  -m "Assisted-by: AI"
```

---

### Task 10: Depend on `m3-sdk` and check the version through the runtime

**Files:**
- Modify: `Cargo.toml` (the `coder-sdk` line) and `Cargo.lock`
- Modify: `crates/scuttle-tui/src/runtime.rs` (a new `Runtime::server_version`)
- Modify: `crates/scuttle-tui/src/main.rs` (the startup `tokio::join!`)
- Test: `crates/scuttle-tui/src/runtime.rs` (`mod tests`)

**Interfaces:**
- Consumes: the Task S1 commit on `m3-sdk`; `coder_sdk::Client::server_version` (redacted since S1).
- Produces: `Runtime::server_version(&self) -> Result<String, coder_sdk::Error>`.

- [ ] **Step 1: Write the failing test**

Add to `mod tests` in `crates/scuttle-tui/src/runtime.rs`:

```rust
    #[tokio::test]
    async fn a_refused_version_check_never_shows_the_token() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/buildinfo"))
            .respond_with(api_error(500, &format!("bad token {TOKEN}")))
            .mount(&server)
            .await;
        let (rt, _rx) = runtime(&server.uri());
        let shown = rt.server_version().await.unwrap_err().to_string();
        assert!(!shown.contains(TOKEN), "{shown}");
        assert!(shown.contains("[redacted]"), "{shown}");
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p scuttle-tui --bin scuttle -- a_refused_version_check_never_shows_the_token`
Expected: FAIL to compile with "no method named `server_version` found for struct `Runtime`".

- [ ] **Step 3: Add the method**

In `impl Runtime`, after `pub async fn organizations`, add:

```rust
    /// The server's version, for the skew warning at startup.
    pub async fn server_version(&self) -> Result<String, coder_sdk::Error> {
        self.client.server_version().await
    }
```

Run: `cargo test -p scuttle-tui --bin scuttle -- a_refused_version_check_never_shows_the_token`
Expected: FAIL: the message reads `bad token test-token-not-real-5f3a`, because `m2-sdk` does not redact it.

- [ ] **Step 4: Repin to `m3-sdk`**

Run: `SDK=<unofficial-coder-sdk-rs checkout>; git -C $SDK log --oneline m3-sdk -1 --grep "redact the session token from server_version errors"`
Expected: one line, which proves Task S1 is on `m3-sdk`.

In the root `Cargo.toml`, replace the `coder-sdk` line with:

```toml
coder-sdk = { git = "file://<unofficial-coder-sdk-rs checkout>", branch = "m3-sdk" }
```

Run: `cargo update -p coder-sdk && grep -c 'branch=m3-sdk#' Cargo.lock`
Expected: `2` (`coder-sdk` and `coder-api-gen`), each pinned to the S1 commit's full SHA.

- [ ] **Step 5: Run the test to verify it passes**

Run: `cargo test -p scuttle-tui --bin scuttle -- a_refused_version_check_never_shows_the_token`
Expected: PASS.

- [ ] **Step 6: Check the version through the runtime**

In `crates/scuttle-tui/src/main.rs`, replace

```rust
    let mut runtime = runtime::Runtime::new(client.clone(), tx);
```

with

```rust
    let mut runtime = runtime::Runtime::new(client, tx);
```

and replace

```rust
    let (version, organizations) = tokio::join!(client.server_version(), runtime.organizations());
```

with

```rust
    let (version, organizations) = tokio::join!(runtime.server_version(), runtime.organizations());
```

- [ ] **Step 7: Run the full checks**

Run: `cargo fmt --all && cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check`
Expected: all green, including `a_rejected_token_exits_before_the_full_screen_ui` in the pty tests.

- [ ] **Step 8: Commit**

```bash
git add Cargo.toml Cargo.lock crates/scuttle-tui/src/runtime.rs crates/scuttle-tui/src/main.rs
git commit -m "fix: redact the session token from the startup version check" \
  -m "scuttle now depends on the local m3-sdk branch of coder-sdk, whose server_version redacts the token from a refusal, and the startup check goes through the runtime like every other request." \
  -m "Assisted-by: AI"
```

---

### Task 11: The `/statusline` editor

**Files:**
- Modify: `crates/scuttle-core/src/commands.rs` (`Command`, `COMMANDS`, `parse`, and the command list test)
- Modify: `crates/scuttle-core/src/app.rs` (`Effect`, `App::command`)
- Modify: `crates/scuttle-tui/src/overlay.rs` (imports, `OverlayOutcome`, `Overlay`, a new `StatuslineState` and `statusline_view`, and `Overlay`'s methods)
- Modify: `crates/scuttle-tui/src/app.rs` (`apply_ui_effect`, `overlay_key`, `apply_settings`, `draw_at`'s overlay height, and new `set_statusline` and `apply_statusline`)
- Modify: `crates/scuttle-tui/src/help.rs` (`KEYS`)
- Modify: `crates/scuttle-tui/src/runtime.rs` (the ignored effects)
- Test: `crates/scuttle-core/src/commands.rs`, `crates/scuttle-core/src/app.rs`, `crates/scuttle-tui/src/overlay.rs`, `crates/scuttle-tui/src/app.rs`, `crates/scuttle-tui/src/help.rs` (`mod tests`)

**Interfaces:**
- Consumes: `StatusField`, `Thresholds`, `StatuslineConfig`, `FieldList`, `config::set_statusline` (Task 1); `Tui::statusline` (Task 6); `Tui::cost_due` (Task 7); `TableState`, `TableView`, `Row::item`, `RowKey::Action`, `TableKey` (existing, `table.rs`).
- Produces:
  - `Command::Statusline`, parsed from `/statusline`, and `Effect::ShowStatusline`.
  - `pub struct StatuslineState { pub table: TableState, pub list: FieldList, pub thresholds: Thresholds }` with `new(&StatuslineConfig)`, `config(&self) -> StatuslineConfig`, and `handle_key`.
  - `Overlay::Statusline(StatuslineState)` and `Overlay::statusline(&StatuslineConfig) -> Overlay`.
  - `OverlayOutcome::Statusline(StatuslineConfig)`: the overlay stays open and the TUI applies and saves the settings.
  - `Tui::set_statusline(&mut self, StatuslineConfig) -> Vec<Effect>` and `Tui::apply_statusline(&mut self, StatuslineConfig)` (private).

- [ ] **Step 1: Write the failing tests**

In `crates/scuttle-core/src/commands.rs`, in `every_listed_command_parses`, replace `"/mcp",` in the expected list with `"/mcp", "/statusline",`, and add:

```rust
    #[test]
    fn parses_statusline() {
        assert_eq!(parse("/statusline"), Ok(Command::Statusline));
    }
```

In `mod tests` of `crates/scuttle-core/src/app.rs`, add:

```rust
    #[test]
    fn statusline_opens_its_editor() {
        let mut app = App::new(BusyBehavior::Queue, true);
        assert_eq!(
            app.update(Msg::Command(Command::Statusline)),
            vec![Effect::ShowStatusline]
        );
    }
```

In `mod tests` of `crates/scuttle-tui/src/overlay.rs`, add:

```rust
    fn statusline_rows(o: &Overlay) -> Vec<(scuttle_core::config::StatusField, bool)> {
        match o {
            Overlay::Statusline(s) => s.list.rows.clone(),
            _ => panic!("not the statusline editor"),
        }
    }

    #[test]
    fn the_statusline_editor_moves_toggles_and_steps_warnings() {
        use scuttle_core::config::{StatusField, StatuslineConfig};
        let app = App::new(BusyBehavior::Queue, true);
        let theme = Theme::terminal(true);
        let ctx = ctx_for(&app, &theme);
        let mut o = Overlay::statusline(&StatuslineConfig::default());
        let send =
            |o: &mut Overlay, code, mods| o.handle_key(KeyEvent::new(code, mods), &ctx);
        assert!(
            matches!(send(&mut o, KeyCode::Up, KeyModifiers::ALT), OverlayOutcome::Stay),
            "the first row cannot move up, so nothing is saved"
        );
        match send(&mut o, KeyCode::Down, KeyModifiers::ALT) {
            OverlayOutcome::Statusline(s) => {
                assert_eq!(s.fields[..2], [StatusField::Effort, StatusField::Model]);
            }
            other => panic!("{other:?}"),
        }
        match send(&mut o, KeyCode::Char(' '), KeyModifiers::NONE) {
            OverlayOutcome::Statusline(s) => assert!(
                !s.fields.contains(&StatusField::Model),
                "the selection followed the moved row"
            ),
            other => panic!("{other:?}"),
        }
        send(&mut o, KeyCode::Up, KeyModifiers::SHIFT);
        assert_eq!(statusline_rows(&o)[0], (StatusField::Model, false));
        send(&mut o, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(statusline_rows(&o)[0], (StatusField::Model, true), "Enter toggles too");
        assert!(matches!(
            send(&mut o, KeyCode::Right, KeyModifiers::NONE),
            OverlayOutcome::Stay
        ), "the model takes no warning");
        send(&mut o, KeyCode::Down, KeyModifiers::NONE);
        send(&mut o, KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(statusline_rows(&o)[2].0, StatusField::Context);
        match send(&mut o, KeyCode::Right, KeyModifiers::NONE) {
            OverlayOutcome::Statusline(s) => assert_eq!(s.thresholds.context, Some(50)),
            other => panic!("{other:?}"),
        }
        match send(&mut o, KeyCode::Left, KeyModifiers::NONE) {
            OverlayOutcome::Statusline(s) => assert_eq!(s.thresholds.context, None),
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            send(&mut o, KeyCode::Esc, KeyModifiers::NONE),
            OverlayOutcome::Close
        ));
    }
```

In `mod tests` of `crates/scuttle-tui/src/app.rs`, add:

```rust
    #[test]
    fn statusline_changes_the_footer_and_saves_each_change() {
        use scuttle_core::config::StatusField;
        let dir = std::env::temp_dir().join(format!("scuttle-statusline-{}", uuid::Uuid::new_v4()));
        let path = dir.join("config.toml");
        let mut t = Tui::new(
            LocalConfig::default(),
            Some(path.clone()),
            Theme::terminal(true),
            Welcome {
                url: "https://x".into(),
                user: String::new(),
                art: vec![],
                show: true,
            },
            0,
        );
        t.update(Msg::ModelsLoaded(vec![serde_json::from_value(json!({"id": uuid::Uuid::new_v4(), "display_name": "Big", "is_default": true, "enabled": true, "reasoning_efforts": []})).unwrap()]));
        assert!(screen(&mut t, 100, 24).contains("/model: Big · new chat"));
        let effects = t.update(Msg::Submit("/statusline".into()));
        show(&mut t, effects);
        let shown = screen(&mut t, 100, 24);
        assert!(shown.contains("[x] model"), "{shown}");
        assert!(shown.contains("[ ] cost"), "{shown}");
        t.handle(key(KeyCode::Char(' '), KeyModifiers::NONE));
        let shown = screen(&mut t, 100, 24);
        assert!(shown.contains("[ ] model"), "{shown}");
        assert!(!shown.contains("/model: Big"), "the footer follows at once:\n{shown}");
        let saved = config::load(&path).unwrap().statusline;
        assert!(!saved.fields.contains(&StatusField::Model), "{saved:?}");
        assert_eq!(t.config.statusline, saved, "the config follows the file");
        for _ in 0..8 {
            t.handle(key(KeyCode::Down, KeyModifiers::NONE));
        }
        t.handle(key(KeyCode::Char(' '), KeyModifiers::NONE));
        assert!(
            config::load(&path).unwrap().statusline.fields.contains(&StatusField::Cost),
            "the ninth row is cost"
        );
        assert!(t.core.cost_in_footer);
        assert!(t.cost_due, "the cost is asked for at the next wakeup");
        t.handle(key(KeyCode::Esc, KeyModifiers::NONE));
        assert!(t.overlay.is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }
```

In `mod tests` of `crates/scuttle-tui/src/help.rs`, add:

```rust
    #[test]
    fn help_names_the_statusline_keys() {
        let lines = text(&help_lines(&Theme::terminal(true), 200));
        let line = lines
            .iter()
            .find(|l| l.starts_with("In /statusline"))
            .unwrap_or_else(|| panic!("no /statusline line in {lines:?}"));
        for needle in [
            "Space or Enter shows or hides",
            "Alt+Up and Alt+Down",
            "Shift+Up and Shift+Down",
            "Left and Right",
        ] {
            assert!(line.contains(needle), "{needle} is missing from {line:?}");
        }
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --workspace -- statusline every_listed_command_parses help_names_the_statusline_keys`
Expected: FAIL to compile with "no variant named `Statusline`".

- [ ] **Step 3: Add the command**

In `crates/scuttle-core/src/commands.rs`, add `Statusline,` after `Mcp,` in `pub enum Command`; after the `/mcp` entry of `COMMANDS`, add:

```rust
    CommandInfo {
        name: "/statusline",
        aliases: &[],
        usage: "/statusline",
        description: "Choose the footer's fields, their order, and their warnings",
    },
```

and in `parse`, after `"mcp" => Ok(Command::Mcp),`, add `"statusline" => Ok(Command::Statusline),`.

In `crates/scuttle-core/src/app.rs`, in `pub enum Effect`, after `ShowMcp,`, add:

```rust
    /// Opens the `/statusline` editor, which the UI owns, since the settings are local.
    ShowStatusline,
```

and in `App::command`, after the `Command::Mcp` arm, add `Command::Statusline => vec![Effect::ShowStatusline],`.

In `crates/scuttle-tui/src/runtime.rs`, add `| Effect::ShowStatusline` after `| Effect::ShowMcp` in the ignored effects.

- [ ] **Step 4: Add the editor overlay**

In `crates/scuttle-tui/src/overlay.rs`, add `use scuttle_core::config::{FieldList, StatuslineConfig, Thresholds};` to the imports; `TableKey` is already imported.

In `pub enum OverlayOutcome`, after `Send(Msg),`, add:

```rust
    /// Keeps the overlay open, and has the UI apply and save these footer settings.
    Statusline(StatuslineConfig),
```

In `pub enum Overlay`, after `Mcp(TableState),`, add:

```rust
    /// The `/statusline` editor: the footer's fields, their order, and their warnings.
    Statusline(StatuslineState),
```

After `fn mcp_view`, add:

```rust
/// The `/statusline` editor's state: every footer field in order, with whether it shows, and
/// the warnings, as edited so far.
pub struct StatuslineState {
    pub table: TableState,
    pub list: FieldList,
    pub thresholds: Thresholds,
}

impl StatuslineState {
    pub fn new(statusline: &StatuslineConfig) -> StatuslineState {
        StatuslineState {
            table: TableState::default(),
            list: FieldList::new(statusline),
            thresholds: statusline.thresholds,
        }
    }

    /// The settings as edited so far.
    pub fn config(&self) -> StatuslineConfig {
        StatuslineConfig {
            fields: self.list.fields(),
            thresholds: self.thresholds,
        }
    }

    /// Space or Enter shows or hides the selected field, Alt or Shift with Up or Down moves
    /// it, and Left and Right step its warning. Every change goes back to the UI to apply and
    /// save; a key that changes nothing stays.
    fn handle_key(&mut self, key: KeyEvent, ctx: &ViewCtx) -> OverlayOutcome {
        let view = statusline_view(self, ctx.theme);
        let Some(at) = self.table.index(&view) else {
            return match self.table.handle_key(key, &view) {
                TableKey::Esc => OverlayOutcome::Close,
                _ => OverlayOutcome::Stay,
            };
        };
        let field = self.list.rows[at].0;
        let before = self.config();
        let moves = key
            .modifiers
            .intersects(KeyModifiers::ALT | KeyModifiers::SHIFT);
        match key.code {
            KeyCode::Up | KeyCode::Down if moves => {
                self.list.move_by(at, key.code == KeyCode::Up);
            }
            KeyCode::Char(' ') | KeyCode::Enter => self.list.toggle(at),
            KeyCode::Left | KeyCode::Right if field.takes_threshold() => {
                self.thresholds.step(field, key.code == KeyCode::Right);
            }
            _ => {
                return match self.table.handle_key(key, &view) {
                    TableKey::Esc => OverlayOutcome::Close,
                    _ => OverlayOutcome::Stay,
                };
            }
        }
        // The selection follows the field, wherever it moved.
        self.table.selected = Some(RowKey::Action(field.name()));
        let after = self.config();
        if after == before {
            OverlayOutcome::Stay
        } else {
            OverlayOutcome::Statusline(after)
        }
    }
}

/// The `/statusline` editor's rows: a check box and the field's name, what it shows, and its
/// warning.
fn statusline_view(state: &StatuslineState, theme: &Theme) -> TableView {
    let rows = state
        .list
        .rows
        .iter()
        .map(|&(field, shown)| {
            let mark = if shown { "[x] " } else { "[ ] " };
            let warning = match state.thresholds.get(field) {
                Some(percent) => format!("warns at {percent}%"),
                None if field.takes_threshold() => "no warning".to_owned(),
                None => String::new(),
            };
            Row::item(
                RowKey::Action(field.name()),
                vec![
                    Line::from(format!("{mark}{}", field.name())),
                    Line::from(Span::styled(field.description(), theme.dim)),
                    Line::from(Span::styled(warning, theme.dim)),
                ],
            )
        })
        .collect();
    TableView {
        title: "Status line".into(),
        widths: vec![
            // Fits "[x] organization".
            Constraint::Length(16),
            Constraint::Fill(1),
            // Fits "warns at 100%".
            Constraint::Length(13),
        ],
        rows,
        hint: Some(
            "Space shows or hides, Alt+Up and Alt+Down move, Left and Right set the warning, Esc closes"
                .into(),
        ),
        ..Default::default()
    }
}
```

In `impl Overlay`, after `pub fn chats`, add:

```rust
    /// The `/statusline` editor, starting from the footer's settings in effect.
    pub fn statusline(statusline: &StatuslineConfig) -> Overlay {
        Overlay::Statusline(StatuslineState::new(statusline))
    }
```

In `pub fn state`, add `Overlay::Statusline(s) => &s.table,`; in `fn state_mut`, add `Overlay::Statusline(s) => &mut s.table,`; in `pub fn view`, add `Overlay::Statusline(s) => statusline_view(s, ctx.theme),`.
In `pub fn handle_key`, after the `if let Overlay::Subagents(s) = self { .. }` block, add:

```rust
        if let Overlay::Statusline(s) = self {
            return s.handle_key(key, ctx);
        }
```

- [ ] **Step 5: Apply and save the settings in the TUI**

In `crates/scuttle-tui/src/app.rs`, in `apply_ui_effect`, after the `Effect::ShowMcp` arm, add:

```rust
            Effect::ShowStatusline => {
                self.overlay = Some(Overlay::statusline(&self.statusline));
            }
```

In `overlay_key`, after `OverlayOutcome::Send(msg) => self.update(msg),`, add:

```rust
            OverlayOutcome::Statusline(statusline) => self.set_statusline(statusline),
```

After `fn apply_settings`, add:

```rust
    /// Makes `statusline` the footer's settings, and asks for the chat's cost at the next
    /// wakeup when the footer starts showing it.
    fn apply_statusline(&mut self, statusline: StatuslineConfig) {
        let shows_cost = statusline.fields.contains(&StatusField::Cost);
        if shows_cost && !self.core.cost_in_footer {
            self.cost_due = true;
        }
        self.core.cost_in_footer = shows_cost;
        self.statusline = statusline;
    }

    /// Applies footer settings from `/statusline` and saves them. A save that fails keeps
    /// them for this session and says why; `self.config` keeps what the file holds.
    fn set_statusline(&mut self, statusline: StatuslineConfig) -> Vec<Effect> {
        self.apply_statusline(statusline.clone());
        if let Some(path) = self.config_path.as_ref() {
            match config::set_statusline(path, &statusline) {
                Ok(()) => self.config.statusline = statusline,
                Err(e) => self.notice(Notice::Error(e.to_string())),
            }
        }
        vec![]
    }
```

In `apply_settings`, before `self.config = new;`, add:

```rust
        // The footer's fields and warnings apply live, as `/statusline` changes them.
        self.apply_statusline(new.statusline.clone());
```

In `draw_at`, replace `(Overlay::WorkspaceDetails(_), Some((view, _, _))) => {` with `(Overlay::WorkspaceDetails(_) | Overlay::Statusline(_), Some((view, _, _))) => {`.

In `crates/scuttle-tui/src/help.rs`, after the `"In /workspace"` entry of `KEYS`, add:

```rust
    KeyInfo {
        keys: "In /statusline",
        action: "Space or Enter shows or hides a field, Alt+Up and Alt+Down move it (Shift+Up and Shift+Down too), Left and Right change its warning; each change saves to config.toml",
    },
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test --workspace -- statusline every_listed_command_parses help_names_the_statusline_keys the_slash_menu_shows_five_rows down_scrolls_the_slash_menu settings`
Expected: PASS; the slash menu tests count `COMMANDS`, so they follow the new entry.

- [ ] **Step 7: Run the full checks**

Run: `cargo fmt --all && cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check`
Expected: all green.

- [ ] **Step 8: Commit**

```bash
git add crates/scuttle-core/src/commands.rs crates/scuttle-core/src/app.rs crates/scuttle-tui/src/overlay.rs crates/scuttle-tui/src/app.rs crates/scuttle-tui/src/help.rs crates/scuttle-tui/src/runtime.rs
git commit -m "feat: choose the footer's fields and warnings with /statusline" \
  -m "/statusline lists every field with a check box. Space or Enter shows or hides one, Alt or Shift with Up or Down moves it, and Left and Right step its warning. Each change applies to the footer at once and saves to config.toml, and /settings applies the same keys live." \
  -m "Assisted-by: AI"
```

---

### Task 12: The `/usage` panel

**Files:**
- Modify: `crates/scuttle-core/src/commands.rs` (`Command`, `COMMANDS`, `parse`, and the command list test)
- Modify: `crates/scuttle-core/src/app.rs` (`Effect`, `App::command`)
- Modify: `crates/scuttle-core/src/panels.rs` (new `context_text`, `cost_text`, and `usage_lines`; `info_lines` uses the first two)
- Modify: `crates/scuttle-tui/src/overlay.rs` (`Overlay`, a new `usage_view`, and `Overlay`'s methods)
- Modify: `crates/scuttle-tui/src/app.rs` (`apply_ui_effect`)
- Modify: `crates/scuttle-tui/src/runtime.rs` (the ignored effects)
- Test: `crates/scuttle-core/src/commands.rs`, `crates/scuttle-core/src/app.rs`, `crates/scuttle-core/src/panels.rs`, `crates/scuttle-tui/src/app.rs` (`mod tests`)

**Interfaces:**
- Consumes: `App::spend`, `App::quota`, `App::chat_cost`, `App::usage_open`, `Msg::UsageClosed`, `App::refresh_limits`, `App::fetch_chat_cost` (Task 3); `usage::{spend_summary, budget_source, quota_summary}` and `time::until` (Task 2); `label_rows` and `label_column` (existing, `overlay.rs`).
- Produces:
  - `Command::Usage`, parsed from `/usage`, and `Effect::ShowUsage`.
  - `pub fn panels::usage_lines(app: &App, now_unix: i64, offset: chrono::FixedOffset) -> Vec<(&'static str, String)>`.
  - `Overlay::Usage(TableState)`, full height, closing with `Msg::UsageClosed`.

- [ ] **Step 1: Write the failing tests**

In `crates/scuttle-core/src/commands.rs`, in `every_listed_command_parses`, replace `"/mcp", "/statusline",` in the expected list with `"/mcp", "/usage", "/statusline",`, and add:

```rust
    #[test]
    fn parses_usage() {
        assert_eq!(parse("/usage"), Ok(Command::Usage));
    }
```

In `mod tests` of `crates/scuttle-core/src/app.rs`, add:

```rust
    #[test]
    fn usage_opens_and_refreshes_the_limits_and_the_chat_cost() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let org = started(&mut app);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(id),
            messages: vec![],
        });
        assert_eq!(
            app.update(Msg::Command(Command::Usage)),
            vec![
                Effect::ShowUsage,
                Effect::FetchSpend { generation: 1 },
                Effect::FetchQuota { org, generation: 1 },
                Effect::FetchCost {
                    chat: id,
                    generation: 1
                },
            ]
        );
        assert!(app.usage_open);
        assert!(matches!(app.chat_cost, Some(CostState::Loading)));
        assert_eq!(
            app.update(Msg::RefreshCost),
            vec![Effect::FetchCost {
                chat: id,
                generation: 2
            }],
            "the cost refreshes after a turn while /usage is open"
        );
        app.update(Msg::UsageClosed);
        assert!(!app.usage_open);
        assert!(app.update(Msg::RefreshCost).is_empty());
    }
```

In `mod tests` of `crates/scuttle-core/src/panels.rs`, add:

```rust
    #[test]
    fn usage_shows_spend_its_period_and_budget_the_chat_cost_context_and_quota() {
        use crate::usage::LimitState;
        let mut app = App::new(BusyBehavior::Queue, true);
        let org = uuid::Uuid::new_v4();
        app.update(Msg::Started {
            org_id: org,
            open_chat: None,
        });
        let chat = serde_json::from_value(json!({
            "id": uuid::Uuid::new_v4(), "parent_chat_id": uuid::Uuid::new_v4(),
            "children": [], "files": [], "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}
        }))
        .unwrap();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: Box::new(chat),
            messages: serde_json::from_value(json!([{"id": 1, "role": "assistant", "content": [], "usage": {"input_tokens": 12000, "context_limit": 200000}}])).unwrap(),
        });
        app.update(Msg::RefreshLimits);
        app.update(Msg::SpendLoaded {
            spend: Box::new(
                serde_json::from_value(json!({
                    "current_spend_micros": 1_200_000,
                    "effective_budget": {"spend_limit_micros": 50_000_000, "limit_source": "user_override"},
                    "period_start": "2026-10-01T00:00:00Z", "period_end": "2026-11-01T00:00:00Z"
                }))
                .unwrap(),
            ),
            generation: 1,
        });
        app.update(Msg::QuotaLoaded {
            org,
            quota: serde_json::from_value(json!({"credits_consumed": 3, "budget": -1})).unwrap(),
            generation: 1,
        });
        assert!(matches!(app.quota(), LimitState::Loaded(_)));
        app.chat_cost = Some(CostState::Loaded(
            serde_json::from_value(json!({"total_cost_micros": 1230000, "request_count": 4, "unpriced_request_count": 1})).unwrap(),
        ));
        let now = "2026-10-02T12:00:00Z"
            .parse::<chrono::DateTime<chrono::Utc>>()
            .unwrap()
            .timestamp();
        let utc = chrono::FixedOffset::east_opt(0).unwrap();
        let lines = usage_lines(&app, now, utc);
        assert_eq!(value(&lines, "AI spend"), Some("$1.20 of $50.00 (2%)"));
        assert_eq!(
            value(&lines, "Period"),
            Some("2026-10-01 00:00 to 2026-11-01 00:00, resets in 29d")
        );
        assert_eq!(
            value(&lines, "Budget"),
            Some("Set for you, in place of your group's budget")
        );
        assert_eq!(
            value(&lines, "Chat cost"),
            Some(
                "$1.23 over 4 requests, for the whole chat tree. Excludes unpriced usage from 1 request."
            )
        );
        assert_eq!(value(&lines, "Context"), Some("12.0k of 200.0k tokens"));
        assert_eq!(
            value(&lines, "Workspace quota"),
            Some("No quota applies; your workspaces use 3 credits")
        );
    }

    #[test]
    fn usage_hides_what_the_deployment_lacks_and_says_what_it_refuses() {
        use crate::usage::{Limit, Refusal};
        let mut app = App::new(BusyBehavior::Queue, true);
        app.update(Msg::Started {
            org_id: uuid::Uuid::new_v4(),
            open_chat: None,
        });
        let utc = chrono::FixedOffset::east_opt(0).unwrap();
        let lines = usage_lines(&app, 0, utc);
        assert_eq!(value(&lines, "AI spend"), Some("Loading…"));
        assert_eq!(value(&lines, "Chat cost"), Some("Start a chat first."));
        assert_eq!(value(&lines, "Context"), None, "a blank chat has no context");
        app.update(Msg::RefreshLimits);
        app.update(Msg::LimitFailed {
            limit: Limit::Spend,
            refusal: Refusal::Unlicensed("AI Gateway is a Premium feature. Contact sales!".into()),
            generation: 1,
        });
        app.update(Msg::LimitFailed {
            limit: Limit::Quota,
            refusal: Refusal::Absent,
            generation: 1,
        });
        let lines = usage_lines(&app, 0, utc);
        assert_eq!(
            value(&lines, "AI spend"),
            Some("AI Gateway is a Premium feature. Contact sales!")
        );
        assert_eq!(
            value(&lines, "Workspace quota"),
            None,
            "an open-source deployment has no quota"
        );
        app.update(Msg::RefreshLimits);
        app.update(Msg::LimitFailed {
            limit: Limit::Spend,
            refusal: Refusal::Failed("HTTP 502".into()),
            generation: 2,
        });
        assert_eq!(
            value(&usage_lines(&app, 0, utc), "AI spend"),
            Some("AI Gateway is a Premium feature. Contact sales!"),
            "a refusal is not asked for again, so its message stays"
        );
    }
```

In `mod tests` of `crates/scuttle-tui/src/app.rs`, add:

```rust
    #[test]
    fn slash_usage_shows_the_limits_and_esc_closes_it() {
        let mut t = info_tui();
        let effects = t.update(Msg::Submit("/usage".into()));
        show(&mut t, effects);
        assert!(matches!(t.overlay, Some(Overlay::Usage(_))));
        t.update(Msg::SpendLoaded {
            spend: Box::new(
                serde_json::from_value(json!({"current_spend_micros": 1_200_000,
                    "effective_budget": {"spend_limit_micros": 50_000_000, "limit_source": "group"}}))
                .unwrap(),
            ),
            generation: 1,
        });
        let shown = flowed(&screen(&mut t, 80, 24));
        assert!(shown.contains("$1.20 of $50.00 (2%)"), "{shown}");
        assert!(shown.contains("Your group's budget"), "{shown}");
        t.handle(key(KeyCode::Esc, KeyModifiers::NONE));
        assert!(t.overlay.is_none());
        assert!(!t.core.usage_open, "the core stops refreshing the cost for it");
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --workspace -- usage every_listed_command_parses`
Expected: FAIL to compile with "no variant named `Usage`".

- [ ] **Step 3: Add the command**

In `crates/scuttle-core/src/commands.rs`, add `Usage,` after `Mcp,` in `pub enum Command`; between the `/mcp` and `/statusline` entries of `COMMANDS`, add:

```rust
    CommandInfo {
        name: "/usage",
        aliases: &[],
        usage: "/usage",
        description: "Show your AI spend, workspace quota, and this chat's cost and context",
    },
```

and in `parse`, after `"mcp" => Ok(Command::Mcp),`, add `"usage" => Ok(Command::Usage),`.

In `crates/scuttle-core/src/app.rs`, in `pub enum Effect`, after `ShowStatusline,`, add:

```rust
    /// Opens the `/usage` panel.
    ShowUsage,
```

and in `App::command`, after the `Command::Statusline` arm, add:

```rust
            Command::Usage => {
                self.usage_open = true;
                let mut effects = vec![Effect::ShowUsage];
                effects.extend(self.refresh_limits());
                effects.extend(self.fetch_chat_cost());
                effects
            }
```

In `crates/scuttle-tui/src/runtime.rs`, add `| Effect::ShowUsage` after `| Effect::ShowStatusline` in the ignored effects.

- [ ] **Step 4: Build the panel's rows**

In `crates/scuttle-core/src/panels.rs`, update the module doc comment to "The rows of the read-only panels: `/info`, `/usage`, `/workspace`, `/git`, and `/mcp`.", and add `use crate::usage::LimitState;` to the imports.

After `pub fn workspace_lines`, add:

```rust
/// The context row of `/info` and `/usage`.
fn context_text(app: &App) -> String {
    match usage::context_usage(app.transcript.messages()) {
        Some(u) => match u.limit {
            Some(limit) => format!(
                "{} of {} tokens",
                usage::format_tokens(u.used),
                usage::format_tokens(limit)
            ),
            None => format!("{} tokens", usage::format_tokens(u.used)),
        },
        None => "unknown".into(),
    }
}

/// The cost row of `/info` and `/usage`, saying for a subagent that it is the whole tree's.
fn cost_text(cost: &types::CodersdkChatCost, subagent: bool) -> String {
    let mut text = format!(
        "{} over {}",
        usage::format_cost_micros(cost.total_cost_micros.unwrap_or(0)),
        usage::count(cost.request_count.unwrap_or(0), "request")
    );
    if subagent {
        text.push_str(", for the whole chat tree");
    }
    if let Some(n) = cost.unpriced_request_count.filter(|n| *n > 0) {
        text.push_str(&format!(
            ". Excludes unpriced usage from {}.",
            usage::count(n, "request")
        ));
    }
    text
}
```

In `info_lines`, replace the `let context = match usage::context_usage(..) { .. };` statement with `let context = context_text(app);`, and replace the body of the `Some(CostState::Loaded(cost)) => { .. }` arm with `lines.push(("Cost", cost_text(cost, chat.parent_chat_id.is_some())))`.

After `info_lines`, add:

```rust
/// The `/usage` panel as label and value pairs: the AI spend with its period and budget, the
/// open chat's cost and context, and the workspace quota. A limit the deployment lacks shows
/// no row, and one it refuses shows the server's message. Times are shown at `offset`.
pub fn usage_lines(
    app: &App,
    now_unix: i64,
    offset: chrono::FixedOffset,
) -> Vec<(&'static str, String)> {
    let mut lines = Vec::new();
    match app.spend() {
        LimitState::Absent => {}
        LimitState::Unknown => lines.push(("AI spend", "Loading…".into())),
        LimitState::Refused(message) => lines.push(("AI spend", message.clone())),
        LimitState::Failed(message) => {
            lines.push(("AI spend", format!("unavailable: {message}")))
        }
        LimitState::Loaded(spend) => {
            lines.push(("AI spend", usage::spend_summary(spend)));
            if let (Some(start), Some(end)) = (spend.period_start, spend.period_end) {
                lines.push((
                    "Period",
                    format!(
                        "{} to {}, resets {}",
                        time::local(start, offset),
                        time::local(end, offset),
                        time::until(end.timestamp(), now_unix)
                    ),
                ));
            }
            if let Some(source) = usage::budget_source(spend) {
                lines.push(("Budget", source));
            }
        }
    }
    match (app.chat.as_deref(), app.chat_cost.as_ref()) {
        (None, _) => lines.push(("Chat cost", "Start a chat first.".into())),
        (Some(chat), Some(CostState::Loaded(cost))) => {
            lines.push(("Chat cost", cost_text(cost, chat.parent_chat_id.is_some())))
        }
        (Some(_), Some(CostState::Failed(message))) => {
            lines.push(("Chat cost", format!("unavailable: {message}")))
        }
        (Some(_), Some(CostState::Hidden)) => {}
        (Some(_), Some(CostState::Loading) | None) => lines.push(("Chat cost", "…".into())),
    }
    if app.chat.is_some() {
        lines.push(("Context", context_text(app)));
    }
    match app.quota() {
        LimitState::Absent => {}
        LimitState::Unknown => lines.push(("Workspace quota", "Loading…".into())),
        LimitState::Refused(message) => lines.push(("Workspace quota", message.clone())),
        LimitState::Failed(message) => {
            lines.push(("Workspace quota", format!("unavailable: {message}")))
        }
        LimitState::Loaded(quota) => lines.push(("Workspace quota", usage::quota_summary(quota))),
    }
    lines
}
```

- [ ] **Step 5: Add the panel overlay**

In `crates/scuttle-tui/src/overlay.rs`, in `pub enum Overlay`, after `Statusline(StatuslineState),`, add:

```rust
    /// The read-only `/usage` panel.
    Usage(TableState),
```

After `fn info_view`, add:

```rust
fn usage_view(ctx: &ViewCtx) -> TableView {
    let rows = label_rows(
        scuttle_core::panels::usage_lines(ctx.app, ctx.now_unix, ctx.offset),
        ctx.theme,
    );
    TableView {
        title: "Usage".into(),
        widths: vec![label_column(&rows), Constraint::Fill(1)],
        rows,
        hint: Some("Up and Down scroll, Esc closes".into()),
        ..Default::default()
    }
}
```

In `impl Overlay`: add `| Overlay::Usage(_)` to `full_height`; add `Overlay::Usage(_) => Some(Msg::UsageClosed),` to `close_msg`; add `| Overlay::Usage(s)` to the shared arm of both `state` and `state_mut`; and add `Overlay::Usage(_) => usage_view(ctx),` to `view`.

In `crates/scuttle-tui/src/app.rs`, in `apply_ui_effect`, after the `Effect::ShowStatusline` arm, add:

```rust
            Effect::ShowUsage => {
                self.overlay = Some(Overlay::Usage(crate::table::TableState::default()));
            }
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test --workspace -- usage every_listed_command_parses info_shows_the_chat_and_its_cost a_summary_shows long_info_values_wrap a_short_terminal_scrolls_info`
Expected: PASS; the `/info` tests prove its cost and context text did not change.

- [ ] **Step 7: Run the full checks**

Run: `cargo fmt --all && cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check`
Expected: all green, with no insta snapshot changed.

- [ ] **Step 8: Commit**

```bash
git add crates/scuttle-core/src/commands.rs crates/scuttle-core/src/app.rs crates/scuttle-core/src/panels.rs crates/scuttle-tui/src/overlay.rs crates/scuttle-tui/src/app.rs crates/scuttle-tui/src/runtime.rs
git commit -m "feat: show spend, quota, and chat cost in /usage" \
  -m "/usage shows the AI spend for the period with its reset time and where the budget comes from, the chat's cost and request counts with unpriced requests, the context, and the workspace quota. A deployment without a limit shows no row for it, and one that refuses shows the server's message." \
  -m "Assisted-by: AI"
```

---

### Task 13: Line up the `/chats` titles behind fixed pin and status columns

Today a `/chats` row has the `›` selection column, a one-cell status column, a one-cell unread column, then the title cell, which holds the subagent indent and the pin, so a pinned title starts three cells after an unpinned one.
The status column holds the spinner while the chat is `running` or `interrupting` (built as a braille frame and painted over with the turn's style, so the `line` style shows `-`), `?` while it is `requires_action`, `!` after an `error`, and a blank while it is `waiting` or has an unknown status.
The unread column holds `•` for an unread chat other than the open one.
After the title, the family column holds `+N` and the busiest subagent's marker, then come `archived`, the age, and the summary from 100 columns.
A subagent row, listed under its expanded parent with depth 1, starts its title with a three-space indent.

This task gives every row two fixed columns before the title: the pin, then one status column that shows the most important state.
The status priority is the spinner while the agent works, then `!` for an error, then `?` for a chat waiting on the user, then 🔵 for unread, then blank.
A chat has one status, so the first three never compete; the order decides only against unread.
A running chat that is also unread shows the spinner, because the agent is still writing what the user has not read, and the 🔵 appears once the chat goes idle.
The selection column, the subagent indent inside the title cell, the family column and its painted marker, the summary column from 100 columns, the search row, and the view cache stay as they are.

**Files:**
- Modify: `crates/scuttle-tui/src/overlay.rs` (new `STATUS_COLUMN`, `STATUS_WIDTH`, `UNREAD`, `pin_width`, and `status_cell`; `chat_cells`, `chat_spinners`, and `chats_view`)
- Modify: `crates/scuttle-tui/src/help.rs` (new `CHATS_MARKERS`; `help_lines_for`)
- Test: `crates/scuttle-tui/src/overlay.rs`, `crates/scuttle-tui/src/help.rs` (`mod tests`)

**Interfaces:**
- Consumes: `ChatRow`'s `pinned`, `unread`, `status`, `depth`, `children`, and `busiest_child` (existing, `chat_list.rs`); `spins`, `marker`, `family_prefix`, `FAMILY_COLUMN`, `SUMMARY_MIN_WIDTH`, and `ViewCtx::pin_icon` from `chats.pin_icon` (existing, `overlay.rs`); `Spinner { row, column, offset }` and the spinner painting in `table::render` (existing, `table.rs`), unchanged; `spinner_frame` (existing, `activity.rs`).
- Produces:
  - `const STATUS_COLUMN: usize = 1`, `const STATUS_WIDTH: u16 = 2`, and `const UNREAD: &str = "\u{1f535}"` in `overlay.rs`.
  - `fn pin_width(icon: &str) -> u16` and `fn status_cell(r: &ChatRow, ctx: &ViewCtx) -> Line<'static>` in `overlay.rs`.
  - `pub const CHATS_MARKERS: &[&str]` in `help.rs`.
  - `chat_cells` keeps seven cells (six below 100 columns): pin, status, title, family, archived, age, and summary, so `FAMILY_COLUMN` stays 3 and the title stays cell 2.
  - No later task consumes these.

The three existing tests that assert the old layout change only by the column moves and the new dot:
- `the_open_chat_never_shows_an_unread_dot`: the unread cell is still cell 1, and it now holds 🔵 instead of `•`.
- `a_pinned_chat_shows_the_pin_icon_and_an_empty_icon_hides_it`: the pin moves out of the title cell 2 into cell 0.
- `the_wide_pin_takes_two_cells_and_leaves_the_other_columns_in_place`: the title now starts six cells after the pin, past the status column, instead of two.

Every other `/chats` test keeps its assertions: `the_summary_is_a_dim_last_column_on_a_wide_overlay_only` (seven and six widths, summary in cell 6), `a_large_family_keeps_its_busiest_marker_visible` (family column 3), `old_chats_keep_the_unit_of_their_age`, the paging and search tests, and `every_list_hint_says_how_to_close_it_and_the_queue_names_both_keys`.
No insta snapshot draws `/chats`.

- [ ] **Step 1: Write the failing tests**

In `mod tests` of `crates/scuttle-tui/src/overlay.rs`, in `the_open_chat_never_shows_an_unread_dot`, replace:

```rust
        let dot = |i: usize| view.rows[i].cells[1].to_string();
        assert_eq!(dot(0), " ", "the open chat is being read");
        assert_eq!(dot(1), "•");
```

with:

```rust
        let dot = |i: usize| view.rows[i].cells[STATUS_COLUMN].to_string();
        assert_eq!(dot(0), " ", "the open chat is being read");
        assert_eq!(dot(1), UNREAD);
```

Replace the body of `a_pinned_chat_shows_the_pin_icon_and_an_empty_icon_hides_it` with:

```rust
        let app = pinned_app("");
        let theme = Theme::terminal(true);
        let o = Overlay::chats(String::new(), &app);
        let view = o.view(&ctx_for(&app, &theme));
        assert_eq!(cell_text(&view, 0, 0), "📌");
        assert_eq!(cell_text(&view, 0, 2), "alpha", "the pin is not in the title cell");
        let nerd = ViewCtx {
            pin_icon: "\u{f0403}",
            ..ctx_for(&app, &theme)
        };
        let view = o.view(&nerd);
        assert_eq!(cell_text(&view, 0, 0), "\u{f0403}");
        assert_eq!(cell_text(&view, 0, 2), "alpha");
        let none = ViewCtx {
            pin_icon: "",
            ..ctx_for(&app, &theme)
        };
        let view = o.view(&none);
        assert_eq!(cell_text(&view, 0, 0), "");
        assert_eq!(cell_text(&view, 0, 2), "alpha");
```

In `the_wide_pin_takes_two_cells_and_leaves_the_other_columns_in_place`, replace:

```rust
            assert_eq!(
                (2..8)
                    .map(|dx| buf[(pin + dx, row)].symbol())
                    .collect::<String>(),
                " alpha",
                "the title starts two cells after the pin at width {width}"
            );
```

with:

```rust
            assert_eq!(
                (2..11)
                    .map(|dx| buf[(pin + dx, row)].symbol())
                    .collect::<String>(),
                "    alpha",
                "the title starts six cells after the pin, past the status column, at width {width}"
            );
```

After `the_wide_pin_takes_two_cells_and_leaves_the_other_columns_in_place`, add:

```rust
    /// An app listing one chat for each `/chats` marker, titled by what it shows: pinned,
    /// plain (with a summary), unread, running and unread, errored and unread, waiting on the
    /// user and unread, and a parent whose subagent `t-child` lists under it once expanded.
    /// Returns the app and the parent's id.
    fn marked_app() -> (App, uuid::Uuid) {
        let parent_id = uuid::Uuid::new_v4();
        let chat = |title: &str, status: &str, minute: u32| {
            json!({"id": uuid::Uuid::new_v4(), "title": title, "status": status,
                "updated_at": format!("2026-09-30T10:{minute:02}:00Z"), "children": [],
                "files": [], "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}})
        };
        let mut pinned = chat("t-pinned", "waiting", 50);
        pinned["pin_order"] = json!(1);
        let mut plain = chat("t-plain", "waiting", 45);
        plain["last_turn_summary"] = json!("Fixing the CI");
        let mut unread = chat("t-unread", "waiting", 49);
        let mut running = chat("t-running", "running", 48);
        let mut errored = chat("t-error", "error", 47);
        let mut asking = chat("t-asking", "requires_action", 46);
        for c in [&mut unread, &mut running, &mut errored, &mut asking] {
            c["has_unread"] = json!(true);
        }
        let mut parent = chat("t-parent", "waiting", 44);
        parent["id"] = json!(parent_id);
        parent["children"] = json!([child(
            uuid::Uuid::new_v4(),
            parent_id,
            "t-child",
            "waiting"
        )]);
        let mut app = App::new(BusyBehavior::Queue, true);
        app.update(Msg::ChatsLoaded {
            query: ListQuery::Default,
            offset: 0,
            chats: serde_json::from_value(json!([
                pinned, unread, running, errored, asking, plain, parent
            ]))
            .unwrap(),
        });
        (app, parent_id)
    }

    /// `/chats` over `app` with `parent` expanded, so its subagent is listed.
    fn expanded_chats(app: &App, parent: uuid::Uuid) -> Overlay {
        let mut o = Overlay::chats(String::new(), app);
        if let Overlay::Chats(state) = &mut o {
            state.expanded.insert(parent);
        }
        o
    }

    /// `o` drawn by the table `ctx.width` columns wide and 16 rows high, with `frame` painted
    /// into the spinner cells.
    fn drawn_chats(o: &Overlay, ctx: &ViewCtx, frame: Option<&str>) -> ratatui::buffer::Buffer {
        let view = o.view(ctx);
        let mut term = Terminal::new(TestBackend::new(ctx.width, 16)).unwrap();
        term.draw(|f| {
            table::render(f, f.area(), &view, o.state(), ctx.theme, frame);
        })
        .unwrap();
        term.backend().buffer().clone()
    }

    /// The column and row where `text` starts in `buf`.
    fn start_of(buf: &ratatui::buffer::Buffer, text: &str) -> (u16, u16) {
        let area = buf.area;
        (0..area.height)
            .flat_map(|y| (0..area.width).map(move |x| (x, y)))
            .find(|&(x, y)| {
                (x..area.width)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
                    .starts_with(text)
            })
            .unwrap_or_else(|| panic!("{text:?} is not drawn"))
    }

    #[test]
    fn every_title_starts_in_one_column_after_the_pin_and_status_columns() {
        let (app, parent) = marked_app();
        let theme = Theme::terminal(true);
        let o = expanded_chats(&app, parent);
        let buf = drawn_chats(&o, &ctx_for(&app, &theme), Some("X"));
        let (x, _) = start_of(&buf, "t-plain");
        for title in [
            "t-pinned",
            "t-unread",
            "t-running",
            "t-error",
            "t-asking",
            "t-parent",
        ] {
            assert_eq!(start_of(&buf, title).0, x, "{title} starts in the title column");
        }
        assert_eq!(
            start_of(&buf, "t-child").0,
            x + 3,
            "a subagent keeps its indent inside the title cell"
        );
        // The pin and status columns are two cells each, and the table puts one cell after
        // each column.
        let (pin, status) = (x - 6, x - 3);
        let at = |title: &str, column: u16| {
            buf[(column, start_of(&buf, title).1)].symbol().to_owned()
        };
        assert_eq!(at("t-pinned", pin), "📌");
        assert_eq!(at("t-unread", status), UNREAD);
        assert_eq!(
            at("t-running", status),
            "X",
            "the table paints the turn's spinner in the status column"
        );
        assert_eq!(at("t-error", status), "!");
        assert_eq!(at("t-asking", status), "?");
        for title in ["t-plain", "t-parent", "t-child"] {
            assert_eq!(
                (at(title, pin), at(title, status)),
                (" ".to_owned(), " ".to_owned()),
                "{title} is unpinned, read, and idle"
            );
        }
    }

    #[test]
    fn the_unread_dot_shows_only_for_an_unread_idle_chat() {
        use unicode_width::UnicodeWidthStr;
        assert_eq!(
            (UNREAD.width(), UNREAD.width_cjk()),
            (2, 2),
            "the dot is two cells under both width rules"
        );
        assert_eq!(usize::from(STATUS_WIDTH), UNREAD.width());
        let (app, parent) = marked_app();
        let theme = Theme::terminal(true);
        let view = expanded_chats(&app, parent).view(&ctx_for(&app, &theme));
        assert_eq!(view.widths[STATUS_COLUMN], Constraint::Length(STATUS_WIDTH));
        let status = |title: &str| {
            let row = (0..view.rows.len())
                .find(|&i| cell_text(&view, i, 2).trim_start() == title)
                .unwrap_or_else(|| panic!("no row for {title}"));
            cell_text(&view, row, STATUS_COLUMN)
        };
        assert_eq!(status("t-unread"), UNREAD);
        assert_eq!(
            status("t-running"),
            spinner_frame(Duration::ZERO),
            "a working chat keeps its spinner while unread"
        );
        assert_eq!(status("t-error"), "!", "an error outranks unread");
        assert_eq!(status("t-asking"), "?", "a question outranks unread");
        for title in ["t-pinned", "t-plain", "t-parent", "t-child"] {
            assert_eq!(status(title), " ", "{title} is read and idle");
        }
    }

    #[test]
    fn a_running_unread_chat_shows_the_spinner_until_it_stops_then_the_dot() {
        let id = uuid::Uuid::new_v4();
        let load = |app: &mut App, status: &str| {
            app.update(Msg::ChatsLoaded {
                query: ListQuery::Default,
                offset: 0,
                chats: serde_json::from_value(json!([{"id": id, "title": "busy",
                    "status": status, "has_unread": true,
                    "updated_at": "2026-09-30T10:00:00Z", "children": [], "files": [],
                    "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}}]))
                .unwrap(),
            });
        };
        let mut app = App::new(BusyBehavior::Queue, true);
        let theme = Theme::terminal(true);
        load(&mut app, "running");
        let view = Overlay::chats(String::new(), &app).view(&ctx_for(&app, &theme));
        assert_eq!(
            cell_text(&view, 0, STATUS_COLUMN),
            spinner_frame(Duration::ZERO)
        );
        assert_eq!(
            view.spinners,
            vec![Spinner {
                row: 0,
                column: STATUS_COLUMN,
                offset: 0
            }],
            "the table paints the spinner in the status column"
        );
        load(&mut app, "waiting");
        let view = Overlay::chats(String::new(), &app).view(&ctx_for(&app, &theme));
        assert_eq!(cell_text(&view, 0, STATUS_COLUMN), UNREAD);
        assert!(view.spinners.is_empty());
    }

    #[test]
    fn a_one_cell_or_empty_pin_icon_keeps_a_two_cell_pin_column() {
        let (app, parent) = marked_app();
        let theme = Theme::terminal(true);
        let o = expanded_chats(&app, parent);
        let default_x = start_of(&drawn_chats(&o, &ctx_for(&app, &theme), None), "t-plain").0;
        for icon in ["*", "\u{f0403}", ""] {
            let ctx = ViewCtx {
                pin_icon: icon,
                ..ctx_for(&app, &theme)
            };
            assert_eq!(o.view(&ctx).widths[0], Constraint::Length(2), "{icon:?}");
            let buf = drawn_chats(&o, &ctx, None);
            let (x, y) = start_of(&buf, "t-pinned");
            assert_eq!(x, default_x, "{icon:?} leaves the titles where 📌 puts them");
            assert_eq!(start_of(&buf, "t-plain").0, x, "{icon:?}");
            let shown = if icon.is_empty() { " " } else { icon };
            assert_eq!(buf[(x - 6, y)].symbol(), shown, "{icon:?}");
        }
        let wide = ViewCtx {
            pin_icon: "PIN",
            ..ctx_for(&app, &theme)
        };
        assert_eq!(
            o.view(&wide).widths[0],
            Constraint::Length(3),
            "a wider icon widens the column for every row"
        );
    }

    #[test]
    fn narrow_chats_drop_the_summary_first_and_keep_the_titles_in_line() {
        let (app, parent) = marked_app();
        let theme = Theme::terminal(true);
        let o = expanded_chats(&app, parent);
        let now = chrono::DateTime::parse_from_rfc3339("2026-09-30T11:00:00Z")
            .unwrap()
            .timestamp();
        let mut starts = Vec::new();
        for width in [60, 79, SUMMARY_MIN_WIDTH - 1, SUMMARY_MIN_WIDTH] {
            let ctx = ViewCtx {
                width,
                now_unix: now,
                ..ctx_for(&app, &theme)
            };
            assert_eq!(
                o.view(&ctx).widths.len(),
                if width >= SUMMARY_MIN_WIDTH { 7 } else { 6 },
                "at width {width}"
            );
            let buf = drawn_chats(&o, &ctx, None);
            let (x, y) = start_of(&buf, "t-plain");
            let row: String = (0..width).map(|x| buf[(x, y)].symbol()).collect();
            assert!(row.contains("15m"), "the age stays at width {width}: {row}");
            assert_eq!(
                row.contains("Fixing the CI"),
                width >= SUMMARY_MIN_WIDTH,
                "the summary drops first: {row}"
            );
            for title in ["t-pinned", "t-running", "t-parent"] {
                assert_eq!(start_of(&buf, title).0, x, "{title} at width {width}");
            }
            starts.push(x);
        }
        assert!(
            starts.windows(2).all(|w| w[0] == w[1]),
            "the fixed columns never move: {starts:?}"
        );
    }
```

In `mod tests` of `crates/scuttle-tui/src/help.rs`, add:

```rust
    #[test]
    fn help_explains_the_chats_markers_in_their_order() {
        let shown = text(&help_lines(&Theme::terminal(true), 300)).join("\n");
        for needle in [
            "Markers in /chats",
            "chats.pin_icon",
            "a spinner while the agent works, ! after an error, ? while it waits on you, then 🔵 for unread messages",
            "keeps its spinner, and the 🔵 shows once it stops",
            "+N",
        ] {
            assert!(shown.contains(needle), "{needle} is missing from:\n{shown}");
        }
        assert!(!shown.contains('•'), "the old unread dot is gone:\n{shown}");
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --workspace -- every_title_starts unread pin narrow_chats help_explains_the_chats_markers`
Expected: FAIL to compile with "cannot find value `STATUS_COLUMN` in this scope".

- [ ] **Step 3: Add the fixed pin and status columns**

In `crates/scuttle-tui/src/overlay.rs`, after `const FAMILY_COLUMN: usize = 3;`, add:

```rust
/// The column of `chat_cells` that holds the status marker or the unread dot.
const STATUS_COLUMN: usize = 1;

/// The status column's width: the two-cell [`UNREAD`] dot, or a one-cell marker.
const STATUS_WIDTH: u16 = 2;

/// The dot that marks a chat with unread messages, U+1F535, two cells wide under both
/// `width` and `width_cjk`.
const UNREAD: &str = "\u{1f535}";

/// The pin column's width: `icon`'s display width, never under two cells, so the default 📌
/// fits and an empty or one-cell icon still keeps every title in line.
fn pin_width(icon: &str) -> u16 {
    u16::try_from(icon.width()).unwrap_or(u16::MAX).max(2)
}

/// A `/chats` row's status cell, most important first: the spinner while the agent works,
/// `!` after an error, `?` while it waits on the user, then [`UNREAD`], else blank. A chat has
/// one status, so the order decides only against unread: a working chat keeps its spinner,
/// and the dot shows once it goes idle.
fn status_cell(r: &ChatRow, ctx: &ViewCtx) -> Line<'static> {
    let attention = spins(r.status.as_ref())
        || matches!(
            r.status,
            Some(ChatStatus::Error | ChatStatus::RequiresAction)
        );
    // The open chat's stream keeps it read, whatever a refetch says.
    let unread = r.unread && Some(r.id) != ctx.app.chat_id;
    if attention {
        Line::from(Span::styled(
            marker(r.status.as_ref(), ctx.elapsed),
            ctx.theme.accent,
        ))
    } else if unread {
        Line::from(UNREAD)
    } else {
        Line::from(" ")
    }
}
```

Replace `fn chat_cells` with:

```rust
/// A chat row's cells: the pin, the status, the title, the subagent count, the archived tag,
/// and the age; `summaries` adds the dim summary as the last one.
fn chat_cells(r: &ChatRow, ctx: &ViewCtx, summaries: bool) -> Vec<Line<'static>> {
    // A subagent is indented inside the title cell, so the pin and status columns stay put.
    let indent = if r.depth > 0 { "   " } else { "" };
    let pin = if r.pinned { ctx.pin_icon } else { "" };
    let family = if r.children > 0 {
        format!(
            "{}{}",
            family_prefix(r.children),
            marker(r.busiest_child.as_ref(), ctx.elapsed)
        )
    } else {
        String::new()
    };
    let when = r
        .updated_unix
        .map(|t| scuttle_core::time::relative(t, ctx.now_unix))
        .unwrap_or_default();
    let mut cells = vec![
        Line::from(pin.to_owned()),
        status_cell(r, ctx),
        Line::from(format!("{indent}{}", r.title)),
        Line::from(Span::styled(family, ctx.theme.dim)),
        Line::from(Span::styled(
            if r.archived { "archived" } else { "" },
            ctx.theme.dim,
        )),
        Line::from(Span::styled(when, ctx.theme.dim)),
    ];
    if summaries {
        cells.push(Line::from(Span::styled(
            r.summary.clone().unwrap_or_default(),
            ctx.theme.dim,
        )));
    }
    cells
}
```

In `fn chat_spinners`, in the first `Spinner`, replace `column: 0,` with `column: STATUS_COLUMN,`.

In `fn chats_view`, replace the first two entries of `let mut widths = vec![..]`:

```rust
        Constraint::Length(1),
        Constraint::Length(1),
```

with:

```rust
        Constraint::Length(pin_width(ctx.pin_icon)),
        Constraint::Length(STATUS_WIDTH),
```

The pin width is computed once per view from `chats.pin_icon`, so every row gets the same column, and the view cache rebuilds it whenever it rebuilds the rows.

- [ ] **Step 4: Explain the markers in `/help`**

In `crates/scuttle-tui/src/help.rs`, after `pub const CHATS_SEARCH`, add:

```rust
/// What the columns before a `/chats` title show, which `/help` explains after the search.
/// The order is `overlay::status_cell`'s.
pub const CHATS_MARKERS: &[&str] = &[
    "Each row in /chats starts with two fixed columns, so every title lines up: the pin, from chats.pin_icon, then the chat's status.",
    "The status shows the most important of these: a spinner while the agent works, ! after an error, ? while it waits on you, then 🔵 for unread messages.",
    "A working chat that is also unread keeps its spinner, and the 🔵 shows once it stops.",
    "+N after a title counts its subagents, followed by the busiest one's marker, and a subagent is indented under its parent.",
];
```

In `help_lines_for`, after the `for sentence in CHATS_SEARCH { .. }` loop and before the final `lines`, add:

```rust
    lines.push(Line::default());
    lines.push(Line::from(Span::styled("Markers in /chats", theme.accent)));
    for sentence in CHATS_MARKERS {
        lines.extend(wrap_line(&Line::from(*sentence), width));
    }
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test --workspace -- every_title_starts unread pin narrow_chats help_ chats the_summary_is_a_dim_last_column a_large_family old_chats`
Expected: PASS; the summary, family, and age tests prove the columns after the title did not move.

- [ ] **Step 6: Run the full checks**

Run: `cargo fmt --all && cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check`
Expected: all green, with no insta snapshot changed.

- [ ] **Step 7: Commit**

```bash
git add crates/scuttle-tui/src/overlay.rs crates/scuttle-tui/src/help.rs
git commit -m "feat(scuttle-tui): line up /chats titles behind fixed pin and status columns" \
  -m "Every /chats row now starts with a pin column as wide as chats.pin_icon, at least two cells, and one two-cell status column, so all titles start in the same column. The status shows the spinner, then !, then ?, then a blue dot for unread, so a running chat keeps its spinner and shows the dot once it stops. /help explains the markers." \
  -m "Assisted-by: AI"
```

---

### Task 14: Show each chat's pull request and its state in `/chats`

The chat list response already carries each chat's pull request: `GET /api/v2/chats` fills `diff_status` on every listed chat and its children (`listChats` in `coderd/exp_chats.go` batch-loads them with `GetChatDiffStatusesByChatIDs`), and the watch socket's `diff_status_change` event updates it.
scuttle stores the generated `types::CodersdkChat` for every row, so `chat.diff_status` is already there, but `ChatRow` drops it.
The fields are `CodersdkChatDiffStatus::pr_number: Option<i64>`, `pull_request_state: Option<String>`, and `pull_request_draft: Option<bool>`.
The server sends `pull_request_state` as free text, normalized by `gitprovider.PRState` to `open`, `closed`, or `merged`, and omits it when the chat has no pull request; draft is the separate boolean, not a state.
The web UI's `getPRIconConfig` (`site/src/pages/AgentsPage/components/ChatsSidebar/tree/statusConfig.ts`) checks merged, then closed, then draft, and calls any other non-empty state open; this task uses the same order.
The web UI colors open green, draft secondary gray, merged purple, and closed red.

This task adds a pull request column after the age.
It draws through Task 19's icon module and its top-level `icons` key, so it runs after Task 19, and it adds no config key of its own.
With `icons = "nerd"` (or `NERD_FONT=1`), the cell is a Nerd Font Octicons glyph in the state's color, then the dim `#123`.
The glyphs are nf-oct-git_pull_request (U+F407) for open, nf-oct-git_pull_request_draft (U+F4DD) for draft, nf-oct-git_merge (U+F419) for merged, and nf-oct-git_pull_request_closed (U+F4DC) for closed, read from the `cmap` and `post` tables of a patched Nerd Font.
Each glyph takes Task 19's two-cell slot, the glyph and a space, so a nerd cell reads `\u{f419} #12`.
In text mode, the default, the cell spells the state out instead, as in `PR #123 merged`, with the state word in its color.
The state takes its color from new `Theme` styles that are plain under `NO_COLOR`, so neither the glyph nor the state word is colored there.
The column shows from 120 columns, wider than the summary's 100, so a narrowing overlay drops the pull requests before the summaries, and it stays out when no listed chat has a pull request.
`/git`'s "Pull request" row gets the same glyph in nerd mode, since this task owns `PrState`; its text-mode row is unchanged.

On top of Task 13's cells, the indexes before the new column stay as they are: pin 0, status 1, title 2, family 3, archived 4, and age 5, so `STATUS_COLUMN` and `FAMILY_COLUMN` keep their values.
From 120 columns, the pull request is the new cell 6 (`PR_COLUMN`) and the summary moves from cell 6 to cell 7.
Between 100 and 119 columns, the summary stays cell 6, and below 100 it stays out, so `the_summary_is_a_dim_last_column_on_a_wide_overlay_only` (at 100 columns) and Task 13's `narrow_chats_drop_the_summary_first_and_keep_the_titles_in_line` (at 60 to 100) keep their assertions.
No test at 120 columns or more draws `/chats` before this task, and no insta snapshot draws `/chats` or `/git`.
`ViewCtx` does not change: the cell reads the icon set from `ctx.theme.icons`.
Task 19's `every_icon_is_a_two_cell_nerd_slot_with_a_text_fallback` and `text_falls_back_to_what_scuttle_showed_before_icons` cover the four new icons without a change, since their text fallback is empty.

**Files:**
- Modify: `crates/scuttle-core/src/chat_list.rs` (new `PrState`, `PrBadge`, and `pr_badge`; `ChatRow` and `row`)
- Modify: `crates/scuttle-tui/src/icons.rs` (new `Icon::PrOpen`, `Icon::PrDraft`, `Icon::PrMerged`, and `Icon::PrClosed`; `Icon::ALL`, `Icon::nerd`, and new `pr_icon`)
- Modify: `crates/scuttle-tui/src/theme.rs` (`Theme`'s new `pr_open`, `pr_draft`, `pr_merged`, and `pr_closed`, and `Theme::pr`)
- Modify: `crates/scuttle-tui/src/overlay.rs` (new `PR_COLUMN`, `PR_MIN_WIDTH`, and `pr_cell`; `chat_cells`, `chats_view`, and `git_view`)
- Modify: `crates/scuttle-tui/src/help.rs` (`CHATS_MARKERS` and `CHATS_MARKERS_NERD`)
- Test: `mod tests` of `chat_list.rs`, `icons.rs`, `theme.rs`, `overlay.rs`, and `help.rs`

**Interfaces:**
- Consumes: Task 19's `Icon`, `IconSet`, `slot(set: IconSet, icon: Icon) -> Slot`, `lead(theme: &Theme, icon: Icon, base: Style) -> Option<Span<'static>>`, and `style(theme: &Theme, base: Style) -> Style` in `icons.rs`, `Theme::icons`, `Theme::colors`, Task 19's `chat_cells` with its archived icon, `CHATS_MARKERS_NERD`, and the `chat_row` and `nerd_theme` test helpers in `overlay.rs`; Task 13's `chat_cells(r: &ChatRow, ctx: &ViewCtx, summaries: bool)`, `pin_width`, `status_cell`, `STATUS_WIDTH`, and `CHATS_MARKERS`; `cell_text`, `ctx_for`, and `pinned_app` (existing, `overlay.rs` tests); `listed` (existing, `chat_list.rs` tests); `Colors` (existing, `theme.rs`); `GitPanel`, `Fetched`, and `LocalGit` (existing, `panels.rs`).
- Produces:
  - `pub enum PrState { Open, Draft, Merged, Closed }` with `PrState::label(self) -> &'static str`, `pub struct PrBadge { pub number: Option<i64>, pub state: PrState }`, `pub fn pr_badge(chat: &types::CodersdkChat) -> Option<PrBadge>`, and `ChatRow::pr: Option<PrBadge>` in `chat_list.rs`.
  - `Icon::PrOpen`, `Icon::PrDraft`, `Icon::PrMerged`, `Icon::PrClosed`, and `pub fn pr_icon(state: PrState) -> Icon` in `icons.rs`.
  - `Theme::pr(&self, state: PrState) -> Style` in `theme.rs`.
  - `const PR_COLUMN: usize = 6`, `const PR_MIN_WIDTH: u16 = 120`, and `fn pr_cell(pr: Option<&PrBadge>, ctx: &ViewCtx) -> Line<'static>` in `overlay.rs`.
  - `chat_cells(r: &ChatRow, ctx: &ViewCtx, prs: bool, summaries: bool)`.
  - No later task consumes these.

- [ ] **Step 1: Write the failing tests**

In `mod tests` of `crates/scuttle-core/src/chat_list.rs`, add:

```rust
    #[test]
    fn a_row_carries_its_pull_request_in_the_web_uis_order() {
        let pr = |state: Option<&str>, draft: bool, number: Option<i64>| {
            let mut c = listed(Uuid::new_v4(), "t", "2026-09-30T10:00:00Z");
            c.diff_status = Some(types::CodersdkChatDiffStatus {
                pull_request_state: state.map(str::to_owned),
                pull_request_draft: Some(draft),
                pr_number: number,
                ..Default::default()
            });
            pr_badge(&c)
        };
        let badge = |number, state| Some(PrBadge { number, state });
        assert_eq!(
            pr(Some("merged"), true, Some(12)),
            badge(Some(12), PrState::Merged),
            "merged wins over draft"
        );
        assert_eq!(
            pr(Some("closed"), true, Some(78)),
            badge(Some(78), PrState::Closed)
        );
        assert_eq!(
            pr(Some("open"), true, Some(56)),
            badge(Some(56), PrState::Draft)
        );
        assert_eq!(
            pr(Some("open"), false, Some(34)),
            badge(Some(34), PrState::Open)
        );
        assert_eq!(
            pr(Some("locked"), false, None),
            badge(None, PrState::Open),
            "any other state reads as open"
        );
        assert_eq!(pr(None, true, Some(9)), None, "no state means no pull request");
        assert_eq!(pr(Some(" "), false, Some(9)), None);
        let plain = listed(Uuid::new_v4(), "t", "2026-09-30T10:00:00Z");
        assert_eq!(pr_badge(&plain), None);
        let mut merged = plain.clone();
        merged.diff_status = Some(types::CodersdkChatDiffStatus {
            pull_request_state: Some("merged".into()),
            pr_number: Some(12),
            ..Default::default()
        });
        assert_eq!(
            row(&merged, Uuid::new_v4(), 0).pr,
            badge(Some(12), PrState::Merged)
        );
        assert_eq!(
            [PrState::Open, PrState::Draft, PrState::Merged, PrState::Closed].map(PrState::label),
            ["open", "draft", "merged", "closed"]
        );
    }
```

In `mod tests` of `crates/scuttle-tui/src/icons.rs`, add:

```rust
    #[test]
    fn each_pull_request_state_has_its_octicon_and_no_text_icon() {
        use scuttle_core::chat_list::PrState;
        let glyph = |state| slot(IconSet::Nerd, pr_icon(state)).text;
        assert_eq!(glyph(PrState::Open), "\u{f407} ");
        assert_eq!(glyph(PrState::Draft), "\u{f4dd} ");
        assert_eq!(glyph(PrState::Merged), "\u{f419} ");
        assert_eq!(glyph(PrState::Closed), "\u{f4dc} ");
        for state in [PrState::Open, PrState::Draft, PrState::Merged, PrState::Closed] {
            assert_eq!(
                slot(IconSet::Text, pr_icon(state)).text,
                "",
                "text mode spells {state:?} out in the cell instead"
            );
        }
    }
```

In `mod tests` of `crates/scuttle-tui/src/theme.rs`, add:

```rust
    #[test]
    fn pull_request_states_take_the_web_uis_colors_and_none_under_no_color() {
        use scuttle_core::chat_list::PrState;
        let t = Theme::terminal_with(true, Colors::Ansi256);
        assert_eq!(t.pr(PrState::Open).fg, Some(Color::Green));
        assert_eq!(t.pr(PrState::Draft).fg, Some(Color::DarkGray));
        assert_eq!(t.pr(PrState::Merged).fg, Some(Color::Magenta));
        assert_eq!(t.pr(PrState::Closed).fg, Some(Color::Red));
        let plain = Theme::terminal_with(true, Colors::None);
        for state in [PrState::Open, PrState::Draft, PrState::Merged, PrState::Closed] {
            assert_eq!(plain.pr(state), Style::new(), "{state:?}");
        }
    }
```

In `mod tests` of `crates/scuttle-tui/src/overlay.rs`, after `narrow_chats_drop_the_summary_first_and_keep_the_titles_in_line`, add:

```rust
    /// An app listing one chat per pull request state and one with no pull request, each with
    /// a summary, titled by what it shows.
    fn pr_app() -> App {
        let chat = |title: &str, minute: u32, pr: serde_json::Value| {
            json!({"id": uuid::Uuid::new_v4(), "title": title, "status": "waiting",
                "updated_at": format!("2026-09-30T10:{minute:02}:00Z"), "children": [],
                "last_turn_summary": "Fixing the CI", "diff_status": pr, "files": [],
                "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}})
        };
        let mut app = App::new(BusyBehavior::Queue, true);
        app.update(Msg::ChatsLoaded {
            query: ListQuery::Default,
            offset: 0,
            chats: serde_json::from_value(json!([
                chat("t-merged", 50, json!({"pr_number": 12, "pull_request_state": "merged",
                    "pull_request_draft": false})),
                chat("t-open", 49, json!({"pr_number": 34, "pull_request_state": "open",
                    "pull_request_draft": false})),
                chat("t-draft", 48, json!({"pr_number": 56, "pull_request_state": "open",
                    "pull_request_draft": true})),
                chat("t-closed", 47, json!({"pr_number": 78, "pull_request_state": "closed",
                    "pull_request_draft": false})),
                chat("t-none", 46, json!({"pull_request_draft": false})),
            ]))
            .unwrap(),
        });
        app
    }

    #[test]
    fn a_wide_chats_list_shows_each_pull_request_state_and_number() {
        let app = pr_app();
        let theme = Theme {
            icons: IconSet::Nerd,
            ..Theme::terminal(true)
        };
        let ctx = ViewCtx {
            width: PR_MIN_WIDTH,
            ..ctx_for(&app, &theme)
        };
        let view = Overlay::chats(String::new(), &app).view(&ctx);
        assert_eq!(
            view.widths.len(),
            8,
            "pin, status, title, family, archived, age, pull request, summary"
        );
        let pr = |title| cell_text(&view, chat_row(&view, title), PR_COLUMN);
        assert_eq!(pr("t-merged"), "\u{f419} #12");
        assert_eq!(pr("t-open"), "\u{f407} #34");
        assert_eq!(pr("t-draft"), "\u{f4dd} #56");
        assert_eq!(pr("t-closed"), "\u{f4dc} #78");
        assert_eq!(pr("t-none"), "");
        assert_eq!(
            cell_text(&view, chat_row(&view, "t-open"), PR_COLUMN + 1),
            "Fixing the CI",
            "the summary stays last"
        );
        assert_eq!(
            view.widths[PR_COLUMN],
            Constraint::Length(5),
            "as wide as the widest cell"
        );
    }

    #[test]
    fn text_icons_spell_out_the_pr_state() {
        let app = pr_app();
        let theme = Theme::terminal(true);
        let ctx = ViewCtx {
            width: PR_MIN_WIDTH,
            ..ctx_for(&app, &theme)
        };
        let view = Overlay::chats(String::new(), &app).view(&ctx);
        let pr = |title| cell_text(&view, chat_row(&view, title), PR_COLUMN);
        assert_eq!(pr("t-merged"), "PR #12 merged");
        assert_eq!(pr("t-open"), "PR #34 open");
        assert_eq!(pr("t-draft"), "PR #56 draft");
        assert_eq!(pr("t-closed"), "PR #78 closed");
        assert_eq!(view.widths[PR_COLUMN], Constraint::Length(13));
    }

    #[test]
    fn the_pr_state_takes_its_color_unless_no_color_is_set() {
        use crate::theme::Colors;
        use ratatui::style::Color;
        let app = pr_app();
        for (colors, merged, closed) in [
            (Colors::Ansi256, Some(Color::Magenta), Some(Color::Red)),
            (Colors::None, None, None),
        ] {
            for icons in [IconSet::Nerd, IconSet::Text] {
                let theme = Theme {
                    icons,
                    ..Theme::terminal_with(true, colors)
                };
                let ctx = ViewCtx {
                    width: PR_MIN_WIDTH,
                    ..ctx_for(&app, &theme)
                };
                let view = Overlay::chats(String::new(), &app).view(&ctx);
                // The glyph is the first span; in words, the state follows the dim number.
                let state = |title| {
                    let cell = &view.rows[chat_row(&view, title)].cells[PR_COLUMN];
                    let at = if icons == IconSet::Nerd { 0 } else { 1 };
                    cell.spans[at].style.fg
                };
                assert_eq!(state("t-merged"), merged, "{colors:?} {icons:?}");
                assert_eq!(state("t-closed"), closed, "{colors:?} {icons:?}");
            }
        }
    }

    #[test]
    fn the_pr_column_drops_before_the_summary() {
        let app = pr_app();
        let theme = Theme::terminal(true);
        let o = Overlay::chats(String::new(), &app);
        let at = |width: u16| {
            o.view(&ViewCtx {
                width,
                ..ctx_for(&app, &theme)
            })
        };
        assert_eq!(at(PR_MIN_WIDTH).widths.len(), 8);
        let view = at(PR_MIN_WIDTH - 1);
        assert_eq!(view.widths.len(), 7, "the pull requests go first");
        assert_eq!(
            cell_text(&view, chat_row(&view, "t-open"), 6),
            "Fixing the CI",
            "the summary takes cell 6 again"
        );
        assert_eq!(at(SUMMARY_MIN_WIDTH - 1).widths.len(), 6);
        let no_prs = pinned_app("Fixing the CI");
        let view = Overlay::chats(String::new(), &no_prs).view(&ViewCtx {
            width: PR_MIN_WIDTH,
            ..ctx_for(&no_prs, &theme)
        });
        assert_eq!(
            view.widths.len(),
            7,
            "with no pull request listed, the column stays out"
        );
    }

    #[test]
    fn the_git_panel_leads_the_pull_request_with_its_state_icon() {
        use scuttle_core::panels::{Fetched, GitPanel, LocalGit};
        let mut app = App::new(BusyBehavior::Queue, true);
        let chat = serde_json::from_value(json!({
            "id": uuid::Uuid::new_v4(),
            "diff_status": {"pr_number": 12, "pull_request_title": "Fix the watch",
                "pull_request_state": "merged", "pull_request_draft": false},
            "children": [], "files": [], "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}
        }))
        .unwrap();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: Box::new(chat),
            messages: vec![],
        });
        app.git_panel = Some(GitPanel {
            diff: Fetched::Loading,
            repos: Default::default(),
            local: LocalGit::NoWorkspace,
        });
        let pr_row = |theme: &Theme| {
            Overlay::Git(TableState::default())
                .view(&ctx_for(&app, theme))
                .rows
                .into_iter()
                .find(|r| r.cells[0].to_string() == "Pull request")
                .expect("a pull request row")
        };
        assert_eq!(
            pr_row(&Theme::terminal(true)).cells[1].to_string(),
            "#12 Fix the watch (merged)",
            "text mode keeps the row as it was"
        );
        let nerd = Theme {
            icons: IconSet::Nerd,
            ..Theme::terminal(true)
        };
        let row = pr_row(&nerd);
        assert_eq!(
            row.cells[1].to_string(),
            "\u{f419} #12 Fix the watch (merged)"
        );
        assert_eq!(
            row.cells[1].spans[0].style.fg,
            Some(ratatui::style::Color::Magenta),
            "the glyph takes the state's color, as in /chats"
        );
    }
```

In `mod tests` of `crates/scuttle-tui/src/help.rs`, add:

```rust
    #[test]
    fn help_explains_the_pull_request_column_in_both_icon_sets() {
        let shown = text(&help_lines(&Theme::terminal(true), 300)).join("\n");
        for needle in [
            "From 120 columns",
            "PR #123 merged",
            "open, draft, merged, or closed",
        ] {
            assert!(shown.contains(needle), "{needle} is missing from:\n{shown}");
        }
        let nerd = Theme {
            icons: IconSet::Nerd,
            ..Theme::terminal(true)
        };
        let shown = text(&help_lines(&nerd, 300)).join("\n");
        for needle in [
            "From 120 columns",
            "\u{f407} open",
            "\u{f4dd} draft",
            "\u{f419} merged",
            "\u{f4dc} closed",
        ] {
            assert!(shown.contains(needle), "{needle} is missing from:\n{shown}");
        }
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --workspace -- pull_request pr_state pr_column`
Expected: FAIL to compile with "cannot find function `pr_badge` in this scope" and "cannot find function `pr_icon` in this scope".

- [ ] **Step 3: Read the pull request into `ChatRow`**

In `crates/scuttle-core/src/chat_list.rs`, before `pub struct ChatRow`, add:

```rust
/// A pull request's state as `/chats` shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrState {
    Open,
    Draft,
    Merged,
    Closed,
}

impl PrState {
    /// The state as a word, as text icons spell it out.
    pub fn label(self) -> &'static str {
        match self {
            PrState::Open => "open",
            PrState::Draft => "draft",
            PrState::Merged => "merged",
            PrState::Closed => "closed",
        }
    }
}

/// The pull request attached to a chat: its number, when the server knows it, and its state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PrBadge {
    pub number: Option<i64>,
    pub state: PrState,
}

/// The pull request `chat`'s diff status names, read as the web UI's `getPRIconConfig` reads
/// it: merged, then closed, then draft, and any other state open. The server leaves the state
/// out when the chat has no pull request.
pub fn pr_badge(chat: &types::CodersdkChat) -> Option<PrBadge> {
    let status = chat.diff_status.as_ref()?;
    let state = status
        .pull_request_state
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())?;
    let state = match state {
        "merged" => PrState::Merged,
        "closed" => PrState::Closed,
        _ if status.pull_request_draft == Some(true) => PrState::Draft,
        _ => PrState::Open,
    };
    Some(PrBadge {
        number: status.pr_number,
        state,
    })
}
```

In `pub struct ChatRow`, after `pub summary: Option<String>,`, add:

```rust
    /// The attached pull request, from the chat's diff status.
    pub pr: Option<PrBadge>,
```

In `fn row`, after the `summary: ..` field, add:

```rust
        pr: pr_badge(chat),
```

- [ ] **Step 4: Add the pull request icons**

In `crates/scuttle-tui/src/icons.rs`, add `use scuttle_core::chat_list::PrState;` after `use ratatui::text::Span;`.
In `pub enum Icon`, after the `ServerOff` variant, add:

```rust
    /// An open pull request. nf-oct-git_pull_request.
    PrOpen,
    /// A draft pull request. nf-oct-git_pull_request_draft.
    PrDraft,
    /// A merged pull request. nf-oct-git_merge.
    PrMerged,
    /// A closed pull request. nf-oct-git_pull_request_closed.
    PrClosed,
```

In `Icon::ALL`, replace `pub const ALL: [Icon; 28] = [` with `pub const ALL: [Icon; 32] = [`, and replace the array's last line, `        Icon::ServerOff,`, with:

```rust
        Icon::ServerOff,
        Icon::PrOpen,
        Icon::PrDraft,
        Icon::PrMerged,
        Icon::PrClosed,
```

In `Icon::nerd`, after the arm `Icon::ServerOff => "\u{ebb5} ",`, add:

```rust
            Icon::PrOpen => "\u{f407} ",
            Icon::PrDraft => "\u{f4dd} ",
            Icon::PrMerged => "\u{f419} ",
            Icon::PrClosed => "\u{f4dc} ",
```

`Icon::text` ends with `_ => ""`, so the four icons draw nothing in text mode and the cell spells the state out.
After `pub fn tool_icon`, add:

```rust
/// The icon for a pull request in `state`, in `/chats` and `/git`.
pub fn pr_icon(state: PrState) -> Icon {
    match state {
        PrState::Open => Icon::PrOpen,
        PrState::Draft => Icon::PrDraft,
        PrState::Merged => Icon::PrMerged,
        PrState::Closed => Icon::PrClosed,
    }
}
```

- [ ] **Step 5: Add the pull request styles to the theme**

In `crates/scuttle-tui/src/theme.rs`, add `use scuttle_core::chat_list::PrState;` after the `scuttle_core::config::IconSet` import.
In `pub struct Theme`, after `pub link_hover: Style,`, add:

```rust
    /// A pull request's state in `/chats`, as the web UI colors it: open green, draft dim,
    /// merged magenta, and closed red. Under `NO_COLOR` none has a color.
    pub pr_open: Style,
    pub pr_draft: Style,
    pub pr_merged: Style,
    pub pr_closed: Style,
```

In `Theme::terminal_with`, after `let link_hover = ..;`, add:

```rust
        let state = |c: Color| {
            if colors == Colors::None {
                Style::new()
            } else {
                Style::new().fg(c)
            }
        };
```

and after `link_hover,` in the `Theme { .. }` literal, add:

```rust
            pr_open: state(Color::Green),
            pr_draft: state(dim),
            pr_merged: state(Color::Magenta),
            pr_closed: state(Color::Red),
```

After `Theme::terminal_with`, add:

```rust
    /// The style of a pull request in `state`.
    pub fn pr(&self, state: PrState) -> Style {
        match state {
            PrState::Open => self.pr_open,
            PrState::Draft => self.pr_draft,
            PrState::Merged => self.pr_merged,
            PrState::Closed => self.pr_closed,
        }
    }
```

- [ ] **Step 6: Add the pull request column and the `/git` glyph**

In `crates/scuttle-tui/src/overlay.rs`, replace `use scuttle_core::chat_list::{ChatRow, Filter, ListQuery, Load, chat_status};` with:

```rust
use scuttle_core::chat_list::{ChatRow, Filter, ListQuery, Load, PrBadge, chat_status, pr_badge};
```

After `const SUMMARY_MIN_WIDTH: u16 = 100;`, add:

```rust
/// The column of `chat_cells` that holds the pull request, when the overlay shows it.
const PR_COLUMN: usize = 6;

/// The narrowest `/chats` that shows the pull request column. It is wider than
/// `SUMMARY_MIN_WIDTH`, so a narrowing overlay drops the pull requests before the summaries.
const PR_MIN_WIDTH: u16 = 120;

/// A chat's pull request cell: with Nerd Font icons, the state's glyph in its color and the
/// dim `#123`; with text icons, the dim `PR #123 ` and the state's word in its color. Empty
/// for a chat without a pull request.
fn pr_cell(pr: Option<&PrBadge>, ctx: &ViewCtx) -> Line<'static> {
    let Some(pr) = pr else {
        return Line::default();
    };
    let style = ctx.theme.pr(pr.state);
    let number = pr.number.map(|n| format!("#{n}"));
    match icons::lead(ctx.theme, icons::pr_icon(pr.state), style) {
        Some(glyph) => {
            let mut spans = vec![glyph];
            spans.extend(number.map(|n| Span::styled(n, ctx.theme.dim)));
            Line::from(spans)
        }
        None => {
            let prefix = match number {
                Some(number) => format!("PR {number} "),
                None => "PR ".to_owned(),
            };
            Line::from(vec![
                Span::styled(prefix, ctx.theme.dim),
                Span::styled(pr.state.label(), style),
            ])
        }
    }
}
```

Replace `fn chat_cells` with:

```rust
/// A chat row's cells: the pin, the status, the title, the subagent count, the archived tag,
/// and the age; `prs` adds the pull request, and `summaries` adds the dim summary as the last.
fn chat_cells(r: &ChatRow, ctx: &ViewCtx, prs: bool, summaries: bool) -> Vec<Line<'static>> {
    // A subagent is indented inside the title cell, so the pin and status columns stay put.
    let indent = if r.depth > 0 { "   " } else { "" };
    let pin = if r.pinned { ctx.pin_icon } else { "" };
    let family = if r.children > 0 {
        format!(
            "{}{}",
            family_prefix(r.children),
            marker(r.busiest_child.as_ref(), ctx.elapsed)
        )
    } else {
        String::new()
    };
    let when = r
        .updated_unix
        .map(|t| scuttle_core::time::relative(t, ctx.now_unix))
        .unwrap_or_default();
    let archived = if r.archived {
        icons::slot(ctx.theme.icons, Icon::Archived).text
    } else {
        ""
    };
    let mut cells = vec![
        Line::from(pin.to_owned()),
        status_cell(r, ctx),
        Line::from(format!("{indent}{}", r.title)),
        Line::from(Span::styled(family, ctx.theme.dim)),
        Line::from(Span::styled(
            archived,
            icons::style(ctx.theme, ctx.theme.dim),
        )),
        Line::from(Span::styled(when, ctx.theme.dim)),
    ];
    if prs {
        cells.push(pr_cell(r.pr.as_ref(), ctx));
    }
    if summaries {
        cells.push(Line::from(Span::styled(
            r.summary.clone().unwrap_or_default(),
            ctx.theme.dim,
        )));
    }
    cells
}
```

In `fn chats_view`, after the `let family_width = ..;` statement, add:

```rust
    // From `PR_MIN_WIDTH`, while a listed chat has a pull request, as wide as the widest cell.
    let pr_width = chat_rows
        .iter()
        .map(|r| pr_cell(r.pr.as_ref(), ctx).width())
        .max()
        .unwrap_or(0) as u16;
    let prs = ctx.width >= PR_MIN_WIDTH && pr_width > 0;
```

Replace `chat_cells(r, ctx, summaries)` with `chat_cells(r, ctx, prs, summaries)`.
After the `let mut widths = vec![..];` statement and before `if summaries {`, add:

```rust
    if prs {
        widths.push(Constraint::Length(pr_width));
    }
```

In `fn git_view`, replace `let mut rows = label_rows(scuttle_core::panels::git_lines(ctx.app), ctx.theme);` with:

```rust
    let mut rows = label_rows(scuttle_core::panels::git_lines(ctx.app), ctx.theme);
    // With Nerd Font icons the pull request leads with its state's glyph, as in `/chats`;
    // text icons draw none, so the row stays as `git_lines` words it.
    if let Some(pr) = ctx.app.chat.as_deref().and_then(pr_badge)
        && let Some(glyph) = icons::lead(
            ctx.theme,
            icons::pr_icon(pr.state),
            ctx.theme.pr(pr.state),
        )
        && let Some(row) = rows
            .iter_mut()
            .find(|r| r.cells.first().is_some_and(|c| c.to_string() == "Pull request"))
        && let Some(value) = row.cells.get_mut(1)
    {
        let text = value.to_string();
        *value = Line::from(vec![glyph, Span::raw(text)]);
    }
```

- [ ] **Step 7: Explain the column in `/help`**

In `crates/scuttle-tui/src/help.rs`, add this last entry to `CHATS_MARKERS`:

```rust
    "From 120 columns, a column after the age shows the chat's pull request and its state, such as PR #123 merged, for open, draft, merged, or closed.",
```

and this last entry to `CHATS_MARKERS_NERD`:

```rust
    "From 120 columns, a column after the age shows the chat's pull request: \u{f407} open, \u{f4dd} draft, \u{f419} merged, or \u{f4dc} closed, then its number.",
```

- [ ] **Step 8: Run the tests to verify they pass**

Run: `cargo test --workspace -- pull_request pr_state pr_column icon chats help_ every_title_starts narrow_chats the_git_panel`
Expected: PASS, including Task 13's and Task 19's `/chats` tests and `the_summary_is_a_dim_last_column_on_a_wide_overlay_only` unchanged.

- [ ] **Step 9: Run the full checks**

Run: `cargo fmt --all && cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check`
Expected: all green, with no insta snapshot changed.

- [ ] **Step 10: Commit**

```bash
git add crates/scuttle-core/src/chat_list.rs crates/scuttle-tui/src/icons.rs crates/scuttle-tui/src/theme.rs crates/scuttle-tui/src/overlay.rs crates/scuttle-tui/src/help.rs
git commit -m "feat: show each chat's pull request and its state in /chats" \
  -m "From 120 columns, /chats adds a column after the age with the chat's pull request. With icons = \"nerd\" it is a Nerd Font glyph for open, draft, merged, or closed in the state's color and its number, and /git's pull request row gets the same glyph; text icons spell it out as PR #123 merged. The state follows the web UI's order, NO_COLOR leaves it plain, and the column drops before the summary." \
  -m "Assisted-by: AI"
```

---

### Task 15: Stop underlining the line being typed, and restore the composer's bottom rule

ratatui-textarea 0.9.2 gives every `TextArea` a `cursor_line_style` of `Style::default().add_modifier(Modifier::UNDERLINED)` (`TextArea::default` in its `textarea.rs`), so the composer underlines the line the cursor is on.
Feedback item 24 was this underline, and M2.5 Task 2 removed the composer's bottom rule for it by mistake.
The composer is the only place scuttle builds a `TextArea`: `Composer::new` and `Composer::set_text` both build one and both call `Composer::configure`, so clearing the style there covers every composer text area.
The one-line editors (renames, titles, and the "Other" answer) are `scuttle_core::line_edit::LineEdit`, drawn by `Tui::draw_editor` as a `Paragraph`, and the overlay filters are plain strings drawn by `table::render`; neither has a cursor-line style, so neither changes.

This task clears the cursor-line style and draws the composer with a rule above and below its text again, so the composer is one row taller and the transcript one row shorter.
The copy notice keeps its place in the top rule, which M2.5's fix wave put there: `Tui::draw_at` draws it at `composer.y`, the top rule's row, and that does not move.
The one-line editor's three-row box still ends at the composer's bottom, and the smallest composer is now three rows, so the box covers the composer exactly instead of borrowing the row above it.

The tests that assert the old height or the missing rule change only by the restored rule:
- `height_grows_with_content_up_to_the_limit`, `long_lines_wrap_and_count_toward_the_height`, `height_counts_the_rows_the_widget_draws`, and `wrapped_rows_get_a_blank_gutter_and_wide_characters_fit` in `composer.rs`: `Composer::height` adds two rows instead of one.
- `settings_creates_the_file_and_applies_the_live_keys` in `app.rs`: four text rows plus two rules.
- `a_tiny_terminal_gives_up_the_margin` in `app.rs`: on a 20 by 6 screen the composer is three rows, so its top rule starts on row 2 instead of row 3.
- `the_composer_keeps_its_top_rule_and_drops_the_bottom_one` in `app.rs` becomes `the_composer_has_a_full_width_rule_above_and_below_its_text`.
- `a_copy_notice_too_wide_for_the_rule_shortens_or_leaves_the_plain_rule` in `app.rs` looked for the last rule on the screen, which is now the bottom rule; it now looks for the second rule from the bottom, the top one.

No insta snapshot changes: the three snapshots in `crates/scuttle-tui/src/snapshots` draw only `transcript_view` and never the composer.
A test this list misses fails only by a row index one higher than before; adapt that index, keep the assertion's meaning, and report it.

**Files:**
- Modify: `crates/scuttle-tui/src/composer.rs` (`Composer::configure` and `Composer::height`; tests)
- Modify: `crates/scuttle-tui/src/app.rs` (`Tui::draw_at`: the composer height and the composer frame; the comment in `Tui::draw_editor`; tests)

**Interfaces:**
- Consumes: `TextArea::set_cursor_line_style` (ratatui-textarea 0.9.2); `Tui::copy`, `screen`, `tui`, and `settings_tui` (existing, `app.rs`).
- Produces: `Composer::height(&self, width: u16) -> u16` now returns the text rows plus two; `Tui::draw_at` gives the composer at least three rows.
  Task 16 relies on the new height in its composer tests.

- [ ] **Step 1: Write the failing tests**

In `mod tests` of `crates/scuttle-tui/src/composer.rs`, change the existing height assertions:

```rust
    // height_grows_with_content_up_to_the_limit
        assert_eq!(c.height(80), 3);
        c.paste("1\n2\n3\n4\n5");
        assert_eq!(c.height(80), 5);

    // long_lines_wrap_and_count_toward_the_height
        assert_eq!(c.height(12), 4 + 2);
        assert_eq!(c.height(80), 1 + 2);

    // height_counts_the_rows_the_widget_draws
            assert_eq!(
                usize::from(c.height(width) - 2),
                drawn,
                "{text:?} at width {width}"
            );

    // wrapped_rows_get_a_blank_gutter_and_wide_characters_fit
        assert_eq!(c.height(width), 4 + 2);
```

Add to the same module:

```rust
    #[test]
    fn no_line_is_underlined_while_typing() {
        use ratatui::style::Modifier;
        let underlined = |c: &Composer| {
            let mut term = Terminal::new(TestBackend::new(30, 3)).unwrap();
            term.draw(|f| f.render_widget(c.widget(), f.area())).unwrap();
            let buf = term.backend().buffer().clone();
            (0..3u16)
                .flat_map(|y| (0..30u16).map(move |x| (x, y)))
                .filter(|&at| buf[at].modifier.contains(Modifier::UNDERLINED))
                .count()
        };
        let mut c = Composer::new(10);
        type_str(&mut c, "the line being typed");
        assert_eq!(underlined(&c), 0);
        c.set_text("a fresh text area\nwith two lines");
        assert_eq!(
            underlined(&c),
            0,
            "set_text builds a new TextArea, and it is cleared too"
        );
        assert_eq!(c.widget().cursor_line_style(), Style::default());
    }
```

In `mod tests` of `crates/scuttle-tui/src/app.rs`, in `settings_creates_the_file_and_applies_the_live_keys`, replace `4 + 1,` with `4 + 2,`.
In `a_tiny_terminal_gives_up_the_margin`, replace:

```rust
        assert!(
            rows[3].starts_with('─'),
            "the composer border starts at column 0: {rows:?}"
        );
```

with:

```rust
        assert!(
            rows[2].starts_with('─'),
            "the composer's top rule starts at column 0: {rows:?}"
        );
```

Replace the whole test `the_composer_keeps_its_top_rule_and_drops_the_bottom_one` with:

```rust
    #[test]
    fn the_composer_has_a_full_width_rule_above_and_below_its_text() {
        let mut t = tui();
        let (w, h) = (60u16, 16u16);
        let shown = screen(&mut t, w, h);
        let rows: Vec<&str> = shown.lines().collect();
        let footer = h as usize - 2;
        assert!(rows[footer - 2].contains("Message the agent"), "{shown}");
        for (row, which) in [(footer - 3, "above"), (footer - 1, "below")] {
            let rule = rows[row].trim();
            assert!(
                rule.chars().count() == usize::from(w) - 2 && rule.chars().all(|c| c == '─'),
                "a full-width rule sits {which} the text row: {shown}"
            );
        }
    }

    #[test]
    fn the_copy_notice_stays_in_the_top_rule_over_a_plain_bottom_rule() {
        let mut t = tui();
        t.copy("abc".into());
        let shown = screen(&mut t, 60, 16);
        let rows: Vec<&str> = shown.lines().collect();
        let footer = 16 - 2;
        assert!(
            rows[footer - 3]
                .trim_start()
                .starts_with("── Copied 3 characters ──"),
            "{shown}"
        );
        let bottom = rows[footer - 1].trim();
        assert!(
            !bottom.is_empty() && bottom.chars().all(|c| c == '─'),
            "the bottom rule never carries the notice: {shown}"
        );
    }
```

In `a_copy_notice_too_wide_for_the_rule_shortens_or_leaves_the_plain_rule`, replace the body of the `rule_at` closure with:

```rust
            let shown = screen(t, w, 20);
            // The composer's top rule, the second rule from the bottom now that one sits
            // below the text too.
            shown
                .lines()
                .map(str::trim)
                .filter(|r| r.starts_with('─'))
                .rev()
                .nth(1)
                .unwrap_or_else(|| panic!("no top rule:\n{shown}"))
                .to_owned()
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p scuttle-tui -- height no_line_is_underlined tiny_terminal rule_above_and_below copy_notice settings_creates`
Expected: FAIL, with `no_line_is_underlined_while_typing` counting underlined cells, the height tests one row short, and `the_composer_has_a_full_width_rule_above_and_below_its_text` finding no rule below the text.

- [ ] **Step 3: Clear the cursor-line style and count both rules**

In `crates/scuttle-tui/src/composer.rs`, replace `fn configure` with:

```rust
    /// A fresh `TextArea` does not wrap and underlines the cursor's line, and `set_text` makes
    /// a fresh one, so both call this.
    fn configure(&mut self) {
        self.area.set_wrap_mode(WrapMode::WordOrGlyph);
        self.area.set_cursor_line_style(Style::default());
        self.sync_gutter();
    }
```

Replace the doc comment and the last line of `pub fn height`:

```rust
    /// The rows the text takes at `width` columns (the composer's full inner width, gutter
    /// included), never more than `max_lines`, plus the rules above and below it.
    pub fn height(&self, width: u16) -> u16 {
```

and

```rust
        rows.clamp(1, usize::from(self.max_lines)) as u16 + 2
```

- [ ] **Step 4: Draw the bottom rule again**

In `crates/scuttle-tui/src/app.rs`, in `Tui::draw_at`, replace the composer height computation with:

```rust
        // The text rows and the rules above and below them, never under three rows.
        let composer_height = self.composer.height(outer.width).min(
            outer
                .height
                .saturating_sub(2 + activity_height + chips_height + hint_height)
                .max(3),
        );
```

Replace the composer frame:

```rust
        let frame = Block::default()
            .borders(Borders::TOP)
            .border_style(self.theme.dim);
```

with:

```rust
        // The copy notice below is drawn over the top rule, so it never moves the transcript.
        let frame = Block::default()
            .borders(Borders::TOP | Borders::BOTTOM)
            .border_style(self.theme.dim);
```

In `Tui::draw_editor`, replace the comment `// The box needs three rows; a short composer lends it the row above.` with:

```rust
        // The box is three rows, as tall as the smallest composer, and ends at its bottom rule.
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p scuttle-tui -- height no_line_is_underlined tiny_terminal rule_above_and_below copy_notice settings_creates a_drag_copy the_slash_menu editor`
Expected: PASS; `a_drag_copy_says_how_much_it_copied_in_the_composer_rule_for_two_seconds` and `a_copy_moves_no_row_for_a_reader_at_the_bottom` pass unchanged, which proves the notice stayed in the top rule and a copy still moves no row.

- [ ] **Step 6: Run the full checks**

Run: `cargo fmt --all && cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check`
Expected: all green, with no insta snapshot changed.

- [ ] **Step 7: Commit**

```bash
git add crates/scuttle-tui/src/composer.rs crates/scuttle-tui/src/app.rs
git commit -m "fix(scuttle-tui): stop underlining the typed line and restore the composer's bottom rule" \
  -m "ratatui-textarea underlines the cursor's line by default, which is what read as a rule under the text. The composer now clears that style on every text area it builds, and draws a full-width rule below the text again, as before M2.5. The copy notice stays in the top rule." \
  -m "Assisted-by: AI"
```

---

### Task 16: Turn a large paste into a snippet that goes as a text file

Today `Tui::handle` hands every bracketed paste to `Composer::paste`, which inserts it as raw text.
The web UI turns a large paste into an attachment instead (`isLargePaste` in `site/src/pages/AgentsPage/components/ChatMessageInput/pasteHelpers.ts`): a paste of 10 or more lines (a trailing newline counts) or 1000 or more characters becomes a `text/plain` file, uploaded through `POST /api/v2/chats/files?organization=<id>` like any attachment and shown as a chip.
The pasted text never enters the message: the sent content is the typed text parts, then one `{"type": "file", "file_id": ..}` part per file, and the chip's "Paste inline" button puts the text back into the editor.

This task follows the web UI's threshold rather than the 3 lines or 800 characters first suggested, so a scuttle user and a web UI user who paste the same text send the same message, and a paste that fits the composer's default ten rows stays inline.
A large paste shows in the composer as one token, `[Pasted text #1 +120 lines]`, or `[Pasted text #2 +1500 chars]` for one long line, numbered per session.
On send, each token leaves the text, as the web UI leaves the paste out, and its snippet goes as the file `paste-<n>.txt`: a chip the core uploads with the message through the same `upload_chat_file` call `/attach` uses.
The generated client sends every upload as `application/octet-stream`, and the SDK is out of scope here, so the `.txt` name and the server's content sniffing (`chatfiles.PrepareStoredFile`) store the paste as `text/plain`, as they already do for an attached `.txt` file.
A message needs text of its own, as it does with other attachments, so a draft that is only a token stays in the composer with a notice.

Pasting the same text again within two seconds expands that snippet's token in place, and Tab with the cursor on or just after a token expands it.
Ctrl+E was the first idea for that key, but ratatui-textarea already binds Ctrl+E to the end of the line, and a terminal without keyboard enhancement can send Cmd+Right as Ctrl+E, which Task 17 makes a line-end key; Tab means nothing inside a message's text, since it completes only a lone `/command` or an `@path` last word, so it takes no binding from anyone.
Backspace just after a token removes the whole token, so a token is never half-deleted into text that would be sent as typed.

The snippets live in the composer for the whole session, and a token resolves wherever it comes back: a history recall, `Effect::RestoreComposer`, a draft the `@path` mention hold puts back, and text edited in `$EDITOR`.
`/settings` never touches the composer's text or its snippets.
Once a send takes a snippet, it is a chip that remembers its text (`Chip::pasted`):
- the model-pick hold keeps the chips above the composer, so the resend carries the paste;
- a send the server refuses for its model puts the uploaded chip back through `App::restore_chips`, with its text;
- any other failed send or create puts the paste back as a chip that uploads again, instead of naming it among the files to attach again.
Ctrl+O copies the draft with every token expanded, and leaves the tokens in the composer.
A built-in command's argument is not a message, so a token in it is expanded inline.
A paste into a one-line editor or an overlay filter is taken before the composer is reached, so it keeps M2's `one_line_paste` behavior and never makes a snippet.
A paste over `attachments::MAX_FILE_BYTES` (10 MiB) is refused with a notice before it is inserted, and the core refuses a larger snippet as a failed chip as well.

The hint row, its priority, and the copy notice do not change; the notices go to the footer, and the mention chips and the held drafts keep their flows.

**Files:**
- Modify: `crates/scuttle-core/src/attachments.rs` (`ChipState::Pasted`, `Chip::pasted`, and `Chip::label`)
- Modify: `crates/scuttle-core/src/app.rs` (`Msg::AttachPaste`, `Effect::UploadText`, `App::sent_pastes`, `push_paste`, `add_chip`, `upload_held`, `take_files`, `note_send`, `restore_chips`, `with_lost_files`, `message_with_files`, `carry_files`, `submit`, and `update`; tests)
- Modify: `crates/scuttle-tui/src/composer.rs` (new `PASTE_LINES`, `PASTE_CHARS`, `PASTE_AGAIN`, `is_large_paste`, `Snippet`, and `token_starts`; `Composer`'s snippets and their methods; `Composer::key_action`; tests)
- Modify: `crates/scuttle-tui/src/app.rs` (new `PASTE_NEEDS_TEXT`; `Tui::handle`, `Tui::submit`, `Tui::copy_draft`, and `Tui::chip_lines`; tests)
- Modify: `crates/scuttle-tui/src/runtime.rs` (`Runtime::run`'s `Effect::UploadText` arm; tests)
- Modify: `crates/scuttle-tui/src/help.rs` (`KEYS`; tests)

**Interfaces:**
- Consumes: Task 15's `Composer::height`, the text rows plus two; `App::current_org`, `upload_held`, `hold_for_chips`, `send_waiting`, `restore_chips`, `with_lost_files`, and `lost_files_notice` (existing, core `app.rs`); `attachments::MAX_FILE_BYTES` and `size_label` (existing); `content_disposition`, `too_big`, and the `Effect::UploadFile` arm (existing, `runtime.rs`); `one_line_paste`, `Tui::hold_for_mentions`, and the `tui`, `started`, `loaded`, `type_text`, `uploads`, `Files`, `chats_tui`, `show`, `settings_tui`, and `remove_settings` test helpers (existing, TUI `app.rs`); `started`, `chat`, `attach_ready`, `seq_of`, `send_failed`, `last_error`, and `with_efforts` (existing, core `app.rs` tests).
- Produces:
  - `ChipState::Pasted` and `pub pasted: Option<String>` on `Chip` in `attachments.rs`.
  - `Msg::AttachPaste { name: String, text: String }`, `Effect::UploadText { local: u64, name: String, text: String, org: Uuid }`, and `App::sent_pastes: BTreeMap<u64, Vec<(String, String)>>` in core `app.rs`.
  - `pub const PASTE_LINES: usize = 10`, `pub const PASTE_CHARS: usize = 1000`, `pub const PASTE_AGAIN: Duration = Duration::from_secs(2)`, `pub fn is_large_paste(text: &str) -> bool`, `pub struct Snippet { pub number: u32, pub text: String }` with `Snippet::token(&self) -> String` and `Snippet::file_name(&self) -> String` in `composer.rs`.
  - `Composer::paste_at(&mut self, text: &str, now: Instant)`, `Composer::expanded_text(&self) -> String`, `Composer::expand_tokens(&self, text: &str) -> String`, and `Composer::split_snippets(&self, text: &str) -> (String, Vec<Snippet>)`.
  - Task 17 adds its arms to `Composer::key_action` beside the Tab and Backspace arms this task adds.

- [ ] **Step 1: Write the failing core tests**

In `mod tests` of `crates/scuttle-core/src/attachments.rs`, in `sizes_and_labels_read_naturally`, add `pasted: None,` after `state,` in the `chip` closure, and add at the end of the test:

```rust
        let paste = Chip {
            local: 2,
            name: "paste-1.txt".into(),
            size: Some(2048),
            state: ChipState::Pasted,
            pasted: Some("x".repeat(2048)),
        };
        assert_eq!(paste.label(), "paste-1.txt 2 KiB uploads when sent");
```

In `mod tests` of `crates/scuttle-core/src/app.rs`, after `attach_ready`, add:

```rust
    #[test]
    fn a_paste_uploads_as_a_text_file_with_its_message() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let org = started(&mut app);
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(Uuid::new_v4()),
            messages: vec![],
        });
        let text = "line\n".repeat(12);
        assert!(
            app.update(Msg::AttachPaste {
                name: "paste-1.txt".into(),
                text: text.clone(),
            })
            .is_empty(),
            "nothing uploads before the send"
        );
        assert_eq!(app.chips[0].state, ChipState::Pasted);
        assert_eq!(app.chips[0].label(), "paste-1.txt 60 B uploads when sent");
        assert_eq!(
            app.update(Msg::Submit("what failed?".into())),
            vec![Effect::UploadText {
                local: 1,
                name: "paste-1.txt".into(),
                text,
                org,
            }]
        );
        let file = Uuid::new_v4();
        let effects = app.update(Msg::FileUploaded {
            local: 1,
            file_id: file,
            size: 60,
        });
        assert!(
            matches!(effects.as_slice(), [Effect::SendMessage { text, turn, .. }]
                if text == "what failed?" && turn.files == vec![file]),
            "{effects:?}"
        );
        assert!(app.chips.is_empty(), "sent chips clear");
    }

    #[test]
    fn a_failed_send_puts_its_pasted_text_back_above_the_composer() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let org = started(&mut app);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(id),
            messages: vec![],
        });
        attach_ready(&mut app, "a.png");
        app.update(Msg::AttachPaste {
            name: "paste-1.txt".into(),
            text: "the log".into(),
        });
        app.update(Msg::Submit("look".into()));
        let local = app.chips[1].local;
        let seq = seq_of(&app.update(Msg::FileUploaded {
            local,
            file_id: Uuid::new_v4(),
            size: 7,
        }));
        app.update(send_failed(id, "look", seq));
        assert_eq!(
            last_error(&app),
            "Could not send the message: boom. These files were not sent: a.png. Attach them again. Your pasted text is back above the composer as paste-1.txt; send again to attach it."
        );
        assert_eq!(app.chips.len(), 1, "only the paste comes back");
        assert_eq!(app.chips[0].state, ChipState::Pasted);
        assert_eq!(app.chips[0].pasted.as_deref(), Some("the log"));
        assert!(app.sent_files.is_empty() && app.sent_pastes.is_empty());
        let local = app.chips[0].local;
        assert_eq!(
            app.update(Msg::Submit("look again".into())),
            vec![Effect::UploadText {
                local,
                name: "paste-1.txt".into(),
                text: "the log".into(),
                org,
            }],
            "the resend uploads it again"
        );
    }

    #[test]
    fn a_send_refused_for_its_model_keeps_its_pasted_text_with_the_held_draft() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let (thinker, _) = with_efforts(&mut app);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: Box::new(types::CodersdkChat {
                id: Some(id),
                last_model_config_id: Some(thinker),
                ..Default::default()
            }),
            messages: vec![],
        });
        app.update(Msg::AttachPaste {
            name: "paste-1.txt".into(),
            text: "the log".into(),
        });
        app.update(Msg::Submit("look".into()));
        let file = Uuid::new_v4();
        let seq = seq_of(&app.update(Msg::FileUploaded {
            local: 1,
            file_id: file,
            size: 7,
        }));
        app.update(Msg::ForChat {
            chat: id,
            msg: Box::new(Msg::ModelUnavailable {
                text: "look".into(),
                files: vec![file],
                message: "Invalid model_config_id: model config not found or disabled.".into(),
                plan_mode: None,
                seq,
            }),
        });
        assert_eq!(app.chips.len(), 1);
        assert_eq!(app.chips[0].state, ChipState::Ready(file));
        assert_eq!(app.chips[0].name, "paste-1.txt");
        assert_eq!(app.chips[0].pasted.as_deref(), Some("the log"));
        assert!(app.sent_pastes.is_empty(), "the refused send is settled");
        assert_eq!(
            app.update(Msg::ModelPickerClosed),
            vec![Effect::RestoreComposer("look".into())]
        );
        assert_eq!(
            app.chips.len(),
            1,
            "the paste waits above the composer for the resend"
        );
    }

    #[test]
    fn a_paste_over_the_upload_limit_is_a_failed_chip() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let text = "a".repeat(crate::attachments::MAX_FILE_BYTES as usize + 1);
        assert!(
            app.update(Msg::AttachPaste {
                name: "paste-1.txt".into(),
                text,
            })
            .is_empty()
        );
        assert_eq!(
            app.chips[0].label(),
            "paste-1.txt: the pasted text is 10485761 bytes; the limit is 10485760 bytes."
        );
    }
```

- [ ] **Step 2: Run the core tests to verify they fail**

Run: `cargo test -p scuttle-core -- paste sizes_and_labels`
Expected: FAIL to compile with "no variant named `Pasted` found for enum `ChipState`" and "no variant named `AttachPaste` found for enum `Msg`".

- [ ] **Step 3: Model a pasted chip in the core**

In `crates/scuttle-core/src/attachments.rs`, add this variant at the end of `pub enum ChipState`:

```rust
    /// A large paste waiting for its message's send, which uploads `Chip::pasted`.
    Pasted,
```

In `pub struct Chip`, after `pub state: ChipState,`, add:

```rust
    /// The text of a chip made from a large paste, kept through its upload so a failed send
    /// can put the paste back instead of losing it.
    pub pasted: Option<String>,
```

In `Chip::label`, after the `ChipState::Held(_)` arm, add:

```rust
            ChipState::Pasted => format!(
                "{} {} uploads when sent",
                self.name,
                size_label(self.size.unwrap_or(0))
            ),
```

In `crates/scuttle-core/src/app.rs`, after the `AttachMention(String),` variant of `pub enum Msg`, add:

```rust
    /// A large paste the composer showed as a token, sent with the message as the text file
    /// `name`. It shows as a chip and uploads with the send, as an `@path` mention does.
    AttachPaste { name: String, text: String },
```

After the `UploadFile { .. },` variant of `pub enum Effect`, add:

```rust
    /// Uploads `text` to `org` as the file `name`, answered as `Effect::UploadFile` is.
    UploadText {
        local: u64,
        name: String,
        text: String,
        org: Uuid,
    },
```

In `pub struct App`, after `sent_files: BTreeMap<u64, Vec<String>>,`, add:

```rust
    /// The name and text of each paste a message in flight carried, by its number, so a failed
    /// send can put them back. An entry leaves with the send's `sent_files` entry.
    sent_pastes: BTreeMap<u64, Vec<(String, String)>>,
```

`App` derives `Default`, so `App::new` needs no change.
In `App::update`, after each `self.sent_files.remove(&seq);` that settles a successful send (the `sent_seq` check at the top of `update`, and the `creating_seq` in `Msg::ChatCreated`), add:

```rust
            self.sent_pastes.remove(&seq);
```

After `Msg::AttachMention(path) => self.add_chip(path, true),`, add:

```rust
            Msg::AttachPaste { name, text } => {
                self.push_paste(name, text);
                vec![]
            }
```

In `fn add_chip`, add `pasted: None,` after the `state: ..,` field of each of its three `Chip { .. }` literals.
After `fn add_chip`, add:

```rust
    /// Adds a chip for the large paste `text`, uploaded as `name` when the message is sent. A
    /// paste over the upload limit is refused as a failed chip.
    fn push_paste(&mut self, name: String, text: String) {
        self.next_chip += 1;
        let size = text.len() as u64;
        let state = if size > attachments::MAX_FILE_BYTES {
            ChipState::Failed(format!(
                "the pasted text is {size} bytes; the limit is {} bytes.",
                attachments::MAX_FILE_BYTES
            ))
        } else {
            ChipState::Pasted
        };
        self.chips.push(Chip {
            local: self.next_chip,
            name,
            size: Some(size),
            state,
            pasted: Some(text),
        });
    }
```

Replace `fn upload_held` with:

```rust
    /// Starts the uploads of the chips held for the send: the files `@path` mentions name and
    /// the large pastes.
    fn upload_held(&mut self) -> Vec<Effect> {
        if !self
            .chips
            .iter()
            .any(|c| matches!(c.state, ChipState::Held(_) | ChipState::Pasted))
        {
            return vec![];
        }
        let Some(org) = self.current_org() else {
            return vec![];
        };
        self.chips
            .iter_mut()
            .filter_map(|chip| {
                let effect = match &chip.state {
                    ChipState::Held(path) => Effect::UploadFile {
                        local: chip.local,
                        path: path.clone(),
                        org,
                    },
                    ChipState::Pasted => Effect::UploadText {
                        local: chip.local,
                        name: chip.name.clone(),
                        text: chip.pasted.clone().unwrap_or_default(),
                        org,
                    },
                    _ => return None,
                };
                chip.state = ChipState::Uploading;
                Some(effect)
            })
            .collect()
    }
```

Replace `fn take_files` with:

```rust
    /// The uploaded files for a message about to go out, with their names and the name and
    /// text of each paste among them; the chips clear with it, except those held for a later
    /// send.
    fn take_files(&mut self) -> (Vec<Uuid>, Vec<String>, Vec<(String, String)>) {
        let (mut files, mut names, mut pastes) = (Vec::new(), Vec::new(), Vec::new());
        for chip in &self.chips {
            if let ChipState::Ready(id) = chip.state {
                files.push(id);
                names.push(chip.name.clone());
                if let Some(text) = &chip.pasted {
                    pastes.push((chip.name.clone(), text.clone()));
                }
            }
        }
        self.chips
            .retain(|c| matches!(c.state, ChipState::Held(_) | ChipState::Pasted));
        (files, names, pastes)
    }
```

Replace `fn note_send` with:

```rust
    /// Records the names of the files the send numbered `seq` carries, and its pastes, if any.
    fn note_send(&mut self, seq: u64, names: Vec<String>, pastes: Vec<(String, String)>) {
        if !names.is_empty() {
            self.sent_files.insert(seq, names);
        }
        if !pastes.is_empty() {
            self.sent_pastes.insert(seq, pastes);
        }
    }
```

In each of the three callers, `fn submit` (the `Effect::CreateChat` path), `fn message_with_files`, and `fn carry_files`, replace `let (files, names) = self.take_files();` with `let (files, names, pastes) = self.take_files();` and `self.note_send(seq, names);` with `self.note_send(seq, names, pastes);`.

Replace `fn restore_chips` with:

```rust
    /// Puts the files of the send numbered `seq`, which the server refused for its model, back
    /// above the composer as uploaded chips, under the names the send recorded and with the
    /// text of any paste among them, so the resend carries them without a second upload. They
    /// go ahead of the chips held for a later send, in the order they were attached.
    fn restore_chips(&mut self, seq: u64, files: Vec<Uuid>) {
        let names = self.sent_files.remove(&seq).unwrap_or_default();
        let pastes = self.sent_pastes.remove(&seq).unwrap_or_default();
        let mut restored = Vec::with_capacity(files.len());
        for (i, id) in files.into_iter().enumerate() {
            self.next_chip += 1;
            let name = names
                .get(i)
                .cloned()
                .unwrap_or_else(|| "attachment".to_owned());
            let pasted = pastes
                .iter()
                .find(|(n, _)| *n == name)
                .map(|(_, text)| text.clone());
            restored.push(Chip {
                local: self.next_chip,
                name,
                size: None,
                state: ChipState::Ready(id),
                pasted,
            });
        }
        self.chips.splice(0..0, restored);
    }
```

Replace `fn with_lost_files` with:

```rust
    /// Settles the failed create or send numbered `seq`, adding the names of its files to
    /// `notice`. Its pastes come back above the composer as chips that upload again, since
    /// nothing else holds their text.
    fn with_lost_files(&mut self, seq: u64, notice: String) -> String {
        let pastes = self.sent_pastes.remove(&seq).unwrap_or_default();
        let names = self.sent_files.remove(&seq).unwrap_or_default();
        let lost: Vec<String> = names
            .into_iter()
            .filter(|n| !pastes.iter().any(|(p, _)| p == n))
            .collect();
        let notice = lost_files_notice(notice, &lost);
        if pastes.is_empty() {
            return notice;
        }
        let back: Vec<String> = pastes.iter().map(|(n, _)| n.clone()).collect();
        for (name, text) in pastes {
            self.push_paste(name, text);
        }
        format!(
            "{}. Your pasted text is back above the composer as {}; send again to attach it.",
            notice.trim_end_matches('.'),
            back.join(", ")
        )
    }
```

- [ ] **Step 4: Run the core tests to verify they pass**

Run: `cargo test -p scuttle-core -- paste sizes_and_labels attachment of_two_identical_sends refuses_for_its_model`
Expected: PASS, with the existing attachment and failed-send tests unchanged.

- [ ] **Step 5: Write the failing composer, runtime, TUI, and help tests**

In `mod tests` of `crates/scuttle-tui/src/composer.rs`, add:

```rust
    /// A paste that becomes a snippet: twelve numbered lines.
    fn big() -> String {
        (1..=12).map(|i| format!("log line {i}\n")).collect()
    }

    #[test]
    fn a_large_paste_becomes_a_token_and_a_small_one_goes_in_as_typed() {
        let now = Instant::now();
        let mut c = Composer::new(10);
        c.paste_at("two\nlines", now);
        assert_eq!(c.text(), "two\nlines");
        c.set_text("");
        c.paste_at(&big(), now);
        assert_eq!(c.text(), "[Pasted text #1 +12 lines]");
        c.paste_at(&"x".repeat(PASTE_CHARS), now + PASTE_AGAIN);
        assert_eq!(
            c.text(),
            "[Pasted text #1 +12 lines][Pasted text #2 +1000 chars]"
        );
        assert_eq!(c.height(80), 3, "the tokens take one row");
        assert!(
            is_large_paste(&"a\n".repeat(9)),
            "nine newlines make ten lines, as in the web UI"
        );
        assert!(!is_large_paste(&"a\n".repeat(8)));
        assert!(!is_large_paste(&"x".repeat(PASTE_CHARS - 1)));
    }

    #[test]
    fn the_same_paste_again_right_away_expands_its_token_and_later_adds_another() {
        let now = Instant::now();
        let mut c = Composer::new(10);
        c.paste_at(&big(), now);
        c.paste_at(&big(), now + Duration::from_millis(500));
        assert_eq!(c.text(), big(), "the second paste expanded the first");
        let mut c = Composer::new(10);
        c.paste_at(&big(), now);
        c.paste_at(&big(), now + PASTE_AGAIN);
        assert_eq!(
            c.text(),
            "[Pasted text #1 +12 lines][Pasted text #2 +12 lines]"
        );
    }

    #[test]
    fn tab_on_a_token_expands_it_and_backspace_after_it_removes_it() {
        let now = Instant::now();
        let mut c = Composer::new(10);
        type_str(&mut c, "see ");
        c.paste_at(&big(), now);
        type_str(&mut c, " please");
        assert_eq!(press(&mut c, KeyCode::Tab), ComposerAction::None);
        assert_eq!(
            c.text(),
            "see [Pasted text #1 +12 lines] please",
            "Tab away from the token does nothing"
        );
        for _ in 0.." please".len() {
            press(&mut c, KeyCode::Left);
        }
        press(&mut c, KeyCode::Tab);
        assert_eq!(c.text(), format!("see {} please", big()));
        let mut c = Composer::new(10);
        type_str(&mut c, "see ");
        c.paste_at(&big(), now);
        press(&mut c, KeyCode::Backspace);
        assert_eq!(c.text(), "see ", "one Backspace removes the whole token");
        press(&mut c, KeyCode::Backspace);
        assert_eq!(c.text(), "see");
    }

    #[test]
    fn a_sent_draft_splits_into_text_and_snippets_and_a_command_takes_them_inline() {
        let now = Instant::now();
        let mut c = Composer::new(10);
        type_str(&mut c, "compare ");
        c.paste_at(&big(), now);
        type_str(&mut c, " and ");
        c.paste_at("short\nlog", now);
        type_str(&mut c, " ");
        c.paste_at(&"y".repeat(PASTE_CHARS), now);
        let (message, snippets) = c.split_snippets(&c.text());
        assert_eq!(message, "compare  and short\nlog");
        assert_eq!(
            snippets.iter().map(Snippet::file_name).collect::<Vec<_>>(),
            ["paste-1.txt", "paste-2.txt"]
        );
        assert_eq!(snippets[0].text, big());
        assert_eq!(
            c.expand_tokens("/title [Pasted text #2 +1000 chars]"),
            format!("/title {}", "y".repeat(PASTE_CHARS))
        );
        assert_eq!(
            c.expanded_text(),
            format!("compare {} and short\nlog {}", big(), "y".repeat(PASTE_CHARS))
        );
    }

    #[test]
    fn a_token_still_resolves_after_a_history_recall_and_set_text() {
        let now = Instant::now();
        let mut c = Composer::new(10);
        type_str(&mut c, "see ");
        c.paste_at(&big(), now);
        assert!(matches!(
            press(&mut c, KeyCode::Enter),
            ComposerAction::Submit(_)
        ));
        press(&mut c, KeyCode::Up);
        assert_eq!(c.text(), "see [Pasted text #1 +12 lines]");
        assert_eq!(c.expanded_text(), format!("see {}", big()));
        c.set_text("edited in $EDITOR [Pasted text #1 +12 lines]");
        assert_eq!(c.split_snippets(&c.text()).1.len(), 1);
    }
```

In `mod tests` of `crates/scuttle-tui/src/runtime.rs`, after `a_file_uploads_with_its_name_and_messages_carry_file_parts`, add:

```rust
    #[tokio::test]
    async fn pasted_text_uploads_as_a_text_file_under_its_name() {
        use wiremock::matchers::{body_string, header, query_param};
        let server = MockServer::start().await;
        let (org, file) = (Uuid::new_v4(), Uuid::new_v4());
        Mock::given(method("POST"))
            .and(path("/api/v2/chats/files"))
            .and(query_param("organization", org.to_string()))
            .and(header(
                "content-disposition",
                "attachment; filename=\"paste-1.txt\"",
            ))
            .and(body_string("one\ntwo\n"))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({"id": file})))
            .mount(&server)
            .await;
        let (mut rt, mut rx) = runtime(&server.uri());
        rt.run(Effect::UploadText {
            local: 2,
            name: "paste-1.txt".into(),
            text: "one\ntwo\n".into(),
            org,
        });
        assert!(matches!(
            next(&mut rx).await,
            Msg::FileUploaded { local: 2, file_id, size: 8 } if file_id == file
        ));
    }

    #[tokio::test]
    async fn pasted_text_over_the_limit_is_refused_before_upload() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v2/chats/files"))
            .respond_with(ResponseTemplate::new(201))
            .expect(0)
            .mount(&server)
            .await;
        let (mut rt, mut rx) = runtime(&server.uri());
        rt.run(Effect::UploadText {
            local: 5,
            name: "paste-1.txt".into(),
            text: "a".repeat(scuttle_core::attachments::MAX_FILE_BYTES as usize + 1),
            org: Uuid::new_v4(),
        });
        match next(&mut rx).await {
            Msg::UploadFailed { local: 5, message } => assert_eq!(
                message,
                "paste-1.txt is 10485761 bytes; the limit is 10485760 bytes."
            ),
            other => panic!("expected UploadFailed, got {other:?}"),
        }
    }
```

In `mod tests` of `crates/scuttle-tui/src/app.rs`, add:

```rust
    /// A paste that becomes a snippet: twelve numbered lines.
    fn big_paste() -> String {
        (1..=12).map(|i| format!("log line {i}\n")).collect()
    }

    #[test]
    fn a_large_paste_shows_a_token_and_goes_as_a_text_file_with_the_message() {
        let mut t = tui();
        started(&mut t);
        loaded(&mut t, json!([]));
        type_text(&mut t, "why did this fail? ");
        t.handle(Event::Paste(big_paste()));
        assert_eq!(
            t.composer.text(),
            "why did this fail? [Pasted text #1 +12 lines]"
        );
        assert!(screen(&mut t, 80, 20).contains("[Pasted text #1 +12 lines]"));
        let effects = t.handle(key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(
            effects.iter().any(|e| matches!(e, Effect::UploadText { name, text, .. }
                if name == "paste-1.txt" && *text == big_paste())),
            "{effects:?}"
        );
        assert_eq!(t.composer.text(), "", "the message waits for its upload");
        let local = t.core.chips[0].local;
        let effects = t.update(Msg::FileUploaded {
            local,
            file_id: uuid::Uuid::new_v4(),
            size: 132,
        });
        assert!(
            effects.iter().any(|e| matches!(e, Effect::SendMessage { text, .. }
                if text == "why did this fail?")),
            "the token left the text: {effects:?}"
        );
    }

    #[test]
    fn a_paste_with_no_message_of_its_own_stays_in_the_composer() {
        let mut t = tui();
        started(&mut t);
        loaded(&mut t, json!([]));
        t.handle(Event::Paste(big_paste()));
        let effects = t.handle(key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(effects.is_empty(), "{effects:?}");
        assert_eq!(t.composer.text(), "[Pasted text #1 +12 lines]");
        assert!(t.core.chips.is_empty());
        assert_eq!(
            t.core.notices.last(),
            Some(&Notice::Info(PASTE_NEEDS_TEXT.into()))
        );
    }

    #[test]
    fn pasting_the_same_text_again_right_away_expands_the_token() {
        let mut t = tui();
        // Two pastes in a row land well inside `PASTE_AGAIN` of each other.
        t.handle(Event::Paste(big_paste()));
        t.handle(Event::Paste(big_paste()));
        assert_eq!(t.composer.text(), big_paste());
    }

    #[test]
    fn ctrl_o_copies_a_draft_with_its_pasted_text_expanded() {
        let mut t = tui();
        type_text(&mut t, "see ");
        t.handle(Event::Paste(big_paste()));
        t.handle(key(KeyCode::Char('o'), KeyModifiers::CONTROL));
        assert_eq!(t.last_copied, Some(format!("see {}", big_paste())));
        assert_eq!(
            t.composer.text(),
            "see [Pasted text #1 +12 lines]",
            "the draft keeps its token"
        );
    }

    #[test]
    fn settings_keep_a_pasted_snippet_in_the_draft() {
        let (mut t, path) = settings_tui();
        type_text(&mut t, "see ");
        t.handle(Event::Paste(big_paste()));
        t.edit_settings_with(|p| std::fs::write(p, "composer_max_lines = 4\n"));
        assert_eq!(t.composer.text(), "see [Pasted text #1 +12 lines]");
        assert_eq!(t.composer.expanded_text(), format!("see {}", big_paste()));
        remove_settings(&path);
    }

    #[test]
    fn a_mention_hold_keeps_the_token_and_the_second_send_attaches_both() {
        let files = Files::with(&["a.md"]);
        let mut t = tui();
        started(&mut t);
        loaded(&mut t, json!([]));
        type_text(&mut t, &format!("compare @{} with ", files.path("a.md")));
        t.handle(Event::Paste(big_paste()));
        let held = t.composer.text();
        let effects = t.handle(key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(uploads(&effects).is_empty(), "{effects:?}");
        assert_eq!(
            t.composer.text(),
            held,
            "the draft, token and all, waits for a second send"
        );
        let effects = t.handle(key(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(uploads(&effects), [files.path("a.md")]);
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, Effect::UploadText { name, .. } if name == "paste-1.txt")),
            "{effects:?}"
        );
    }

    #[test]
    fn a_large_paste_into_the_rename_editor_stays_one_line() {
        let (mut t, _, _) = chats_tui();
        show(&mut t, vec![Effect::ShowChats(String::new())]);
        t.handle(key(KeyCode::Down, KeyModifiers::NONE));
        t.handle(key(KeyCode::Char('e'), KeyModifiers::CONTROL));
        for _ in 0.."Fix the flaky watch reconnect test".len() {
            t.handle(key(KeyCode::Backspace, KeyModifiers::NONE));
        }
        t.handle(Event::Paste(big_paste()));
        assert_eq!(
            t.core.editor.as_ref().map(|e| e.line.text().to_owned()),
            Some(one_line_paste(&big_paste()))
        );
        assert_eq!(t.composer.text(), "", "no snippet was made");
    }

    #[test]
    fn a_paste_over_the_upload_limit_is_refused_with_its_size() {
        let mut t = tui();
        let max = scuttle_core::attachments::MAX_FILE_BYTES;
        t.handle(Event::Paste("a".repeat(max as usize + 1)));
        assert_eq!(t.composer.text(), "");
        assert_eq!(
            t.core.notices.last(),
            Some(&Notice::Error(
                "The paste is 10485761 bytes, over the 10485760-byte limit for an attachment, so it was not added.".into()
            ))
        );
    }
```

In `mod tests` of `crates/scuttle-tui/src/help.rs`, add:

```rust
    #[test]
    fn help_says_how_a_pasted_text_token_expands() {
        let lines = text(&help_lines(&Theme::terminal(true), 300));
        let line = lines
            .iter()
            .find(|l| l.starts_with("Tab on a pasted text token"))
            .unwrap_or_else(|| panic!("no pasted-text line in {lines:?}"));
        for fact in [
            "Expand the large paste it stands for",
            "pasting the same text again right away",
            "Backspace after it removes it",
            "sent as a text file",
        ] {
            assert!(line.contains(fact), "{fact:?} is missing from {line:?}");
        }
    }
```

- [ ] **Step 6: Run the tests to verify they fail**

Run: `cargo test -p scuttle-tui -- paste token snippet help_says_how_a_pasted`
Expected: FAIL to compile with "cannot find function `is_large_paste` in this scope" and "no method named `paste_at` found for struct `Composer`".

- [ ] **Step 7: Add snippets to the composer**

In `crates/scuttle-tui/src/composer.rs`, add `use std::time::{Duration, Instant};` before the `crossterm` import, and after `pub const SLASH_ROWS: usize = 5;`, add:

```rust
/// A paste of at least this many lines becomes a snippet, as the web UI's `isLargePaste` says.
pub const PASTE_LINES: usize = 10;

/// A paste of at least this many characters becomes a snippet, as the web UI's
/// `isLargePaste` says.
pub const PASTE_CHARS: usize = 1000;

/// How soon pasting a snippet's text again expands its token instead of adding another.
pub const PASTE_AGAIN: Duration = Duration::from_secs(2);

/// Whether a paste of `text` is large enough to become a snippet. As in the web UI, a
/// trailing newline counts as a line.
pub fn is_large_paste(text: &str) -> bool {
    text.split('\n').count() >= PASTE_LINES || text.chars().count() >= PASTE_CHARS
}

/// A large paste the composer shows as a token and sends as a text file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snippet {
    pub number: u32,
    pub text: String,
}

impl Snippet {
    /// What the composer shows in place of the text, such as `[Pasted text #1 +120 lines]`,
    /// or `[Pasted text #2 +1500 chars]` for a paste on one line.
    pub fn token(&self) -> String {
        match self.text.lines().count() {
            n if n > 1 => format!("[Pasted text #{} +{n} lines]", self.number),
            _ => format!(
                "[Pasted text #{} +{} chars]",
                self.number,
                self.text.chars().count()
            ),
        }
    }

    /// The name the text uploads under.
    pub fn file_name(&self) -> String {
        format!("paste-{}.txt", self.number)
    }
}

/// The char columns at which `token` starts on `line`.
fn token_starts(line: &str, token: &str) -> Vec<usize> {
    line.match_indices(token)
        .map(|(byte, _)| line[..byte].chars().count())
        .collect()
}
```

In `pub struct Composer`, after `slash_selected: usize,`, add:

```rust
    /// Every large paste of the session, numbered from 1. None is dropped, so a token that
    /// comes back through the history, a restored draft, or `$EDITOR` still resolves.
    snippets: Vec<Snippet>,
    /// The newest snippet and when it was pasted, so pasting its text again expands it.
    last_paste: Option<(u32, Instant)>,
```

In `Composer::new`, after `slash_selected: 0,`, add:

```rust
            snippets: Vec::new(),
            last_paste: None,
```

After `pub fn paste`, add:

```rust
    /// Inserts a bracketed paste that arrived at `now`. A large one shows as its snippet's
    /// token, and the same text pasted again within `PASTE_AGAIN` expands that token in place.
    pub fn paste_at(&mut self, text: &str, now: Instant) {
        let text = normalize_line_endings(text);
        if let Some((number, at)) = self.last_paste.take()
            && now.saturating_duration_since(at) < PASTE_AGAIN
            && self.snippet(number).is_some_and(|s| s.text == text)
            && self.expand(number)
        {
            return;
        }
        if !is_large_paste(&text) {
            self.paste(&text);
            return;
        }
        let snippet = Snippet {
            number: self.snippets.len() as u32 + 1,
            text,
        };
        self.area.insert_str(snippet.token());
        self.last_paste = Some((snippet.number, now));
        self.snippets.push(snippet);
        self.slash_selected = 0;
        self.sync_gutter();
    }

    fn snippet(&self, number: u32) -> Option<&Snippet> {
        self.snippets.iter().find(|s| s.number == number)
    }

    /// Every snippet token in the text, in reading order: its row, its first char column, its
    /// length in chars, and its snippet's number.
    fn tokens(&self) -> Vec<(usize, usize, usize, u32)> {
        let mut found = Vec::new();
        for (row, line) in self.area.lines().iter().enumerate() {
            for snippet in &self.snippets {
                let token = snippet.token();
                for start in token_starts(line, &token) {
                    found.push((row, start, token.chars().count(), snippet.number));
                }
            }
        }
        found.sort();
        found
    }

    /// The token the cursor is on or just after.
    fn token_at_cursor(&self) -> Option<(usize, usize, usize, u32)> {
        let cursor = self.area.cursor();
        self.tokens()
            .into_iter()
            .find(|&(row, start, len, _)| row == cursor.0 && (start..=start + len).contains(&cursor.1))
    }

    /// The token that ends right at the cursor.
    fn token_before_cursor(&self) -> Option<(usize, usize, usize, u32)> {
        let cursor = self.area.cursor();
        self.tokens()
            .into_iter()
            .find(|&(row, start, len, _)| row == cursor.0 && start + len == cursor.1)
    }

    /// Replaces the first token of snippet `number` with its text; whether there was one.
    fn expand(&mut self, number: u32) -> bool {
        match self.tokens().into_iter().find(|t| t.3 == number) {
            Some(token) => self.expand_token(token),
            None => false,
        }
    }

    /// Replaces `token`, as `tokens` lists it, with its snippet's text.
    fn expand_token(&mut self, (row, start, len, number): (usize, usize, usize, u32)) -> bool {
        let Some(text) = self.snippet(number).map(|s| s.text.clone()) else {
            return false;
        };
        self.replace(row, start, len, &text)
    }

    /// Puts `with` in place of the `len` chars from column `start` of `row`, leaving the cursor
    /// after it.
    fn replace(&mut self, row: usize, start: usize, len: usize, with: &str) -> bool {
        let (Ok(row), Ok(start)) = (u16::try_from(row), u16::try_from(start)) else {
            return false;
        };
        self.area.move_cursor(CursorMove::Jump(row, start));
        self.area.delete_str(len);
        self.area.insert_str(with);
        self.slash_selected = 0;
        self.sync_gutter();
        true
    }

    /// The text with every snippet token replaced by its text, as Ctrl+O copies it.
    pub fn expanded_text(&self) -> String {
        self.expand_tokens(&self.text())
    }

    /// `text` with every snippet token replaced by its snippet's text, for a command's
    /// argument, which is not a message and so carries no file.
    pub fn expand_tokens(&self, text: &str) -> String {
        self.snippets
            .iter()
            .fold(text.to_owned(), |t, s| t.replace(&s.token(), &s.text))
    }

    /// `text` without its snippet tokens, trimmed, and the snippets they stand for, each once,
    /// in the order they first appear, for a message that sends them as text files.
    pub fn split_snippets(&self, text: &str) -> (String, Vec<Snippet>) {
        let mut found: Vec<(usize, &Snippet)> = self
            .snippets
            .iter()
            .filter_map(|s| text.find(&s.token()).map(|at| (at, s)))
            .collect();
        found.sort_by_key(|(at, _)| *at);
        let rest = found
            .iter()
            .fold(text.to_owned(), |t, (_, s)| t.replace(&s.token(), ""));
        (
            rest.trim().to_owned(),
            found.into_iter().map(|(_, s)| s.clone()).collect(),
        )
    }
```

In `fn key_action`, before the `KeyCode::Tab => {` arm, add:

```rust
            // Tab on a snippet token expands it. Inside a message's text Tab has no other use:
            // it completes only a lone `/command` or an `@path` last word.
            KeyCode::Tab if self.token_at_cursor().is_some() => {
                if let Some(token) = self.token_at_cursor() {
                    self.expand_token(token);
                }
            }
            // Backspace right after a token removes all of it, so no half token is sent as text.
            KeyCode::Backspace if !ctrl && !alt && self.token_before_cursor().is_some() => {
                if let Some((row, start, len, _)) = self.token_before_cursor() {
                    self.replace(row, start, len, "");
                }
            }
```

- [ ] **Step 8: Upload a paste in the runtime**

In `crates/scuttle-tui/src/runtime.rs`, in `Runtime::run`, after the `Effect::UploadFile { .. } => { .. }` arm, add:

```rust
            Effect::UploadText {
                local,
                name,
                text,
                org,
            } => {
                self.uploads.retain(|_, upload| !upload.is_finished());
                let upload = self.spawn_tracked(Box::pin(async move {
                    let failed = |message: String| Msg::UploadFailed { local, message };
                    let bytes = text.into_bytes();
                    let size = bytes.len() as u64;
                    if size > scuttle_core::attachments::MAX_FILE_BYTES {
                        return failed(too_big(&name, size));
                    }
                    // The generated client sends `application/octet-stream`; the server reads
                    // the bytes and the `.txt` name and stores the paste as `text/plain`.
                    let disposition = content_disposition(&name);
                    match client
                        .api()
                        .upload_chat_file(&org, &disposition, bytes)
                        .await
                    {
                        Ok(r) => match r.into_inner().id {
                            Some(file_id) => Msg::FileUploaded {
                                local,
                                file_id,
                                size,
                            },
                            None => failed("the server returned no file id".into()),
                        },
                        Err(e) => failed(redact.err(e).await),
                    }
                }));
                self.uploads.insert(local, upload);
            }
```

`Effect::CancelUpload` already aborts any upload by its chip number, so removing a pasted chip while it uploads stops it.

- [ ] **Step 9: Send snippets from the TUI**

In `crates/scuttle-tui/src/app.rs`, after `const QUEUED_HINT: &str = ..;`, add:

```rust
/// What a send says when the draft is only pasted text, which goes as a file and so needs a
/// message of its own, as other attachments do.
const PASTE_NEEDS_TEXT: &str = "Type a message to send with the pasted text.";
```

In `Tui::handle`, in the `Event::Paste(text) => { .. }` arm, replace:

```rust
                self.pasted_dots
                    .extend(crate::paths::dot_parts(&text).map(str::to_owned));
                self.composer.paste(&text);
                vec![]
```

with:

```rust
                let max = scuttle_core::attachments::MAX_FILE_BYTES;
                if text.len() as u64 > max {
                    self.notice(Notice::Error(format!(
                        "The paste is {} bytes, over the {max}-byte limit for an attachment, so it was not added.",
                        text.len()
                    )));
                    return vec![];
                }
                self.pasted_dots
                    .extend(crate::paths::dot_parts(&text).map(str::to_owned));
                self.composer.paste_at(&text, Instant::now());
                vec![]
```

In `Tui::submit`, replace the arm `_ => return self.update(Msg::Submit(text)),` with:

```rust
                // A command's argument is not a message, so a snippet in it goes in as text.
                _ => {
                    let text = self.composer.expand_tokens(&text);
                    return self.update(Msg::Submit(text));
                }
```

and replace the end of `Tui::submit`:

```rust
        self.sent_mentions = std::mem::take(&mut self.mentioned);
        self.sent_pasted_dots = std::mem::take(&mut self.pasted_dots);
        self.update(Msg::Submit(text))
    }
```

with:

```rust
        // The tokens leave the text, as the web UI leaves a large paste out of the message,
        // and their snippets go as text files the message carries.
        let (message, snippets) = self.composer.split_snippets(&text);
        if !snippets.is_empty() && message.is_empty() {
            self.composer.set_text(&text);
            self.notice(Notice::Info(PASTE_NEEDS_TEXT.into()));
            return vec![];
        }
        self.sent_mentions = std::mem::take(&mut self.mentioned);
        self.sent_pasted_dots = std::mem::take(&mut self.pasted_dots);
        let mut effects = Vec::new();
        for snippet in snippets {
            effects.extend(self.update(Msg::AttachPaste {
                name: snippet.file_name(),
                text: snippet.text,
            }));
        }
        effects.extend(self.update(Msg::Submit(message)));
        effects
    }
```

`message` is `text` trimmed when there is no token, and the core trims every submit, so a draft without snippets sends exactly what it sent before.
In `Tui::copy_draft`, replace `let text = self.composer.text();` with `let text = self.composer.expanded_text();`.
In `Tui::chip_lines`, replace `ChipState::Ready(_) | ChipState::Held(_) => (chip.label(), self.theme.accent),` with:

```rust
                ChipState::Ready(_) | ChipState::Held(_) | ChipState::Pasted => {
                    (chip.label(), self.theme.accent)
                }
```

- [ ] **Step 10: Explain the token in `/help`**

In `crates/scuttle-tui/src/help.rs`, after the `KeyInfo` whose `keys` is `"Tab after @"`, add:

```rust
    KeyInfo {
        keys: "Tab on a pasted text token",
        action: "Expand the large paste it stands for, which is otherwise sent as a text file; pasting the same text again right away does the same, and Backspace after it removes it",
    },
```

- [ ] **Step 11: Run the tests to verify they pass**

Run: `cargo test --workspace -- paste token snippet help_ attachment mention copy_draft ctrl_o settings_ editor`
Expected: PASS, with `paste_is_verbatim_and_ctrl_g_opens_the_editor`, `paste_and_set_text_normalize_crlf_and_cr_line_endings`, `pasting_while_the_editor_is_open_types_into_it_not_the_composer`, `a_pasted_dotfile_mention_is_left_out_and_a_typed_one_attaches`, and `a_bracketed_paste_reaches_the_composer` unchanged, since their pastes are under the threshold.

- [ ] **Step 12: Run the full checks**

Run: `cargo fmt --all && cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check`
Expected: all green, with no insta snapshot changed.

- [ ] **Step 13: Commit**

```bash
git add crates/scuttle-core/src/attachments.rs crates/scuttle-core/src/app.rs crates/scuttle-tui/src/composer.rs crates/scuttle-tui/src/app.rs crates/scuttle-tui/src/runtime.rs crates/scuttle-tui/src/help.rs
git commit -m "feat: send a large paste as a text file behind a composer token" \
  -m "A paste of 10 lines or 1000 characters, the web UI's threshold, shows in the composer as [Pasted text #1 +120 lines] and goes with the message as paste-1.txt, uploaded like any attachment, while the token leaves the text. Pasting the same text again right away, or Tab on the token, expands it, and Backspace after it removes it. Ctrl+O copies the expanded text, and a failed send puts the paste back above the composer." \
  -m "Assisted-by: AI"
```

---

### Task 17: Home, End, and Cmd+Left and Cmd+Right act on the line being typed

Today the keys that should move along the line being typed disagree:
- Home reaches `ratatui_textarea::TextArea::input`, which moves to the start of the line (`CursorMove::Head`) whatever the modifiers.
- End never reaches the composer: `Tui::key` takes `KeyCode::End` with any modifiers before the composer and sets `scroll_from_bottom = 0`, which jumps the transcript to the latest message, the "end of the written chat messages" the user saw, and leaves the cursor where it was.
- Ctrl+A and Ctrl+E already move to the start and end of the line, as the widget's emacs bindings.
- Ctrl+Home reaches the widget as Home, so it moves to the start of the line, not the message, and Ctrl+End is taken by the same `KeyCode::End` jump.
- Cmd+Left and Cmd+Right: scuttle pushes `KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES`, and a terminal that speaks that protocol reports Cmd as Super, so they arrive as Super+Left and Super+Right; crossterm parses the legacy `CSI 1;9D` form the same way.
  ratatui-textarea's `From<KeyEvent> for Input` keeps only Ctrl, Alt, and Shift, so Super+Left reaches it as a plain Left and moves one character.
  A terminal that does not report Super sends its own mapping for Cmd+Left and Cmd+Right, commonly Home and End or Ctrl+A and Ctrl+E; this was not checked against Warp itself, so the fix makes every one of those forms move along the line.
- The one-line editors (`Tui` maps keys through `edit_key` to `scuttle_core::line_edit::Edit`) take Home and End, ignore every Ctrl key, and take Super+Left and Super+Right as Left and Right.

This task makes Home, End, Super+Left, Super+Right, Ctrl+A, and Ctrl+E move to the start and end of the current line in the composer and in the one-line editors.
Ctrl+Home and Ctrl+End move to the start and end of the whole message in the composer, and to the ends of the text in a one-line editor, which has one line.
End keeps its jump to the latest message in one place: End with no modifiers jumps when the cursor is already at the end of its line, which is always true on an empty composer.
So End moves to the end of the line first, and a second End jumps, and a reader with nothing typed keeps the one-key jump; this keeps every existing End test passing, since each presses End with the cursor at the end of its line.
Moving the jump to another key was the other option, but every free key would be a new one to learn, and a draft's End would still need two meanings.
None of these keys has another binding: `/help` lists Ctrl+A and Ctrl+E only inside `/chats`, where the overlay has the keyboard, and Home and Super+arrows appear nowhere.
Task 16's Tab and Backspace arms on a snippet token are untouched.

**Files:**
- Modify: `crates/scuttle-tui/src/composer.rs` (new `Composer::at_line_end`; `Composer::key_action`; tests)
- Modify: `crates/scuttle-tui/src/app.rs` (`edit_key` and the `KeyCode::End` arm of `Tui::key`; tests)
- Modify: `crates/scuttle-tui/src/help.rs` (`KEYS`; tests)

**Interfaces:**
- Consumes: Task 16's `Composer::key_action` with its Tab and Backspace arms; `CursorMove::{Head, End, Top, Bottom}` (ratatui-textarea 0.9.2); `Edit::{Home, End}` (existing, `line_edit.rs`); the `tui`, `numbered_rows`, `screen`, and `key` test helpers (existing, `app.rs`).
- Produces: `Composer::at_line_end(&self) -> bool`; `edit_key` maps Super+Left, Ctrl+A, and Ctrl+Home to `Edit::Home`, and Super+Right, Ctrl+E, and Ctrl+End to `Edit::End`.
  No later task consumes these.

- [ ] **Step 1: Write the failing tests**

In `mod tests` of `crates/scuttle-tui/src/composer.rs`, add:

```rust
    /// The cursor's line and column.
    fn cursor(c: &Composer) -> (usize, usize) {
        let at = c.widget().cursor();
        (at.0, at.1)
    }

    #[test]
    fn home_end_and_cmd_arrows_move_to_the_ends_of_the_line() {
        let mut c = Composer::new(10);
        c.set_text("first line\nsecond line");
        let moves = [
            (key(KeyCode::Home, KeyModifiers::NONE), (1, 0)),
            (key(KeyCode::End, KeyModifiers::NONE), (1, 11)),
            (key(KeyCode::Left, KeyModifiers::SUPER), (1, 0)),
            (key(KeyCode::Right, KeyModifiers::SUPER), (1, 11)),
            (key(KeyCode::Char('a'), KeyModifiers::CONTROL), (1, 0)),
            (key(KeyCode::Char('e'), KeyModifiers::CONTROL), (1, 11)),
            (key(KeyCode::Home, KeyModifiers::CONTROL), (0, 0)),
            (key(KeyCode::End, KeyModifiers::NONE), (0, 10)),
            (key(KeyCode::End, KeyModifiers::CONTROL), (1, 11)),
            (key(KeyCode::Left, KeyModifiers::SUPER), (1, 0)),
            (key(KeyCode::Home, KeyModifiers::CONTROL), (0, 0)),
        ];
        for (k, want) in moves {
            c.handle_key(k, SendShortcut::Enter);
            assert_eq!(cursor(&c), want, "after {k:?}");
        }
        assert_eq!(c.text(), "first line\nsecond line", "no key typed anything");
    }

    #[test]
    fn at_line_end_follows_the_cursor_line() {
        let mut c = Composer::new(10);
        assert!(c.at_line_end(), "an empty composer");
        c.set_text("one\ntwo");
        assert!(c.at_line_end());
        press(&mut c, KeyCode::Left);
        assert!(!c.at_line_end());
        press(&mut c, KeyCode::End);
        assert!(c.at_line_end());
    }
```

In `mod tests` of `crates/scuttle-tui/src/app.rs`, add:

```rust
    /// A Tui scrolled up from the bottom of thirty rows, with `typed` in the composer.
    fn scrolled_up(typed: &str) -> Tui {
        let mut t = tui();
        numbered_rows(&mut t);
        screen(&mut t, 60, 20);
        t.handle(key(KeyCode::PageUp, KeyModifiers::NONE));
        assert!(t.scroll_from_bottom > 0, "the reader scrolled up");
        t.composer.set_text(typed);
        t
    }

    #[test]
    fn end_moves_to_the_line_end_first_and_then_jumps_to_the_latest_message() {
        let mut t = scrolled_up("abc");
        t.handle(key(KeyCode::Home, KeyModifiers::NONE));
        let up = t.scroll_from_bottom;
        t.handle(key(KeyCode::End, KeyModifiers::NONE));
        assert_eq!(t.scroll_from_bottom, up, "the first End moved the cursor");
        assert!(t.composer.at_line_end());
        t.handle(key(KeyCode::End, KeyModifiers::NONE));
        assert_eq!(
            t.scroll_from_bottom, 0,
            "End at the end of the line jumps to the latest message"
        );
        assert_eq!(t.composer.text(), "abc");
    }

    #[test]
    fn end_on_an_empty_composer_jumps_and_ctrl_end_never_scrolls() {
        let mut t = scrolled_up("");
        t.handle(key(KeyCode::End, KeyModifiers::CONTROL));
        assert!(t.scroll_from_bottom > 0, "Ctrl+End belongs to the composer");
        t.handle(key(KeyCode::End, KeyModifiers::NONE));
        assert_eq!(t.scroll_from_bottom, 0);
    }

    #[test]
    fn cmd_arrows_and_home_move_within_the_line_without_scrolling() {
        let mut t = scrolled_up("one\ntwo three");
        let up = t.scroll_from_bottom;
        t.handle(key(KeyCode::Left, KeyModifiers::SUPER));
        assert!(!t.composer.at_line_end());
        t.handle(key(KeyCode::Right, KeyModifiers::SUPER));
        assert!(t.composer.at_line_end());
        t.handle(key(KeyCode::Home, KeyModifiers::NONE));
        t.handle(key(KeyCode::End, KeyModifiers::CONTROL));
        assert_eq!(t.scroll_from_bottom, up, "no line key scrolled the transcript");
        assert_eq!(t.composer.text(), "one\ntwo three");
    }

    #[test]
    fn the_one_line_editor_takes_every_line_key() {
        let cases = [
            (KeyCode::Home, KeyModifiers::NONE, Edit::Home),
            (KeyCode::End, KeyModifiers::NONE, Edit::End),
            (KeyCode::Left, KeyModifiers::SUPER, Edit::Home),
            (KeyCode::Right, KeyModifiers::SUPER, Edit::End),
            (KeyCode::Char('a'), KeyModifiers::CONTROL, Edit::Home),
            (KeyCode::Char('e'), KeyModifiers::CONTROL, Edit::End),
            (KeyCode::Home, KeyModifiers::CONTROL, Edit::Home),
            (KeyCode::End, KeyModifiers::CONTROL, Edit::End),
            (KeyCode::Left, KeyModifiers::NONE, Edit::Left),
            (KeyCode::Char('a'), KeyModifiers::NONE, Edit::Char('a')),
        ];
        for (code, mods, want) in cases {
            assert_eq!(edit_key(key(code, mods)), Some(want), "{code:?} {mods:?}");
        }
        assert_eq!(
            edit_key(key(KeyCode::Char('p'), KeyModifiers::CONTROL)),
            None,
            "other Ctrl keys still do nothing in the editor"
        );
    }
```

In `mod tests` of `crates/scuttle-tui/src/help.rs`, add:

```rust
    #[test]
    fn help_names_the_line_keys_and_the_jump_to_the_latest_message() {
        let lines = text(&help_lines(&Theme::terminal(true), 300));
        let line = |start: &str| {
            lines
                .iter()
                .find(|l| l.starts_with(start))
                .unwrap_or_else(|| panic!("no {start:?} line in {lines:?}"))
                .clone()
        };
        let home = line("Home, End");
        for fact in ["start or end of the line", "Cmd+Left and Cmd+Right", "Ctrl+A and Ctrl+E"] {
            assert!(home.contains(fact), "{fact:?} is missing from {home:?}");
        }
        assert!(line("Ctrl+Home, Ctrl+End").contains("whole message"));
        let jump = line("End at a line's end");
        assert!(jump.contains("Jump to the latest message"), "{jump:?}");
        assert!(jump.contains("with nothing typed"), "{jump:?}");
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p scuttle-tui -- home_end end_moves end_on_an_empty cmd_arrows at_line_end the_one_line_editor_takes help_names_the_line_keys`
Expected: FAIL to compile with "no method named `at_line_end` found for struct `Composer`".

- [ ] **Step 3: Move along the line in the composer**

In `crates/scuttle-tui/src/composer.rs`, after `fn on_last_row`, add:

```rust
    /// Whether the cursor is at the end of its line, as it always is on an empty composer.
    pub fn at_line_end(&self) -> bool {
        let cursor = self.area.cursor();
        self.area
            .lines()
            .get(cursor.0)
            .is_none_or(|line| cursor.1 >= line.chars().count())
    }
```

In `fn key_action`, after `let alt = key.modifiers.contains(KeyModifiers::ALT);`, add:

```rust
        let sup = key.modifiers.contains(KeyModifiers::SUPER);
```

and before the final `_ => { self.area.input(key); }` arm, add:

```rust
            // Cmd+Left and Cmd+Right arrive as Super+Left and Super+Right; the widget drops
            // Super and would move one character, so they go to the ends of the line here.
            KeyCode::Left if sup => self.area.move_cursor(CursorMove::Head),
            KeyCode::Right if sup => self.area.move_cursor(CursorMove::End),
            // The widget reads Ctrl+Home and Ctrl+End as Home and End, so the ends of the
            // whole message are reached here.
            KeyCode::Home if ctrl => {
                self.area.move_cursor(CursorMove::Top);
                self.area.move_cursor(CursorMove::Head);
            }
            KeyCode::End if ctrl => {
                self.area.move_cursor(CursorMove::Bottom);
                self.area.move_cursor(CursorMove::End);
            }
```

Home, End, Ctrl+A, and Ctrl+E already reach the widget's own line bindings through `self.area.input(key)`.

- [ ] **Step 4: Leave End to the composer until the cursor is at the line's end**

In `crates/scuttle-tui/src/app.rs`, in `Tui::key`, replace the arm:

```rust
            KeyCode::End => {
                self.scroll_from_bottom = 0;
                return vec![];
            }
```

with:

```rust
            // End moves to the end of the line being typed; once the cursor is there, as on
            // an empty composer, it jumps to the latest message. Ctrl+End and Shift+End stay
            // with the composer.
            KeyCode::End if key.modifiers.is_empty() && self.composer.at_line_end() => {
                self.scroll_from_bottom = 0;
                return vec![];
            }
```

Replace `pub(crate) fn edit_key` with:

```rust
/// The editor key for `key`, if the one-line editor uses it. The editor holds one line, so the
/// keys that move to the ends of a line or of the whole text all go to its ends.
pub(crate) fn edit_key(key: KeyEvent) -> Option<Edit> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let sup = key.modifiers.contains(KeyModifiers::SUPER);
    match key.code {
        KeyCode::Home => return Some(Edit::Home),
        KeyCode::End => return Some(Edit::End),
        KeyCode::Left if sup => return Some(Edit::Home),
        KeyCode::Right if sup => return Some(Edit::End),
        KeyCode::Char('a') if ctrl => return Some(Edit::Home),
        KeyCode::Char('e') if ctrl => return Some(Edit::End),
        _ => {}
    }
    if ctrl {
        return None;
    }
    Some(match key.code {
        KeyCode::Char(c) => Edit::Char(c),
        KeyCode::Backspace => Edit::Backspace,
        KeyCode::Delete => Edit::Delete,
        KeyCode::Left => Edit::Left,
        KeyCode::Right => Edit::Right,
        KeyCode::Enter => Edit::Submit,
        KeyCode::Esc => Edit::Cancel,
        _ => return None,
    })
}
```

The editor still takes keys before an open overlay, so Ctrl+E while renaming from `/chats` moves to the end of the title instead of reaching the overlay, as every other editor key already does.

- [ ] **Step 5: Update `/help`**

In `crates/scuttle-tui/src/help.rs`, after the `KeyInfo` whose `keys` is `"Up, Down"`, add:

```rust
    KeyInfo {
        keys: "Home, End",
        action: "Move to the start or end of the line you are typing, here and in the rename and answer boxes; Cmd+Left and Cmd+Right, and Ctrl+A and Ctrl+E, do the same",
    },
    KeyInfo {
        keys: "Ctrl+Home, Ctrl+End",
        action: "Move to the start or end of the whole message",
    },
```

Replace the entry:

```rust
    KeyInfo {
        keys: "End",
        action: "Jump to the latest message",
    },
```

with:

```rust
    KeyInfo {
        keys: "End at a line's end",
        action: "Jump to the latest message; with nothing typed, End alone jumps",
    },
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test -p scuttle-tui -- home_end end_moves end_on_an_empty cmd_arrows at_line_end the_one_line_editor_takes help_ a_reader a_key_that_only_edits_the_composer editor`
Expected: PASS, with `a_key_that_only_edits_the_composer_reuses_the_transcript_lines` and the reader tests that press End unchanged.

- [ ] **Step 7: Run the full checks**

Run: `cargo fmt --all && cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check`
Expected: all green, with no insta snapshot changed.

- [ ] **Step 8: Commit**

```bash
git add crates/scuttle-tui/src/composer.rs crates/scuttle-tui/src/app.rs crates/scuttle-tui/src/help.rs
git commit -m "fix(scuttle-tui): make Home, End, and Cmd+arrows act on the line being typed" \
  -m "End jumped to the latest message without moving the cursor, and Cmd+Left and Cmd+Right moved one character. Home, End, Cmd+Left, Cmd+Right, Ctrl+A, and Ctrl+E now move to the ends of the line in the composer and the one-line editors, and Ctrl+Home and Ctrl+End to the ends of the message. End jumps to the latest message once the cursor is at the end of its line, or with nothing typed." \
  -m "Assisted-by: AI"
```

---

### Task 18: Blank the activity row while the transcript animates the same state

The activity row above the composer shows `App::activity`, which reads the live turn's newest block: `Thinking` for reasoning, `Tool(name)` for a tool call or an unfinished result, `Writing` for text, `Interrupting`, `Waiting` while a sent message has not been picked up, and `Working` while the chat runs with nothing streamed for the step.
With no live block, `Tool` also covers the latest turn's calls that have no result yet, joined as `execute, read_file` or `linear__save_issue and 3 more`.
`activity::label` words each one ("Thinking…", "Running linear__save_issue and 3 more…", "Writing…"), and `Tui::draw_at` draws `activity_line` in a one-row `activity_row` whenever `App::activity` is `Some`.
Since M1.6, `transcript_view::build` also animates the markers of blocks in progress, listing their rows in `View::spinners`: the summarized reasoning block the agent is thinking in (only while `Activity::Thinking`, and not when the reasoning is expanded), and every tool call of the latest turn without a finished result (while the activity is anything but `Waiting`).
So the screen shows the same state twice: `⠸ Thinking` in the transcript over `⠸ Thinking…` in the activity row, and four animated `linear__save_issue(..)` calls over `⠋ Running linear__save_issue and 3 more…`.

This task leaves the activity row blank while it would say `Thinking` or `Running ..` and one of the transcript's animated markers is on screen.
It keeps the row in every other case:
- waiting for the first token after a send (`Waiting`), when the transcript animates nothing, since its tool markers stop while the activity is `Waiting`;
- a turn whose newest block is finished text but which still runs (`Writing`), or that runs with nothing streamed yet (`Working`);
- interrupting (`Interrupting`);
- reasoning shown expanded, which has no animated marker;
- an animated marker scrolled out of view, so the reader still sees what the agent does.
Compaction has no activity of its own: `/compact` sends `Effect::Compact`, and while the server compacts the chat runs, so the row shows `Working…` or the running tool's name by these same rules.
A reconnect has none either: `App::activity` keeps reading the transcript's last status and blocks, and the footer shows the connection, so a reconnect follows the same rules.

The row keeps its height while it is blank, rather than collapsing.
A turn moves between thinking, tools, and writing many times, and a collapsing row would change the transcript's height at each move, so the reader's lines would jump and `keep_top` would re-anchor on every one; a blank row moves nothing.
The hint row above it, its priority, the chips, and the composer below it stay where they are.

**Files:**
- Modify: `crates/scuttle-tui/src/activity.rs` (new `shows_activity`; tests)
- Modify: `crates/scuttle-tui/src/app.rs` (`Tui::draw_at`: the activity row; tests)

**Interfaces:**
- Consumes: `App::activity` and `Activity` (existing, core `app.rs`); `View::spinners` (existing, `transcript_view.rs`); `activity_line` (existing, `activity.rs`); Task 15's three-row composer; the `tui`, `loaded`, and `screen` test helpers (existing, `app.rs`).
- Produces: `pub fn shows_activity(activity: &Activity, spinners: &[usize], top: usize, rows: usize) -> bool` in `activity.rs`.
  No later task consumes it.

- [ ] **Step 1: Write the failing tests**

In `mod tests` of `crates/scuttle-tui/src/activity.rs`, add:

```rust
    #[test]
    fn the_row_gives_way_only_to_an_animated_marker_on_screen() {
        let tools = Activity::Tool("linear__save_issue and 3 more".into());
        assert!(!shows_activity(&Activity::Thinking, &[5], 0, 10));
        assert!(!shows_activity(&tools, &[3, 4, 5, 6], 0, 10));
        assert!(
            shows_activity(&Activity::Thinking, &[5], 6, 10),
            "the marker is above the rows on screen"
        );
        assert!(
            shows_activity(&Activity::Thinking, &[16], 6, 10),
            "the marker is below them"
        );
        assert!(
            shows_activity(&Activity::Thinking, &[], 0, 10),
            "expanded reasoning does not animate"
        );
        for other in [
            Activity::Waiting,
            Activity::Writing,
            Activity::Working,
            Activity::Interrupting,
        ] {
            assert!(shows_activity(&other, &[5], 0, 10), "{other:?}");
        }
    }
```

In `mod tests` of `crates/scuttle-tui/src/app.rs`, add:

```rust
    /// The row of `shown` where the composer's placeholder is.
    fn placeholder_row(shown: &str) -> usize {
        shown
            .lines()
            .position(|l| l.contains("Message the agent"))
            .unwrap_or_else(|| panic!("no composer:\n{shown}"))
    }

    #[test]
    fn the_activity_row_stays_blank_while_the_transcript_animates_the_thought() {
        use scuttle_core::live::LiveBlock;
        let mut t = tui();
        loaded(
            &mut t,
            json!([{"id": 1, "role": "user", "content": [{"type": "text", "text": "plan it"}]}]),
        );
        t.core.transcript.status = Some(coder_sdk::ChatStatus::Running);
        t.core
            .transcript
            .live
            .blocks
            .push(LiveBlock::Reasoning("hmm".into()));
        t.view_revision += 1;
        let thinking = screen(&mut t, 60, 20);
        assert_eq!(
            thinking.matches("Thinking").count(),
            1,
            "only the transcript's marker says it:\n{thinking}"
        );
        t.core
            .transcript
            .live
            .blocks
            .push(LiveBlock::Text("the answer".into()));
        t.view_revision += 1;
        let writing = screen(&mut t, 60, 20);
        assert!(
            writing.contains("Writing…"),
            "finished text animates nothing, so the row shows:\n{writing}"
        );
        assert_eq!(
            placeholder_row(&thinking),
            placeholder_row(&writing),
            "the blank row kept its height, so nothing moved"
        );
    }

    #[test]
    fn the_activity_row_stays_blank_while_parallel_tool_calls_animate() {
        let mut t = tui();
        let call = |id: &str, n: u32| {
            json!({"type": "tool-call", "tool_call_id": id, "tool_name": "execute",
                "args": {"command": format!("make {n}")}})
        };
        loaded(
            &mut t,
            json!([
                {"id": 1, "role": "user", "content": [{"type": "text", "text": "build all four"}]},
                {"id": 2, "role": "assistant", "content": [
                    call("a", 1), call("b", 2), call("c", 3), call("d", 4)
                ]}
            ]),
        );
        t.core.transcript.status = Some(coder_sdk::ChatStatus::Running);
        t.view_revision += 1;
        assert_eq!(
            t.core.activity(),
            Some(scuttle_core::app::Activity::Tool("execute and 3 more".into()))
        );
        let shown = screen(&mut t, 80, 24);
        for n in 1..=4 {
            assert!(shown.contains(&format!("execute(make {n})")), "{shown}");
        }
        assert_eq!(t.view.spinners.len(), 4, "each running call animates");
        assert!(
            !shown.contains("Running execute"),
            "the row does not repeat the four calls:\n{shown}"
        );
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p scuttle-tui -- the_row_gives_way the_activity_row_stays_blank`
Expected: FAIL to compile with "cannot find function `shows_activity` in this scope".

- [ ] **Step 3: Decide when the row gives way**

In `crates/scuttle-tui/src/activity.rs`, after `pub fn label`, add:

```rust
/// Whether the activity row shows `activity`. It gives way while it would say the agent is
/// thinking or running a tool and one of the transcript's animated markers, the rows in
/// `spinners`, is among the `rows` lines from `top`, since that marker already says so.
pub fn shows_activity(activity: &Activity, spinners: &[usize], top: usize, rows: usize) -> bool {
    let repeats = matches!(activity, Activity::Thinking | Activity::Tool(_));
    let marker_shown = spinners
        .iter()
        .any(|&line| line >= top && line < top + rows);
    !(repeats && marker_shown)
}
```

- [ ] **Step 4: Blank the row in `Tui::draw_at`**

In `crates/scuttle-tui/src/app.rs`, replace `use crate::activity::{SPINNER_INTERVAL, SpinnerStyle, activity_line, next_seed};` with:

```rust
use crate::activity::{SPINNER_INTERVAL, SpinnerStyle, activity_line, next_seed, shows_activity};
```

In `Tui::draw_at`, replace:

```rust
        if let Some(activity) = activity.as_ref() {
            let elapsed = now.saturating_duration_since(self.epoch);
```

with:

```rust
        // The row stays reserved while it gives way to the transcript's own marker, so a
        // turn moving between thinking, tools, and writing never moves the transcript.
        if let Some(activity) = activity.as_ref()
            && shows_activity(
                activity,
                &self.view.spinners,
                top,
                usize::from(transcript.height),
            )
        {
            let elapsed = now.saturating_duration_since(self.epoch);
```

`activity_height` still comes from `activity.is_some()`, so the layout above does not change.

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p scuttle-tui -- the_row_gives_way the_activity_row activity spinner a_reader keep_top`
Expected: PASS, with the existing activity and spinner tests unchanged.

- [ ] **Step 6: Run the full checks**

Run: `cargo fmt --all && cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check`
Expected: all green, with no insta snapshot changed.

- [ ] **Step 7: Commit**

```bash
git add crates/scuttle-tui/src/activity.rs crates/scuttle-tui/src/app.rs
git commit -m "fix(scuttle-tui): stop the activity row repeating the transcript's animated marker" \
  -m "While the model reasons or tools run, the transcript animates their markers, and the activity row said the same thing again, as Thinking twice or four tool calls over Running execute and 3 more. The row now stays blank while it would say thinking or running and such a marker is on screen, and keeps its height, so the transcript never moves. It still shows while waiting, writing, working, or interrupting." \
  -m "Assisted-by: AI"
```

---

### Task 19: Draw Nerd Font icons with `icons = "nerd"`, and suggest a Nerd Font in text mode

scuttle draws its markers as text and emoji today, and it cannot tell which font the terminal draws with.
This task adds one top-level key, `icons = "nerd" | "text"`, that drives every icon.
When the file leaves `icons` out, the `NERD_FONT` environment variable decides: `1`, `true`, or `yes` means nerd, and `0`, `false`, or `no` means text, ignoring case and surrounding spaces.
When neither is set, scuttle draws text.
`main` reads `NERD_FONT` once and hands the answer to the new `Tui::set_icon_env`, so tests never read the real environment, and the pty tests remove `NERD_FONT` from the child's environment so their screens are drawn in text.
`/settings` applies `icons` live, so `restart_keys` does not name it.
`chats.pin_icon` becomes optional: a value in the file still wins, and when it is left out the pin follows the icon set, `📌` in text and nf-oct-pin in nerd.

The icon module is `crates/scuttle-tui/src/icons.rs`, in the TUI, because the glyphs, their slots, and their styles are rendering; the core only parses the key, as `IconSet` in `config.rs`, the way it parses `spinner`.
The set in effect rides on `Theme` as `Theme::icons`, next to the palette, because every surface already takes the theme: `ViewCtx`, `footer::status_line`, `transcript_view::build`, `help_lines_for`, and `Tui::chip_lines`.
So no surface's signature changes, and every existing test that builds `Theme::terminal` draws text, as it does today.
`Tui::new` keeps its M2.5 signature and sets `Theme::icons` from the file; `set_icon_env` and `/settings` set it again.

Every icon follows the audit's width policy:
- A Nerd Font glyph takes a fixed two-cell slot, the glyph and a space, which `icons::slot` counts as 2 and never measures, so a glyph that a non-Mono font draws wider spills onto scuttle's own space.
- Nothing counts an icon with `width_cjk`, which would make a glyph two cells while ratatui places it in one.
- In text mode an icon falls back to exactly what scuttle draws there today, which for most icons is nothing, measured with `wrap::cells_width`.
- The one-cell state markers (`⏺ ◌ ✗ ◼`) and every spinner stay as they are, because the painters overwrite exactly one cell; a tool's kind glyph gets its own slot after the marker.
- A glyph drawn in nerd mode takes `Theme::icon(style)`, which is plain under `NO_COLOR`; a text fallback keeps the style it has today.

The surfaces this task covers, all from the audit's high and medium rows:
- The transcript: a kind glyph after each tool head's state marker (shell, file read, edit, search, web, MCP, subagent, plan, question, workspace, and a generic glyph for the rest so every head lines up), the error line, the queued messages, and the files sent with a message.
- The composer's file chips, with an error glyph on a failed chip.
- The footer: the connection while it connects or reconnects, and error notices.
  The fields keep their `/command:` labels, which the user wants as hints, and info notices and the chat status words stay text.
- `/chats`: the default pin, the error and question markers, the unread dot (🔵 stays in text mode; nf-oct-dot_fill in the accent color in nerd mode), and the archived tag, whose column shrinks from 8 cells to 2.
- `/subagents`: the same error and question markers, in a 2-cell column in nerd mode.
- `/workspace`: a state glyph before each workspace's status, and one on the "none" row.
- `/mcp`: on and off glyphs before each server's state, and an error glyph before a failed server's detail.
- The welcome screen's Nerd Font tip in text mode, a link to https://www.nerdfonts.com/ registered like an assistant-text link, and `/help`'s Icons section in text mode, or its glyph-naming markers in nerd mode.

Left out on purpose:
- `/statusline` and `/usage` field glyphs: the audit proposes them as the footer's legend, and the footer keeps its labels, so they would name glyphs the footer never draws.
  `/usage`'s threshold flag would also need the thresholds in `ViewCtx`, and it is a new warning rather than an icon.
- `/git`'s pull request state glyph, which lands with Task 14, since it owns `PrState`.
- The low-priority rows: the family marker after `+N`, the `/workspace` actions, the `/git` labels, `/info`, the reasoning line, the activity row, and the question menu.

The existing tests and snapshots that change, all in text mode, and only where this list says:
- `transcript_view::tests::welcome_shows_on_a_blank_chat`: its snapshot gains a blank row and the tip row.
- `transcript_view::tests::narrow_terminal_welcome`: it draws 14 rows instead of 12, so both rows of the wrapped tip show, and its snapshot gains a blank row and the two tip rows.
- `config::tests::the_pin_icon_defaults_to_a_pin_and_takes_any_text` is replaced by `an_unset_pin_icon_follows_the_icons_and_a_set_one_takes_any_text`, since the default pin is now `None`.
- `config::tests::the_template_holds_only_defaults_and_every_key_in_it_parses`: the uncommented `icons` and `pin_icon` examples are not the defaults, so the comparison resets them and two asserts check that they parse.
- `config::tests::saving_a_setting_keeps_the_template_and_a_users_edits_to_it` and `app::tests::settings_creates_the_file_and_applies_the_live_keys`: `pin_icon` reads as `Some("*")`.
- `tests/pty.rs`'s `spawn_with_args` removes `NERD_FONT`; no assertion changes.

Every other existing test draws text mode and keeps its assertions.
Text mode's output changes in exactly two places, the welcome tip and the `/help` Icons section, and no existing test asserts the rows below the welcome's start hint or the line count of `/help`.

**Files:**
- Create: `crates/scuttle-tui/src/icons.rs` (`Icon`, `Slot`, `slot`, `style`, `lead`, `line_with`, `tool_icon`, and `NERD_FONTS_URL`)
- Modify: `crates/scuttle-core/src/config.rs` (new `IconSet`; `LocalConfig::icons`, `ChatsConfig::pin_icon`, and `TEMPLATE`; tests)
- Modify: `crates/scuttle-core/src/panels.rs` (`McpRow::on` and `McpRow::failed`; `mcp_groups`; tests)
- Modify: `crates/scuttle-tui/src/theme.rs` (`Theme::colors`, `Theme::icons`, and `Theme::icon`; tests)
- Modify: `crates/scuttle-tui/src/main.rs` (`mod icons` and the `NERD_FONT` read)
- Modify: `crates/scuttle-tui/src/app.rs` (`Tui::icon_env`, `Tui::new`, new `Tui::set_icon_env`, `Tui::apply_icons`, and `Tui::pin_icon`; `Tui::apply_settings`, the two `ViewCtx` literals, and `Tui::chip_lines`; tests)
- Modify: `crates/scuttle-tui/src/transcript_view.rs` (`file_label`, `render_items`, `build_transcript`, and new `TIP` and `push_nerd_font_tip`; tests)
- Modify: `crates/scuttle-tui/src/footer.rs` (`connection_status`, `field_for`, and `status_line`; tests)
- Modify: `crates/scuttle-tui/src/overlay.rs` (`workspace_view`, new `workspace_status`, `mcp_view`, new `status_span`, `status_cell`, `chat_cells`, `chats_view`, and `subagents_view`; tests)
- Modify: `crates/scuttle-tui/src/help.rs` (new `CHATS_MARKERS_NERD` and `ICONS_HELP`; `help_lines_for`; tests)
- Modify: `crates/scuttle-tui/src/snapshots/scuttle__transcript_view__tests__welcome_shows_on_a_blank_chat.snap` and `crates/scuttle-tui/src/snapshots/scuttle__transcript_view__tests__narrow_terminal_welcome.snap`
- Modify: `crates/scuttle-tui/tests/pty.rs` (`spawn_with_args`)

**Interfaces:**
- Consumes: `cells_width`, `wrap_line`, and `cols_on_rows` (existing, `wrap.rs`); `LinkHit`, `Out::links_numbered`, `Out::push_wrapped`, `Out::flush_hidden`, and `Out::extend_rows` (existing, `transcript_view.rs`); `subagent::action` (existing, `subagent.rs`); `spins`, `marker`, `STATUS_COLUMN`, `STATUS_WIDTH`, `UNREAD`, and `pin_width` (Task 13, `overlay.rs`); `marked_app`, `expanded_chats`, `drawn_chats`, `start_of`, `cell_text`, and `ctx_for` (Task 13 and existing, `overlay.rs` tests); `settings_tui`, `remove_settings`, `screen`, `find`, `mouse`, `release`, `style_at`, `lit`, `started`, and `tui` (existing, `app.rs` tests); `ChipState` and `Chip` (existing, `attachments.rs`); `Colors` (existing, `theme.rs`).
- Produces:
  - `pub enum IconSet { Nerd, Text }` with `IconSet::from_env(value: &str) -> Option<IconSet>` and `IconSet::resolve(file: Option<IconSet>, env: Option<IconSet>) -> IconSet`, `LocalConfig::icons: Option<IconSet>`, and `ChatsConfig::pin_icon: Option<String>` in `config.rs`.
  - `McpRow::on: bool` and `McpRow::failed: bool` in `panels.rs`.
  - In `icons.rs`: `pub use scuttle_core::config::IconSet`, `pub enum Icon` with `#[cfg(test)] Icon::ALL: [Icon; 28]`, `pub struct Slot { pub text: &'static str, pub width: u16 }`, `pub fn slot(set: IconSet, icon: Icon) -> Slot`, `pub fn style(theme: &Theme, base: Style) -> Style`, `pub fn lead(theme: &Theme, icon: Icon, base: Style) -> Option<Span<'static>>`, `pub fn line_with(theme: &Theme, before: &str, icon: Icon, after: &str, base: Style) -> Vec<Span<'static>>`, `pub fn tool_icon(name: &str) -> Icon`, and `pub const NERD_FONTS_URL: &str`.
  - `Icon::nerd` holds each glyph and its space, and `Icon::text` ends with `_ => ""`.
  - `Theme::colors: Colors`, `Theme::icons: IconSet` (text in a new theme), and `Theme::icon(&self, style: Style) -> Style` in `theme.rs`.
  - `Tui::set_icon_env(&mut self, env: Option<IconSet>)` and `fn pin_icon(&self) -> &str` in `app.rs`.
  - `fn status_span(status: Option<&ChatStatus>, ctx: &ViewCtx) -> Span<'static>` and the test helpers `nerd_theme() -> Theme` and `chat_row(view: &TableView, title: &str) -> usize` in `overlay.rs`; `chat_cells` draws the archived tag through `Icon::Archived`.
  - `pub const CHATS_MARKERS_NERD: &[&str]` and `pub const ICONS_HELP: &[&str]` in `help.rs`.
  - Task 14 consumes `Icon`, `IconSet`, `slot`, `lead`, `style`, `Theme::icons`, `Theme::colors`, `chat_cells`, `CHATS_MARKERS_NERD`, `nerd_theme`, and `chat_row`.

- [ ] **Step 1: Write the failing tests**

In `mod tests` of `crates/scuttle-core/src/config.rs`, replace the test `the_pin_icon_defaults_to_a_pin_and_takes_any_text` with:

```rust
    #[test]
    fn an_unset_pin_icon_follows_the_icons_and_a_set_one_takes_any_text() {
        assert_eq!(LocalConfig::default().chats.pin_icon, None);
        let pin = |text: &str| load_from_str(text).unwrap().chats.pin_icon;
        assert_eq!(
            pin("[chats]\npin_icon = \"\u{f0403}\"\n").as_deref(),
            Some("\u{f0403}")
        );
        assert_eq!(pin("[chats]\npin_icon = \"\"\n").as_deref(), Some(""));
        assert_eq!(
            pin("[chats]\npin_icon = \"📌\"\n").as_deref(),
            Some("📌"),
            "a pin written out stays, whatever the icons"
        );
    }

    #[test]
    fn icons_take_nerd_or_text_and_the_environment_decides_only_when_unset() {
        assert_eq!(LocalConfig::default().icons, None);
        assert_eq!(
            load_from_str("icons = \"nerd\"\n").unwrap().icons,
            Some(IconSet::Nerd)
        );
        assert_eq!(
            load_from_str("icons = \"text\"\n").unwrap().icons,
            Some(IconSet::Text)
        );
        assert!(matches!(
            load_from_str("icons = \"emoji\"\n"),
            Err(ConfigError::Parse(_))
        ));
        for value in ["1", "true", "yes", "TRUE", " Yes "] {
            assert_eq!(IconSet::from_env(value), Some(IconSet::Nerd), "{value:?}");
        }
        for value in ["0", "false", "no", "No"] {
            assert_eq!(IconSet::from_env(value), Some(IconSet::Text), "{value:?}");
        }
        for value in ["", "2", "nerd", "on"] {
            assert_eq!(IconSet::from_env(value), None, "{value:?}");
        }
        assert_eq!(
            IconSet::resolve(None, None),
            IconSet::Text,
            "neither set means text"
        );
        assert_eq!(IconSet::resolve(None, Some(IconSet::Nerd)), IconSet::Nerd);
        assert_eq!(
            IconSet::resolve(Some(IconSet::Text), Some(IconSet::Nerd)),
            IconSet::Text,
            "the file wins over the environment"
        );
        assert_eq!(
            IconSet::resolve(Some(IconSet::Nerd), Some(IconSet::Text)),
            IconSet::Nerd
        );
        assert!(TEMPLATE.contains("# icons = \"text\""));
    }
```

In `the_template_holds_only_defaults_and_every_key_in_it_parses`, replace:

```rust
            LocalConfig {
                welcome: WelcomeConfig::default(),
                density: BTreeMap::new(),
```

with:

```rust
            LocalConfig {
                welcome: WelcomeConfig::default(),
                density: BTreeMap::new(),
                icons: None,
                chats: ChatsConfig::default(),
```

and after `assert_eq!(cfg.density.get("read_file"), Some(&Density::Summary));`, add:

```rust
        assert_eq!(cfg.icons, Some(IconSet::Text), "the example icons parse");
        assert_eq!(
            cfg.chats.pin_icon.as_deref(),
            Some("📌"),
            "the example pin parses"
        );
```

In `saving_a_setting_keeps_the_template_and_a_users_edits_to_it`, replace `assert_eq!(cfg.chats.pin_icon, "*");` with `assert_eq!(cfg.chats.pin_icon.as_deref(), Some("*"));`.

In `mod tests` of `crates/scuttle-core/src/panels.rs`, add:

```rust
    #[test]
    fn mcp_rows_say_whether_a_server_is_on_and_whether_it_failed() {
        let (github, linear) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let mut app = App::new(BusyBehavior::Queue, true);
        let chat = serde_json::from_value(json!({
            "id": uuid::Uuid::new_v4(), "mcp_server_ids": [github],
            "inline_mcp_servers": [{"slug": "local-tools", "url": "http://localhost:9000/mcp",
                "tool_allow_list": [], "tool_deny_list": []}],
            "context": {"resources": [{"kind": "mcp_server", "source": "playwright",
                "status": "error", "error": "spawn failed", "tools": []}]},
            "children": [], "files": [], "labels": {}
        }))
        .unwrap();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: Box::new(chat),
            messages: vec![],
        });
        let server = |id, name: &str| -> coder_sdk::types::CodersdkMcpServerConfig {
            serde_json::from_value(json!({"id": id, "display_name": name,
                "url": format!("https://{name}.example/mcp"), "availability": "default_off",
                "enabled": true, "auth_connected": true, "tool_allow_list": [],
                "tool_deny_list": []}))
            .unwrap()
        };
        let outcome = |id, outcome: &str, error: &str| coder_sdk::McpConnectOutcome {
            config_id: id,
            slug: String::new(),
            outcome: outcome.into(),
            tool_count: 0,
            error: error.into(),
        };
        app.mcp_panel = Some(McpPanel {
            servers: Fetched::Loaded(vec![server(github, "GitHub"), server(linear, "Linear")]),
            health: Fetched::Loaded(Some(vec![
                outcome(github, "error", "refused"),
                outcome(linear, "connected", ""),
            ])),
        });
        let (groups, _) = mcp_groups(&app);
        let rows: Vec<(&str, bool, bool)> = groups
            .iter()
            .flat_map(|g| &g.rows)
            .map(|r| (r.name.as_str(), r.on, r.failed))
            .collect();
        assert_eq!(
            rows,
            [
                ("GitHub", true, true),
                ("Linear", false, false),
                ("local-tools", true, false),
                ("playwright", true, true),
            ]
        );
        assert_eq!(groups[0].rows[0].detail, "failed: refused");
    }
```

Create `crates/scuttle-tui/src/icons.rs` holding only its tests for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::Colors;
    use ratatui::style::{Color, Modifier};
    use unicode_width::UnicodeWidthStr;

    /// Whether `c` is in the Basic Multilingual Plane's Private Use Area, where every glyph
    /// this set draws sits.
    fn private_use(c: char) -> bool {
        ('\u{e000}'..='\u{f8ff}').contains(&c)
    }

    #[test]
    fn every_icon_is_a_two_cell_nerd_slot_with_a_text_fallback() {
        for icon in Icon::ALL {
            let nerd = slot(IconSet::Nerd, icon);
            let chars: Vec<char> = nerd.text.chars().collect();
            assert_eq!(chars.len(), 2, "{icon:?} is a glyph and a space");
            assert!(private_use(chars[0]), "{icon:?} is a Nerd Font glyph");
            assert_eq!(chars[1], ' ', "{icon:?} ends its slot with a space");
            assert_eq!(nerd.width, 2, "{icon:?}");
            assert_eq!(cells_width(nerd.text), 2, "{icon:?} is drawn in two cells");
            assert_eq!(
                nerd.text.width_cjk(),
                3,
                "{icon:?}: width_cjk would count three cells, so the slot never uses it"
            );
            let text = slot(IconSet::Text, icon);
            assert!(
                !text.text.chars().any(private_use),
                "{icon:?} falls back to text any font has"
            );
            assert_eq!(usize::from(text.width), cells_width(text.text), "{icon:?}");
        }
    }

    #[test]
    fn text_falls_back_to_what_scuttle_showed_before_icons() {
        let text = |icon| slot(IconSet::Text, icon).text;
        let shown = [
            (Icon::Pin, "📌"),
            (Icon::Asking, "?"),
            (Icon::Failed, "!"),
            (Icon::Unread, "\u{1f535}"),
            (Icon::Archived, "archived"),
            (Icon::Queued, "queued · "),
            (Icon::Attached, "attached "),
        ];
        for (icon, fallback) in shown {
            assert_eq!(text(icon), fallback, "{icon:?}");
        }
        for icon in Icon::ALL
            .into_iter()
            .filter(|i| !shown.iter().any(|(s, _)| s == i))
        {
            assert_eq!(text(icon), "", "{icon:?} adds nothing in text mode");
        }
    }

    #[test]
    fn a_glyph_is_plain_under_no_color_and_a_fallback_keeps_its_style() {
        let accent = Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD);
        let nerd = Theme {
            icons: IconSet::Nerd,
            ..Theme::terminal(true)
        };
        let plain = Theme {
            icons: IconSet::Nerd,
            ..Theme::terminal_with(true, Colors::None)
        };
        let text = Theme::terminal_with(true, Colors::None);
        assert_eq!(
            lead(&nerd, Icon::Unread, accent),
            Some(Span::styled("\u{f444} ", accent))
        );
        assert_eq!(
            lead(&plain, Icon::Unread, accent),
            Some(Span::raw("\u{f444} "))
        );
        assert_eq!(
            lead(&text, Icon::Failed, accent),
            Some(Span::styled("!", accent)),
            "a text fallback keeps the style it had"
        );
        assert_eq!(
            lead(&text, Icon::Error, accent),
            None,
            "nothing to draw is no span"
        );
        assert_eq!(
            line_with(&text, "  ", Icon::Queued, "next", accent),
            vec![Span::styled("  queued · next", accent)],
            "text mode keeps the one span it drew before icons"
        );
        assert_eq!(
            line_with(&nerd, "  ", Icon::Queued, "next", accent),
            vec![
                Span::styled("  ", accent),
                Span::styled("\u{ea82} ", accent),
                Span::styled("next", accent),
            ]
        );
        assert_eq!(
            line_with(&plain, "", Icon::Error, "Error: x", accent),
            vec![Span::raw("\u{ea87} "), Span::styled("Error: x", accent)]
        );
        assert_eq!(style(&plain, accent), Style::new());
        assert_eq!(style(&text, accent), accent);
    }

    #[test]
    fn each_tool_name_maps_to_its_kind() {
        for (name, icon) in [
            ("execute", Icon::Terminal),
            ("process_output", Icon::Terminal),
            ("process_list", Icon::Terminal),
            ("process_signal", Icon::Terminal),
            ("read_file", Icon::File),
            ("read_template", Icon::File),
            ("read_skill", Icon::File),
            ("read_skill_file", Icon::File),
            ("write_file", Icon::Edit),
            ("edit_files", Icon::Edit),
            ("web_search", Icon::Web),
            ("web_fetch", Icon::Web),
            ("find_tools", Icon::Search),
            ("search_docs", Icon::Search),
            ("github__create_issue", Icon::Mcp),
            ("github__search_issues", Icon::Mcp),
            ("spawn_agent", Icon::Agent),
            ("spawn_explore_agent", Icon::Agent),
            ("wait_agent", Icon::Agent),
            ("message_agent", Icon::Agent),
            ("interrupt_agent", Icon::Agent),
            ("close_agent", Icon::Agent),
            ("propose_plan", Icon::Plan),
            ("ask_user_question", Icon::Question),
            ("create_workspace", Icon::Workspace),
            ("start_workspace", Icon::Workspace),
            ("stop_workspace", Icon::Workspace),
            ("list_templates", Icon::Workspace),
            ("attach_file", Icon::Tool),
            ("computer", Icon::Tool),
            ("advisor", Icon::Tool),
            ("something_new", Icon::Tool),
        ] {
            assert_eq!(tool_icon(name), icon, "{name}");
        }
    }
}
```

In `crates/scuttle-tui/src/main.rs`, after `mod highlight;`, add `mod icons;`.

In `mod tests` of `crates/scuttle-tui/src/theme.rs`, add:

```rust
    #[test]
    fn a_new_theme_draws_text_icons_and_an_icon_is_plain_under_no_color() {
        let t = Theme::terminal_with(true, Colors::Ansi256);
        assert_eq!(
            t.icons,
            IconSet::Text,
            "a theme draws text until the Tui picks the icons"
        );
        assert_eq!(t.colors, Colors::Ansi256);
        assert_eq!(t.icon(t.accent), t.accent);
        let plain = Theme::terminal_with(true, Colors::None);
        assert_eq!(plain.icon(plain.accent), Style::new());
        assert_eq!(plain.icon(plain.error), Style::new());
    }
```

In `mod tests` of `crates/scuttle-tui/src/transcript_view.rs`, in `narrow_terminal_welcome`, replace `insta::assert_snapshot!(draw(&view, 40, 12).backend());` with `insta::assert_snapshot!(draw(&view, 40, 14).backend());`, so both rows of the wrapped tip show.
Then add:

```rust
    /// The theme the tests draw with, with Nerd Font icons.
    fn nerd() -> Theme {
        Theme {
            icons: IconSet::Nerd,
            ..Theme::terminal(true)
        }
    }

    #[test]
    fn nerd_icons_put_each_tool_kind_in_its_own_slot_after_the_state_marker() {
        let names = [
            "execute",
            "read_file",
            "edit_files",
            "find_tools",
            "web_search",
            "github__create_issue",
            "propose_plan",
            "ask_user_question",
            "start_workspace",
            "attach_file",
        ];
        let calls: Vec<serde_json::Value> = names
            .iter()
            .enumerate()
            .map(|(i, name)| {
                json!({"type": "tool-call", "tool_call_id": format!("c{i}"),
                    "tool_name": name, "args": {"path": "a"}})
            })
            .collect();
        let app = app_with(json!([{"id": 1, "role": "assistant", "content": calls}]));
        let view_with = |theme: &Theme| {
            build(
                &app,
                &Default::default(),
                &Default::default(),
                &welcome(),
                theme,
                80,
            )
        };
        let text = tool_heads(&view_with(&Theme::terminal(true)));
        let glyphs = tool_heads(&view_with(&nerd()));
        assert_eq!(text.len(), names.len(), "{text:#?}");
        assert_eq!(glyphs.len(), names.len(), "{glyphs:#?}");
        for ((name, plain), head) in names.iter().zip(&text).zip(&glyphs) {
            assert_eq!(*plain, format!("◌ {name}(a)"), "text mode adds no glyph");
            let kind = icons::slot(IconSet::Nerd, icons::tool_icon(name)).text;
            assert_eq!(
                *head,
                format!("◌ {kind}{name}(a)"),
                "the kind's slot follows the one-cell marker"
            );
        }
        let theme = nerd();
        let view = view_with(&theme);
        let head = &view.lines[row_of(&view, "execute(a)")];
        assert_eq!(head.spans[0].content, "◌ ");
        // The wrap joins cells of one style, so the kind shares a span with the name.
        assert!(
            head.spans[1].content.starts_with("\u{ea85} execute"),
            "{head:?}"
        );
        assert_eq!(head.spans[1].style, theme.accent, "the kind takes the name's accent");
    }

    #[test]
    fn nerd_icons_lead_the_error_the_queued_messages_and_the_sent_files() {
        let mut app = app_with(json!([
            {"id": 1, "role": "user", "content": [
                {"type": "text", "text": "look"},
                {"type": "file", "file_id": "6f1c1b6e-8d4b-4c55-9a7e-1d2b3c4d5e6f", "file_name": "shot.png"},
                {"type": "file", "file_id": "7f1c1b6e-8d4b-4c55-9a7e-1d2b3c4d5e6f", "media_type": "text/plain"}
            ]}
        ]));
        app.transcript.queued = serde_json::from_value(
            json!([{"id": 5, "content": [{"type": "text", "text": "next question"}]}]),
        )
        .unwrap();
        app.transcript.last_error = Some("boom".into());
        let lines = |theme: &Theme| {
            texts(&build(
                &app,
                &Default::default(),
                &Default::default(),
                &welcome(),
                theme,
                60,
            ))
        };
        let text = lines(&Theme::terminal(true));
        for row in [
            "  attached shot.png",
            "  attached a text/plain file",
            "  queued · next question",
            "Error: boom",
        ] {
            assert!(text.iter().any(|l| l == row), "{row:?} is not in {text:#?}");
        }
        let glyphs = lines(&nerd());
        for row in [
            "  \u{ec34} shot.png",
            "  \u{ec34} a text/plain file",
            "  \u{ea82} next question",
            "\u{ea87} Error: boom",
        ] {
            assert!(
                glyphs.iter().any(|l| l == row),
                "{row:?} is not in {glyphs:#?}"
            );
        }
    }

    #[test]
    fn text_icons_suggest_a_nerd_font_with_a_link_over_its_name() {
        let app = App::new(BusyBehavior::Queue, true);
        let view_at = |theme: &Theme, width| {
            build(
                &app,
                &Default::default(),
                &Default::default(),
                &welcome(),
                theme,
                width,
            )
        };
        let theme = Theme::terminal(true);
        let wide = view_at(&theme, 80);
        let shown = texts(&wide);
        assert_eq!(shown[8], "", "a blank row sets the tip off");
        assert_eq!(shown[9], "scuttle works so much better with a Nerd Font!");
        assert_eq!(
            wide.links,
            vec![LinkHit {
                line: 9,
                cols: 36..45,
                url: icons::NERD_FONTS_URL.to_owned(),
                link: 0,
            }],
            "the link covers exactly the words Nerd Font"
        );
        let tip = &wide.lines[9];
        assert_eq!(tip.spans[1].content, "Nerd Font");
        assert!(tip.spans[1].style.add_modifier.contains(Modifier::UNDERLINED));
        assert_eq!(tip.spans[0].style, theme.dim);
        let narrow = view_at(&theme, 40);
        let shown = texts(&narrow);
        assert_eq!(shown[11], "scuttle works so much better with a ");
        assert_eq!(shown[12], "Nerd Font!");
        assert_eq!(
            narrow.links,
            vec![LinkHit {
                line: 12,
                cols: 0..9,
                url: icons::NERD_FONTS_URL.to_owned(),
                link: 0,
            }],
            "the link follows the wrap onto the next row"
        );
        let nerd_view = view_at(&nerd(), 80);
        assert!(nerd_view.links.is_empty(), "nerd mode shows no tip");
        assert!(
            !texts(&nerd_view).iter().any(|l| l.contains("Nerd Font")),
            "{:#?}",
            texts(&nerd_view)
        );
    }
```


In `mod tests` of `crates/scuttle-tui/src/footer.rs`, add:

```rust
    #[test]
    fn nerd_icons_mark_the_connection_and_error_notices_and_the_labels_stay() {
        let nerd = Theme {
            icons: IconSet::Nerd,
            ..Theme::terminal(true)
        };
        let text_theme = Theme::terminal(true);
        let mut app = App::new(BusyBehavior::Queue, true);
        app.connection = Connection::Connecting;
        assert!(text(&footer_line(&app, None, &nerd, 80)).contains("\u{eb2d} connecting"));
        let plain = text(&footer_line(&app, None, &text_theme, 80));
        assert!(plain.contains("connecting"), "{plain}");
        assert!(!plain.contains('\u{eb2d}'), "{plain}");
        app.connection = Connection::Reconnecting { attempt: 2 };
        assert!(
            text(&footer_line(&app, None, &nerd, 80)).contains("\u{ead0} reconnecting (attempt 2)")
        );
        app.plan_mode = true;
        assert!(
            text(&footer_line(&app, None, &nerd, 80)).contains("/plan-mode: on"),
            "the fields keep their /command: labels"
        );
        let error = Notice::Error("Could not send message: HTTP 409".into());
        let line = footer_line(&app, Some(&error), &nerd, 80);
        assert_eq!(text(&line), "\u{ea87} Could not send message: HTTP 409");
        assert_eq!(line.spans[0].style, nerd.error);
        assert_eq!(
            text(&footer_line(&app, Some(&error), &text_theme, 80)),
            "Could not send message: HTTP 409"
        );
        let narrow = footer_line(&app, Some(&error), &nerd, 10);
        assert_eq!(text(&narrow), "\u{ea87} Could no");
        assert!(cells_width(&text(&narrow)) <= 10);
        let info = Notice::Info("Settings applied.".into());
        assert_eq!(
            text(&footer_line(&app, Some(&info), &nerd, 80)),
            "Settings applied.",
            "an info notice takes no icon"
        );
    }
```

In `mod tests` of `crates/scuttle-tui/src/overlay.rs`, after `narrow_chats_drop_the_summary_first_and_keep_the_titles_in_line`, add:

```rust
    /// The test theme with Nerd Font icons.
    fn nerd_theme() -> Theme {
        Theme {
            icons: IconSet::Nerd,
            ..Theme::terminal(true)
        }
    }

    /// The row of `view` whose title cell reads `title`.
    fn chat_row(view: &TableView, title: &str) -> usize {
        (0..view.rows.len())
            .find(|&i| cell_text(view, i, 2).trim_start() == title)
            .unwrap_or_else(|| panic!("no row for {title}"))
    }

    #[test]
    fn nerd_icons_mark_the_chat_status_and_unread_and_keep_the_titles_in_line() {
        let (app, parent) = marked_app();
        let o = expanded_chats(&app, parent);
        let text_theme = Theme::terminal(true);
        let text_x = start_of(&drawn_chats(&o, &ctx_for(&app, &text_theme), None), "t-plain").0;
        let theme = nerd_theme();
        let pin = icons::slot(IconSet::Nerd, Icon::Pin).text;
        let nerd = ViewCtx {
            pin_icon: pin,
            ..ctx_for(&app, &theme)
        };
        let buf = drawn_chats(&o, &nerd, Some("X"));
        let (x, _) = start_of(&buf, "t-plain");
        assert_eq!(x, text_x, "nerd icons leave the titles where text puts them");
        for title in [
            "t-pinned",
            "t-unread",
            "t-running",
            "t-error",
            "t-asking",
            "t-parent",
        ] {
            assert_eq!(start_of(&buf, title).0, x, "{title} starts in the title column");
        }
        let (pin_x, status) = (x - 6, x - 3);
        let at =
            |title: &str, column: u16| buf[(column, start_of(&buf, title).1)].symbol().to_owned();
        assert_eq!(at("t-pinned", pin_x), "\u{f435}");
        assert_eq!(at("t-unread", status), "\u{f444}");
        assert_eq!(at("t-running", status), "X", "the spinner keeps its one cell");
        assert_eq!(at("t-error", status), "\u{f421}");
        assert_eq!(at("t-asking", status), "\u{f420}");
        let view = o.view(&nerd);
        assert_eq!(view.widths[0], Constraint::Length(2), "the pin's slot is two cells");
        assert_eq!(view.widths[STATUS_COLUMN], Constraint::Length(STATUS_WIDTH));
        let unread = chat_row(&view, "t-unread");
        let dot = &view.rows[unread].cells[STATUS_COLUMN].spans[0];
        assert_eq!(dot.content, "\u{f444} ");
        assert_eq!(dot.style, theme.accent, "the dot follows the theme's accent");
        let plain = Theme {
            icons: IconSet::Nerd,
            ..Theme::terminal_with(true, crate::theme::Colors::None)
        };
        let view = o.view(&ViewCtx {
            pin_icon: pin,
            ..ctx_for(&app, &plain)
        });
        let style = |title: &str| view.rows[chat_row(&view, title)].cells[STATUS_COLUMN].spans[0].style;
        let none = ratatui::style::Style::new();
        assert_eq!(style("t-unread"), none, "NO_COLOR leaves the glyph plain");
        assert_eq!(style("t-error"), none);
    }

    #[test]
    fn an_archived_chat_shows_the_archive_icon_in_a_two_cell_column() {
        let mut app = App::new(BusyBehavior::Queue, true);
        app.update(Msg::ChatsLoaded {
            query: ListQuery::Archived,
            offset: 0,
            chats: serde_json::from_value(json!([{"id": uuid::Uuid::new_v4(), "title": "old",
                "status": "waiting", "archived": true, "updated_at": "2026-09-30T10:00:00Z",
                "children": [], "files": [], "mcp_server_ids": [], "inline_mcp_servers": [],
                "labels": {}}]))
            .unwrap(),
        });
        let mut o = Overlay::chats(String::new(), &app);
        if let Overlay::Chats(state) = &mut o {
            state.filter = Filter::Archived;
        }
        let text_theme = Theme::terminal(true);
        let view = o.view(&ctx_for(&app, &text_theme));
        assert_eq!(cell_text(&view, chat_row(&view, "old"), 4), "archived");
        assert_eq!(view.widths[4], Constraint::Length(8));
        let theme = nerd_theme();
        let view = o.view(&ctx_for(&app, &theme));
        assert_eq!(cell_text(&view, chat_row(&view, "old"), 4), "\u{f411} ");
        assert_eq!(
            view.widths[4],
            Constraint::Length(2),
            "the icon gives the title six more columns"
        );
    }

    #[test]
    fn nerd_icons_mark_each_subagents_status_in_a_two_cell_column() {
        let parent = uuid::Uuid::new_v4();
        let kid = |title: &str, status: &str| {
            json!({"id": uuid::Uuid::new_v4(), "parent_chat_id": parent, "title": title,
                "status": status, "children": [], "files": [], "mcp_server_ids": [],
                "inline_mcp_servers": [], "labels": {}})
        };
        let chat = serde_json::from_value(json!({"id": parent, "title": "root",
            "children": [kid("k-error", "error"), kid("k-asking", "requires_action"),
                kid("k-running", "running")],
            "files": [], "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}}))
        .unwrap();
        let mut app = App::new(BusyBehavior::Queue, true);
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: Box::new(chat),
            messages: vec![],
        });
        let o = Overlay::Subagents(SubagentsState {
            table: TableState::default(),
            scroll: 0,
        });
        let markers = |theme: &Theme| {
            let view = o.view(&ctx_for(&app, theme));
            let cells: Vec<String> = view.rows.iter().map(|r| r.cells[0].to_string()).collect();
            (view.widths[0], cells, view.spinners)
        };
        let (width, cells, spinners) = markers(&Theme::terminal(true));
        assert_eq!(width, Constraint::Length(1));
        assert_eq!(cells, ["!", "?", spinner_frame(Duration::ZERO)]);
        let (width, cells, nerd_spinners) = markers(&nerd_theme());
        assert_eq!(width, Constraint::Length(STATUS_WIDTH));
        assert_eq!(
            cells,
            ["\u{f421} ", "\u{f420} ", spinner_frame(Duration::ZERO)]
        );
        assert_eq!(nerd_spinners, spinners, "the spinner stays in its one cell");
    }

    #[test]
    fn nerd_icons_lead_each_workspace_status_and_the_none_row() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let ws = |name: &str, status: &str| WorkspaceRef {
            id: uuid::Uuid::new_v4(),
            name: name.into(),
            status: status.into(),
            ..Default::default()
        };
        app.update(Msg::WorkspacesLoaded(vec![
            ws("up", "running"),
            ws("down", "stopped"),
            ws("broken", "failed"),
            ws("booting", "starting"),
        ]));
        let o = Overlay::open(Picker::Workspace, &app).unwrap();
        let status = |view: &TableView, name: &str| {
            let row = view
                .rows
                .iter()
                .find(|r| r.cells[0].to_string() == name)
                .unwrap_or_else(|| panic!("no row for {name}"));
            row.cells[2].to_string()
        };
        let text_theme = Theme::terminal(true);
        let view = o.view(&ctx_for(&app, &text_theme));
        assert_eq!(status(&view, "up"), "running");
        assert_eq!(status(&view, "broken"), "failed");
        assert_eq!(view.rows[0].cells.len(), 1, "none has no status in text");
        assert_eq!(view.widths[2], Constraint::Length(9));
        let theme = nerd_theme();
        let view = o.view(&ctx_for(&app, &theme));
        assert_eq!(status(&view, "up"), "\u{eb7b} running");
        assert_eq!(status(&view, "down"), "\u{eb7a} stopped");
        assert_eq!(status(&view, "broken"), "\u{ea87} failed");
        assert_eq!(status(&view, "booting"), "\u{eb19} starting");
        assert_eq!(view.rows[0].cells[2].to_string(), "\u{eabd} ");
        assert_eq!(view.widths[2], Constraint::Length(11));
        let up = view
            .rows
            .iter()
            .find(|r| r.cells[0].to_string() == "up")
            .unwrap();
        assert_eq!(up.cells[2].spans[0].style, theme.ok);
    }

    #[test]
    fn nerd_icons_show_each_mcp_server_on_or_off_and_mark_a_failure() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let (github, linear) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let chat = serde_json::from_value(json!({
            "id": uuid::Uuid::new_v4(), "mcp_server_ids": [github], "inline_mcp_servers": [],
            "children": [], "files": [], "labels": {}
        }))
        .unwrap();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: Box::new(chat),
            messages: vec![],
        });
        app.mcp_panel = Some(scuttle_core::panels::McpPanel {
            servers: scuttle_core::panels::Fetched::Loaded(
                serde_json::from_value(json!([
                    {"id": github, "display_name": "GitHub", "url": "https://github.example/mcp",
                        "tool_allow_list": [], "tool_deny_list": []},
                    {"id": linear, "display_name": "Linear", "url": "https://linear.example/mcp",
                        "tool_allow_list": [], "tool_deny_list": []}
                ]))
                .unwrap(),
            ),
            health: scuttle_core::panels::Fetched::Loaded(Some(vec![
                coder_sdk::McpConnectOutcome {
                    config_id: github,
                    slug: "github".into(),
                    outcome: "error".into(),
                    tool_count: 0,
                    error: "refused".into(),
                },
            ])),
        });
        let o = Overlay::Mcp(TableState::default());
        let row = |view: &TableView, name: &str| {
            let row = view
                .rows
                .iter()
                .find(|r| r.cells[0].to_string() == format!("  {name}"))
                .unwrap_or_else(|| panic!("no row for {name}"));
            (row.cells[2].to_string(), row.cells[3].to_string())
        };
        let text_theme = Theme::terminal(true);
        let view = o.view(&ctx_for(&app, &text_theme));
        assert_eq!(
            row(&view, "GitHub"),
            ("on".to_owned(), "failed: refused".to_owned())
        );
        assert_eq!(row(&view, "Linear"), ("off".to_owned(), String::new()));
        assert_eq!(view.widths[2], Constraint::Length(18));
        let theme = nerd_theme();
        let view = o.view(&ctx_for(&app, &theme));
        assert_eq!(
            row(&view, "GitHub"),
            (
                "\u{ebb3} on".to_owned(),
                "\u{ea87} failed: refused".to_owned()
            )
        );
        assert_eq!(
            row(&view, "Linear"),
            ("\u{ebb5} off".to_owned(), String::new())
        );
        assert_eq!(
            view.widths[2],
            Constraint::Length(20),
            "fits off (next message) and its icon"
        );
    }
```

In `mod tests` of `crates/scuttle-tui/src/help.rs`, add:

```rust
    #[test]
    fn text_help_says_where_to_get_a_nerd_font_and_how_to_turn_icons_on() {
        let shown = text(&help_lines(&Theme::terminal(true), 300));
        let at = |title: &str| {
            shown
                .iter()
                .position(|l| l == title)
                .unwrap_or_else(|| panic!("no {title} section in {shown:#?}"))
        };
        let (markers, icons, search) = (at("Markers in /chats"), at("Icons"), at("Searching /chats"));
        assert!(
            markers < icons && icons < search,
            "after the markers, and the search stays last"
        );
        let section = shown[icons..search].join("\n");
        for needle in [
            "scuttle works so much better with a Nerd Font!",
            crate::icons::NERD_FONTS_URL,
            "icons = \"nerd\"",
            "NERD_FONT=1",
            "Mono",
        ] {
            assert!(section.contains(needle), "{needle} is missing from:\n{section}");
        }
    }

    #[test]
    fn nerd_help_describes_the_glyphs_it_shows_and_drops_the_tip() {
        let nerd = Theme {
            icons: IconSet::Nerd,
            ..Theme::terminal(true)
        };
        let lines = text(&help_lines(&nerd, 300));
        assert!(!lines.iter().any(|l| l == "Icons"), "{lines:#?}");
        let shown = lines.join("\n");
        assert!(!shown.contains("nerdfonts.com"), "{shown}");
        for needle in [
            "Markers in /chats",
            "chats.pin_icon",
            "\u{f421} after an error",
            "\u{f420} while it waits on you",
            "\u{f444} for unread messages",
            "\u{f411} after the title marks an archived chat",
            "+N",
        ] {
            assert!(shown.contains(needle), "{needle} is missing from:\n{shown}");
        }
        assert!(!shown.contains('🔵'), "nerd mode names the glyph it draws, not the emoji");
    }
```

In `mod tests` of `crates/scuttle-tui/src/app.rs`, in `settings_creates_the_file_and_applies_the_live_keys`, replace `assert_eq!(t.config.chats.pin_icon, "*");` with `assert_eq!(t.config.chats.pin_icon.as_deref(), Some("*"));`.
Then add:

```rust
    #[test]
    fn nerd_font_decides_the_icons_only_while_the_file_sets_none() {
        let mut t = tui();
        assert_eq!(t.theme.icons, IconSet::Text, "neither set means text");
        assert_eq!(t.pin_icon(), "📌");
        t.set_icon_env(Some(IconSet::Nerd));
        assert_eq!(t.theme.icons, IconSet::Nerd);
        assert_eq!(t.pin_icon(), "\u{f435} ", "the unset pin follows the icons");
        t.set_icon_env(None);
        assert_eq!(t.theme.icons, IconSet::Text);
        let mut set = Tui::new(
            LocalConfig {
                icons: Some(IconSet::Text),
                ..LocalConfig::default()
            },
            None,
            Theme::terminal(true),
            Welcome {
                url: "https://x".into(),
                user: "nick".into(),
                art: vec![],
                show: true,
            },
            0,
        );
        set.set_icon_env(Some(IconSet::Nerd));
        assert_eq!(set.theme.icons, IconSet::Text, "the file wins over NERD_FONT");
    }

    #[test]
    fn settings_switch_the_icons_live_and_a_set_pin_wins() {
        let (mut t, path) = settings_tui();
        assert!(screen(&mut t, 80, 24).contains("better with a Nerd Font!"));
        t.edit_settings_with(|p| std::fs::write(p, "icons = \"nerd\"\n"));
        assert_eq!(t.theme.icons, IconSet::Nerd);
        assert_eq!(
            t.core.notices.last(),
            Some(&Notice::Info("Settings applied.".into())),
            "icons apply without a restart"
        );
        assert!(
            !screen(&mut t, 80, 24).contains("Nerd Font"),
            "the tip goes at once"
        );
        assert_eq!(t.pin_icon(), "\u{f435} ");
        t.edit_settings_with(|p| std::fs::write(p, "icons = \"nerd\"\n[chats]\npin_icon = \"*\"\n"));
        assert_eq!(t.pin_icon(), "*", "a pin in the file wins over the icon set's");
        t.edit_settings_with(|p| std::fs::write(p, "icons = \"text\"\n"));
        assert_eq!(t.theme.icons, IconSet::Text);
        assert_eq!(t.pin_icon(), "📌");
        assert!(screen(&mut t, 80, 24).contains("better with a Nerd Font!"));
        remove_settings(&path);
    }

    #[test]
    fn the_nerd_font_tip_lights_up_and_opens_nerdfonts_com() {
        let mut t = tui();
        let shown = screen(&mut t, 80, 24);
        let (x, y) = find(&shown, "Nerd Font!");
        mouse(&mut t, MouseEventKind::Moved, x + 1, y);
        assert!(lit(style_at(&mut t, 80, 24, x, y)), "the link lights up");
        assert!(lit(style_at(&mut t, 80, 24, x + 8, y)), "to its last letter");
        assert!(
            !lit(style_at(&mut t, 80, 24, x + 9, y)),
            "the ! after it is not the link"
        );
        mouse(&mut t, MouseEventKind::Down(MouseButton::Left), x + 2, y);
        assert_eq!(
            release(&mut t, x + 2, y),
            vec![Effect::OpenLink("https://www.nerdfonts.com/".into())]
        );
    }

    #[test]
    fn nerd_icons_mark_the_composer_chips_and_a_failed_one() {
        use scuttle_core::attachments::{Chip, ChipState};
        let mut t = tui();
        started(&mut t);
        t.update(Msg::Submit("/attach /tmp/notes.md".into()));
        let shown = screen(&mut t, 60, 14);
        assert!(shown.contains("[notes.md"), "text mode keeps the chip: {shown}");
        t.set_icon_env(Some(IconSet::Nerd));
        let shown = screen(&mut t, 60, 14);
        assert!(shown.contains("[\u{ec34} notes.md"), "{shown}");
        t.core.chips.push(Chip {
            local: 99,
            name: "big.zip".into(),
            size: None,
            state: ChipState::Failed("too large".into()),
            pasted: None,
        });
        let shown = screen(&mut t, 60, 14);
        assert!(shown.contains("[\u{ea87} big.zip: too large]"), "{shown}");
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --workspace -- icon nerd pin_icon mcp_rows`
Expected: FAIL to compile with "cannot find type `IconSet` in this scope", "cannot find function `slot` in this scope", and "no field `on` on type `&McpRow`".

- [ ] **Step 3: Parse `icons` and make the pin optional**

In `crates/scuttle-core/src/config.rs`, after `pub enum SpinnerSetting { .. }`, add:

```rust
/// Which icons scuttle draws, `icons`: Nerd Font glyphs, or the plain text and emoji that
/// any font shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IconSet {
    Nerd,
    Text,
}

impl IconSet {
    /// The set a `NERD_FONT` value asks for: `1`, `true`, or `yes` for nerd, and `0`, `false`,
    /// or `no` for text, ignoring case and surrounding spaces. Any other value asks for neither.
    pub fn from_env(value: &str) -> Option<IconSet> {
        match value.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" => Some(IconSet::Nerd),
            "0" | "false" | "no" => Some(IconSet::Text),
            _ => None,
        }
    }

    /// The set in effect: the file's `icons`, else what `NERD_FONT` asked for, else text,
    /// since scuttle cannot tell which font the terminal draws with.
    pub fn resolve(file: Option<IconSet>, env: Option<IconSet>) -> IconSet {
        file.or(env).unwrap_or(IconSet::Text)
    }
}
```

Replace `pub struct ChatsConfig` and its `impl Default for ChatsConfig` with:

```rust
/// The `[chats]` table: how `/chats` draws its rows.
#[derive(Debug, Clone, PartialEq, Deserialize, Default)]
#[serde(default)]
pub struct ChatsConfig {
    /// The marker before a pinned chat's title. Any text works, and an empty string shows
    /// none. Left out, the icon set's pin shows: 📌 in text, or nf-oct-pin with Nerd Font icons.
    pub pin_icon: Option<String>,
}
```

In `pub struct LocalConfig`, after `pub spinner: SpinnerSetting,`, add:

```rust
    /// Which icons to draw. `None` leaves it to the `NERD_FONT` environment variable, then text.
    pub icons: Option<IconSet>,
```

In `impl Default for LocalConfig`, after `spinner: SpinnerSetting::Random,`, add `icons: None,`.
In `TEMPLATE`, after the line `# spinner = "random"` and its blank line, add:

```text
# Icons: "nerd" draws Nerd Font glyphs, which need a Nerd Font as the terminal's font, and
# "text" keeps plain text and emoji. Left out, NERD_FONT=1 in the environment picks "nerd",
# and otherwise scuttle uses "text". Get a font at https://www.nerdfonts.com/.
# icons = "text"

```

In `TEMPLATE`, replace the two comment lines above `# pin_icon = "📌"` with:

```text
# The marker before a pinned chat in /chats. Any text works, and "" shows none. Left out,
# it follows icons: 📌 in text, or the Octicons pin with "nerd" icons.
```

No new comment line holds ` = `, so `the_template_holds_only_defaults_and_every_key_in_it_parses` uncomments only `icons = "text"` and the keys it already did.
`icons` does not match the secret-name check, and `restart_keys` leaves it out, so `/settings` applies it live.

- [ ] **Step 4: Say whether each MCP server is on and whether it failed**

In `crates/scuttle-core/src/panels.rs`, in `pub struct McpRow`, after `pub detail: String,`, add:

```rust
    /// Whether the server is on for the next message, which `state` words.
    pub on: bool,
    /// Whether the server's last connection failed, which `detail` explains.
    pub failed: bool,
```

In `mcp_groups`, in the organization servers' closure, replace:

```rust
                    let mut detail = Vec::new();
                    if s.auth_connected == Some(false) {
                        detail.push("needs reconnecting in the web UI".to_owned());
                    } else if let Some(o) = outcomes.iter().find(|o| Some(o.config_id) == s.id) {
                        detail.push(outcome_text(o));
                    }
```

with:

```rust
                    let mut detail = Vec::new();
                    let outcome = outcomes.iter().find(|o| Some(o.config_id) == s.id);
                    let reconnect = s.auth_connected == Some(false);
                    if reconnect {
                        detail.push("needs reconnecting in the web UI".to_owned());
                    } else if let Some(o) = outcome {
                        detail.push(outcome_text(o));
                    }
```

and in that closure's `McpRow { .. }`, after `detail: detail.join("; "),`, add:

```rust
                        on,
                        failed: !reconnect && outcome.is_some_and(|o| o.outcome == "error"),
```

In the inline servers' `McpRow { .. }`, after `detail: tool_lists(&s.tool_allow_list, &s.tool_deny_list).join("; "),`, add:

```rust
            on: true,
            failed: false,
```

In the workspace servers' closure, replace:

```rust
            let detail = match r.error.clone().filter(|e| !e.is_empty()) {
```

with:

```rust
            let error = r.error.clone().filter(|e| !e.is_empty());
            let failed = error.is_some();
            let detail = match error {
```

and in that closure's `McpRow { .. }`, after `detail,`, add:

```rust
                on: true,
                failed,
```

- [ ] **Step 5: Add the icon module**

In `crates/scuttle-tui/src/icons.rs`, above the test module, add:

```rust
//! The icons scuttle draws: each one as a Nerd Font glyph, or as the text scuttle showed in
//! its place before icons, whichever `Theme::icons` picks.
//!
//! A glyph always takes a slot of two cells, the glyph and a space, counted here and never
//! measured. A Nerd Font glyph is one cell wide by `width`, the measure ratatui places cells
//! by, but a non-Mono font draws many of them wider, and the space takes what spills.
//! `width_cjk` would count a glyph as two cells and put scuttle out of step with its own
//! buffer, so nothing here uses it. The codepoints were read from the `cmap` and `post`
//! tables of patched Nerd Fonts.

use ratatui::style::Style;
use ratatui::text::Span;

pub use scuttle_core::config::IconSet;

use crate::theme::Theme;
use crate::wrap::cells_width;

/// Where the Nerd Font tip sends the reader.
pub const NERD_FONTS_URL: &str = "https://www.nerdfonts.com/";

/// Something scuttle marks with an icon. The transcript, the composer, the footer,
/// `/workspace`, and `/mcp` use Codicons (nf-cod), and `/chats` and `/subagents` use
/// Octicons (nf-oct), so each surface keeps to one family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    /// A shell call: `execute` and the `process_*` tools. nf-cod-terminal.
    Terminal,
    /// A read: `read_file`, `read_template`, and the skill reads. nf-cod-file.
    File,
    /// A write: `write_file` and `edit_files`. nf-cod-edit.
    Edit,
    /// A search: `find_tools`, and any other tool named for searching. nf-cod-search.
    Search,
    /// The web: `web_search` and `web_fetch`. nf-cod-globe.
    Web,
    /// An MCP server's tool, named `server__tool`. nf-cod-mcp.
    Mcp,
    /// A subagent call. nf-cod-agent.
    Agent,
    /// `propose_plan`. nf-cod-checklist.
    Plan,
    /// `ask_user_question`. nf-cod-question.
    Question,
    /// A workspace or template call. nf-cod-vm.
    Workspace,
    /// Any other tool, so every tool head lines up. nf-cod-tools.
    Tool,
    /// An error: the transcript's error line, an error notice, a failed chip, a failed
    /// workspace, or a failed MCP server. nf-cod-error.
    Error,
    /// A queued message, in place of `queued ·`. nf-cod-history.
    Queued,
    /// A file sent with a message, in place of `attached`. nf-cod-attach.
    Attached,
    /// A file chip above the composer, inside its brackets. nf-cod-attach.
    Chip,
    /// The footer while the stream connects. nf-cod-plug.
    Connecting,
    /// The footer while the stream reconnects. nf-cod-debug_disconnect.
    Reconnecting,
    /// A pinned chat, unless `chats.pin_icon` names another marker. nf-oct-pin.
    Pin,
    /// A chat waiting on the user, in place of `?`. nf-oct-question.
    Asking,
    /// A chat that stopped on an error, in place of `!`. nf-oct-alert.
    Failed,
    /// A chat with unread messages, in place of 🔵. nf-oct-dot_fill.
    Unread,
    /// An archived chat, in place of `archived`. nf-oct-archive.
    Archived,
    /// A running workspace. nf-cod-vm_running.
    WorkspaceRunning,
    /// A stopped, canceled, or deleted workspace. nf-cod-vm_outline.
    WorkspaceStopped,
    /// A workspace between states, such as starting. nf-cod-loading.
    WorkspaceBusy,
    /// The workspace picker's "none" row. nf-cod-circle_slash.
    NoWorkspace,
    /// An MCP server that is on. nf-cod-pass_filled.
    ServerOn,
    /// An MCP server that is off. nf-cod-circle_large.
    ServerOff,
}

impl Icon {
    /// Every icon, for the tests that check each one.
    #[cfg(test)]
    pub const ALL: [Icon; 28] = [
        Icon::Terminal,
        Icon::File,
        Icon::Edit,
        Icon::Search,
        Icon::Web,
        Icon::Mcp,
        Icon::Agent,
        Icon::Plan,
        Icon::Question,
        Icon::Workspace,
        Icon::Tool,
        Icon::Error,
        Icon::Queued,
        Icon::Attached,
        Icon::Chip,
        Icon::Connecting,
        Icon::Reconnecting,
        Icon::Pin,
        Icon::Asking,
        Icon::Failed,
        Icon::Unread,
        Icon::Archived,
        Icon::WorkspaceRunning,
        Icon::WorkspaceStopped,
        Icon::WorkspaceBusy,
        Icon::NoWorkspace,
        Icon::ServerOn,
        Icon::ServerOff,
    ];

    /// The glyph and the space that ends its slot.
    fn nerd(self) -> &'static str {
        match self {
            Icon::Terminal => "\u{ea85} ",
            Icon::File => "\u{ea7b} ",
            Icon::Edit => "\u{ea73} ",
            Icon::Search => "\u{ea6d} ",
            Icon::Web => "\u{eb01} ",
            Icon::Mcp => "\u{ec47} ",
            Icon::Agent => "\u{ec67} ",
            Icon::Plan => "\u{eab3} ",
            Icon::Question => "\u{eb32} ",
            Icon::Workspace => "\u{ea7a} ",
            Icon::Tool => "\u{eb6d} ",
            Icon::Error => "\u{ea87} ",
            Icon::Queued => "\u{ea82} ",
            Icon::Attached | Icon::Chip => "\u{ec34} ",
            Icon::Connecting => "\u{eb2d} ",
            Icon::Reconnecting => "\u{ead0} ",
            Icon::Pin => "\u{f435} ",
            Icon::Asking => "\u{f420} ",
            Icon::Failed => "\u{f421} ",
            Icon::Unread => "\u{f444} ",
            Icon::Archived => "\u{f411} ",
            Icon::WorkspaceRunning => "\u{eb7b} ",
            Icon::WorkspaceStopped => "\u{eb7a} ",
            Icon::WorkspaceBusy => "\u{eb19} ",
            Icon::NoWorkspace => "\u{eabd} ",
            Icon::ServerOn => "\u{ebb3} ",
            Icon::ServerOff => "\u{ebb5} ",
        }
    }

    /// What text mode shows in the icon's place: exactly what scuttle showed there before
    /// icons, which for most icons is nothing.
    fn text(self) -> &'static str {
        match self {
            Icon::Pin => "📌",
            Icon::Asking => "?",
            Icon::Failed => "!",
            // U+1F535, two cells wide under both width rules.
            Icon::Unread => "\u{1f535}",
            Icon::Archived => "archived",
            Icon::Queued => "queued · ",
            Icon::Attached => "attached ",
            _ => "",
        }
    }
}

/// What an icon takes on screen: its text, and the cells it is counted as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Slot {
    pub text: &'static str,
    pub width: u16,
}

/// The slot `icon` takes in `set`. A Nerd Font glyph's slot is always two cells, the glyph and
/// a space; a text fallback is as wide as `wrap.rs` measures it.
pub fn slot(set: IconSet, icon: Icon) -> Slot {
    match set {
        IconSet::Nerd => Slot {
            text: icon.nerd(),
            width: 2,
        },
        IconSet::Text => {
            let text = icon.text();
            Slot {
                text,
                width: cells_width(text) as u16,
            }
        }
    }
}

/// The style an icon in `base` takes: a glyph loses its color under `NO_COLOR`, and a text
/// fallback keeps `base`, as scuttle drew it before icons.
pub fn style(theme: &Theme, base: Style) -> Style {
    match theme.icons {
        IconSet::Nerd => theme.icon(base),
        IconSet::Text => base,
    }
}

/// `icon` as a span in `base`, or `None` when `theme`'s set draws nothing for it.
pub fn lead(theme: &Theme, icon: Icon, base: Style) -> Option<Span<'static>> {
    let slot = slot(theme.icons, icon);
    (!slot.text.is_empty()).then(|| Span::styled(slot.text, style(theme, base)))
}

/// `before`, then `icon`, then `after`, all in `base`. Text mode keeps the one span scuttle
/// drew before icons; nerd mode gives the glyph its own span, so `NO_COLOR` can leave it plain.
pub fn line_with(
    theme: &Theme,
    before: &str,
    icon: Icon,
    after: &str,
    base: Style,
) -> Vec<Span<'static>> {
    match theme.icons {
        IconSet::Text => vec![Span::styled(
            format!("{before}{}{after}", icon.text()),
            base,
        )],
        IconSet::Nerd => {
            let mut spans = Vec::new();
            if !before.is_empty() {
                spans.push(Span::styled(before.to_owned(), base));
            }
            spans.push(Span::styled(icon.nerd(), theme.icon(base)));
            spans.push(Span::styled(after.to_owned(), base));
            spans
        }
    }
}

/// The kind of tool `name` calls, which picks the glyph after its state marker. Tool names
/// are the ones `coderd/x/chatd` registers.
pub fn tool_icon(name: &str) -> Icon {
    match name {
        "execute" | "process_output" | "process_list" | "process_signal" => Icon::Terminal,
        "read_file" | "read_template" | "read_skill" | "read_skill_file" => Icon::File,
        "write_file" | "edit_files" => Icon::Edit,
        "web_search" | "web_fetch" => Icon::Web,
        "propose_plan" => Icon::Plan,
        "ask_user_question" => Icon::Question,
        "create_workspace" | "start_workspace" | "stop_workspace" | "list_templates" => {
            Icon::Workspace
        }
        _ if crate::subagent::action(name).is_some() => Icon::Agent,
        // An MCP tool is named `server__tool`, so the server's words come first.
        _ if name.contains("__") => Icon::Mcp,
        _ if name == "find_tools" || name.contains("search") => Icon::Search,
        _ => Icon::Tool,
    }
}
```

- [ ] **Step 6: Carry the icon set on the theme**

In `crates/scuttle-tui/src/theme.rs`, after `use ratatui::style::{Color, Modifier, Style};`, add `use scuttle_core::config::IconSet;`.
In `pub struct Theme`, after `pub link_hover: Style,`, add:

```rust
    /// How many colors the terminal shows; under `Colors::None` an icon's glyph is plain.
    pub colors: Colors,
    /// Which icons to draw. A new theme draws text; `Tui` sets it from `icons` and
    /// `NERD_FONT`.
    pub icons: IconSet,
```

In `Theme::terminal_with`, after `link_hover,` in the `Theme { .. }` literal, add:

```rust
            colors,
            icons: IconSet::Text,
```

After `Theme::terminal_with`, add:

```rust
    /// `style` for an icon's glyph: as it is, or without color under `NO_COLOR`.
    pub fn icon(&self, style: Style) -> Style {
        if self.colors == Colors::None {
            Style::new()
        } else {
            style
        }
    }
```

- [ ] **Step 7: Pick the icons in the `Tui` and read `NERD_FONT` once**

In `crates/scuttle-tui/src/app.rs`, add `use crate::icons::{self, Icon, IconSet};` beside the other `crate::` imports.
In `pub struct Tui`, after `theme: Theme,`, add:

```rust
    /// What `NERD_FONT` asked for at startup, which picks the icons while `config.toml` sets
    /// no `icons`.
    icon_env: Option<IconSet>,
```

In `Tui::new`, before `let spinner = SpinnerStyle::pick(config.spinner, seed);`, add:

```rust
        // The environment's answer arrives through `set_icon_env`, so until then the file decides.
        let theme = Theme {
            icons: IconSet::resolve(config.icons, None),
            ..theme
        };
```

and in the `Tui { .. }` literal, after `theme,`, add `icon_env: None,`.
After `Tui::set_keyboard_enhanced`, add:

```rust
    /// Records what `NERD_FONT` asks for, which `main` reads once at startup, and draws with
    /// the icons it picks while `config.toml` sets none. Tests pass a value instead of reading
    /// the environment.
    pub fn set_icon_env(&mut self, env: Option<IconSet>) {
        self.icon_env = env;
        self.apply_icons();
    }

    /// Draws with the icons `config.toml` and `NERD_FONT` pick, rebuilding the lines and rows
    /// that show them.
    fn apply_icons(&mut self) {
        let icons = IconSet::resolve(self.config.icons, self.icon_env);
        if self.theme.icons != icons {
            self.theme.icons = icons;
            self.view_revision += 1;
            self.overlay_view = None;
            self.preview_view = None;
        }
    }

    /// The marker before a pinned chat in `/chats`: `chats.pin_icon` when the file sets it,
    /// else the icon set's pin.
    fn pin_icon(&self) -> &str {
        match self.config.chats.pin_icon.as_deref() {
            Some(icon) => icon,
            None => icons::slot(self.theme.icons, Icon::Pin).text,
        }
    }
```

In `Tui::apply_settings`, replace:

```rust
        // The density and the pin icon are read when the lines and the rows are built.
        self.view_revision += 1;
        self.overlay_view = None;
        self.config = new;
```

with:

```rust
        // The density, the pin icon, and the icons are read when the lines and the rows are
        // built.
        self.view_revision += 1;
        self.overlay_view = None;
        self.config = new;
        self.apply_icons();
```

In the two `ViewCtx { .. }` literals of `Tui::overlay_key` and `Tui::draw_at`, replace `pin_icon: &self.config.chats.pin_icon,` with `pin_icon: self.pin_icon(),`.
In `Tui::chip_lines`, replace:

```rust
            let text = format!("[{text}]");
            let w = crate::wrap::cells_width(&text);
```

with:

```rust
            // The icon leads the name inside the brackets; a failed chip's says it failed.
            let icon = match chip.state {
                ChipState::Failed(_) => Icon::Error,
                _ => Icon::Chip,
            };
            let spans = icons::line_with(&self.theme, "[", icon, &format!("{text}]"), style);
            let w: usize = spans
                .iter()
                .map(|s| crate::wrap::cells_width(&s.content))
                .sum();
```

then replace `crate::wrap::wrap_line(&Line::from(Span::styled(text, style)), width as u16);` with `crate::wrap::wrap_line(&Line::from(spans), width as u16);`, and replace `row.push(Span::styled(text, style));` with `row.extend(spans);`.

In `crates/scuttle-tui/src/main.rs`, after the `let mut tui = app::Tui::new(..);` statement, add:

```rust
    // The icons follow `NERD_FONT` while config.toml sets no `icons`; it is read once, here.
    tui.set_icon_env(
        std::env::var("NERD_FONT")
            .ok()
            .as_deref()
            .and_then(config::IconSet::from_env),
    );
```

In `crates/scuttle-tui/tests/pty.rs`, in `spawn_with_args`, after `cmd.env_remove("CODER_SESSION_TOKEN");`, add:

```rust
    // The icons follow NERD_FONT, so the screens these tests wait for are drawn in text.
    cmd.env_remove("NERD_FONT");
```

- [ ] **Step 8: Draw the transcript's icons and the Nerd Font tip**

In `crates/scuttle-tui/src/transcript_view.rs`, replace `use ratatui::style::Style;` with `use ratatui::style::{Modifier, Style};`, and add `use crate::icons::{self, Icon, IconSet};` before `use crate::markdown;`.
Replace `fn file_label` with:

```rust
/// What names a sent file after its icon: its name, else its type.
fn file_label(p: &types::CodersdkChatMessagePart) -> String {
    let name = [&p.file_name, &p.name, &p.workspace_file_name]
        .into_iter()
        .find_map(|n| n.as_deref().filter(|n| !n.trim().is_empty()));
    match (name, p.media_type.as_deref().filter(|t| !t.is_empty())) {
        (Some(name), _) => name.to_owned(),
        (None, Some(kind)) => format!("a {kind} file"),
        (None, None) => "a file".to_owned(),
    }
}
```

In `render_items`, in the `Item::File(label)` arm, replace `let line = Line::from(Span::styled(format!("  {label}"), out.theme.dim));` with:

```rust
                let line = Line::from(icons::line_with(
                    out.theme,
                    "  ",
                    Icon::Attached,
                    &label,
                    out.theme.dim,
                ));
```

In the `Item::Tool { .. }` arm, replace:

```rust
                let head_budget = width.saturating_sub(name.chars().count() + 6).max(8);
                let mut head = vec![marker];
```

with:

```rust
                // The kind's icon gets its own slot after the state marker, which the spinner
                // paints over, so the marker stays one cell.
                let kind = icons::lead(out.theme, icons::tool_icon(name), out.theme.accent);
                let kind_width = kind.as_ref().map_or(0, |k| cells_width(&k.content));
                let head_budget = width
                    .saturating_sub(name.chars().count() + 6 + kind_width)
                    .max(8);
                let mut head = vec![marker];
                head.extend(kind);
```

After `fn welcome_lines`, add:

```rust
/// The tip text mode shows under the welcome: the words before the link, the link's text,
/// and what follows it.
const TIP: [&str; 3] = ["scuttle works so much better with a ", "Nerd Font", "!"];

/// Appends a blank row and the Nerd Font tip, with "Nerd Font" registered as a link to
/// nerdfonts.com on every row it wraps onto, so it lights up and opens as an assistant's
/// link does.
fn push_nerd_font_tip(out: &mut Out) {
    out.flush_hidden();
    out.extend_rows(vec![(Line::default(), false)], false);
    let [before, link, after] = TIP;
    let line = Line::from(vec![
        Span::styled(before, out.theme.dim),
        Span::styled(link, Style::new().add_modifier(Modifier::UNDERLINED)),
        Span::styled(after, out.theme.dim),
    ]);
    let rows = wrap_line(&line, out.width);
    let first = out.view.lines.len();
    let start = cells_width(before);
    let id = out.links_numbered;
    out.links_numbered += 1;
    for (row, cols) in cols_on_rows(&rows, &(start..start + cells_width(link))) {
        out.view.links.push(LinkHit {
            line: first + row,
            cols: cols.start as u16..cols.end as u16,
            url: icons::NERD_FONTS_URL.to_owned(),
            link: id,
        });
    }
    out.push_wrapped(vec![rows], false);
}
```

In `build_transcript`, replace:

```rust
        if let Some(welcome) = welcome {
            out.push(welcome_lines(welcome, theme, out.width), false);
        }
```

with:

```rust
        if let Some(welcome) = welcome {
            out.push(welcome_lines(welcome, theme, out.width), false);
            // Only text mode suggests the font the icons need.
            if theme.icons == IconSet::Text {
                push_nerd_font_tip(&mut out);
            }
        }
```

Replace the `for queued in &source.transcript.queued { .. }` loop with:

```rust
    for queued in &source.transcript.queued {
        let text = one_line(
            &queued_text(queued),
            (width as usize).saturating_sub(12).max(8),
        );
        out.push(
            vec![Line::from(icons::line_with(
                theme,
                "  ",
                Icon::Queued,
                &text,
                theme.dim,
            ))],
            false,
        );
    }
```

and in the `if let Some(err) = source.transcript.last_error.as_ref() { .. }` block, replace:

```rust
            vec![Line::from(Span::styled(
                format!("Error: {err}"),
                theme.error,
            ))],
```

with:

```rust
            vec![Line::from(icons::line_with(
                theme,
                "",
                Icon::Error,
                &format!("Error: {err}"),
                theme.error,
            ))],
```

Replace the contents of `crates/scuttle-tui/src/snapshots/scuttle__transcript_view__tests__welcome_shows_on_a_blank_chat.snap` with:

```text
---
source: crates/scuttle-tui/src/transcript_view.rs
expression: "draw(&view, 80, 12).backend()"
---
"                                                                                "
"scuttle                                                                         "
"An unofficial terminal client for Coder Agents                                  "
"                                                                                "
"Signed in as nick                                                               "
"Deployment https://dogfood.example                                              "
"                                                                                "
"Type a message to start. /help lists commands.                                  "
"                                                                                "
"scuttle works so much better with a Nerd Font!                                  "
"                                                                                "
"                                                                                "
```

Replace the contents of `crates/scuttle-tui/src/snapshots/scuttle__transcript_view__tests__narrow_terminal_welcome.snap` with:

```text
---
source: crates/scuttle-tui/src/transcript_view.rs
expression: "draw(&view, 40, 14).backend()"
---
"                                        "
"scuttle                                 "
"An unofficial terminal client for Coder "
"Agents                                  "
"                                        "
"Signed in as nick                       "
"Deployment https://dogfood.example      "
"                                        "
"Type a message to start. /help lists    "
"commands.                               "
"                                        "
"scuttle works so much better with a     "
"Nerd Font!                              "
"                                        "
```

- [ ] **Step 9: Mark the footer's connection and error notices**

In `crates/scuttle-tui/src/footer.rs`, add `use crate::icons::{self, Icon, IconSet};` before `use crate::markdown::drawable;`.
Replace `fn connection_status` with:

```rust
/// The connection, a provider retry, or the chat's status. A stream that connects or
/// reconnects leads with `set`'s icon, the one footer state worth catching at a glance.
fn connection_status(app: &App, set: IconSet) -> String {
    match app.connection {
        Connection::Reconnecting { attempt } => {
            let icon = icons::slot(set, Icon::Reconnecting).text;
            match app.last_stream_error.as_deref() {
                Some(error) => format!("{icon}reconnecting (attempt {attempt}): {error}"),
                None => format!("{icon}reconnecting (attempt {attempt})"),
            }
        }
        Connection::Connecting => format!("{}connecting", icons::slot(set, Icon::Connecting).text),
        Connection::Idle => "new chat".into(),
        Connection::Live => match (&app.transcript.retry, &app.transcript.status) {
            (Some(retry), _) => retry_status(retry),
            (None, Some(ChatStatus::RequiresAction)) => "action required".into(),
            (None, Some(s)) => s.as_str().replace('_', " "),
            (None, None) => "ready".into(),
        },
    }
}
```

Replace the line `fn field_for(app: &App, name: StatusField, thresholds: &Thresholds) -> Option<Field> {` with `fn field_for(app: &App, name: StatusField, thresholds: &Thresholds, set: IconSet) -> Option<Field> {`, and in it replace `StatusField::Status => Some(Field::plain(connection_status(app))),` with `StatusField::Status => Some(Field::plain(connection_status(app, set))),`.
In `status_line`, replace both `field_for(app, name, thresholds)` calls with `field_for(app, name, thresholds, theme.icons)`, and replace:

```rust
    if let Some(notice) = notice {
        let (text, style) = match notice {
            Notice::Info(t) => (t.clone(), theme.dim),
            Notice::Error(t) => (t.clone(), theme.error),
        };
        return Line::from(Span::styled(fit(text, width), style));
    }
```

with:

```rust
    if let Some(notice) = notice {
        return match notice {
            Notice::Info(t) => Line::from(Span::styled(fit(t.clone(), width), theme.dim)),
            // An error leads with its icon when the line has room for the icon's slot.
            Notice::Error(t) => {
                let icon = usize::from(icons::slot(theme.icons, Icon::Error).width);
                match width.checked_sub(icon).filter(|_| icon > 0) {
                    Some(room) => Line::from(icons::line_with(
                        theme,
                        "",
                        Icon::Error,
                        &fit(t.clone(), room),
                        theme.error,
                    )),
                    None => Line::from(Span::styled(fit(t.clone(), width), theme.error)),
                }
            }
        };
    }
```

- [ ] **Step 10: Draw the overlays' icons**

In `crates/scuttle-tui/src/overlay.rs`, add `use crate::icons::{self, Icon, IconSet};` before `use crate::table::{..};`.
In `fn workspace_view`, replace:

```rust
    if filter.trim().is_empty() {
        rows.push(Row::item(
            RowKey::Workspace(None),
            vec![Line::from("none (no workspace)")],
        ));
    }
```

with:

```rust
    if filter.trim().is_empty() {
        let mut none = vec![Line::from("none (no workspace)")];
        // The icon sits in the status column, where the other rows have theirs.
        if let Some(icon) = icons::lead(ctx.theme, Icon::NoWorkspace, ctx.theme.dim) {
            none.extend([Line::default(), Line::from(icon)]);
        }
        rows.push(Row::item(RowKey::Workspace(None), none));
    }
```

replace `Line::from(w.status.clone()),` with `workspace_status(&w.status, ctx),`, and replace the status column's `Constraint::Length(9),` with:

```rust
            // The longest status, and its icon's slot with Nerd Font icons.
            Constraint::Length(9 + icons::slot(ctx.theme.icons, Icon::WorkspaceBusy).width),
```

After `fn workspace_view`, add:

```rust
/// A workspace's status cell: the word, led with Nerd Font icons by its state's icon in the
/// state's color.
fn workspace_status(status: &str, ctx: &ViewCtx) -> Line<'static> {
    let (icon, style) = match status {
        "" => return Line::from(String::new()),
        "running" => (Icon::WorkspaceRunning, ctx.theme.ok),
        "failed" => (Icon::Error, ctx.theme.error),
        "stopped" | "canceled" | "deleted" => (Icon::WorkspaceStopped, ctx.theme.dim),
        _ => (Icon::WorkspaceBusy, ctx.theme.warn),
    };
    let mut spans: Vec<Span<'static>> = icons::lead(ctx.theme, icon, style).into_iter().collect();
    spans.push(Span::raw(status.to_owned()));
    Line::from(spans)
}
```

In `fn mcp_view`, replace:

```rust
            let cells = vec![
                Line::from(format!("  {}", r.name)),
                Line::from(Span::styled(r.url.clone(), ctx.theme.dim)),
                Line::from(r.state.clone()),
                Line::from(Span::styled(r.detail.clone(), ctx.theme.dim)),
            ];
```

with:

```rust
            let (icon, style) = if r.on {
                (Icon::ServerOn, ctx.theme.ok)
            } else {
                (Icon::ServerOff, ctx.theme.dim)
            };
            let mut state: Vec<Span<'static>> =
                icons::lead(ctx.theme, icon, style).into_iter().collect();
            state.push(Span::raw(r.state.clone()));
            let mut detail: Vec<Span<'static>> = if r.failed {
                icons::lead(ctx.theme, Icon::Error, ctx.theme.error)
                    .into_iter()
                    .collect()
            } else {
                Vec::new()
            };
            detail.push(Span::styled(r.detail.clone(), ctx.theme.dim));
            let cells = vec![
                Line::from(format!("  {}", r.name)),
                Line::from(Span::styled(r.url.clone(), ctx.theme.dim)),
                Line::from(state),
                Line::from(detail),
            ];
```

and replace:

```rust
            // Fits "off (next message)".
            Constraint::Length(18),
```

with:

```rust
            // Fits "off (next message)", and its icon's slot with Nerd Font icons.
            Constraint::Length(18 + icons::slot(ctx.theme.icons, Icon::ServerOff).width),
```

After `fn marker`, add:

```rust
/// A chat's status marker in the accent color: the spinner frame for `ctx.elapsed` while the
/// agent works, the icon for an error or a question, else one blank cell. The table repaints a
/// spinning marker on timer frames, and the spinner stays one cell in both icon sets.
fn status_span(status: Option<&ChatStatus>, ctx: &ViewCtx) -> Span<'static> {
    let icon = match status {
        _ if spins(status) => return Span::styled(spinner_frame(ctx.elapsed), ctx.theme.accent),
        Some(ChatStatus::RequiresAction) => Icon::Asking,
        Some(ChatStatus::Error) => Icon::Failed,
        _ => return Span::styled(" ", ctx.theme.accent),
    };
    icons::lead(ctx.theme, icon, ctx.theme.accent)
        .unwrap_or_else(|| Span::styled(" ", ctx.theme.accent))
}
```

In `fn status_cell`, replace:

```rust
    if attention {
        Line::from(Span::styled(
            marker(r.status.as_ref(), ctx.elapsed),
            ctx.theme.accent,
        ))
    } else if unread {
        Line::from(UNREAD)
    } else {
```

with:

```rust
    if attention {
        Line::from(status_span(r.status.as_ref(), ctx))
    } else if unread {
        // The emoji keeps its own colors; the glyph takes the accent, so it follows the theme.
        match icons::lead(ctx.theme, Icon::Unread, ctx.theme.accent) {
            Some(dot) if ctx.theme.icons == IconSet::Nerd => Line::from(dot),
            _ => Line::from(UNREAD),
        }
    } else {
```

Text mode still draws `UNREAD`, the same dot as `Icon::Unread`'s fallback, and `marker` stays for the family column.
In `fn chat_cells`, replace:

```rust
        Line::from(Span::styled(
            if r.archived { "archived" } else { "" },
            ctx.theme.dim,
        )),
```

with:

```rust
        Line::from(Span::styled(
            if r.archived {
                icons::slot(ctx.theme.icons, Icon::Archived).text
            } else {
                ""
            },
            icons::style(ctx.theme, ctx.theme.dim),
        )),
```

In `fn chats_view`, replace the archived column's `Constraint::Length(8),` with:

```rust
        // "archived", or its icon's two-cell slot.
        Constraint::Length(icons::slot(ctx.theme.icons, Icon::Archived).width),
```

In `fn subagents_view`, replace:

```rust
                    Line::from(Span::styled(
                        marker(status.as_ref(), ctx.elapsed),
                        ctx.theme.accent,
                    )),
```

with:

```rust
                    Line::from(status_span(status.as_ref(), ctx)),
```

and replace the first width, `Constraint::Length(1),`, with:

```rust
            // The one-cell marker, or an icon's two-cell slot.
            Constraint::Length(match ctx.theme.icons {
                IconSet::Nerd => STATUS_WIDTH,
                IconSet::Text => 1,
            }),
```

- [ ] **Step 11: Describe the icons in `/help`**

In `crates/scuttle-tui/src/help.rs`, add `use crate::icons::IconSet;` beside the other `crate::` imports.
After `pub const CHATS_MARKERS`, add:

```rust
/// `CHATS_MARKERS` for Nerd Font icons, naming the glyphs `/chats` draws in their place.
pub const CHATS_MARKERS_NERD: &[&str] = &[
    "Each row in /chats starts with two fixed columns, so every title lines up: the pin, from chats.pin_icon or else the Octicons pin \u{f435}, then the chat's status.",
    "The status shows the most important of these: a spinner while the agent works, \u{f421} after an error, \u{f420} while it waits on you, then \u{f444} for unread messages.",
    "A working chat that is also unread keeps its spinner, and the \u{f444} shows once it stops.",
    "\u{f411} after the title marks an archived chat.",
    "+N after a title counts its subagents, followed by the busiest one's marker, and a subagent is indented under its parent.",
];

/// Where to get a Nerd Font and how to turn icons on, which `/help` shows in text mode
/// between the `/chats` markers and the search, since the welcome screen's tip is not always
/// on screen.
pub const ICONS_HELP: &[&str] = &[
    "scuttle works so much better with a Nerd Font! https://www.nerdfonts.com/",
    "Install one, set it as your terminal font, then set icons = \"nerd\" in config.toml or NERD_FONT=1 in the environment. A Mono variant keeps every icon inside its cell.",
];
```

In `help_lines_for`, replace:

```rust
    for sentence in CHATS_MARKERS {
        lines.extend(wrap_line(&Line::from(*sentence), width));
    }
```

with:

```rust
    let markers = match theme.icons {
        IconSet::Nerd => CHATS_MARKERS_NERD,
        IconSet::Text => CHATS_MARKERS,
    };
    for sentence in markers {
        lines.extend(wrap_line(&Line::from(*sentence), width));
    }
    if theme.icons == IconSet::Text {
        lines.push(Line::default());
        lines.push(Line::from(Span::styled("Icons", theme.accent)));
        for sentence in ICONS_HELP {
            lines.extend(wrap_line(&Line::from(*sentence), width));
        }
    }
```

- [ ] **Step 12: Run the tests to verify they pass**

Run: `cargo test --workspace -- icon nerd pin_icon mcp welcome the_template saving_a_setting settings_ chats subagent workspace footer help_ notice`
Expected: PASS, including Task 13's `/chats` tests, the two welcome snapshots as Step 8 wrote them, and every existing text-mode assertion unchanged.

- [ ] **Step 13: Run the full checks**

Run: `cargo fmt --all && cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check`
Expected: all green, with no insta snapshot changed beyond the two welcome snapshots.

- [ ] **Step 14: Commit**

```bash
git add crates/scuttle-core/src/config.rs crates/scuttle-core/src/panels.rs crates/scuttle-tui/src/icons.rs crates/scuttle-tui/src/theme.rs crates/scuttle-tui/src/main.rs crates/scuttle-tui/src/app.rs crates/scuttle-tui/src/transcript_view.rs crates/scuttle-tui/src/footer.rs crates/scuttle-tui/src/overlay.rs crates/scuttle-tui/src/help.rs crates/scuttle-tui/src/snapshots/scuttle__transcript_view__tests__welcome_shows_on_a_blank_chat.snap crates/scuttle-tui/src/snapshots/scuttle__transcript_view__tests__narrow_terminal_welcome.snap crates/scuttle-tui/tests/pty.rs
git commit -m "feat: draw Nerd Font icons with icons = \"nerd\", and suggest a Nerd Font in text mode" \
  -m "A top-level icons key picks Nerd Font glyphs or text; left out, NERD_FONT=1 picks the glyphs, and otherwise scuttle draws text as before. Glyphs mark each tool's kind in the transcript, the error, queued, and attached lines, the composer chips, the footer's connection and error notices, and the /chats, /subagents, /workspace, and /mcp markers, each in a two-cell slot and plain under NO_COLOR. The footer keeps its /command: labels, an explicit chats.pin_icon still wins, and /settings applies the change live." \
  -m "In text mode the welcome screen says scuttle works so much better with a Nerd Font, with a clickable link to nerdfonts.com, and /help explains how to turn icons on." \
  -m "Assisted-by: AI"
```

---

## Spec coverage

| Requirement | Task |
|-------------|------|
| `/statusline` chooses which fields appear and in what order, stored locally (section 6) | 1, 11 |
| Fields `model`, `workspace`, `status`, `context` keep their M1 to M2.5 behavior (section 6) | 6 |
| `cost`, `spend`, `quota`, and `queue` fields and their sources (section 6) | 2, 3, 4, 6 |
| Default fields, with the M2.5 command hints and brand accent kept (section 6, M2.5) | 1, 6 |
| Threshold warnings, off by default, highlighted even when hidden (section 6) | 1, 2, 6 |
| Spend and quota refresh every 60 seconds and after each completed turn; cost after each turn (section 6) | 3, 7 |
| Money in integer micros, formatted only for display (section 6) | 2 |
| `/usage` overlay: spend, period and reset, budget source, chat cost and request counts with unpriced requests, context, quota; `null` budget unlimited, `-1` no quota (section 6) | 2, 12 |
| `404` hides the spend and quota fields and sections (section 8) | 3, 6, 12 |
| `403` hides the field and shows the server's message once in `/usage` (section 8) | 3, 12 |
| A `401` stops background refreshes and shows one message (section 8) | 3 |
| Cost in the footer (M2 design, "Moved out of M2") | 3, 6, 7 |
| `"\r\n"` in a notice becomes two spaces; stale notices replay (M2 design section 17) | 5 |
| The footer drops the connection status before plan mode at narrow widths (M2 design section 17) | 6 |
| Footer `fit()` newline and grapheme handling (M2 design section 17) | 5 |
| A graceful-shutdown handler for SIGINT, SIGTERM, and SIGHUP outside a handoff (parked for M3) | 8 |
| A bounded exit that cannot hang on a stuck `spawn_blocking` read (parked for M3) | 9 |
| The SDK's `server_version` gets the token redaction of the other hand-built requests (parked for M3) | S1, 10 |
| `main.rs` calls `server_version()` outside `runtime.rs` (M2 design section 17, "Later") | 10 |
| `/chats` titles align behind fixed pin and status columns, and unread shows as 🔵 (feedback item 47) | 13 |
| `/chats` shows each chat's pull request with a state icon and its number, Nerd Font glyphs with `icons = "nerd"` and a plain-text fallback such as `PR #123 merged`, colored by state but never under `NO_COLOR`, and dropped at narrow widths before the summary (feedback item 48) | 19, 14 |
| No text in the composer is underlined, and the full-width rule below the composer is back, with the copy notice still in the top rule (feedback item 49) | 15 |
| A paste over the threshold shows as a `[Pasted text #1 +120 lines]` token and goes as a text file attachment, as the web UI attaches it; pasting it again right away, or a key on the token, expands it inline; it survives `/settings`, the held drafts, and a refused send, and Ctrl+O copies it expanded (feedback item 50) | 16 |
| Cmd+Left and Cmd+Right and Home and End act on the current line, in the composer and the one-line editors, and Ctrl+End reaches the end of the whole message (feedback item 51) | 17 |
| The activity row does not repeat a live animated reasoning or tool marker, for one tool or several in parallel, and stays when nothing in the transcript animates (feedback item 52) | 18 |
| One `icons = "nerd" \| "text"` setting, with the `NERD_FONT` fallback and text by default, drives every icon from the audit's high and medium surfaces, in two-cell slots, plain under `NO_COLOR`, switched live by `/settings`, with an explicit `chats.pin_icon` still winning; in text mode the welcome screen says "scuttle works so much better with a Nerd Font!" with a clickable, hover-lit link to https://www.nerdfonts.com/, and `/help` explains how to turn icons on (feedback item 53) | 19, 14 |
