# M2: Navigation and Chat Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make scuttle the author's only Coder Agents client for a working day: switch between chats and subagents with live list updates, manage chats (`/chats`, `/title`, `/queue`), answer plan-mode questions, attach files, pick models from a grouped table, insert skills from the slash menu, and inspect the chat (`/info`, `/workspace`, `/git`, `/mcp`), each as one slash command that opens one overlay or runs one action.

**Architecture:** M2 keeps M1's split: `scuttle-core` owns every state change and decision with no terminal code, `scuttle-tui` draws and maps keys to `Msg` values, and only `runtime.rs` calls the API or starts a process.
The core now owns the chat stream generation, so a stream event is applied only when both its chat and its generation are current, and one switch operation serves `/chats`, `/subagents`, `/parent`, and `/new`.
A long-lived watch socket feeds a core `chat_list` module whose merge rules follow the web UI, and one generic table overlay in the TUI replaces the M1 pickers and draws every list.
Two `coder-sdk` changes come first and run in their own worktree: socket upgrade timeouts with readable errors, a typed git-watch stream, a typed MCP connect summary from the debug runs endpoint, and a send that can carry an empty MCP selection.

**Tech Stack:** Rust 2024, ratatui 0.30.2, ratatui-textarea 0.9.2, crossterm 0.29, tokio (`process`), toml_edit, coder-sdk (repinned by Tasks 1, 25, 28, and 30), nucleo-matcher 0.3.1 (new), chrono 0.4 (new direct dependency of `scuttle-core`), insta, wiremock, tokio-tungstenite 0.30, reqwest-websocket 0.6 (SDK only).

**Spec:** `docs/specs/2026-09-30-scuttle-m2-design.md` is binding, including the ten decisions in its section 18, which the author approved.
`docs/specs/2026-09-28-scuttle-design.md` is the base spec it amends.
Both are in the `m2-design` worktree; executors read the M2 design for every task, because each task cites its sections.

## Global Constraints

- SDK work happens in `~/git/nickvigilante/unofficial-coder-sdk-rs/.worktrees/m2-sdk` on branch `m2-sdk`, created from `main`.
- scuttle work happens in `~/git/nickvigilante/scuttle/.worktrees/m2` on branch `m2`, created from `m1-polish` after M1.6 lands.
- SDK tasks never touch scuttle, and scuttle tasks never touch the SDK.
- Never push, never commit on `main`, never add a remote, and stage only the files a task names; never `git add -A`.
- Commit messages use Conventional Commits and end with the trailer `Assisted-by: AI`; they never name an AI model or vendor.
- A commit scope is a crate name (`coder-sdk`, `scuttle-core`, `scuttle-tui`), or no scope when a change spans crates.
- `scuttle-core` has no terminal dependencies (no ratatui, crossterm, arboard) and never touches the filesystem or the network.
- API calls and process launches happen only in `crates/scuttle-tui/src/runtime.rs`, including the pager, `git config`, and the browser opener.
- The session token is never printed, logged, rendered, or written anywhere by scuttle or the SDK.
- The local config file never holds secrets; M2 adds no config key.
- New dependencies, each justified where it is added: `nucleo-matcher = "0.3.1"` (Task 7, verified on crates.io as the latest release, MPL-2.0) and `chrono` (Task 22, already resolved as 0.4.45 in `Cargo.lock` through `coder-api-gen`, so it adds no code to the build).
- Every crate sets `publish = false`, edition 2024, and the toolchain pinned in `rust-toolchain.toml`.
- Markdown uses one sentence per line and no em dashes, en dashes, or spaced double hyphens as punctuation; the same rule applies to code comments and user-facing strings.
- The existing insta snapshots must not change; if one does, the task broke M1 rendering.
- New tests never sleep to let time pass: TUI tests pass explicit `Instant`s and times, and runtime tests wait on the channel with a timeout.
- Run `cargo fmt --all` before each commit.
- Every task ends green on `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo fmt --all --check`.
- Crate APIs in this plan were checked against the sources of ratatui 0.30.2 (`ratatui-widgets` 0.3.2), ratatui-textarea 0.9.2, nucleo-matcher 0.3.1, reqwest-websocket 0.6, and `coder-api-gen` on the SDK's `main`, but not compiled.
  SDK code is cited by path and symbol, because the SDK's history was rewritten and older commit IDs no longer exist.
  When a signature differs in the resolved version, adapt the implementation and the test setup, keep every test assertion, and report the adaptation.

## M1.6 interfaces this plan builds on

M1.6 (`docs/plans/2026-09-30-m1-6-polish.md` on `m1-polish`) lands before M2 starts, and the tasks marked "Depends on M1.6" are written against these interfaces from it.
If the merged code names something differently, the implementer uses the merged name, keeps every assertion, and reports the difference.

1. **Effort (M1.6 Task 1).** `App::effort(&self) -> Option<String>` follows the web UI: the `/effort` choice (`selected_effort`), else the open chat's `last_reasoning_effort` if the model offers it, else the model's default, else its highest; `effort_label` is gone, and every create and send carries `App::effort`.
   Clearing `selected_effort` on a switch (Task 3) is therefore what makes the opened chat's own effort apply.
1. **The `/effort` slider (M1.6 Task 2).** The slider lives in `PickerState` with `Picker::Effort`, and `PickerState::height(&self) -> u16` sizes it.
   Task 7 keeps `PickerState` and `Tui::picker` for the slider only and moves the model, workspace, and organization lists to the table overlay.
1. **Tool summaries (M1.6 Task 3).** `args_summary` returns `Option<String>`, and `ToolResultInfo` gains `summary`; Task 32 builds orphan tool results with them.
1. **Animated markers (M1.6 Task 4).** `View::spinners` and `Ctx::activity` exist, `activity::spinner_frame(Duration) -> &'static str` stays public, and `Tui::animation_deadline` still animates only while `App::activity` is `Some`; Task 7 adds the overlay condition.
1. **Links (M1.6 Task 5).** `Effect::OpenLink(String)` opens an `http` or `https` URL like `/web` and copies it when no browser opens, answered by `Msg::LinkOpened { url, outcome }`; `Tui::mouse` returns `Vec<Effect>`.
   Task 26 opens a pull request with `Effect::OpenLink`, and Task 32 extends the effects `Tui::mouse` returns.
1. **Organizations (M1.6 Task 6).** `OrgRef::can_create_chats: bool` (`true` when unknown), `Runtime::organizations` fills it with an authorization check, `pick_organization` skips denied organizations, and choosing one is refused.
   Task 4 moves `Runtime::organizations`'s body, check included, into a free function, and Task 7 draws denied organizations as disabled rows.

## Decisions this plan makes that the spec left open

- **The effort slider stays in `PickerState`.** The design turns the M1 `PickerState` into the table overlay, but M1.6 built the `/effort` slider inside `PickerState`, so Task 7 moves only the model, workspace, and organization lists to the table and leaves the slider where M1.6 put it.
- **Task split.** The design's 24 tasks become 4 SDK tasks and 34 scuttle tasks, split wherever a reviewer could reject one part and approve its neighbor: the watch merge rules apart from the socket, the preview stream apart from the `/subagents` popup, question menus apart from plan-mode serialization, `/attach` apart from `@path`, the `/workspace` table apart from its details, the `/git` panel apart from the pager, and scroll anchoring apart from the wheel fix.
- **Repins.** Each SDK task has its own repin task in scuttle, placed just before the first task that needs it.
  Because nobody pushes, a repin points `coder-sdk` at the local repository (`git = "https://github.com/nickvigilante/unofficial-coder-sdk-rs"`), as M1 did, with the new `rev`; after the author pushes `m2-sdk`, one follow-up switches the URL back to GitHub with the same `rev`.
- **Healthy stream.** The reconnect counter resets only after a stream stays open for `STREAM_HEALTHY_AFTER` (10 seconds), reported by the runtime as `Msg::StreamHealthy`, instead of on its first event, because the first event after a reopen is the server's snapshot and says nothing about whether the connection will hold.
  The watch socket uses the same rule with `Msg::WatchHealthy`.
- **Loads that apply.** A `ChatLoaded` or `ChatLoadFailed` applies when it answers the load in flight; with no load in flight it applies only to a blank screen, which is how the startup path and the existing tests deliver a chat.
- **`/new` while a load is in flight** still waits, as in M1.5; only opening another chat supersedes a load in flight, as section 4 allows.
- **Chat list size.** `GET /chats` defaults to 50 rows and enforces no maximum (`coderd/database/queries/chats.sql:783`, `coderd/exp_chats.go:445-446`), which resolves the design's unknown; scuttle asks for 50 per page.
- **Chat list order.** Pinned chats first by `pin_order`, then by `updated_at`, newest first, matching the design's list and the server's own order.
- **Row actions are not optimistic.** Archive, pin, rename, and read state change the local list only after the server accepts, so a refused request (for example, `409` while a subagent starts running) leaves the list as it was.
- **Archive confirmation.** The first Ctrl+A on a row asks "Archive “<title>”? Press Ctrl+A again." and the second sends it; any other key cancels.
- **One line editor.** The core owns a single `LineEdit` for the inline rename, `/title`, and the "Other" answer, so all three share tested cursor and cancel behavior.
- **Question menu keys.** The question menu takes Up, Down, Enter, and Esc only while the composer is empty, so typing a free-form reply still works and Up still recalls history once the menu is dismissed with Esc.
- **Ctrl+Enter for "Implement the plan"** acts only when the composer is empty and a proposed plan is waiting, so it never changes Ctrl+Enter as the send key.
- **Attachments on a failed send.** Chips clear when the message is sent; a failed send restores the text, as today, and the notice asks the user to attach the files again.
- **Attachment checks.** The server classifies the uploaded bytes against its allowlist (`coderd/x/chatfiles/mime.go:29-33`), which accepts source code as plain text, so an extension allowlist would wrongly refuse `main.rs`.
  The core instead rejects, by extension, formats the server never accepts (archives, executables, audio, video, office files), and the runtime checks the size with file metadata before reading anything, so a file over 10 MiB (`codersdk/chats.go:57`) never uploads.
- **`@path`.** On send, each `@<path>` token that names an existing file is attached, and the token stays in the text so the agent sees the file name.
- **Process launches.** The design puts the pager next to the `$EDITOR` handoff in the TUI; this plan keeps the terminal handoff in the TUI but puts the pager resolution, `git config core.pager`, and the pager process in `runtime.rs`, as the global constraint requires.
- **Workspace web link.** `Effect::OpenWorkspaceWeb { owner, workspace }` lets the runtime build `<deployment>/@<owner>/<workspace>` from its base URL, so the core never needs the deployment URL.
- **Empty MCP selection.** The generated request type drops an empty `mcp_server_ids` (`skip_serializing_if = "Vec::is_empty"`), but the server reads an empty list as "turn every server off" (`codersdk/chats.go:766`), so the cuttable SDK Task S4 adds a send that always writes the field.
- **Debug runs.** The debug runs endpoint is experimental and not in the generated client (`coderd/chat_routes.go:127-129`), so SDK Task S3 wraps it with a hand-written type.

## Review Focus

1. Switching chats while the main stream and a subagent preview are both live: no event from either old stream reaches the chat that is now open or its preview, even when the user returns to the first chat.
   Pinned by `switching_with_a_live_preview_drops_both_old_streams` in Task 12 and `a_to_b_and_back_to_a_drops_the_first_visits_events` in Task 3.
1. The watch socket drops and reconnects while the user switches chats: the refetch updates the list, the old chat is marked unread by its status change, and the newly open chat's record, transcript, and status are untouched.
   Pinned by `a_watch_reconnect_during_a_switch_refetches_without_touching_the_open_chat` in Task 6.
1. Archiving a chat while one of its subagents is running, or starts running in the second before the request lands: scuttle refuses while the list shows a running family member, and a `409` from the server leaves the chat unarchived with the server's message.
   Pinned by `archiving_waits_for_a_running_subagent_and_a_refusal_leaves_the_list` in Task 9.
1. Attaching a file over the 10 MiB limit, or of a type the server rejects: scuttle says why before any bytes are sent, the chip shows the reason, and Backspace removes it.
   Pinned by `a_file_over_the_limit_is_rejected_before_upload` in Task 19 and `an_unsupported_type_is_rejected_before_upload` in Task 19.
1. A personal skill named like a built-in command, such as `new` or `model`: the command keeps its name, the skill is listed as `/<username>:<name>`, and choosing it sends `/personal/<name>`, which the server resolves.
   Pinned by `a_personal_skill_named_like_a_command_gets_a_qualified_label` in Task 21.

---

## File Structure

```text
unofficial-coder-sdk-rs (branch m2-sdk)
  crates/coder-sdk/src/stream.rs    upgrade timeout, readable durations, upgrade error bodies,
                                    watch_chat_git
  crates/coder-sdk/src/debug.rs     new: McpConnectOutcome and latest_mcp_connect
  crates/coder-sdk/src/messages.rs  new: send_chat_message_with_mcp_servers (cuttable)
  crates/coder-sdk/src/lib.rs       exports

scuttle (branch m2)
  Cargo.toml                        coder-sdk rev, nucleo-matcher, chrono
  crates/scuttle-core/src/
    app.rs                          stream generation, chat switch, user, watch, preview,
                                    title, queue, plan-mode requests, attachments, skills,
                                    info, workspace, git, mcp, older history
    chat_list.rs                    new: pages, order, watch merge, filters, rows
    fuzzy.rs                        new: fuzzy ranking over nucleo-matcher
    line_edit.rs                    new: the one-line editor
    question.rs                     new: ask_user_question parsing and answer text
    attachments.rs                  new: attachment chips and type checks
    skills.rs                       new: the merged slash menu and skill aliases
    panels.rs                       new: /info, /workspace details, /git, and /mcp rows
    time.rs                         new: relative and local times
    commands.rs                     every new command and alias
    transcript.rs                   unresolved tool calls, older history
  crates/scuttle-tui/src/
    table.rs                        new: the shared table widget and its state
    overlay.rs                      new: every overlay, its rows, and its keys
    picker.rs                       the effort slider, as M1.6 left it
    paths.rs                        new: @path completion and the files a message names
    app.rs                          overlays, Ctrl+R, question menu, chips, title editor,
                                    pager handoff, scroll anchor
    runtime.rs                      ForStream, preview slot, watch, every new request,
                                    git watch, pager, uploads
    transcript_view.rs              build from a Transcript, message start lines, orphans
    composer.rs                     the merged slash menu, @path completion
    help.rs                         new keys
    terminal.rs                     alternate scroll mode
    main.rs                         drain cap
```

---

## SDK tasks

These four tasks change only `unofficial-coder-sdk-rs` and can run in parallel with M1.6.
Create the worktree once, before Task S1:

```bash
cd ~/git/nickvigilante/unofficial-coder-sdk-rs && git worktree add .worktrees/m2-sdk -b m2-sdk main
```

Every SDK task runs its commands from `~/git/nickvigilante/unofficial-coder-sdk-rs/.worktrees/m2-sdk` and ends green on `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo fmt --all --check`.

### Task S1: WebSocket upgrade timeout and readable watchdog durations

`Client::open` (`crates/coder-sdk/src/stream.rs:49-72`) bounds only the TCP connect (`client.rs`, `CONNECT_TIMEOUT`), so a server that accepts the connection and never answers the upgrade hangs the stream task forever.
M2 runs up to four sockets, so each upgrade gets its own limit.
The watchdog's reason uses `Duration::as_secs`, which reads "0 seconds" for the 200 ms test timeout.

**Files:**
- Modify: `crates/coder-sdk/src/stream.rs` (`open`, a new `open_within`, `describe`, `frames`, tests)

**Interfaces:**
- Consumes: nothing new.
- Produces: `pub const UPGRADE_TIMEOUT: Duration` (15 seconds), exported from `lib.rs`; `pub(crate) async fn Client::open_within(&self, path_and_query: &str, limit: Duration) -> Result<reqwest_websocket::WebSocket>`; `pub(crate) fn describe(d: Duration) -> String`.
  Task S2 changes `open_within`'s error path, and scuttle Task 1 repins to this commit.

- [ ] **Step 1: Write the failing tests**

Add to the test module in `crates/coder-sdk/src/stream.rs`:

```rust
    #[test]
    fn durations_read_naturally() {
        assert_eq!(super::describe(Duration::from_millis(200)), "200 ms");
        assert_eq!(super::describe(Duration::from_secs(1)), "1 second");
        assert_eq!(super::describe(Duration::from_secs(45)), "45 seconds");
        assert_eq!(super::describe(Duration::from_millis(1500)), "1500 ms");
    }

    #[tokio::test]
    async fn a_hung_upgrade_fails_after_the_upgrade_timeout() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (hold_tx, hold_rx) = tokio::sync::oneshot::channel::<()>();
        tokio::spawn(async move {
            // Accept the TCP connection and never answer the upgrade request.
            let (_tcp, _) = listener.accept().await.unwrap();
            let _ = hold_rx.await;
        });
        let result = tokio::time::timeout(
            Duration::from_secs(10),
            client(addr).open_within("/api/v2/chats/watch", Duration::from_millis(200)),
        )
        .await
        .expect("the upgrade timeout must fire before the test ceiling");
        drop(hold_tx);
        match result {
            Err(Error::Transport(reason)) => assert!(reason.contains("200 ms"), "{reason}"),
            other => panic!("expected a transport error, got {:?}", other.map(|_| ())),
        }
    }
```

In `a_silent_stream_ends_with_an_error_after_the_idle_timeout`, replace the final `assert!(matches!(events[0], Err(Error::StreamClosed { code: None, .. })));` with:

```rust
        match &events[0] {
            Err(Error::StreamClosed { code: None, reason }) => {
                assert_eq!(reason, "no frame received for 200 ms");
            }
            other => panic!("expected StreamClosed, got {other:?}"),
        }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p coder-sdk stream`
Expected: FAIL to compile, because `describe` and `open_within` do not exist.

- [ ] **Step 3: Implement the upgrade limit and the durations**

In `crates/coder-sdk/src/stream.rs`, add after `STREAM_IDLE_TIMEOUT`:

```rust
/// How long a WebSocket upgrade may take once the TCP connection is up. The connect timeout
/// alone lets a server that accepts the connection but never answers hang the caller forever.
pub const UPGRADE_TIMEOUT: Duration = Duration::from_secs(15);

/// `d` as people say it: whole seconds as seconds, anything else in milliseconds.
pub(crate) fn describe(d: Duration) -> String {
    if d.subsec_nanos() == 0 && d.as_secs() > 0 {
        let secs = d.as_secs();
        format!("{secs} second{}", if secs == 1 { "" } else { "s" })
    } else {
        format!("{} ms", d.as_millis())
    }
}
```

Replace `async fn open(&self, path_and_query: &str)` with:

```rust
    async fn open(&self, path_and_query: &str) -> Result<reqwest_websocket::WebSocket> {
        self.open_within(path_and_query, UPGRADE_TIMEOUT).await
    }

    /// Opens a WebSocket, failing with `Error::Transport` when the upgrade takes longer than
    /// `limit`.
    pub(crate) async fn open_within(
        &self,
        path_and_query: &str,
        limit: Duration,
    ) -> Result<reqwest_websocket::WebSocket> {
        match tokio::time::timeout(limit, self.upgrade(path_and_query)).await {
            Ok(result) => result,
            Err(_) => Err(Error::Transport(format!(
                "the WebSocket upgrade took longer than {}",
                describe(limit)
            ))),
        }
    }

    async fn upgrade(&self, path_and_query: &str) -> Result<reqwest_websocket::WebSocket> {
        let url = self
            .base_url()
            .join(path_and_query)
            .map_err(|e| Error::Transport(e.to_string()))?;
        let response = self
            .ws_http()
            .get(url)
            .upgrade()
            .send()
            .await
            .map_err(|e| Error::Transport(e.to_string()))?;
        let status = response.status().as_u16();
        if status == 401 {
            return Err(Error::Unauthorized);
        }
        if status != 101 {
            return Err(Error::from_status(status, b""));
        }
        response
            .into_websocket()
            .await
            .map_err(|e| Error::Transport(e.to_string()))
    }
```

In `frames`, change the idle reason to `reason: format!("no frame received for {}", describe(idle)),`.
In `crates/coder-sdk/src/lib.rs`, change the stream export to `pub use stream::{STREAM_IDLE_TIMEOUT, StreamEvent, UPGRADE_TIMEOUT, WatchEvent};`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all && git add crates/coder-sdk/src/stream.rs crates/coder-sdk/src/lib.rs && git commit -m "fix(coder-sdk): time out hung WebSocket upgrades and name short timeouts in milliseconds

Assisted-by: AI"
```

---

### Task S2: Upgrade errors carry the server's message, and a typed git-watch stream

A failed upgrade today reports `HTTP 400` with no text, because `upgrade` passes an empty body to `Error::from_status`.
`/api/v2/chats/{chat}/stream/git` answers `400` with fixed messages when the chat cannot be watched (`codersdk/chats.go:1756-1772`, sent from `coderd/exp_chats.go:2003-2060`), and `/git` shows them.
On success the socket sends one `WorkspaceAgentGitServerMessage` JSON object per frame (`coderd/exp_chats.go:2086-2113`, `codersdk/workspaceagents.go:749-766`); a `changes` message is a delta keyed by `repo_root`.

**Files:**
- Modify: `crates/coder-sdk/src/stream.rs` (`upgrade`, `watch_chat_git`, tests)

**Interfaces:**
- Consumes: `Client::open` and `frames` from Task S1.
- Produces: `pub async fn Client::watch_chat_git(&self, chat: uuid::Uuid) -> Result<BoxStream<'static, Result<types::CodersdkWorkspaceAgentGitServerMessage>>>`; upgrade failures now carry the server's `message`.
  scuttle Task 26 calls `watch_chat_git` and matches `Error::Api { status: 400, message, .. }`.

- [ ] **Step 1: Write the failing tests**

Add to the test module in `crates/coder-sdk/src/stream.rs`:

```rust
    #[tokio::test]
    async fn a_refused_upgrade_reports_the_servers_message() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        let chat = uuid::Uuid::new_v4();
        Mock::given(method("GET"))
            .and(path(format!("/api/v2/chats/{chat}/stream/git")))
            .respond_with(ResponseTemplate::new(400).set_body_json(
                serde_json::json!({"message": "Chat has no workspace to watch."}),
            ))
            .mount(&server)
            .await;
        let client = Client::new(&Session {
            url: server.uri().parse().unwrap(),
            token: SecretString::from("test-token-not-real"),
        })
        .unwrap();
        match client.watch_chat_git(chat).await {
            Err(Error::Api {
                status: 400,
                message,
                ..
            }) => assert_eq!(message, "Chat has no workspace to watch."),
            Err(e) => panic!("expected the server's message, got {e:?}"),
            Ok(_) => panic!("expected an error"),
        }
    }

    #[tokio::test]
    async fn the_git_watch_yields_typed_messages() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
            let frame = serde_json::json!({
                "type": "changes",
                "repositories": [{"repo_root": "/home/coder/scuttle", "branch": "m2",
                    "remote_origin": "https://github.com/x/scuttle", "unified_diff": "diff --git a/x b/x\n"}]
            });
            ws.send(Message::text(frame.to_string())).await.unwrap();
            ws.close(Some(CloseFrame {
                code: CloseCode::Normal,
                reason: "".into(),
            }))
            .await
            .unwrap();
            while ws.next().await.is_some() {}
        });
        let events: Vec<_> = tokio::time::timeout(
            Duration::from_secs(10),
            client(addr)
                .watch_chat_git(uuid::Uuid::new_v4())
                .await
                .unwrap()
                .collect(),
        )
        .await
        .unwrap();
        assert_eq!(events.len(), 1, "{events:?}");
        let msg = events[0].as_ref().unwrap();
        assert_eq!(msg.type_.as_ref().map(|t| t.0.as_str()), Some("changes"));
        assert_eq!(msg.repositories[0].branch.as_deref(), Some("m2"));
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p coder-sdk stream`
Expected: FAIL to compile, because `watch_chat_git` does not exist.

- [ ] **Step 3: Implement the error body and the stream**

In `upgrade`, replace the `if status != 101 { .. }` block with:

```rust
        if status != 101 {
            // The body holds the server's message, such as why a chat cannot be watched.
            let body = response.into_inner().bytes().await.unwrap_or_default();
            return Err(Error::from_status(status, &body));
        }
```

Add after `watch_chats`:

```rust
    /// Streams the workspace git state of `chat`: one message per frame. A `changes` message
    /// is a delta keyed by `repo_root`, and a repository with `removed` set is gone. The server
    /// answers `400` with a fixed message when the chat has no workspace or agent to watch,
    /// which arrives as `Error::Api`. Like `watch_chats`, it ends with `Error::StreamClosed`
    /// after [`STREAM_IDLE_TIMEOUT`] of silence, and there is no cursor to resume from.
    pub async fn watch_chat_git(
        &self,
        chat: uuid::Uuid,
    ) -> Result<BoxStream<'static, Result<crate::types::CodersdkWorkspaceAgentGitServerMessage>>>
    {
        let socket = self.open(&format!("/api/v2/chats/{chat}/stream/git")).await?;
        Ok(frames(socket, STREAM_IDLE_TIMEOUT)
            .map(|frame| {
                let text = frame?;
                serde_json::from_str(&text).map_err(|e| Error::Decode(e.to_string()))
            })
            .boxed())
    }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all && git add crates/coder-sdk/src/stream.rs && git commit -m "feat(coder-sdk): stream a chat's workspace git state and keep the server's message on refused upgrades

Assisted-by: AI"
```

---

### Task S3: The latest MCP connect outcomes from a chat's debug runs

chatd reconnects to every selected MCP server on each step and only logs a failed connect (`coderd/x/chatd/mcpclient/mcpclient.go:217-223`, `:376-383`).
The outcomes are recorded in the debug run summary under `mcp_connect` (`coderd/x/chatd/active_turn_debug.go:143-203`), as `ConnectSummary { config_id, slug, outcome, duration_ms, tool_count, error }` (`coderd/x/chatd/mcpclient/mcpclient.go:96-105`), with outcomes `connected`, `timeout`, `error`, and `no_tools` (`:80-89`).
`GET /api/experimental/chats/{chat}/debug/runs` (`coderd/chat_routes.go:127-129`, handler `coderd/exp_chats.go:8945-8970`) returns up to 100 run summaries, newest first (`coderd/database/queries/chatdebug.sql:191-197`); it is not in the generated client.

**Files:**
- Create: `crates/coder-sdk/src/debug.rs`
- Modify: `crates/coder-sdk/src/lib.rs` (module and exports)

**Interfaces:**
- Consumes: `Client::http`, `Client::base_url`, `Error::from_status`.
- Produces: `pub struct McpConnectOutcome { pub config_id: uuid::Uuid, pub slug: String, pub outcome: String, pub tool_count: i64, pub error: String }` deriving `Debug, Clone, PartialEq, Eq, serde::Deserialize`; `pub async fn Client::latest_mcp_connect(&self, chat: uuid::Uuid) -> Result<Option<Vec<McpConnectOutcome>>>`, which returns the newest run's outcomes, one per server (the last entry wins), or `None` when no run recorded any.
  scuttle Task 29 calls it.

- [ ] **Step 1: Write the failing tests**

Create `crates/coder-sdk/src/debug.rs` with only the test module for now:

```rust
#[cfg(test)]
mod tests {
    use secrecy::SecretString;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use crate::{Client, Error, Session};

    fn client(url: &str) -> Client {
        Client::new(&Session {
            url: url.parse().unwrap(),
            token: SecretString::from("test-token-not-real"),
        })
        .unwrap()
    }

    #[tokio::test]
    async fn the_newest_run_with_outcomes_wins_and_the_last_entry_per_server_counts() {
        let server = MockServer::start().await;
        let chat = uuid::Uuid::new_v4();
        let (github, linear) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        Mock::given(method("GET"))
            .and(path(format!("/api/experimental/chats/{chat}/debug/runs")))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"id": uuid::Uuid::new_v4(), "summary": {}},
                {"id": uuid::Uuid::new_v4(), "summary": {"mcp_connect": [
                    {"config_id": github, "slug": "github", "outcome": "connected", "duration_ms": 40, "tool_count": 12},
                    {"config_id": linear, "slug": "linear", "outcome": "timeout", "duration_ms": 5000, "error": "context deadline exceeded"},
                    {"config_id": github, "slug": "github", "outcome": "error", "duration_ms": 9, "error": "401 Unauthorized"}
                ]}},
                {"id": uuid::Uuid::new_v4(), "summary": {"mcp_connect": [
                    {"config_id": linear, "slug": "linear", "outcome": "connected", "duration_ms": 30}
                ]}}
            ])))
            .mount(&server)
            .await;
        let outcomes = client(&server.uri())
            .latest_mcp_connect(chat)
            .await
            .unwrap()
            .expect("a run recorded outcomes");
        let by_slug: Vec<(&str, &str)> = outcomes
            .iter()
            .map(|o| (o.slug.as_str(), o.outcome.as_str()))
            .collect();
        assert_eq!(by_slug, [("github", "error"), ("linear", "timeout")]);
        assert_eq!(outcomes[0].error, "401 Unauthorized");
    }

    #[tokio::test]
    async fn no_recorded_outcomes_is_none_and_a_refusal_is_an_error() {
        let server = MockServer::start().await;
        let (quiet, hidden) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        Mock::given(method("GET"))
            .and(path(format!("/api/experimental/chats/{quiet}/debug/runs")))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/api/experimental/chats/{hidden}/debug/runs")))
            .respond_with(
                ResponseTemplate::new(404)
                    .set_body_json(serde_json::json!({"message": "Resource not found"})),
            )
            .mount(&server)
            .await;
        let c = client(&server.uri());
        assert_eq!(c.latest_mcp_connect(quiet).await.unwrap(), None);
        assert!(matches!(
            c.latest_mcp_connect(hidden).await,
            Err(Error::Api { status: 404, .. })
        ));
    }
}
```

Add `mod debug;` to `crates/coder-sdk/src/lib.rs`.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p coder-sdk debug`
Expected: FAIL to compile, because `latest_mcp_connect` does not exist.

- [ ] **Step 3: Implement the request**

Put this above the test module in `crates/coder-sdk/src/debug.rs`:

```rust
//! The experimental chat debug runs endpoint, which the generated client does not cover.

use serde::Deserialize;

use crate::{Client, Error, Result};

/// One MCP server's connect outcome, from a debug run's `mcp_connect` summary.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct McpConnectOutcome {
    pub config_id: uuid::Uuid,
    pub slug: String,
    /// `connected`, `timeout`, `error`, or `no_tools`, or a newer value.
    pub outcome: String,
    #[serde(default)]
    pub tool_count: i64,
    /// The redacted connect error, empty when the outcome is `connected` or `no_tools`.
    #[serde(default)]
    pub error: String,
}

#[derive(Deserialize)]
struct RunSummary {
    #[serde(default)]
    summary: serde_json::Map<String, serde_json::Value>,
}

impl Client {
    /// The MCP connect outcomes of `chat`'s newest debug run that recorded any, one per
    /// server with its latest entry, in the order the servers first appear. `None` when no run
    /// recorded outcomes, which is also what a user without debug logging sees. The endpoint is
    /// experimental and may change.
    pub async fn latest_mcp_connect(
        &self,
        chat: uuid::Uuid,
    ) -> Result<Option<Vec<McpConnectOutcome>>> {
        let url = self
            .base_url()
            .join(&format!("/api/experimental/chats/{chat}/debug/runs"))
            .map_err(|e| Error::Transport(e.to_string()))?;
        let response = self.http().get(url).send().await?;
        let status = response.status().as_u16();
        let body = response.bytes().await?;
        if status != 200 {
            return Err(Error::from_status(status, &body));
        }
        let runs: Vec<RunSummary> =
            serde_json::from_slice(&body).map_err(|e| Error::Decode(e.to_string()))?;
        for run in runs {
            let Some(entries) = run.summary.get("mcp_connect") else {
                continue;
            };
            let entries: Vec<McpConnectOutcome> = serde_json::from_value(entries.clone())
                .map_err(|e| Error::Decode(e.to_string()))?;
            if entries.is_empty() {
                continue;
            }
            let mut latest: Vec<McpConnectOutcome> = Vec::new();
            for entry in entries {
                match latest.iter_mut().find(|o| o.config_id == entry.config_id) {
                    Some(slot) => *slot = entry,
                    None => latest.push(entry),
                }
            }
            return Ok(Some(latest));
        }
        Ok(None)
    }
}
```

In `crates/coder-sdk/src/lib.rs`, add `pub use debug::McpConnectOutcome;` after the other `pub use` lines.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all && git add crates/coder-sdk/src/debug.rs crates/coder-sdk/src/lib.rs && git commit -m "feat(coder-sdk): read the latest MCP connect outcomes from a chat's debug runs

Assisted-by: AI"
```

---

### Task S4 (cuttable): Send a message with an explicit MCP selection

`CreateChatMessageRequest.MCPServerIDs` is a pointer: absent means "no change" and an empty list means "turn every server off" (`codersdk/chats.go:766`, applied in `coderd/exp_chats.go:2792-2797`).
The generated `CodersdkCreateChatMessageRequest` skips an empty `mcp_server_ids`, so it cannot say "off".
Cut this task together with scuttle Tasks 30 and 31.

**Files:**
- Create: `crates/coder-sdk/src/messages.rs`
- Modify: `crates/coder-sdk/src/lib.rs` (module)

**Interfaces:**
- Consumes: `Client::http`, `Client::base_url`, `Error::from_status`.
- Produces: `pub async fn Client::send_chat_message_with_mcp_servers(&self, chat: uuid::Uuid, body: &crate::types::CodersdkCreateChatMessageRequest, mcp_server_ids: &[uuid::Uuid]) -> Result<()>`.
  scuttle Task 31 calls it.

- [ ] **Step 1: Write the failing test**

Create `crates/coder-sdk/src/messages.rs` with the test module:

```rust
#[cfg(test)]
mod tests {
    use secrecy::SecretString;
    use wiremock::matchers::{body_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use crate::types::{CodersdkChatInputPart, CodersdkChatInputPartType, CodersdkCreateChatMessageRequest};
    use crate::{Client, Session};

    #[tokio::test]
    async fn an_empty_selection_is_sent_as_an_empty_list() {
        let server = MockServer::start().await;
        let chat = uuid::Uuid::new_v4();
        Mock::given(method("POST"))
            .and(path(format!("/api/v2/chats/{chat}/messages")))
            .and(body_json(serde_json::json!({
                "content": [{"type": "text", "text": "hi"}],
                "mcp_server_ids": []
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"queued": false})))
            .expect(1)
            .mount(&server)
            .await;
        let client = Client::new(&Session {
            url: server.uri().parse().unwrap(),
            token: SecretString::from("test-token-not-real"),
        })
        .unwrap();
        let body = CodersdkCreateChatMessageRequest {
            content: vec![CodersdkChatInputPart {
                type_: Some(CodersdkChatInputPartType("text".into())),
                text: Some("hi".into()),
                ..Default::default()
            }],
            ..Default::default()
        };
        client
            .send_chat_message_with_mcp_servers(chat, &body, &[])
            .await
            .unwrap();
    }
}
```

Add `mod messages;` to `crates/coder-sdk/src/lib.rs`.

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p coder-sdk messages`
Expected: FAIL to compile, because the method does not exist.

- [ ] **Step 3: Implement the send**

Put this above the test module in `crates/coder-sdk/src/messages.rs`:

```rust
//! Sending a chat message with fields the generated request type cannot express.

use crate::{Client, Error, Result};

impl Client {
    /// Sends `body` to `chat` like the generated `send_chat_message`, but always sends
    /// `mcp_server_ids`, so an empty slice turns every MCP server off for the chat instead of
    /// leaving the selection unchanged.
    pub async fn send_chat_message_with_mcp_servers(
        &self,
        chat: uuid::Uuid,
        body: &crate::types::CodersdkCreateChatMessageRequest,
        mcp_server_ids: &[uuid::Uuid],
    ) -> Result<()> {
        let mut value = serde_json::to_value(body).map_err(|e| Error::Decode(e.to_string()))?;
        value["mcp_server_ids"] = serde_json::json!(mcp_server_ids);
        let url = self
            .base_url()
            .join(&format!("/api/v2/chats/{chat}/messages"))
            .map_err(|e| Error::Transport(e.to_string()))?;
        let response = self.http().post(url).json(&value).send().await?;
        let status = response.status().as_u16();
        if status == 200 {
            return Ok(());
        }
        let bytes = response.bytes().await?;
        Err(Error::from_status(status, &bytes))
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all && git add crates/coder-sdk/src/messages.rs crates/coder-sdk/src/lib.rs && git commit -m "feat(coder-sdk): send a chat message with an explicit, possibly empty, MCP selection

Assisted-by: AI"
```

---

## scuttle tasks

Create the worktree once, after M1.6 has landed on `m1-polish`:

```bash
cd ~/git/nickvigilante/scuttle && git worktree add .worktrees/m2 -b m2 m1-polish
```

Every scuttle task runs its commands from `~/git/nickvigilante/scuttle/.worktrees/m2`.

### Task 1: Repin coder-sdk for the socket timeouts

**Files:**
- Modify: `Cargo.toml` (the `coder-sdk` line in `[workspace.dependencies]`)
- Modify: `Cargo.lock`

**Interfaces:**
- Consumes: the Task S1 commit on `m2-sdk`.
- Produces: nothing new in scuttle's code; every stream and the later watch socket get the upgrade timeout.

- [ ] **Step 1: Find the commit to pin**

The commit to pin is the tip of `m2-sdk` when this task runs, or the tip of the SDK's `main` if the author has already merged `m2-sdk` into it.
Run: `SDK=~/git/nickvigilante/unofficial-coder-sdk-rs; REV=$(git -C $SDK rev-parse m2-sdk); git -C $SDK log --oneline $REV --grep "time out hung WebSocket upgrades"`
Expected: one line, which proves Task S1 is in `$REV`; the full SHA in `$REV` is called `<rev>` below.

- [ ] **Step 2: Repin**

In `Cargo.toml`, replace the `coder-sdk` line with:

```toml
coder-sdk = { git = "https://github.com/nickvigilante/unofficial-coder-sdk-rs", rev = "<rev>" }
```

with `<rev>` replaced by the SHA from Step 1.
Run: `cargo update -p coder-sdk`
Expected: `Cargo.lock` now names `<rev>` for `coder-sdk` and `coder-api-gen`.

- [ ] **Step 3: Run the tests**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings`
Expected: PASS with no code change, since Task S1 added API without changing any existing signature.

- [ ] **Step 4: Commit**

```bash
cargo fmt --all --check && git add Cargo.toml Cargo.lock && git commit -m "build: repin coder-sdk for WebSocket upgrade timeouts

Assisted-by: AI"
```

---

### Task 2: Core-owned stream generation and the healthy-stream backoff reset

M1 tags stream messages `ForChat { chat }`, which is not enough for A to B and back to A: an event from the first visit to A still in the channel carries the right chat ID (design section 4, "The stream generation").
The core now counts generations, stream effects carry them, and a stream message applies only when both its chat and its generation are current.
The reconnect counter also stops resetting on the first event after a reopen, which is the server's snapshot, and resets once the stream has stayed open for `STREAM_HEALTHY_AFTER`.

**Files:**
- Modify: `crates/scuttle-core/src/app.rs` (`Msg`, `Effect`, `App` fields, `update`, `new_chat`, tests)
- Modify: `crates/scuttle-tui/src/runtime.rs` (`Runtime`, `StreamSender`, `open_stream`, `run`, tests)

**Interfaces:**
- Consumes: nothing new.
- Produces: `Msg::ForStream { chat: Uuid, generation: u64, msg: Box<Msg> }`; `Msg::StreamHealthy`; `Effect::OpenStream { chat: Uuid, after_id: Option<i64>, generation: u64 }`; `Effect::ReconnectAfter { chat: Uuid, after_id: Option<i64>, delay: Duration, generation: u64 }`; `App::stream_generation(&self) -> u64`; private `App::open_stream(&mut self, chat: Uuid, after_id: Option<i64>) -> Effect`, `App::reconnect(&mut self, chat: Uuid, delay: Duration) -> Effect`, `App::close_stream(&mut self) -> Effect`; `runtime::STREAM_HEALTHY_AFTER: Duration`; `Runtime::healthy_after: Duration` (crate-visible, for tests).
  Task 3 calls `close_stream`, Task 6 reuses `healthy_after`, and Task 12 extends `StreamSender`.

- [ ] **Step 1: Write the failing core tests**

In the test module of `crates/scuttle-core/src/app.rs`, replace `stream_event_after_reconnect_resets_backoff` with:

```rust
    #[test]
    fn only_a_healthy_stream_resets_backoff() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            chat: chat(id),
            messages: vec![],
        });
        app.update(Msg::StreamEnded { error: None });
        app.update(ev(
            json!({"type": "status", "status": {"status": "waiting"}}),
        ));
        assert_eq!(app.connection, Connection::Live);
        let effects = app.update(Msg::StreamEnded { error: None });
        assert_eq!(
            effects,
            vec![Effect::ReconnectAfter {
                chat: id,
                after_id: None,
                delay: Duration::from_millis(1000),
                generation: app.stream_generation(),
            }],
            "the snapshot after a reopen does not prove the connection holds"
        );
        app.update(Msg::StreamHealthy);
        let effects = app.update(Msg::StreamEnded { error: None });
        assert_eq!(
            effects,
            vec![Effect::ReconnectAfter {
                chat: id,
                after_id: None,
                delay: Duration::from_millis(500),
                generation: app.stream_generation(),
            }]
        );
    }

    #[test]
    fn a_stream_message_applies_only_while_its_generation_is_current() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            chat: chat(id),
            messages: vec![],
        });
        let first = app.stream_generation();
        app.update(Msg::StreamEnded { error: None });
        let second = app.stream_generation();
        assert!(second > first);
        let running = |generation| Msg::ForStream {
            chat: id,
            generation,
            msg: Box::new(ev(
                json!({"type": "status", "status": {"status": "running"}}),
            )),
        };
        assert!(app.update(running(first)).is_empty());
        assert_eq!(app.transcript.status, None, "the replaced stream is ignored");
        app.update(running(second));
        assert_eq!(app.transcript.status, Some(ChatStatus::Running));
    }
```

In every other core test that compares against an `Effect::OpenStream` or `Effect::ReconnectAfter` literal (`loaded_chat_opens_the_stream_after_the_last_message`, `second_submit_while_creating_does_not_create_twice`, `stream_end_schedules_backoff_reconnect_with_after_id`, `stream_gap_reconnects_immediately`, `repeated_stream_gaps_back_off`, and `a_failed_chat_load_goes_idle_and_the_next_submit_retries_it`), add `generation: app.stream_generation(),` to the literal.
Each literal is compared right after the `update` that produced it, so the current generation is the one the effect carries.

- [ ] **Step 2: Run the core tests to verify they fail**

Run: `cargo test -p scuttle-core`
Expected: FAIL to compile, because `Msg::ForStream`, `Msg::StreamHealthy`, `stream_generation`, and the `generation` fields do not exist.

- [ ] **Step 3: Implement the generation in the core**

In `crates/scuttle-core/src/app.rs`, add to `Msg`, after `ForChat`:

```rust
    /// A message from the chat stream opened with `generation`, applied only while that chat
    /// is open and that stream is current. REST replies keep `ForChat`, because a late reply
    /// about the same chat is still true on a later visit.
    ForStream {
        chat: Uuid,
        generation: u64,
        msg: Box<Msg>,
    },
    /// The chat stream stayed open long enough to count as healthy; the backoff restarts.
    StreamHealthy,
```

Change the two stream effects to:

```rust
    OpenStream {
        chat: Uuid,
        after_id: Option<i64>,
        generation: u64,
    },
    ReconnectAfter {
        chat: Uuid,
        after_id: Option<i64>,
        delay: Duration,
        generation: u64,
    },
```

Add to `App`, after `reconnect_attempt`:

```rust
    /// Bumped by every `OpenStream`, `ReconnectAfter`, and `CloseStream`.
    stream_generation: u64,
```

Add to `impl App`, after `can_interrupt`:

```rust
    /// The generation of the chat stream the core last opened, reconnected, or closed.
    pub fn stream_generation(&self) -> u64 {
        self.stream_generation
    }

    fn open_stream(&mut self, chat: Uuid, after_id: Option<i64>) -> Effect {
        self.stream_generation += 1;
        Effect::OpenStream {
            chat,
            after_id,
            generation: self.stream_generation,
        }
    }

    fn reconnect(&mut self, chat: Uuid, delay: Duration) -> Effect {
        self.stream_generation += 1;
        Effect::ReconnectAfter {
            chat,
            after_id: self.transcript.last_message_id(),
            delay,
            generation: self.stream_generation,
        }
    }

    fn close_stream(&mut self) -> Effect {
        self.stream_generation += 1;
        Effect::CloseStream
    }
```

In `update`, add these arms after `Msg::ForChat`:

```rust
            Msg::ForStream {
                chat,
                generation,
                msg,
            } => {
                if self.chat_id == Some(chat) && self.stream_generation == generation {
                    self.update(*msg)
                } else {
                    vec![]
                }
            }
            Msg::StreamHealthy => {
                self.reconnect_attempt = 0;
                vec![]
            }
```

Then replace each stream effect construction:

- In `Msg::ChatLoaded`, replace the `let mut effects = vec![Effect::OpenStream { .. }];` statement with `let after_id = self.transcript.last_message_id();` followed by `let mut effects = vec![self.open_stream(id, after_id)];`.
- In `Msg::ChatCreated`, replace `let mut effects = vec![Effect::OpenStream { chat: id, after_id: None }];` with `let mut effects = vec![self.open_stream(id, None)];`.
- In the `Applied::Reconnect(_)` arm of `Msg::Stream`, replace the `vec![Effect::ReconnectAfter { .. }]` with `vec![self.reconnect(chat, delay)]`.
- In the other arm of `Msg::Stream`, delete `self.reconnect_attempt = 0;`.
- In `Msg::StreamEnded`, replace the `vec![Effect::ReconnectAfter { .. }]` with `vec![self.reconnect(chat, backoff(self.reconnect_attempt))]`.
- In `new_chat`, replace `vec![Effect::CloseStream, Effect::ClearView]` with `vec![self.close_stream(), Effect::ClearView]`.

- [ ] **Step 4: Run the core tests to verify they pass**

Run: `cargo test -p scuttle-core`
Expected: PASS.

- [ ] **Step 5: Write the failing runtime tests**

In the test module of `crates/scuttle-tui/src/runtime.rs`, change `untag` to unwrap both tags:

```rust
    /// The message inside a `Msg::ForChat` or `Msg::ForStream`, or the message itself.
    fn untag(msg: Msg) -> Msg {
        match msg {
            Msg::ForChat { msg, .. } | Msg::ForStream { msg, .. } => *msg,
            other => other,
        }
    }
```

In `a_stale_stream_sender_delivers_nothing`, add `tag: 1,` to the `StreamSender` literal and change the assertion to `assert!(matches!(rx.try_recv(), Ok(Msg::ForStream { generation: 1, .. })));`.
In `conn_of`, change the outer pattern `Msg::ForChat { msg, .. }` to `Msg::ForStream { msg, .. }`.
Add `generation: 1,` to every `Effect::OpenStream` and `Effect::ReconnectAfter` literal in the test module, and replace `stream_messages_are_tagged_with_their_chat` with:

```rust
    #[tokio::test]
    async fn stream_messages_carry_their_chat_and_the_core_generation() {
        let url = serve_tagged_streams().await;
        let (mut rt, mut rx) = runtime(&url);
        let chat = Uuid::new_v4();
        rt.run(Effect::OpenStream {
            chat,
            after_id: None,
            generation: 7,
        });
        match next(&mut rx).await {
            Msg::ForStream {
                chat: tagged,
                generation: 7,
                msg,
            } => {
                assert_eq!(tagged, chat);
                assert!(matches!(*msg, Msg::Stream(_)), "{msg:?}");
            }
            other => panic!("expected a tagged stream event, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn closing_during_a_delayed_reconnect_sends_nothing() {
        let url = serve_tagged_streams().await;
        let (mut rt, mut rx) = runtime(&url);
        rt.run(Effect::ReconnectAfter {
            chat: Uuid::new_v4(),
            after_id: None,
            delay: Duration::from_millis(300),
            generation: 1,
        });
        rt.run(Effect::CloseStream);
        assert!(
            tokio::time::timeout(Duration::from_millis(1500), rx.recv())
                .await
                .is_err(),
            "a reconnect still waiting on its delay must not open after CloseStream"
        );
    }

    #[tokio::test]
    async fn a_stream_that_stays_open_reports_healthy() {
        let url = serve_tagged_streams().await;
        let (mut rt, mut rx) = runtime(&url);
        rt.healthy_after = Duration::from_millis(50);
        rt.run(Effect::OpenStream {
            chat: Uuid::new_v4(),
            after_id: None,
            generation: 3,
        });
        loop {
            match next(&mut rx).await {
                Msg::ForStream {
                    generation: 3, msg, ..
                } if matches!(*msg, Msg::StreamHealthy) => break,
                Msg::ForStream { .. } => continue,
                other => panic!("unexpected {other:?}"),
            }
        }
    }
```

- [ ] **Step 6: Run the runtime tests to verify they fail**

Run: `cargo test -p scuttle-tui runtime`
Expected: FAIL to compile, because `StreamSender::tag` and `Runtime::healthy_after` do not exist.

- [ ] **Step 7: Implement the tags and the healthy timer in the runtime**

In `crates/scuttle-tui/src/runtime.rs`, add after the imports:

```rust
/// How long a stream must stay open before its reconnect backoff restarts.
pub const STREAM_HEALTHY_AFTER: Duration = Duration::from_secs(10);
```

Add to `Runtime`, after `stream_generation`:

```rust
    /// How long a stream stays open before it reports `Msg::StreamHealthy`; tests shorten it.
    pub(crate) healthy_after: Duration,
```

and `healthy_after: STREAM_HEALTHY_AFTER,` to `Runtime::new`.
Add to `StreamSender`, after `chat`:

```rust
    /// The core's generation for this stream, which the core checks before applying anything.
    tag: u64,
```

and change the send in `StreamSender::send` to:

```rust
        let _ = self.tx.send(Msg::ForStream {
            chat: self.chat,
            generation: self.tag,
            msg: Box::new(msg),
        });
```

Replace `open_stream` with:

```rust
    fn open_stream(&mut self, chat: Uuid, after_id: Option<i64>, delay: Duration, tag: u64) {
        // Bump first so the old task goes quiet even if it is mid-flight on another thread.
        let mine = self.stream_generation.fetch_add(1, Ordering::SeqCst) + 1;
        if let Some(old) = self.stream.take() {
            old.abort();
        }
        let client = self.client.clone();
        let healthy_after = self.healthy_after;
        let out = StreamSender {
            tx: self.tx.clone(),
            generation: self.stream_generation.clone(),
            mine,
            chat,
            tag,
        };
        self.stream = Some(tokio::spawn(async move {
            if !delay.is_zero() {
                let jitter = Duration::from_millis(u64::from(Uuid::new_v4().as_bytes()[0]) * 2);
                tokio::time::sleep(delay + jitter).await;
            }
            let mut stream = match client.stream_chat(chat, after_id).await {
                Ok(s) => s,
                Err(e) => {
                    out.send(Msg::StreamEnded {
                        error: Some(e.to_string()),
                    });
                    return;
                }
            };
            let healthy = tokio::time::sleep(healthy_after);
            tokio::pin!(healthy);
            let mut reported = false;
            loop {
                let item = tokio::select! {
                    item = stream.next() => item,
                    () = &mut healthy, if !reported => {
                        reported = true;
                        if !out.send(Msg::StreamHealthy) {
                            return;
                        }
                        continue;
                    }
                };
                let Some(item) = item else {
                    break;
                };
                let msg = match item {
                    Ok(ev) => Msg::Stream(ev),
                    Err(coder_sdk::Error::Decode(_)) => continue,
                    Err(e) => Msg::StreamEnded {
                        error: Some(e.to_string()),
                    },
                };
                let ended = matches!(msg, Msg::StreamEnded { .. });
                if !out.send(msg) || ended {
                    return;
                }
            }
            out.send(Msg::StreamEnded { error: None });
        }));
    }
```

In `run`, change the two stream arms to:

```rust
            Effect::OpenStream {
                chat,
                after_id,
                generation,
            } => self.open_stream(chat, after_id, Duration::ZERO, generation),
            Effect::ReconnectAfter {
                chat,
                after_id,
                delay,
                generation,
            } => self.open_stream(chat, after_id, delay, generation),
```

- [ ] **Step 8: Run the tests to verify they pass**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings`
Expected: PASS, including `turn_tests.rs`, whose stream events now arrive as `ForStream` with the generation the core issued.

- [ ] **Step 9: Commit**

```bash
cargo fmt --all && git add crates/scuttle-core/src/app.rs crates/scuttle-tui/src/runtime.rs && git commit -m "fix: apply chat stream events only from the current stream and reset backoff once a stream holds

Assisted-by: AI"
```

---

### Task 3: One chat switch for opening a chat and starting a blank one

Depends on M1.6: `App::effort` (interface 1).

`/chats`, `/subagents`, `/parent`, and `/new` all change the open chat, so M2 turns the M1.5 `/new` into one reset with two entry points (design section 4, "Open a chat").
Opening a chat refuses while a creation is in flight, closes the stream, resets per-chat state as `/new` does, also resets the chosen model and effort so the opened chat's own apply, keeps the composer text, and loads the chat.
A later switch supersedes a load in flight, and a load reply applies only while it answers the current load.

**Files:**
- Modify: `crates/scuttle-core/src/app.rs` (`Msg::OpenChat`, `reset_chat_state`, `open_chat`, `load_reply_applies`, `new_chat`, the `ChatLoaded` and `ChatLoadFailed` arms, tests)

**Interfaces:**
- Consumes: `App::close_stream` and `App::stream_generation` from Task 2.
- Produces: `Msg::OpenChat(Uuid)`; private `App::reset_chat_state(&mut self) -> Vec<Effect>`, `App::open_chat(&mut self, id: Uuid) -> Vec<Effect>`, and `App::load_reply_applies(&self, id: Uuid) -> bool`.
  Task 5 marks the opened chat read inside `open_chat`, Task 12 closes the preview inside `reset_chat_state`, and Tasks 8 and 13 send `Msg::OpenChat`.

- [ ] **Step 1: Write the failing tests**

Add to the test module of `crates/scuttle-core/src/app.rs`:

```rust
    fn running() -> Msg {
        ev(json!({"type": "status", "status": {"status": "running"}}))
    }

    #[test]
    fn opening_a_chat_closes_the_old_one_and_loads_the_new_one() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        app.update(Msg::ChatLoaded {
            chat: chat(a),
            messages: vec![message(1)],
        });
        app.update(running());
        let effects = app.update(Msg::OpenChat(b));
        assert_eq!(
            effects,
            vec![Effect::CloseStream, Effect::ClearView, Effect::LoadChat(b)]
        );
        assert!(
            !effects.iter().any(|e| matches!(e, Effect::Interrupt(_))),
            "leaving a running chat never interrupts it"
        );
        assert_eq!(app.chat_id, None);
        assert_eq!(app.transcript.messages().count(), 0);
        assert_eq!(app.activity(), None);
        assert_eq!(app.connection, Connection::Connecting);
        app.update(Msg::ChatLoaded {
            chat: chat(b),
            messages: vec![message(4)],
        });
        assert_eq!(app.chat_id, Some(b));
        assert_eq!(app.transcript.last_message_id(), Some(4));
    }

    #[test]
    fn a_slow_reply_for_an_abandoned_chat_is_dropped() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        app.update(Msg::OpenChat(a));
        app.update(Msg::OpenChat(b));
        let notices = app.notices.len();
        assert!(
            app.update(Msg::ChatLoaded {
                chat: chat(a),
                messages: vec![message(1)],
            })
            .is_empty()
        );
        assert!(
            app.update(Msg::ChatLoadFailed {
                chat_id: a,
                message: "gone".into(),
            })
            .is_empty()
        );
        assert_eq!(app.chat_id, None);
        assert_eq!(app.notices.len(), notices, "an abandoned load fails silently");
        app.update(Msg::ChatLoaded {
            chat: chat(b),
            messages: vec![],
        });
        assert_eq!(app.chat_id, Some(b));
    }

    #[test]
    fn a_to_b_and_back_to_a_drops_the_first_visits_events() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let visit = |app: &mut App, id: Uuid| {
            app.update(Msg::OpenChat(id));
            app.update(Msg::ChatLoaded {
                chat: chat(id),
                messages: vec![message(1)],
            });
            app.stream_generation()
        };
        let first = visit(&mut app, a);
        visit(&mut app, b);
        let second = visit(&mut app, a);
        let reply = |generation| Msg::ForStream {
            chat: a,
            generation,
            msg: Box::new(ev(json!({"type": "message", "message": {"id": 9, "role": "assistant", "content": [{"type": "text", "text": "late"}]}}))),
        };
        app.update(reply(first));
        assert_eq!(app.transcript.messages().count(), 1, "the first visit's event is dropped");
        app.update(reply(second));
        assert_eq!(app.transcript.messages().count(), 2);
    }

    #[test]
    fn an_opened_chat_uses_its_own_model() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let (thinker, plain) = with_efforts(&mut app);
        app.update(Msg::ModelChosen(plain));
        let id = Uuid::new_v4();
        app.update(Msg::OpenChat(id));
        app.update(Msg::ChatLoaded {
            chat: Box::new(types::CodersdkChat {
                id: Some(id),
                last_model_config_id: Some(thinker),
                ..Default::default()
            }),
            messages: vec![],
        });
        assert_eq!(app.selected_model, Some(thinker));
        assert_eq!(app.selected_effort, None, "the opened chat's own effort applies");
    }

    #[test]
    fn opening_waits_while_a_chat_is_being_created() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        app.update(Msg::Submit("hi".into()));
        assert!(app.update(Msg::OpenChat(Uuid::new_v4())).is_empty());
        assert_eq!(
            app.notices.last(),
            Some(&Notice::Info(
                "Wait for this chat to finish starting, then open another.".into()
            ))
        );
    }

    #[test]
    fn text_waiting_on_a_superseded_load_goes_back_to_the_composer() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        app.update(Msg::OpenChat(Uuid::new_v4()));
        app.update(Msg::Submit("hello".into()));
        let effects = app.update(Msg::OpenChat(Uuid::new_v4()));
        assert!(
            effects.contains(&Effect::RestoreComposer("hello".into())),
            "{effects:?}"
        );
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p scuttle-core`
Expected: FAIL to compile, because `Msg::OpenChat` does not exist.

- [ ] **Step 3: Implement the switch**

Add to `Msg`, after `Started`:

```rust
    /// Opens an existing chat in place of the open one, from `/chats`, `/subagents`, or
    /// `/parent`.
    OpenChat(Uuid),
```

Add the arm `Msg::OpenChat(id) => self.open_chat(id),` to `update`, after the `Msg::Started` arm.
Add to `impl App`, before `new_chat`:

```rust
    /// Whether a load reply for `id` still answers something: the load in flight, or, with none
    /// in flight, a blank screen.
    fn load_reply_applies(&self, id: Uuid) -> bool {
        match self.loading {
            Some(loading) => loading == id,
            None => self.chat_id.is_none(),
        }
    }

    /// Clears what belongs to the open chat and closes its stream, as a launch with no chat ID
    /// starts. The chat itself, running or not, is left alone on the server, and text waiting
    /// on a superseded load goes back to the composer.
    fn reset_chat_state(&mut self) -> Vec<Effect> {
        let mut effects = vec![self.close_stream(), Effect::ClearView];
        if let Some(text) = self.pending_text.take() {
            effects.push(Effect::RestoreComposer(text));
        }
        self.chat_id = None;
        self.chat = None;
        self.loading = None;
        self.failed_load = None;
        self.transcript = Transcript::default();
        self.connection = Connection::Idle;
        self.last_stream_error = None;
        self.reconnect_attempt = 0;
        self.awaiting_reply = false;
        self.sent_id = None;
        // A chat brings its own workspace and plan mode; a new one has none until chosen.
        self.selected_workspace = None;
        self.plan_mode = false;
        effects
    }

    /// Opens the existing chat `id` in place of the open one and of any load in flight.
    fn open_chat(&mut self, id: Uuid) -> Vec<Effect> {
        // The create reply would otherwise open the chat the user just left.
        if self.creating.is_some() {
            self.info("Wait for this chat to finish starting, then open another.");
            return vec![];
        }
        if self.chat_id == Some(id) {
            return vec![];
        }
        let mut effects = self.reset_chat_state();
        // The opened chat's last model and effort apply, not a choice made for another chat.
        self.selected_model = None;
        self.selected_effort = None;
        self.loading = Some(id);
        self.connection = Connection::Connecting;
        effects.push(Effect::LoadChat(id));
        effects
    }
```

Replace the body of `new_chat` after its two guards with:

```rust
        let mut effects = self.reset_chat_state();
        if let Some(org) = self.org_id {
            effects.extend(self.load_lists_for(org));
        }
        self.info("New chat. Type a message to start it.");
        effects
```

In the `Msg::ChatLoaded` arm, right after the `let Some(id) = chat.id else { .. };` statement, add:

```rust
                if !self.load_reply_applies(id) {
                    return vec![];
                }
```

and make the first statement of the `Msg::ChatLoadFailed` arm:

```rust
                if !self.load_reply_applies(chat_id) {
                    return vec![];
                }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings`
Expected: PASS, including every `/new` test, whose behavior did not change.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all && git add crates/scuttle-core/src/app.rs && git commit -m "feat(scuttle-core): open an existing chat through the same reset as /new

Assisted-by: AI"
```

---

### Task 4: The signed-in user and the organization lookup retry

The runtime fetches `GET /api/v2/users/me` for the welcome screen, the skill labels (Task 21), and owner checks (design section 16), which also closes M1 final review Minor 5.
When the organization lookup failed at startup, `/new` and `/organization` retry it (design section 17).
User-scoped fetches start from a new `Msg::SessionStarted`, which the main loop sends once after startup, so the organization startup path and its tests stay as they are.

**Files:**
- Modify: `crates/scuttle-core/src/app.rs` (`UserRef`, `App::me`, `App::saved_org`, `orgs_retry`, `Msg`, `Effect`, `update`, `new_chat`, `organization_command`, tests)
- Modify: `crates/scuttle-tui/src/runtime.rs` (`fetch_organizations`, `FetchMe`, `FetchOrganizations`, tests)
- Modify: `crates/scuttle-tui/src/app.rs` (`Tui::new`, `Tui::update`, tests)
- Modify: `crates/scuttle-tui/src/main.rs` (send `SessionStarted`)
- Modify: `crates/scuttle-tui/tests/pty.rs` (`fake_coder`, `welcome_screen_then_quit`)

**Interfaces:**
- Consumes: nothing new.
- Produces: `app::UserRef { pub id: Uuid, pub username: String }` deriving `Debug, Clone, PartialEq, Eq`; `App::me: Option<UserRef>`; `App::saved_org: Option<Uuid>`; `Msg::SessionStarted`; `Msg::UserLoaded(UserRef)`; `Effect::FetchMe`; `Effect::FetchOrganizations`; `runtime::fetch_organizations(client: &Client) -> Result<Vec<OrgRef>, coder_sdk::Error>`.
  Tasks 5, 6, and 21 add their user-scoped fetches to the `SessionStarted` arm, and Task 21 reads `App::me`.

- [ ] **Step 1: Write the failing core tests**

Add to the test module of `crates/scuttle-core/src/app.rs`:

```rust
    #[test]
    fn the_session_start_fetches_user_scoped_state() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let effects = app.update(Msg::SessionStarted);
        assert!(effects.contains(&Effect::FetchMe), "{effects:?}");
        let me = UserRef {
            id: Uuid::new_v4(),
            username: "nick".into(),
        };
        app.update(Msg::UserLoaded(me.clone()));
        assert_eq!(app.me, Some(me));
    }

    #[test]
    fn new_and_organization_retry_a_failed_organization_lookup() {
        let mut app = App::new(BusyBehavior::Queue, true);
        app.update(Msg::OrganizationsFailed {
            message: "HTTP 502".into(),
            open_chat: None,
        });
        assert_eq!(
            app.update(Msg::Command(Command::New)),
            vec![Effect::FetchOrganizations]
        );
        assert!(
            app.update(Msg::Command(Command::Organization(None)))
                .is_empty(),
            "a retry already in flight is not sent twice"
        );
        app.update(Msg::OrganizationsFailed {
            message: "HTTP 502".into(),
            open_chat: None,
        });
        assert_eq!(
            app.update(Msg::Command(Command::Organization(None))),
            vec![Effect::FetchOrganizations]
        );
    }

    #[test]
    fn a_retried_lookup_picks_the_saved_organization_and_loads_its_lists() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let (product, coder) = (org("Product", false), org("Coder", true));
        app.saved_org = Some(product.id);
        app.update(Msg::OrganizationsFailed {
            message: "HTTP 502".into(),
            open_chat: None,
        });
        app.update(Msg::Command(Command::New));
        let effects = app.update(Msg::OrganizationsLoaded(vec![coder, product.clone()]));
        assert_eq!(app.org_id, Some(product.id));
        assert!(effects.contains(&Effect::FetchModels(product.id)), "{effects:?}");
        assert!(effects.contains(&Effect::FetchPrefs));
        assert_eq!(
            app.notices.last(),
            Some(&Notice::Info("New chats will use Product.".into()))
        );
    }
```

- [ ] **Step 2: Run the core tests to verify they fail**

Run: `cargo test -p scuttle-core`
Expected: FAIL to compile, because `UserRef`, `Msg::SessionStarted`, and `Effect::FetchMe` do not exist.

- [ ] **Step 3: Implement the user and the retry in the core**

Add after `OrgRef` in `crates/scuttle-core/src/app.rs`:

```rust
/// The signed-in user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserRef {
    pub id: Uuid,
    pub username: String,
}
```

Add to `Msg`, after `Started`:

```rust
    /// Sent once by the main loop after startup; starts the fetches that belong to the user
    /// rather than to an organization or a chat.
    SessionStarted,
    UserLoaded(UserRef),
```

Add to `Effect`, after `FetchPrefs`:

```rust
    FetchMe,
    /// Retries the organization lookup that failed at startup.
    FetchOrganizations,
```

Add to `App`, after `organizations`:

```rust
    /// The signed-in user, once loaded.
    pub me: Option<UserRef>,
    /// The organization saved in the local config, used when a retried lookup picks one.
    pub saved_org: Option<Uuid>,
    /// Set while a retried organization lookup is in flight.
    orgs_retry: bool,
```

Add these arms to `update`, after `Msg::Started`:

```rust
            Msg::SessionStarted => vec![Effect::FetchMe],
            Msg::UserLoaded(user) => {
                self.me = Some(user);
                vec![]
            }
```

Replace the `Msg::OrganizationsLoaded` arm with:

```rust
            Msg::OrganizationsLoaded(organizations) => {
                self.organizations = organizations;
                if !std::mem::take(&mut self.orgs_retry) || self.org_id.is_some() {
                    return vec![];
                }
                let Some(org) = pick_organization(self.saved_org, &self.organizations) else {
                    self.error("You are not a member of any organization.");
                    return vec![];
                };
                self.org_id = Some(org);
                let mut effects = vec![Effect::FetchPrefs];
                // An open chat keeps the lists of its own organization.
                if self.chat_id.is_none() {
                    effects.extend(self.load_lists_for(org));
                }
                self.info(format!("New chats will use {}.", self.org_label(Some(org))));
                effects
            }
```

At the top of the `Msg::OrganizationsFailed` arm, add `self.orgs_retry = false;`.
Add to `impl App`, before `organization_command`:

```rust
    /// Retries the organization lookup once at a time.
    fn retry_organizations(&mut self) -> Vec<Effect> {
        if self.orgs_retry {
            self.info("Still loading your organizations.");
            return vec![];
        }
        self.orgs_retry = true;
        self.info("Retrying your organizations.");
        vec![Effect::FetchOrganizations]
    }
```

Make the first statement of `organization_command`:

```rust
        if self.organizations.is_empty() {
            return self.retry_organizations();
        }
```

In `new_chat`, replace the `if self.chat_id.is_none() && self.failed_load.is_none() { .. }` guard with:

```rust
        if self.chat_id.is_none() && self.failed_load.is_none() {
            if self.org_id.is_none() {
                return self.retry_organizations();
            }
            self.info("This is already a new chat.");
            return vec![];
        }
```

and, at the end of `new_chat`, before `self.info("New chat. ..")`, add:

```rust
        if self.org_id.is_none() {
            effects.extend(self.retry_organizations());
        }
```

- [ ] **Step 4: Run the core tests to verify they pass**

Run: `cargo test -p scuttle-core`
Expected: PASS.

- [ ] **Step 5: Write the failing runtime and TUI tests**

Add to the test module of `crates/scuttle-tui/src/runtime.rs`:

```rust
    #[tokio::test]
    async fn the_signed_in_user_is_loaded() {
        let server = MockServer::start().await;
        let id = Uuid::new_v4();
        Mock::given(method("GET"))
            .and(path("/api/v2/users/me"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": id, "username": "nick", "email": "nick@example.com",
                "created_at": "2026-01-01T00:00:00Z", "organization_ids": [], "roles": []
            })))
            .mount(&server)
            .await;
        let (mut rt, mut rx) = runtime(&server.uri());
        rt.run(Effect::FetchMe);
        match next(&mut rx).await {
            Msg::UserLoaded(user) => {
                assert_eq!(user.id, id);
                assert_eq!(user.username, "nick");
            }
            other => panic!("expected UserLoaded, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_retried_organization_lookup_reports_the_list_or_the_failure() {
        let server = MockServer::start().await;
        let org = Uuid::new_v4();
        Mock::given(method("GET"))
            .and(path("/api/v2/users/me/organizations"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!([org_json(org, "Coder", true)])),
            )
            .up_to_n_times(1)
            .mount(&server)
            .await;
        let (mut rt, mut rx) = runtime(&server.uri());
        rt.run(Effect::FetchOrganizations);
        assert!(
            matches!(next(&mut rx).await, Msg::OrganizationsLoaded(ref o) if o.len() == 1 && o[0].id == org)
        );
        rt.run(Effect::FetchOrganizations);
        assert!(matches!(
            next(&mut rx).await,
            Msg::OrganizationsFailed { open_chat: None, .. }
        ));
    }
```

Add to the test module of `crates/scuttle-tui/src/app.rs`:

```rust
    #[test]
    fn the_welcome_screen_names_the_signed_in_user() {
        let mut t = Tui::new(
            scuttle_core::config::LocalConfig::default(),
            None,
            Theme::terminal(true),
            Welcome {
                url: "https://x".into(),
                user: String::new(),
                art: vec![],
                show: true,
            },
        );
        assert!(!screen(&mut t, 60, 20).contains("Signed in as"));
        t.update(Msg::UserLoaded(scuttle_core::app::UserRef {
            id: uuid::Uuid::new_v4(),
            username: "nick".into(),
        }));
        assert!(screen(&mut t, 60, 20).contains("Signed in as nick"));
    }
```

- [ ] **Step 6: Run the TUI tests to verify they fail**

Run: `cargo test -p scuttle-tui`
Expected: FAIL: the runtime ignores `FetchMe` and `FetchOrganizations` (the channel times out), and the welcome screen never names the user.

- [ ] **Step 7: Implement the runtime, the welcome name, and the session start**

In `crates/scuttle-tui/src/runtime.rs`, change the core import to `use scuttle_core::app::{Effect, Msg, OrgRef, UserRef, WorkspaceRef};`, move the body of `Runtime::organizations` into a free function, and make the method call it:

```rust
/// The user's organizations, in the server's order, which is not stable; pick one with
/// `scuttle_core::app::pick_organization`.
pub async fn fetch_organizations(client: &Client) -> Result<Vec<OrgRef>, coder_sdk::Error> {
    match client.api().get_organizations_by_user("me").await {
        Ok(r) => Ok(r
            .into_inner()
            .into_iter()
            .map(|o| OrgRef {
                id: o.id,
                name: o.name.unwrap_or_default(),
                display_name: o.display_name.unwrap_or_default(),
                is_default: o.is_default,
            })
            .collect()),
        Err(e) => Err(coder_sdk::Error::from_progenitor(e).await),
    }
}
```

```rust
    /// The user's organizations; see [`fetch_organizations`].
    pub async fn organizations(&self) -> Result<Vec<OrgRef>, coder_sdk::Error> {
        fetch_organizations(&self.client).await
    }
```

Depends on M1.6: `Runtime::organizations` as M1.6 left it also runs the create-permission check and fills `OrgRef::can_create_chats`; move that whole body, check included, into `fetch_organizations`, replacing `self.client` with `client`, so a retried lookup (below) gets the same answer as startup.
The body shown above is the M1.5 part of it.
Add these arms to `run`, after `Effect::FetchPrefs`:

```rust
            Effect::FetchMe => self.spawn(Box::pin(async move {
                match client.api().get_user_by_name("me").await {
                    Ok(u) => {
                        let u = u.into_inner();
                        Msg::UserLoaded(UserRef {
                            id: u.id,
                            username: u.username,
                        })
                    }
                    Err(e) => Msg::ApiFailed {
                        action: "load your user",
                        message: err(e).await,
                    },
                }
            })),
            Effect::FetchOrganizations => self.spawn(Box::pin(async move {
                match fetch_organizations(&client).await {
                    Ok(organizations) => Msg::OrganizationsLoaded(organizations),
                    Err(e) => Msg::OrganizationsFailed {
                        message: e.to_string(),
                        open_chat: None,
                    },
                }
            })),
```

In `crates/scuttle-tui/src/app.rs`, in `Tui::new`, build the core before the struct literal so the saved organization reaches it:

```rust
        let mut core = App::new(config.busy_behavior, config.mouse);
        core.saved_org = config.organization;
```

and use `core,` in the literal.
In `Tui::update`, after `let effects = self.core.update(msg);`, add:

```rust
        if self.welcome.user.is_empty()
            && let Some(me) = self.core.me.as_ref()
        {
            self.welcome.user = me.username.clone();
        }
```

In `crates/scuttle-tui/src/main.rs`, replace `let mut pending = first;` with:

```rust
    let mut pending = first;
    pending.extend(tui.update(Msg::SessionStarted));
```

In `crates/scuttle-tui/tests/pty.rs`, add to `fake_coder`, before `server`:

```rust
    Mock::given(method("GET"))
        .and(path("/api/v2/users/me"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": uuid::Uuid::new_v4(), "username": "nick", "email": "nick@example.com",
            "created_at": "2026-01-01T00:00:00Z", "organization_ids": [], "roles": []
        })))
        .mount(&server)
        .await;
```

and in `welcome_screen_then_quit`, after `s.wait_for("/help");`, add `s.wait_for("Signed in as nick");`.

- [ ] **Step 8: Run the tests to verify they pass**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings`
Expected: PASS.

- [ ] **Step 9: Commit**

```bash
cargo fmt --all && git add crates/scuttle-core/src/app.rs crates/scuttle-tui/src/runtime.rs crates/scuttle-tui/src/app.rs crates/scuttle-tui/src/main.rs crates/scuttle-tui/tests/pty.rs && git commit -m "feat: show the signed-in user and retry a failed organization lookup from /new and /organization

Assisted-by: AI"
```

---

### Task 5: The chat list: pages, order, and watch merge rules

A new `chat_list` module in the core holds the loaded chats and the rules for keeping them current, so every rule is unit-tested without a socket (design sections 2 and 4, "The watch socket").
`GET /api/v2/chats` returns root chats with their subagents in `children` (`coderd/database/queries/chats.sql:767`, `coderd/exp_chats.go:519-535`), 50 by default (`chats.sql:783`); `archived:true` lists archived chats, since the default is `archived:false` (`coderd/searchquery/search.go:523-525`), and `search:"<text>"` searches titles, PR titles, and message bodies.
The merge rules follow the web UI's `mergeWatchedChatSummary` (`site/src/api/queries/chats.ts:595-661`).

**Files:**
- Create: `crates/scuttle-core/src/chat_list.rs`
- Modify: `crates/scuttle-core/src/lib.rs` (`pub mod chat_list;`)
- Modify: `crates/scuttle-core/src/app.rs` (`App::chats`, `Msg`, `Effect::FetchChats`, `load_chats`, `open_chat`, `SessionStarted`, tests)
- Modify: `crates/scuttle-tui/src/runtime.rs` (`FetchChats`, tests)

**Interfaces:**
- Consumes: `Msg::SessionStarted` (Task 4) and `App::open_chat` (Task 3).
- Produces, in `chat_list`: `PAGE_SIZE: i64 = 50`; `enum Load { Idle, Loading, Loaded, Failed(String) }`; `enum ListQuery { Default, Archived, Search(String) }` with `q(&self) -> Option<String>`; `struct Page { pub chats: Vec<types::CodersdkChat>, pub load: Load, pub exhausted: bool }`; `struct ChatList { pub main: Page, pub archived: Page, pub search: Option<(String, Page)>, pub watch_live: bool }` with `page(&self, &ListQuery) -> Option<&Page>`, `begin_load(&mut self, &ListQuery, more: bool) -> Option<i64>`, `apply_page(&mut self, &ListQuery, offset: i64, Vec<types::CodersdkChat>)`, `fail(&mut self, &ListQuery, String)`, `find(&self, Uuid) -> Option<&types::CodersdkChat>`, `set_read(&mut self, Uuid, bool)`, `apply_watch(&mut self, kind: &str, chat: &types::CodersdkChat, open: Option<Uuid>) -> bool`, `update_copies(&mut self, Uuid, impl FnMut(&mut types::CodersdkChat)) -> bool`, `family_running(&self, Uuid) -> bool`; `fn chat_status(&types::CodersdkChat) -> Option<ChatStatus>`; `fn is_active(Option<&ChatStatus>) -> bool`.
  In `app`: `App::chats: ChatList`; `Effect::FetchChats { query: ListQuery, offset: i64 }`; `Msg::ChatsLoaded { query: ListQuery, offset: i64, chats: Vec<types::CodersdkChat> }`; `Msg::ChatsFailed { query: ListQuery, message: String }`; `Msg::LoadChats { query: ListQuery, more: bool }`; private `App::load_chats(&mut self, ListQuery, more: bool) -> Vec<Effect>`.
  Tasks 6, 8, 9, 10, and 13 build on these.

- [ ] **Step 1: Write the failing module tests**

Create `crates/scuttle-core/src/chat_list.rs` with the test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn listed(id: Uuid, title: &str, updated: &str) -> types::CodersdkChat {
        types::CodersdkChat {
            id: Some(id),
            title: Some(title.into()),
            updated_at: Some(updated.parse().unwrap()),
            ..Default::default()
        }
    }

    fn titles(page: &Page) -> Vec<&str> {
        page.chats.iter().filter_map(|c| c.title.as_deref()).collect()
    }

    fn with_status(mut c: types::CodersdkChat, status: &str) -> types::CodersdkChat {
        c.status = Some(types::CodersdkChatStatus(status.into()));
        c
    }

    #[test]
    fn pinned_chats_come_first_then_the_newest() {
        let mut list = ChatList::default();
        let mut pinned = listed(Uuid::new_v4(), "pinned", "2026-09-01T00:00:00Z");
        pinned.pin_order = Some(1);
        list.begin_load(&ListQuery::Default, false);
        list.apply_page(
            &ListQuery::Default,
            0,
            vec![
                listed(Uuid::new_v4(), "old", "2026-09-29T00:00:00Z"),
                pinned,
                listed(Uuid::new_v4(), "new", "2026-09-30T00:00:00Z"),
            ],
        );
        assert_eq!(titles(&list.main), ["pinned", "new", "old"]);
        assert_eq!(list.main.load, Load::Loaded);
        assert!(list.main.exhausted, "a short first page is the whole list");
    }

    #[test]
    fn later_pages_append_until_one_comes_back_short() {
        let mut list = ChatList::default();
        let page = |n: usize, day: u32| -> Vec<types::CodersdkChat> {
            (0..n)
                .map(|i| listed(Uuid::new_v4(), &format!("{day}-{i}"), &format!("2026-09-{day:02}T00:{:02}:00Z", 59 - i)))
                .collect()
        };
        assert_eq!(list.begin_load(&ListQuery::Default, false), Some(0));
        assert_eq!(list.begin_load(&ListQuery::Default, true), None, "one load at a time");
        list.apply_page(&ListQuery::Default, 0, page(50, 30));
        assert!(!list.main.exhausted);
        assert_eq!(list.begin_load(&ListQuery::Default, true), Some(50));
        list.apply_page(&ListQuery::Default, 50, page(3, 20));
        assert_eq!(list.main.chats.len(), 53);
        assert!(list.main.exhausted);
        assert_eq!(list.begin_load(&ListQuery::Default, true), None);
        list.fail(&ListQuery::Default, "boom".into());
        assert_eq!(list.main.load, Load::Failed("boom".into()));
    }

    #[test]
    fn a_fresh_status_change_is_taken_and_marks_other_chats_unread() {
        let (open, other) = (Uuid::new_v4(), Uuid::new_v4());
        let mut list = ChatList::default();
        list.apply_page(
            &ListQuery::Default,
            0,
            vec![
                listed(open, "open", "2026-09-30T10:00:00Z"),
                listed(other, "other", "2026-09-30T09:00:00Z"),
            ],
        );
        for id in [open, other] {
            let fresh = with_status(listed(id, "", "2026-09-30T11:00:00Z"), "running");
            assert!(list.apply_watch("status_change", &fresh, Some(open)));
        }
        let status = |id| list.find(id).and_then(chat_status);
        assert_eq!(status(open), Some(ChatStatus::Running));
        assert_eq!(status(other), Some(ChatStatus::Running));
        assert_eq!(list.find(open).and_then(|c| c.has_unread), None);
        assert_eq!(list.find(other).and_then(|c| c.has_unread), Some(true));
        let stale = with_status(listed(other, "", "2026-09-30T08:00:00Z"), "waiting");
        list.apply_watch("status_change", &stale, Some(open));
        assert_eq!(status(other), Some(ChatStatus::Running), "an older payload is ignored");
    }

    #[test]
    fn a_title_is_taken_even_from_an_older_payload() {
        let id = Uuid::new_v4();
        let mut list = ChatList::default();
        list.apply_page(&ListQuery::Default, 0, vec![listed(id, "Untitled", "2026-09-30T10:00:00Z")]);
        list.apply_watch("title_change", &listed(id, "Fix the watch test", "2026-09-30T09:00:00Z"), None);
        assert_eq!(list.find(id).and_then(|c| c.title.as_deref()), Some("Fix the watch test"));
    }

    #[test]
    fn created_adds_a_root_at_the_top_or_a_subagent_under_its_parent() {
        let (root, child, fresh) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let mut list = ChatList::default();
        list.apply_page(&ListQuery::Default, 0, vec![listed(root, "root", "2026-09-30T10:00:00Z")]);
        let mut sub = listed(child, "explore", "2026-09-30T10:01:00Z");
        sub.parent_chat_id = Some(root);
        list.apply_watch("created", &sub, None);
        list.apply_watch("created", &listed(fresh, "fresh", "2026-09-30T12:00:00Z"), None);
        assert_eq!(titles(&list.main), ["fresh", "root"]);
        assert_eq!(list.find(root).map(|c| c.children.len()), Some(1));
        assert!(list.find(child).is_some());
    }

    #[test]
    fn deleted_marks_the_chat_archived_and_created_unarchives_it() {
        let id = Uuid::new_v4();
        let mut list = ChatList::default();
        list.apply_page(&ListQuery::Default, 0, vec![listed(id, "done", "2026-09-30T10:00:00Z")]);
        list.apply_page(&ListQuery::Archived, 0, vec![]);
        list.apply_watch("deleted", &listed(id, "done", "2026-09-30T10:00:00Z"), None);
        assert_eq!(list.find(id).and_then(|c| c.archived), Some(true));
        assert_eq!(titles(&list.archived), ["done"]);
        list.apply_watch("created", &listed(id, "done", "2026-09-30T10:00:00Z"), None);
        assert_eq!(list.main.chats[0].archived, Some(false));
        assert!(list.archived.chats.is_empty());
    }

    #[test]
    fn context_dirty_keeps_the_loaded_resources() {
        let id = Uuid::new_v4();
        let mut list = ChatList::default();
        let mut loaded = listed(id, "t", "2026-09-30T10:00:00Z");
        loaded.context = Some(types::CodersdkChatContext {
            resources: vec![types::CodersdkChatContextResource {
                skill_name: Some("deploy".into()),
                ..Default::default()
            }],
            ..Default::default()
        });
        list.apply_page(&ListQuery::Default, 0, vec![loaded]);
        let mut dirty = listed(id, "t", "2026-09-30T10:00:00Z");
        dirty.context = Some(types::CodersdkChatContext {
            dirty: Some(true),
            ..Default::default()
        });
        list.apply_watch("context_dirty", &dirty, None);
        let context = list.find(id).and_then(|c| c.context.as_ref()).unwrap();
        assert_eq!(context.dirty, Some(true));
        assert_eq!(context.resources.len(), 1);
    }

    #[test]
    fn a_running_subagent_makes_its_family_running() {
        let (root, child) = (Uuid::new_v4(), Uuid::new_v4());
        let mut parent = listed(root, "root", "2026-09-30T10:00:00Z");
        parent.children = vec![with_status(listed(child, "sub", "2026-09-30T10:00:00Z"), "running")];
        let mut list = ChatList::default();
        list.apply_page(&ListQuery::Default, 0, vec![parent]);
        assert!(list.family_running(root));
        assert!(list.family_running(child));
    }

    #[test]
    fn search_results_for_an_old_query_are_dropped_and_the_query_is_quoted() {
        let mut list = ChatList::default();
        list.begin_load(&ListQuery::Search("watch".into()), false);
        list.begin_load(&ListQuery::Search("reconnect".into()), false);
        list.apply_page(
            &ListQuery::Search("watch".into()),
            0,
            vec![listed(Uuid::new_v4(), "stale", "2026-09-30T10:00:00Z")],
        );
        assert!(list.page(&ListQuery::Search("reconnect".into())).unwrap().chats.is_empty());
        assert_eq!(
            ListQuery::Search("say \"hi\"".into()).q().as_deref(),
            Some("search:\"say  hi \"")
        );
        assert_eq!(ListQuery::Archived.q().as_deref(), Some("archived:true"));
        assert_eq!(ListQuery::Default.q(), None);
    }
}
```

Add `pub mod chat_list;` to `crates/scuttle-core/src/lib.rs`.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p scuttle-core chat_list`
Expected: FAIL to compile, because the module has no items.

- [ ] **Step 3: Implement the module**

Put this above the test module in `crates/scuttle-core/src/chat_list.rs`:

```rust
//! The chat list: pages from `GET /chats`, their order, and the watch socket's merge rules.

use std::cmp::Ordering;

use coder_sdk::{ChatStatus, types};
use uuid::Uuid;

/// Chats asked for per page, which is also the server's default.
pub const PAGE_SIZE: i64 = 50;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Load {
    #[default]
    Idle,
    Loading,
    Loaded,
    Failed(String),
}

/// Which server list a page of chats belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ListQuery {
    /// Unarchived chats, the server's default.
    Default,
    Archived,
    /// A full-text search across titles, PR titles, and message bodies.
    Search(String),
}

impl ListQuery {
    /// The `q` parameter for `GET /chats`.
    pub fn q(&self) -> Option<String> {
        match self {
            ListQuery::Default => None,
            ListQuery::Archived => Some("archived:true".into()),
            // A quote inside the value would end it early.
            ListQuery::Search(text) => Some(format!("search:\"{}\"", text.replace('"', " "))),
        }
    }
}

#[derive(Debug, Default)]
pub struct Page {
    pub chats: Vec<types::CodersdkChat>,
    pub load: Load,
    /// Whether the last page came back short, so there is nothing more to load.
    pub exhausted: bool,
}

#[derive(Debug, Default)]
pub struct ChatList {
    pub main: Page,
    pub archived: Page,
    /// The results of the last server search, with the text searched for.
    pub search: Option<(String, Page)>,
    /// Whether the watch socket is connected, so the list is live.
    pub watch_live: bool,
}

/// A chat's status, if it has one.
pub fn chat_status(chat: &types::CodersdkChat) -> Option<ChatStatus> {
    chat.status.as_ref().map(|s| ChatStatus::parse(s.as_str()))
}

/// Whether `status` means the agent is working or waiting on the user.
pub fn is_active(status: Option<&ChatStatus>) -> bool {
    matches!(
        status,
        Some(ChatStatus::Running | ChatStatus::Interrupting | ChatStatus::RequiresAction)
    )
}

/// Pinned chats first by pin order, then the most recently updated.
fn sort(chats: &mut [types::CodersdkChat]) {
    let pin = |c: &types::CodersdkChat| c.pin_order.filter(|p| *p > 0);
    chats.sort_by(|a, b| match (pin(a), pin(b)) {
        (Some(x), Some(y)) => x.cmp(&y),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => b.updated_at.cmp(&a.updated_at),
    });
}

impl ChatList {
    pub fn page(&self, query: &ListQuery) -> Option<&Page> {
        match query {
            ListQuery::Default => Some(&self.main),
            ListQuery::Archived => Some(&self.archived),
            ListQuery::Search(text) => self
                .search
                .as_ref()
                .filter(|(searched, _)| searched == text)
                .map(|(_, page)| page),
        }
    }

    fn page_mut(&mut self, query: &ListQuery) -> Option<&mut Page> {
        match query {
            ListQuery::Default => Some(&mut self.main),
            ListQuery::Archived => Some(&mut self.archived),
            ListQuery::Search(text) => self
                .search
                .as_mut()
                .filter(|(searched, _)| searched == text)
                .map(|(_, page)| page),
        }
    }

    /// Marks `query` as loading and returns the offset to fetch: the start, or with `more` the
    /// end of what is loaded. `None` while a load is in flight or when nothing is left. A new
    /// search replaces the previous one.
    pub fn begin_load(&mut self, query: &ListQuery, more: bool) -> Option<i64> {
        if let ListQuery::Search(text) = query
            && !more
        {
            self.search = Some((text.clone(), Page::default()));
        }
        let page = self.page_mut(query)?;
        if page.load == Load::Loading || (more && page.exhausted) {
            return None;
        }
        page.load = Load::Loading;
        Some(if more { page.chats.len() as i64 } else { 0 })
    }

    /// Applies a page fetched at `offset`. A first page replaces a list that held one page, and
    /// otherwise updates the chats it names in place, so pages already scrolled to stay.
    pub fn apply_page(&mut self, query: &ListQuery, offset: i64, chats: Vec<types::CodersdkChat>) {
        let full = chats.len() as i64 >= PAGE_SIZE;
        let Some(page) = self.page_mut(query) else {
            return;
        };
        if offset == 0 && page.chats.len() as i64 <= PAGE_SIZE {
            page.chats = chats;
            page.exhausted = !full;
        } else {
            for chat in chats {
                match page.chats.iter_mut().find(|c| c.id.is_some() && c.id == chat.id) {
                    Some(slot) => *slot = chat,
                    None => page.chats.push(chat),
                }
            }
            if offset > 0 {
                page.exhausted = !full;
            }
        }
        page.load = Load::Loaded;
        // Search results keep the server's relevance order.
        if !matches!(query, ListQuery::Search(_)) {
            sort(&mut page.chats);
        }
    }

    pub fn fail(&mut self, query: &ListQuery, message: String) {
        if let Some(page) = self.page_mut(query) {
            page.load = Load::Failed(message);
        }
    }

    fn pages(&self) -> impl Iterator<Item = &Page> {
        [&self.main, &self.archived]
            .into_iter()
            .chain(self.search.as_ref().map(|(_, page)| page))
    }

    /// The chat or subagent `id` from any loaded page.
    pub fn find(&self, id: Uuid) -> Option<&types::CodersdkChat> {
        self.pages()
            .flat_map(|page| page.chats.iter())
            .flat_map(|root| std::iter::once(root).chain(root.children.iter()))
            .find(|c| c.id == Some(id))
    }

    /// Runs `f` on every loaded copy of chat or subagent `id`; returns whether there was one.
    pub fn update_copies(
        &mut self,
        id: Uuid,
        mut f: impl FnMut(&mut types::CodersdkChat),
    ) -> bool {
        let mut found = false;
        let pages = [&mut self.main, &mut self.archived]
            .into_iter()
            .chain(self.search.as_mut().map(|(_, page)| page));
        for page in pages {
            for root in page.chats.iter_mut() {
                if root.id == Some(id) {
                    f(root);
                    found = true;
                }
                for child in root.children.iter_mut() {
                    if child.id == Some(id) {
                        f(child);
                        found = true;
                    }
                }
            }
        }
        found
    }

    pub fn set_read(&mut self, id: Uuid, read: bool) {
        self.update_copies(id, |c| c.has_unread = Some(!read));
    }

    /// Whether the family of `id` (its root and every subagent) has an active member.
    pub fn family_running(&self, id: Uuid) -> bool {
        let root = self
            .find(id)
            .and_then(|c| c.parent_chat_id)
            .and_then(|parent| self.find(parent))
            .or_else(|| self.find(id));
        root.is_some_and(|r| {
            std::iter::once(r)
                .chain(r.children.iter())
                .any(|c| is_active(chat_status(c).as_ref()))
        })
    }

    /// Merges one watch event about `chat`; `open` is the chat on screen, which is never
    /// marked unread. Returns whether the list changed.
    pub fn apply_watch(
        &mut self,
        kind: &str,
        chat: &types::CodersdkChat,
        open: Option<Uuid>,
    ) -> bool {
        let Some(id) = chat.id else {
            return false;
        };
        let changed = match kind {
            "status_change" => self.update_copies(id, |c| {
                if chat.updated_at >= c.updated_at {
                    c.status = chat.status.clone();
                    c.updated_at = chat.updated_at;
                    if open != Some(id) {
                        c.has_unread = Some(true);
                    }
                }
            }),
            // Title generation can publish an older snapshot, so the title is always taken.
            "title_change" => self.update_copies(id, |c| c.title = chat.title.clone()),
            "summary_change" => {
                self.update_copies(id, |c| c.last_turn_summary = chat.last_turn_summary.clone())
            }
            "chat_summary_change" => self.update_copies(id, |c| c.summary = chat.summary.clone()),
            "diff_status_change" => {
                self.update_copies(id, |c| c.diff_status = chat.diff_status.clone())
            }
            // Watch payloads leave the resources out, so only the flags are merged.
            "context_dirty" => self.update_copies(id, |c| {
                let Some(new) = chat.context.as_ref() else {
                    return;
                };
                let context = c.context.get_or_insert_with(Default::default);
                context.dirty = new.dirty;
                context.dirty_since = new.dirty_since;
                context.error = new.error.clone();
            }),
            "action_required" => self.update_copies(id, |c| {
                c.status = Some(types::CodersdkChatStatus("requires_action".into()));
            }),
            "created" => self.insert(chat.clone()),
            "deleted" => self.archive(id),
            _ => false,
        };
        if changed {
            sort(&mut self.main.chats);
            sort(&mut self.archived.chats);
        }
        changed
    }

    /// A new chat, or one that was unarchived: a root goes to the top, a subagent under its
    /// parent.
    fn insert(&mut self, chat: types::CodersdkChat) -> bool {
        let Some(id) = chat.id else {
            return false;
        };
        self.archived.chats.retain(|c| c.id != Some(id));
        if self.update_copies(id, |c| c.archived = Some(false)) {
            return true;
        }
        match chat.parent_chat_id {
            Some(parent) => match self.main.chats.iter_mut().find(|c| c.id == Some(parent)) {
                Some(root) => {
                    root.children.push(chat);
                    true
                }
                None => false,
            },
            None => {
                self.main.chats.insert(0, chat);
                true
            }
        }
    }

    /// Archiving publishes `deleted` once for each family member.
    fn archive(&mut self, id: Uuid) -> bool {
        let changed = self.update_copies(id, |c| c.archived = Some(true));
        let root = self.main.chats.iter().find(|c| c.id == Some(id)).cloned();
        if let Some(root) = root
            && self.archived.load == Load::Loaded
            && !self.archived.chats.iter().any(|c| c.id == Some(id))
        {
            self.archived.chats.insert(0, root);
        }
        changed
    }
}
```

- [ ] **Step 4: Run the module tests to verify they pass**

Run: `cargo test -p scuttle-core chat_list`
Expected: PASS.

- [ ] **Step 5: Write the failing app and runtime tests**

Add to the test module of `crates/scuttle-core/src/app.rs`, with `use crate::chat_list::ListQuery;` at the top of the module:

```rust
    #[test]
    fn the_first_page_loads_at_session_start_and_opening_a_chat_marks_it_read() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let effects = app.update(Msg::SessionStarted);
        assert!(
            effects.contains(&Effect::FetchChats {
                query: ListQuery::Default,
                offset: 0
            }),
            "{effects:?}"
        );
        let id = Uuid::new_v4();
        app.update(Msg::ChatsLoaded {
            query: ListQuery::Default,
            offset: 0,
            chats: vec![types::CodersdkChat {
                id: Some(id),
                has_unread: Some(true),
                ..Default::default()
            }],
        });
        app.update(Msg::OpenChat(id));
        assert_eq!(app.chats.find(id).and_then(|c| c.has_unread), Some(false));
        assert_eq!(
            app.update(Msg::LoadChats {
                query: ListQuery::Default,
                more: true
            }),
            vec![],
            "a short page has nothing more"
        );
    }
```

Add to the test module of `crates/scuttle-tui/src/runtime.rs`:

```rust
    #[tokio::test]
    async fn chat_pages_ask_for_fifty_from_an_offset_with_the_query() {
        use wiremock::matchers::query_param;
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/chats"))
            .and(query_param("limit", "50"))
            .and(query_param("offset", "50"))
            .and(query_param("q", "archived:true"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"id": Uuid::new_v4(), "title": "old", "children": [], "files": [],
                 "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}}
            ])))
            .mount(&server)
            .await;
        let (mut rt, mut rx) = runtime(&server.uri());
        rt.run(Effect::FetchChats {
            query: scuttle_core::chat_list::ListQuery::Archived,
            offset: 50,
        });
        match next(&mut rx).await {
            Msg::ChatsLoaded {
                query: scuttle_core::chat_list::ListQuery::Archived,
                offset: 50,
                chats,
            } => assert_eq!(chats[0].title.as_deref(), Some("old")),
            other => panic!("expected ChatsLoaded, got {other:?}"),
        }
    }
```

- [ ] **Step 6: Run the tests to verify they fail**

Run: `cargo test --workspace`
Expected: FAIL to compile, because `App::chats`, `Effect::FetchChats`, and the chat list messages do not exist.

- [ ] **Step 7: Implement the app wiring and the fetch**

In `crates/scuttle-core/src/app.rs`, add `use crate::chat_list::{ChatList, ListQuery};`.
Add to `Msg`, after `ChatCreated`:

```rust
    ChatsLoaded {
        query: ListQuery,
        offset: i64,
        chats: Vec<types::CodersdkChat>,
    },
    ChatsFailed {
        query: ListQuery,
        message: String,
    },
    /// Loads the first page of `query`, or with `more` the next one.
    LoadChats {
        query: ListQuery,
        more: bool,
    },
```

Add `FetchChats { query: ListQuery, offset: i64 },` to `Effect`, after `LoadChat`.
Add `pub chats: ChatList,` to `App`, after `transcript`.
Add to `impl App`:

```rust
    fn load_chats(&mut self, query: ListQuery, more: bool) -> Vec<Effect> {
        match self.chats.begin_load(&query, more) {
            Some(offset) => vec![Effect::FetchChats { query, offset }],
            None => vec![],
        }
    }
```

Replace the `Msg::SessionStarted` arm with:

```rust
            Msg::SessionStarted => {
                let mut effects = vec![Effect::FetchMe];
                effects.extend(self.load_chats(ListQuery::Default, false));
                effects
            }
```

Add these arms to `update`:

```rust
            Msg::ChatsLoaded {
                query,
                offset,
                chats,
            } => {
                self.chats.apply_page(&query, offset, chats);
                // The open chat's stream keeps it read on the server.
                if let Some(open) = self.chat_id {
                    self.chats.set_read(open, true);
                }
                vec![]
            }
            Msg::ChatsFailed { query, message } => {
                self.chats.fail(&query, message);
                vec![]
            }
            Msg::LoadChats { query, more } => self.load_chats(query, more),
```

In `open_chat`, before `self.loading = Some(id);`, add `self.chats.set_read(id, true);`.
In `crates/scuttle-tui/src/runtime.rs`, add this arm to `run`, after `Effect::LoadChat`:

```rust
            Effect::FetchChats { query, offset } => self.spawn(Box::pin(async move {
                let q = query.q();
                match client
                    .api()
                    .list_chats(
                        None,
                        None,
                        Some(scuttle_core::chat_list::PAGE_SIZE),
                        Some(offset),
                        q.as_deref(),
                    )
                    .await
                {
                    Ok(r) => Msg::ChatsLoaded {
                        query,
                        offset,
                        chats: r.into_inner(),
                    },
                    Err(e) => Msg::ChatsFailed {
                        query,
                        message: err(e).await,
                    },
                }
            })),
```

- [ ] **Step 8: Run the tests to verify they pass**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings`
Expected: PASS.

- [ ] **Step 9: Commit**

```bash
cargo fmt --all && git add crates/scuttle-core/src/chat_list.rs crates/scuttle-core/src/lib.rs crates/scuttle-core/src/app.rs crates/scuttle-tui/src/runtime.rs && git commit -m "feat: keep a paged chat list with the web UI's watch merge rules

Assisted-by: AI"
```

---

### Task 6: The watch socket

The runtime starts one watch task at the session start and keeps it for the life of the process, reconnecting with the chat stream's backoff and resetting it once the socket stays open (design section 4, "The watch socket").
The socket sends no snapshot and has no cursor, so each connect refetches the first page, as the web UI does on open (`site/src/pages/AgentsPage/AgentsPageLayout.tsx:690-694`).
For the open chat the watch updates the record (title, diff status, archived) but never the transcript or the status; `context_dirty` refetches the open chat so skills and `/info` see new resources.
An archived open chat refuses to send, matching the web UI's read-only view (`site/src/pages/AgentsPage/AgentChatPageView.tsx:889`).
The main loop's drain gets a cap, because the watch adds steady traffic (design section 17).

**Files:**
- Modify: `crates/scuttle-core/src/app.rs` (`Msg`, `Effect`, `watch_attempt`, `apply_watch`, `is_archived`, `submit`, tests)
- Modify: `crates/scuttle-tui/src/runtime.rs` (`watch` slot, `open_watch`, `jitter`, `RefreshChat`, tests)
- Modify: `crates/scuttle-tui/src/main.rs` (`MAX_DRAIN`, `update_queued`, tests)

**Interfaces:**
- Consumes: `ChatList::apply_watch` and `App::load_chats` (Task 5); `Runtime::healthy_after` (Task 2).
- Produces: `Effect::OpenWatch { delay: Duration }`; `Effect::RefreshChat(Uuid)`; `Msg::WatchConnected`; `Msg::Watch(coder_sdk::WatchEvent)`; `Msg::WatchEnded { error: Option<String> }`; `Msg::WatchHealthy`; `Msg::ChatRefreshed(Box<types::CodersdkChat>)`, which the runtime wraps in `ForChat`; `App::is_archived(&self) -> bool`; private `App::apply_watch(&mut self, ev: coder_sdk::WatchEvent) -> Vec<Effect>`; `runtime::jitter() -> Duration`; `main::MAX_DRAIN: usize = 256`.
  Tasks 18, 21, 22, and 26 extend `apply_watch` or use `RefreshChat`.

- [ ] **Step 1: Write the failing core tests**

Add to the test module of `crates/scuttle-core/src/app.rs`:

```rust
    fn listed(id: Uuid, title: &str, updated: &str) -> types::CodersdkChat {
        types::CodersdkChat {
            id: Some(id),
            title: Some(title.into()),
            updated_at: Some(updated.parse().unwrap()),
            ..Default::default()
        }
    }

    fn watch(kind: &str, chat: types::CodersdkChat) -> Msg {
        Msg::Watch(coder_sdk::WatchEvent {
            kind: kind.into(),
            event: Some(types::CodersdkChatWatchEvent {
                chat: Some(chat),
                kind: Some(types::CodersdkChatWatchEventKind(kind.into())),
                tool_calls: vec![],
            }),
            raw: serde_json::Value::Null,
        })
    }

    #[test]
    fn the_watch_opens_at_session_start_and_backs_off_until_it_holds() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let effects = app.update(Msg::SessionStarted);
        assert!(effects.contains(&Effect::OpenWatch { delay: Duration::ZERO }));
        assert_eq!(
            app.update(Msg::WatchEnded { error: None }),
            vec![Effect::OpenWatch { delay: backoff(1) }]
        );
        assert_eq!(
            app.update(Msg::WatchEnded { error: None }),
            vec![Effect::OpenWatch { delay: backoff(2) }]
        );
        app.update(Msg::WatchHealthy);
        assert_eq!(
            app.update(Msg::WatchEnded { error: None }),
            vec![Effect::OpenWatch { delay: backoff(1) }]
        );
    }

    #[test]
    fn a_watch_reconnect_during_a_switch_refetches_without_touching_the_open_chat() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        app.update(Msg::WatchConnected);
        app.update(Msg::ChatsLoaded {
            query: ListQuery::Default,
            offset: 0,
            chats: vec![
                listed(a, "A", "2026-09-30T10:00:00Z"),
                listed(b, "B", "2026-09-30T09:00:00Z"),
            ],
        });
        app.update(Msg::ChatLoaded {
            chat: chat(a),
            messages: vec![message(1)],
        });
        assert_eq!(
            app.update(Msg::WatchEnded {
                error: Some("reset".into())
            }),
            vec![Effect::OpenWatch { delay: backoff(1) }]
        );
        assert!(!app.chats.watch_live);
        app.update(Msg::OpenChat(b));
        let mut open_b = chat(b);
        open_b.title = Some("B".into());
        app.update(Msg::ChatLoaded {
            chat: open_b,
            messages: vec![message(7)],
        });
        app.update(running());
        assert_eq!(
            app.update(Msg::WatchConnected),
            vec![Effect::FetchChats {
                query: ListQuery::Default,
                offset: 0
            }]
        );
        assert!(app.chats.watch_live);
        app.update(Msg::ChatsLoaded {
            query: ListQuery::Default,
            offset: 0,
            chats: vec![
                listed(a, "A", "2026-09-30T10:00:00Z"),
                types::CodersdkChat {
                    has_unread: Some(true),
                    ..listed(b, "B", "2026-09-30T09:00:00Z")
                },
            ],
        });
        let mut done = listed(a, "A", "2026-09-30T10:05:00Z");
        done.status = Some(types::CodersdkChatStatus("waiting".into()));
        app.update(watch("status_change", done));
        app.update(watch("status_change", {
            let mut b_waiting = listed(b, "B", "2026-09-30T10:06:00Z");
            b_waiting.status = Some(types::CodersdkChatStatus("waiting".into()));
            b_waiting
        }));
        assert_eq!(app.chats.find(a).and_then(|c| c.has_unread), Some(true));
        assert_eq!(app.chats.find(b).and_then(|c| c.has_unread), Some(false));
        assert_eq!(app.chat_id, Some(b));
        assert_eq!(app.chat.as_ref().and_then(|c| c.title.as_deref()), Some("B"));
        assert_eq!(app.transcript.messages().count(), 1);
        assert_eq!(
            app.transcript.status,
            Some(ChatStatus::Running),
            "the chat stream owns the open chat's status"
        );
    }

    #[test]
    fn the_watch_updates_the_open_chats_record_and_refetches_it_when_its_context_changes() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            chat: chat(id),
            messages: vec![],
        });
        app.update(watch("title_change", listed(id, "Renamed", "2026-09-30T10:00:00Z")));
        assert_eq!(app.chat.as_ref().and_then(|c| c.title.as_deref()), Some("Renamed"));
        assert_eq!(
            app.update(watch("context_dirty", listed(id, "Renamed", "2026-09-30T10:00:00Z"))),
            vec![Effect::RefreshChat(id)]
        );
        let mut fresh = chat(id);
        fresh.title = Some("Fresh".into());
        app.update(Msg::ForChat {
            chat: id,
            msg: Box::new(Msg::ChatRefreshed(fresh)),
        });
        assert_eq!(app.chat.as_ref().and_then(|c| c.title.as_deref()), Some("Fresh"));
    }

    #[test]
    fn an_archived_chat_does_not_send() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            chat: chat(id),
            messages: vec![],
        });
        app.update(watch("deleted", listed(id, "t", "2026-09-30T10:00:00Z")));
        assert!(app.is_archived());
        assert_eq!(
            app.update(Msg::Submit("hello".into())),
            vec![Effect::RestoreComposer("hello".into())]
        );
        assert_eq!(
            app.notices.last(),
            Some(&Notice::Error(
                "This chat is archived. Ctrl+A in /chats unarchives it.".into()
            ))
        );
    }
```

- [ ] **Step 2: Run the core tests to verify they fail**

Run: `cargo test -p scuttle-core`
Expected: FAIL to compile, because the watch messages and effects do not exist.

- [ ] **Step 3: Implement the watch in the core**

Add to `Msg`, after `ChatsFailed`:

```rust
    /// The watch socket connected; the first page is refetched, since the socket sends no
    /// snapshot.
    WatchConnected,
    Watch(coder_sdk::WatchEvent),
    WatchEnded {
        error: Option<String>,
    },
    /// The watch socket stayed open long enough to count as healthy; its backoff restarts.
    WatchHealthy,
    /// A fresh copy of the open chat, after `Effect::RefreshChat`.
    ChatRefreshed(Box<types::CodersdkChat>),
```

Add to `Effect`, after `FetchChats`:

```rust
    /// Opens the watch socket after `delay`, replacing any open one.
    OpenWatch {
        delay: Duration,
    },
    /// Refetches the open chat's record without touching its transcript.
    RefreshChat(Uuid),
```

Add `watch_attempt: u32,` to `App`, after `reconnect_attempt`.
Replace the `Msg::SessionStarted` arm with:

```rust
            Msg::SessionStarted => {
                let mut effects = vec![
                    Effect::FetchMe,
                    Effect::OpenWatch {
                        delay: Duration::ZERO,
                    },
                ];
                effects.extend(self.load_chats(ListQuery::Default, false));
                effects
            }
```

Add these arms to `update`:

```rust
            Msg::WatchConnected => {
                self.chats.watch_live = true;
                self.load_chats(ListQuery::Default, false)
            }
            Msg::Watch(ev) => self.apply_watch(ev),
            Msg::WatchEnded { .. } => {
                self.chats.watch_live = false;
                self.watch_attempt += 1;
                vec![Effect::OpenWatch {
                    delay: backoff(self.watch_attempt),
                }]
            }
            Msg::WatchHealthy => {
                self.watch_attempt = 0;
                vec![]
            }
            Msg::ChatRefreshed(chat) => {
                if chat.id.is_some() && chat.id == self.chat_id {
                    self.chat = Some(chat);
                }
                vec![]
            }
```

Add to `impl App`:

```rust
    /// Whether the open chat is archived, which makes it read-only.
    pub fn is_archived(&self) -> bool {
        self.chat.as_ref().is_some_and(|c| c.archived == Some(true))
    }

    /// Merges a watch event into the list and, for the open chat, into its record. The chat
    /// stream owns the open chat's transcript and status, so the watch never touches them.
    fn apply_watch(&mut self, ev: coder_sdk::WatchEvent) -> Vec<Effect> {
        let Some(chat) = ev.event.and_then(|e| e.chat) else {
            return vec![];
        };
        self.chats.apply_watch(&ev.kind, &chat, self.chat_id);
        let Some(id) = chat.id.filter(|id| Some(*id) == self.chat_id) else {
            return vec![];
        };
        let Some(open) = self.chat.as_mut() else {
            return vec![];
        };
        match ev.kind.as_str() {
            "title_change" => open.title = chat.title.clone(),
            "diff_status_change" => open.diff_status = chat.diff_status.clone(),
            "deleted" => open.archived = Some(true),
            "created" => open.archived = Some(false),
            "context_dirty" => return vec![Effect::RefreshChat(id)],
            _ => {}
        }
        vec![]
    }
```

In `submit`, right before `if let Some(chat) = self.chat_id {`, add:

```rust
        if self.chat_id.is_some() && self.is_archived() {
            self.error("This chat is archived. Ctrl+A in /chats unarchives it.");
            return vec![Effect::RestoreComposer(text)];
        }
```

- [ ] **Step 4: Run the core tests to verify they pass**

Run: `cargo test -p scuttle-core`
Expected: PASS.

- [ ] **Step 5: Write the failing runtime and main-loop tests**

Add to the test module of `crates/scuttle-tui/src/runtime.rs`:

```rust
    #[tokio::test]
    async fn the_watch_reports_connecting_each_event_and_its_end() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
            let frame = serde_json::json!({"kind": "title_change", "chat": {
                "id": Uuid::new_v4(), "title": "Renamed", "children": [], "files": [],
                "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}
            }});
            ws.send(Message::text(frame.to_string())).await.unwrap();
            let _ = ws.close(None).await;
            while ws.next().await.is_some() {}
        });
        let (mut rt, mut rx) = runtime(&format!("http://{addr}"));
        rt.run(Effect::OpenWatch {
            delay: Duration::ZERO,
        });
        assert!(matches!(next(&mut rx).await, Msg::WatchConnected));
        match next(&mut rx).await {
            Msg::Watch(ev) => {
                assert_eq!(ev.kind, "title_change");
                let title = ev.event.and_then(|e| e.chat).and_then(|c| c.title);
                assert_eq!(title.as_deref(), Some("Renamed"));
            }
            other => panic!("expected a watch event, got {other:?}"),
        }
        assert!(matches!(next(&mut rx).await, Msg::WatchEnded { .. }));
    }

    #[tokio::test]
    async fn a_refused_watch_ends_with_the_reason() {
        let server = MockServer::start().await;
        let (mut rt, mut rx) = runtime(&server.uri());
        rt.run(Effect::OpenWatch {
            delay: Duration::ZERO,
        });
        match next(&mut rx).await {
            Msg::WatchEnded { error: Some(e) } => assert!(!e.contains(TOKEN), "{e}"),
            other => panic!("expected WatchEnded, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_chat_refresh_is_tagged_with_its_chat() {
        let server = MockServer::start().await;
        let chat = Uuid::new_v4();
        Mock::given(method("GET"))
            .and(path(format!("/api/v2/chats/{chat}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": chat, "title": "fresh", "children": [], "files": [],
                "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}
            })))
            .mount(&server)
            .await;
        let (mut rt, mut rx) = runtime(&server.uri());
        rt.run(Effect::RefreshChat(chat));
        match next(&mut rx).await {
            Msg::ForChat { chat: tagged, msg } => {
                assert_eq!(tagged, chat);
                assert!(matches!(*msg, Msg::ChatRefreshed(ref c) if c.title.as_deref() == Some("fresh")));
            }
            other => panic!("expected a tagged refresh, got {other:?}"),
        }
    }
```

In the test module of `crates/scuttle-tui/src/main.rs`, add:

```rust
    #[test]
    fn a_burst_is_drained_in_bounded_batches() {
        let mut t = tui();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        for n in 0..MAX_DRAIN + 44 {
            tx.send(Msg::ModelsFailed {
                message: format!("failure {n}"),
            })
            .unwrap();
        }
        let first = rx.try_recv().unwrap();
        update_queued(&mut t, first, &mut rx);
        assert_eq!(t.core.notices.len(), MAX_DRAIN);
        let left = std::iter::from_fn(|| rx.try_recv().ok()).count();
        assert_eq!(left, 44);
    }
```

- [ ] **Step 6: Run the TUI tests to verify they fail**

Run: `cargo test -p scuttle-tui`
Expected: FAIL to compile, because `MAX_DRAIN` does not exist, and the runtime does not run `OpenWatch` or `RefreshChat`.

- [ ] **Step 7: Implement the watch task, the refresh, and the cap**

In `crates/scuttle-tui/src/runtime.rs`, add a free function after `over_ssh`:

```rust
/// Up to half a second of random delay, so reconnecting clients do not arrive together.
fn jitter() -> Duration {
    Duration::from_millis(u64::from(Uuid::new_v4().as_bytes()[0]) * 2)
}
```

and use `tokio::time::sleep(delay + jitter()).await;` in `open_stream` in place of its inline jitter.
Add `watch: Option<JoinHandle<()>>,` to `Runtime`, after `stream`, and `watch: None,` to `Runtime::new`.
Add to `impl Runtime`, after `open_stream`:

```rust
    /// Opens the chat list watch after `delay`, replacing any open one. There is one watch for
    /// the life of the process, so its events are not tagged.
    fn open_watch(&mut self, delay: Duration) {
        if let Some(old) = self.watch.take() {
            old.abort();
        }
        let client = self.client.clone();
        let tx = self.tx.clone();
        let healthy_after = self.healthy_after;
        self.watch = Some(tokio::spawn(async move {
            if !delay.is_zero() {
                tokio::time::sleep(delay + jitter()).await;
            }
            let mut stream = match client.watch_chats().await {
                Ok(s) => s,
                Err(e) => {
                    let _ = tx.send(Msg::WatchEnded {
                        error: Some(e.to_string()),
                    });
                    return;
                }
            };
            let _ = tx.send(Msg::WatchConnected);
            let healthy = tokio::time::sleep(healthy_after);
            tokio::pin!(healthy);
            let mut reported = false;
            loop {
                let item = tokio::select! {
                    item = stream.next() => item,
                    () = &mut healthy, if !reported => {
                        reported = true;
                        let _ = tx.send(Msg::WatchHealthy);
                        continue;
                    }
                };
                let msg = match item {
                    Some(Ok(ev)) => Msg::Watch(ev),
                    Some(Err(coder_sdk::Error::Decode(_))) => continue,
                    Some(Err(e)) => Msg::WatchEnded {
                        error: Some(e.to_string()),
                    },
                    None => Msg::WatchEnded { error: None },
                };
                let ended = matches!(msg, Msg::WatchEnded { .. });
                let _ = tx.send(msg);
                if ended {
                    return;
                }
            }
        }));
    }
```

Add these arms to `run`:

```rust
            Effect::OpenWatch { delay } => self.open_watch(delay),
            Effect::RefreshChat(chat) => self.spawn(Box::pin(async move {
                let msg = match client.api().get_chat_by_id(&chat).await {
                    Ok(c) => Msg::ChatRefreshed(Box::new(c.into_inner())),
                    Err(e) => Msg::ApiFailed {
                        action: "refresh the chat",
                        message: err(e).await,
                    },
                };
                Msg::ForChat {
                    chat,
                    msg: Box::new(msg),
                }
            })),
```

In `crates/scuttle-tui/src/main.rs`, add after `earliest`:

```rust
/// The most channel messages applied before a draw, so a burst of watch or stream traffic
/// cannot keep the screen from updating.
const MAX_DRAIN: usize = 256;
```

and replace the loop in `update_queued` with:

```rust
    let mut effects = tui.update(first);
    for _ in 1..MAX_DRAIN {
        let Ok(msg) = rx.try_recv() else {
            break;
        };
        effects.extend(tui.update(msg));
    }
    effects
```

Update the doc comment of `update_queued` to say it applies up to `MAX_DRAIN` messages.

- [ ] **Step 8: Run the tests to verify they pass**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings`
Expected: PASS; the pty tests' fake Coder answers the watch with `404`, which only schedules a quiet reconnect.

- [ ] **Step 9: Commit**

```bash
cargo fmt --all && git add crates/scuttle-core/src/app.rs crates/scuttle-tui/src/runtime.rs crates/scuttle-tui/src/main.rs && git commit -m "feat: keep the chat list live over the watch socket and cap each drain

Assisted-by: AI"
```

---

### Task 7: Fuzzy ranking and the shared table overlay

Depends on M1.6: the `/effort` slider and organization permissions (interfaces 2 and 6).

`/chats`, `/model`, `/workspace`, `/queue`, `/mcp`, and `/subagents` share one overlay widget with a filter line, grouped rows, and a status line (design section 16).
This task adds the fuzzy ranking to the core, the widget to the TUI, and moves the model, workspace, and organization pickers onto it with their behavior unchanged; the effort slider stays in `PickerState`, where M1.6 built it, and later tasks add overlays and columns.
Rows are rebuilt from the core on every draw and key, and the selection is kept by a row key, not an index, so a watch update that reorders rows cannot move the selection to another chat.

`nucleo-matcher` 0.3.1 is the dependency: it is the matcher behind Helix's picker, pure Rust with no terminal code (only `memchr` and `unicode-segmentation`), which keeps it allowed in `scuttle-core`, and it ranks by subsequence with word-boundary bonuses, which is what "fix watch" needs to find "Fix the flaky watch reconnect test".
Its license is MPL-2.0, a file-level copyleft that has no effect on an unpublished personal tool that does not modify it.

**Files:**
- Create: `crates/scuttle-core/src/fuzzy.rs`
- Modify: `crates/scuttle-core/src/lib.rs` (`pub mod fuzzy;`)
- Modify: `crates/scuttle-core/Cargo.toml` (`nucleo-matcher = "0.3.1"`)
- Create: `crates/scuttle-tui/src/table.rs`
- Create: `crates/scuttle-tui/src/overlay.rs`
- Modify: `crates/scuttle-tui/src/picker.rs` (module doc: only the slider opens it now)
- Modify: `crates/scuttle-tui/src/main.rs` (modules)
- Modify: `crates/scuttle-tui/src/app.rs` (`overlay` beside `picker`, `overlay_key`, `draw_at`, `animation_deadline`, tests)

**Interfaces:**
- Consumes: nothing new.
- Produces: `scuttle_core::fuzzy::rank<T>(query: &str, items: Vec<T>, text: impl Fn(&T) -> String) -> Vec<T>`.
  In `table`: `enum RowKey { Model(Uuid), Workspace(Option<Uuid>), Organization(Uuid) }` deriving `Debug, Clone, PartialEq, Eq, Hash`; `enum RowKind { Item, Disabled }`; `struct Row { pub key: RowKey, pub cells: Vec<Line<'static>>, pub kind: RowKind }` with `Row::item`, `Row::disabled`, `Row::selectable`; `struct TableView { pub title: String, pub widths: Vec<Constraint>, pub rows: Vec<Row>, pub status: Option<String>, pub hint: Option<String>, pub filterable: bool }` deriving `Default`; `struct TableState { pub filter: String, pub selected: Option<RowKey> }` with `with_selected`, `index`, `selected_row`, `handle_key(&mut self, KeyEvent, &TableView) -> TableKey`; `enum TableKey { Handled, Enter, Esc, Unhandled }`; `fn render(&mut Frame, Rect, &TableView, &TableState, &Theme)`.
  In `overlay`: `struct ViewCtx<'a> { pub app: &'a App, pub theme: &'a Theme }` (Task 8 adds `now_unix: i64` and `elapsed: Duration`); `enum OverlayOutcome { Stay, Close, CloseWith(Msg) }` (Task 8 adds `Send(Msg)`); `enum Overlay { Model(TableState), Workspace(TableState), Organization(TableState) }` with `open(Picker, &App) -> Option<Overlay>` (`None` for `Picker::Effort`, which stays the slider), `full_height(&self) -> bool`, `state(&self) -> &TableState`, `view(&self, &ViewCtx) -> TableView`, `handle_key(&mut self, KeyEvent, &ViewCtx) -> OverlayOutcome`, `animates(&self, &App) -> bool`.
  In `app`: `Tui::overlay: Option<Overlay>`, beside `Tui::picker`, which now holds only the effort slider; private `Tui::overlay_key(&mut self, KeyEvent) -> Vec<Effect>`.
  Every later overlay adds a `RowKey` variant, an `Overlay` variant, and arms in these methods; Task 14 adds group headers (`RowKey::None`, `RowKind::Header`, `Row::header`).
  Each piece arrives with the first code that builds it, because the binary crate warns about variants nothing constructs.

- [ ] **Step 1: Write the failing fuzzy tests**

Create `crates/scuttle-core/src/fuzzy.rs` with:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    const TITLES: [&str; 3] = [
        "Fix the flaky watch reconnect test",
        "Draft the M2 design",
        "Old reconnect spike",
    ];

    fn ranked(query: &str) -> Vec<&'static str> {
        rank(query, TITLES.to_vec(), |t| t.to_string())
    }

    #[test]
    fn an_empty_query_keeps_every_item_in_order() {
        assert_eq!(ranked(""), TITLES);
        assert_eq!(ranked("  "), TITLES);
    }

    #[test]
    fn every_word_must_match_and_case_is_ignored() {
        assert_eq!(ranked("fix watch"), ["Fix the flaky watch reconnect test"]);
        assert_eq!(ranked("DRAFT"), ["Draft the M2 design"]);
        let reconnect = ranked("reconnect");
        assert_eq!(reconnect.len(), 2);
        assert!(ranked("zzz").is_empty());
    }
}
```

Add `pub mod fuzzy;` to `crates/scuttle-core/src/lib.rs` and `nucleo-matcher = "0.3.1"` under `[dependencies]` in `crates/scuttle-core/Cargo.toml`.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p scuttle-core fuzzy`
Expected: FAIL to compile, because `rank` does not exist.

- [ ] **Step 3: Implement the ranking**

Put this above the test module in `crates/scuttle-core/src/fuzzy.rs`:

```rust
//! Fuzzy ranking for list filters, over nucleo-matcher.

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

/// The `items` whose `text` matches `query`, best first, with equal scores in their input
/// order. Each whitespace-separated word of the query must match. An empty query keeps every
/// item.
pub fn rank<T>(query: &str, items: Vec<T>, text: impl Fn(&T) -> String) -> Vec<T> {
    if query.trim().is_empty() {
        return items;
    }
    let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
    let mut matcher = Matcher::new(Config::DEFAULT);
    let mut buf = Vec::new();
    let mut scored: Vec<(u32, T)> = items
        .into_iter()
        .filter_map(|item| {
            let haystack = text(&item);
            let score = pattern.score(Utf32Str::new(&haystack, &mut buf), &mut matcher)?;
            Some((score, item))
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0));
    scored.into_iter().map(|(_, item)| item).collect()
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p scuttle-core fuzzy`
Expected: PASS.

- [ ] **Step 5: Write the failing table and overlay tests**

Create `crates/scuttle-tui/src/table.rs` with the test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn view(rows: Vec<Row>) -> TableView {
        TableView {
            title: "Model".into(),
            widths: vec![Constraint::Fill(1)],
            rows,
            filterable: true,
            ..Default::default()
        }
    }

    fn press(state: &mut TableState, view: &TableView, code: KeyCode) -> TableKey {
        state.handle_key(KeyEvent::new(code, KeyModifiers::NONE), view)
    }

    fn selected(state: &TableState, view: &TableView) -> Option<RowKey> {
        state.selected_row(view).map(|r| r.key.clone())
    }

    #[test]
    fn moving_skips_disabled_rows_and_stops_at_the_ends() {
        let (a, off, b) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let v = view(vec![
            Row::disabled(RowKey::Model(off), vec![Line::from("first, but off")]),
            Row::item(RowKey::Model(a), vec![Line::from("A")]),
            Row::disabled(RowKey::Model(off), vec![Line::from("off")]),
            Row::item(RowKey::Model(b), vec![Line::from("B")]),
        ]);
        let mut s = TableState::default();
        assert_eq!(selected(&s, &v), Some(RowKey::Model(a)));
        press(&mut s, &v, KeyCode::Down);
        assert_eq!(selected(&s, &v), Some(RowKey::Model(b)));
        press(&mut s, &v, KeyCode::Down);
        assert_eq!(selected(&s, &v), Some(RowKey::Model(b)));
        press(&mut s, &v, KeyCode::Up);
        press(&mut s, &v, KeyCode::Up);
        assert_eq!(selected(&s, &v), Some(RowKey::Model(a)));
    }

    #[test]
    fn the_selection_follows_its_row_when_the_rows_change() {
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let mut s = TableState::with_selected(RowKey::Model(b));
        let reordered = view(vec![
            Row::item(RowKey::Model(b), vec![Line::from("B")]),
            Row::item(RowKey::Model(a), vec![Line::from("A")]),
        ]);
        assert_eq!(s.index(&reordered), Some(0));
        press(&mut s, &reordered, KeyCode::Down);
        assert_eq!(selected(&s, &reordered), Some(RowKey::Model(a)));
    }

    #[test]
    fn typing_edits_the_filter_and_enter_and_esc_go_to_the_caller() {
        let v = view(vec![]);
        let mut s = TableState::default();
        assert_eq!(press(&mut s, &v, KeyCode::Char('s')), TableKey::Handled);
        assert_eq!(press(&mut s, &v, KeyCode::Char('o')), TableKey::Handled);
        assert_eq!(s.filter, "so");
        press(&mut s, &v, KeyCode::Backspace);
        assert_eq!(s.filter, "s");
        assert_eq!(
            s.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL), &v),
            TableKey::Unhandled,
            "control keys are not filter text"
        );
        assert_eq!(press(&mut s, &v, KeyCode::Enter), TableKey::Enter);
        assert_eq!(press(&mut s, &v, KeyCode::Esc), TableKey::Esc);
    }

    #[test]
    fn an_empty_table_shows_its_status_and_the_filter_line() {
        let mut v = view(vec![]);
        v.status = Some("Loading models…".into());
        let s = TableState {
            filter: "son".into(),
            ..Default::default()
        };
        let theme = Theme::terminal(true);
        let mut term = Terminal::new(TestBackend::new(40, 6)).unwrap();
        term.draw(|f| render(f, f.area(), &v, &s, &theme)).unwrap();
        let buf = term.backend().buffer().clone();
        let shown: String = (0..6)
            .map(|y| (0..40).map(|x| buf[(x, y)].symbol().to_owned()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(shown.contains("Model"), "{shown}");
        assert!(shown.contains("> son"), "{shown}");
        assert!(shown.contains("Loading models…"), "{shown}");
    }
}
```

Create `crates/scuttle-tui/src/overlay.rs` with the test module, which repeats `picker.rs`'s list tests against the table:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyModifiers};
    use scuttle_core::app::{OrgRef, WorkspaceRef};
    use scuttle_core::config::BusyBehavior;
    use serde_json::json;

    pub(crate) fn press(o: &mut Overlay, app: &App, code: KeyCode) -> OverlayOutcome {
        let theme = Theme::terminal(true);
        let ctx = ViewCtx { app, theme: &theme };
        o.handle_key(KeyEvent::new(code, KeyModifiers::NONE), &ctx)
    }

    fn models(app: &mut App, list: serde_json::Value) {
        app.update(Msg::ModelsLoaded(serde_json::from_value(list).unwrap()));
    }

    #[test]
    fn model_rows_filter_fuzzily_and_enter_chooses() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let (sonnet, gpt) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        models(&mut app, json!([
            {"id": gpt, "display_name": "GPT-5", "enabled": true, "reasoning_efforts": []},
            {"id": sonnet, "display_name": "Claude Sonnet", "enabled": true, "reasoning_efforts": []}
        ]));
        let mut o = Overlay::open(Picker::Model, &app).unwrap();
        for c in "son".chars() {
            press(&mut o, &app, KeyCode::Char(c));
        }
        assert!(matches!(
            press(&mut o, &app, KeyCode::Enter),
            OverlayOutcome::CloseWith(Msg::ModelChosen(id)) if id == sonnet
        ));
    }

    #[test]
    fn disabled_models_are_not_offered() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let (a, b) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        app.models = serde_json::from_value(json!([
            {"id": a, "display_name": "A", "enabled": true, "reasoning_efforts": []},
            {"id": b, "display_name": "B", "enabled": false, "reasoning_efforts": []}
        ]))
        .unwrap();
        let theme = Theme::terminal(true);
        let ctx = ViewCtx {
            app: &app,
            theme: &theme,
        };
        let view = Overlay::open(Picker::Model, &app).unwrap().view(&ctx);
        assert_eq!(view.rows.len(), 1, "a disabled model is not offered");
    }

    #[test]
    fn workspace_rows_offer_none_first_and_escape_closes() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let ws = uuid::Uuid::new_v4();
        app.update(Msg::WorkspacesLoaded(vec![WorkspaceRef {
            id: ws,
            name: "dev".into(),
        }]));
        let mut o = Overlay::open(Picker::Workspace, &app).unwrap();
        assert!(matches!(
            press(&mut o, &app, KeyCode::Enter),
            OverlayOutcome::CloseWith(Msg::WorkspaceChosen(None))
        ));
        let mut o = Overlay::open(Picker::Workspace, &app).unwrap();
        press(&mut o, &app, KeyCode::Down);
        assert!(matches!(
            press(&mut o, &app, KeyCode::Enter),
            OverlayOutcome::CloseWith(Msg::WorkspaceChosen(Some(id))) if id == ws
        ));
        assert!(matches!(press(&mut o, &app, KeyCode::Esc), OverlayOutcome::Close));
    }

    #[test]
    fn organization_rows_mark_the_default_and_the_current_one() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let (product, coder) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let org = |id, name: &str, is_default| OrgRef {
            id,
            name: name.to_lowercase(),
            display_name: name.into(),
            is_default,
            can_create_chats: true,
        };
        let mut denied = org(uuid::Uuid::new_v4(), "Legal", false);
        denied.can_create_chats = false;
        app.update(Msg::OrganizationsLoaded(vec![
            org(product, "Product", false),
            org(coder, "Coder", true),
            denied,
        ]));
        app.update(Msg::Started {
            org_id: coder,
            open_chat: None,
        });
        let theme = Theme::terminal(true);
        let ctx = ViewCtx {
            app: &app,
            theme: &theme,
        };
        let mut o = Overlay::open(Picker::Organization, &app).unwrap();
        let view = o.view(&ctx);
        let labels: Vec<String> = view.rows.iter().map(|r| r.cells[0].to_string()).collect();
        assert_eq!(labels, ["Product", "Coder (default, current)", "Legal"]);
        assert!(!view.rows[2].selectable(), "a denied organization cannot be chosen");
        assert_eq!(view.rows[2].cells[1].to_string(), "no permission to create chats");
        assert!(
            matches!(o.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &ctx),
                OverlayOutcome::CloseWith(Msg::OrganizationChosen(id)) if id == coder),
            "the table starts on the current organization"
        );
    }
}
```


- [ ] **Step 6: Run the TUI tests to verify they fail**

Run: `cargo test -p scuttle-tui`
Expected: FAIL to compile, because `table` and `overlay` have no items and are not declared.

- [ ] **Step 7: Implement the widget**

Put this above the test module in `crates/scuttle-tui/src/table.rs`:

```rust
//! The shared table overlay: a filter line, grouped rows, a status line, and a hint.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Table};
use uuid::Uuid;

use crate::theme::Theme;

/// What a row stands for, so the selection survives the rows being rebuilt.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RowKey {
    Model(Uuid),
    Workspace(Option<Uuid>),
    Organization(Uuid),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowKind {
    Item,
    /// Shown dim and never selected, such as an organization where chats cannot be created.
    Disabled,
}

#[derive(Debug, Clone)]
pub struct Row {
    pub key: RowKey,
    pub cells: Vec<Line<'static>>,
    pub kind: RowKind,
}

impl Row {
    pub fn item(key: RowKey, cells: Vec<Line<'static>>) -> Row {
        Row {
            key,
            cells,
            kind: RowKind::Item,
        }
    }

    pub fn disabled(key: RowKey, cells: Vec<Line<'static>>) -> Row {
        Row {
            key,
            cells,
            kind: RowKind::Disabled,
        }
    }

    pub fn selectable(&self) -> bool {
        self.kind == RowKind::Item
    }
}

#[derive(Debug, Clone, Default)]
pub struct TableView {
    pub title: String,
    pub widths: Vec<Constraint>,
    pub rows: Vec<Row>,
    /// Loading, empty, or error text: in place of the rows when there are none, else below.
    pub status: Option<String>,
    /// A dim line at the bottom, such as the keys the overlay takes.
    pub hint: Option<String>,
    /// Whether typing filters the rows, which shows the filter line.
    pub filterable: bool,
}

#[derive(Debug, Clone, Default)]
pub struct TableState {
    pub filter: String,
    /// The selected row's key; `None` selects the first selectable row.
    pub selected: Option<RowKey>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableKey {
    /// Navigation or filter typing, already applied.
    Handled,
    Enter,
    Esc,
    /// A key the table does not use, for the overlay to handle.
    Unhandled,
}

impl TableState {
    pub fn with_selected(key: RowKey) -> TableState {
        TableState {
            selected: Some(key),
            ..TableState::default()
        }
    }

    /// The index of the selected row, or of the first selectable one.
    pub fn index(&self, view: &TableView) -> Option<usize> {
        self.selected
            .as_ref()
            .and_then(|key| {
                view.rows
                    .iter()
                    .position(|r| r.selectable() && &r.key == key)
            })
            .or_else(|| view.rows.iter().position(Row::selectable))
    }

    pub fn selected_row<'v>(&self, view: &'v TableView) -> Option<&'v Row> {
        self.index(view).map(|i| &view.rows[i])
    }

    fn step(&mut self, view: &TableView, delta: isize) {
        let selectable: Vec<usize> = view
            .rows
            .iter()
            .enumerate()
            .filter(|(_, r)| r.selectable())
            .map(|(i, _)| i)
            .collect();
        let Some(current) = self.index(view) else {
            return;
        };
        let at = selectable.iter().position(|i| *i == current).unwrap_or(0) as isize;
        let next = (at + delta).clamp(0, selectable.len() as isize - 1) as usize;
        self.selected = Some(view.rows[selectable[next]].key.clone());
    }

    pub fn handle_key(&mut self, key: KeyEvent, view: &TableView) -> TableKey {
        let plain = !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        match key.code {
            KeyCode::Esc => return TableKey::Esc,
            KeyCode::Enter => return TableKey::Enter,
            KeyCode::Up => self.step(view, -1),
            KeyCode::Down => self.step(view, 1),
            KeyCode::PageUp => self.step(view, -10),
            KeyCode::PageDown => self.step(view, 10),
            KeyCode::Char(c) if view.filterable && plain => {
                self.filter.push(c);
                self.selected = None;
            }
            KeyCode::Backspace if view.filterable => {
                self.filter.pop();
                self.selected = None;
            }
            _ => return TableKey::Unhandled,
        }
        TableKey::Handled
    }
}

/// Draws `view` in a bordered box that clears what is under it.
pub fn render(f: &mut Frame, area: Rect, view: &TableView, state: &TableState, theme: &Theme) {
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {} ", view.title));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let status_below = view.status.is_some() && !view.rows.is_empty();
    let [filter, body, status, hint] = Layout::vertical([
        Constraint::Length(u16::from(view.filterable)),
        Constraint::Min(1),
        Constraint::Length(u16::from(status_below)),
        Constraint::Length(u16::from(view.hint.is_some())),
    ])
    .areas(inner);
    if view.filterable {
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("> ", theme.accent),
                Span::raw(state.filter.clone()),
            ])),
            filter,
        );
    }
    if view.rows.is_empty() {
        let text = view.status.clone().unwrap_or_default();
        f.render_widget(Paragraph::new(Span::styled(text, theme.dim)), body);
    } else {
        let rows = view.rows.iter().map(|r| {
            let style = match r.kind {
                RowKind::Item => Style::default(),
                RowKind::Disabled => theme.dim,
            };
            ratatui::widgets::Row::new(r.cells.clone()).style(style)
        });
        let table = Table::new(rows, view.widths.clone())
            .row_highlight_style(theme.accent)
            .highlight_symbol("› ");
        let mut widget_state =
            ratatui::widgets::TableState::default().with_selected(state.index(view));
        f.render_stateful_widget(table, body, &mut widget_state);
        if let Some(text) = view.status.as_ref() {
            f.render_widget(Paragraph::new(Span::styled(text.clone(), theme.dim)), status);
        }
    }
    if let Some(text) = view.hint.as_ref() {
        f.render_widget(Paragraph::new(Span::styled(text.clone(), theme.dim)), hint);
    }
}
```

- [ ] **Step 8: Implement the overlays and move the pickers onto them**

Put this above the test module in `crates/scuttle-tui/src/overlay.rs`:

```rust
//! The overlays drawn over the transcript: which one is open, its rows, and its keys.

use coder_sdk::types;
use crossterm::event::KeyEvent;
use ratatui::layout::Constraint;
use ratatui::text::{Line, Span};
use scuttle_core::app::{App, Msg, Picker};
use scuttle_core::fuzzy;

use crate::table::{Row, RowKey, TableKey, TableState, TableView};
use crate::theme::Theme;

/// What overlay rows are built from.
pub struct ViewCtx<'a> {
    pub app: &'a App,
    pub theme: &'a Theme,
}

/// What a key pressed in an overlay asks the UI to do.
#[derive(Debug)]
pub enum OverlayOutcome {
    Stay,
    Close,
    /// Closes the overlay, then sends the message to the core.
    CloseWith(Msg),
}

pub enum Overlay {
    Model(TableState),
    Workspace(TableState),
    Organization(TableState),
}

fn model_label(m: &types::CodersdkChatModel) -> String {
    m.display_name
        .clone()
        .or_else(|| m.model.clone())
        .unwrap_or_default()
}

fn model_view(app: &App, filter: &str) -> TableView {
    // `enabled != Some(false)` repeats the core's own filter on load.
    let models: Vec<&types::CodersdkChatModel> = app
        .models
        .iter()
        .filter(|m| m.enabled != Some(false))
        .collect();
    let rows = fuzzy::rank(filter, models, |m| model_label(m))
        .into_iter()
        .filter_map(|m| Some(Row::item(RowKey::Model(m.id?), vec![Line::from(model_label(m))])))
        .collect();
    TableView {
        title: "Model".into(),
        widths: vec![Constraint::Fill(1)],
        rows,
        filterable: true,
        ..Default::default()
    }
}

fn workspace_view(app: &App, filter: &str) -> TableView {
    let mut rows = vec![Row::item(
        RowKey::Workspace(None),
        vec![Line::from("none (no workspace)")],
    )];
    rows.extend(
        fuzzy::rank(filter, app.workspaces.iter().collect(), |w| w.name.clone())
            .into_iter()
            .map(|w| Row::item(RowKey::Workspace(Some(w.id)), vec![Line::from(w.name.clone())])),
    );
    TableView {
        title: "Workspace".into(),
        widths: vec![Constraint::Fill(1)],
        rows,
        filterable: true,
        ..Default::default()
    }
}

fn organization_view(ctx: &ViewCtx) -> TableView {
    let app = ctx.app;
    let rows = app
        .organizations
        .iter()
        .map(|o| {
            let mut marks = Vec::new();
            if o.is_default {
                marks.push("default");
            }
            if Some(o.id) == app.org_id {
                marks.push("current");
            }
            let label = if marks.is_empty() {
                o.label().to_owned()
            } else {
                format!("{} ({})", o.label(), marks.join(", "))
            };
            // M1.6 checks where chats may be created; a denied organization is dim.
            if o.can_create_chats {
                Row::item(RowKey::Organization(o.id), vec![Line::from(label)])
            } else {
                Row::disabled(
                    RowKey::Organization(o.id),
                    vec![
                        Line::from(label),
                        Line::from(Span::styled("no permission to create chats", ctx.theme.dim)),
                    ],
                )
            }
        })
        .collect();
    TableView {
        title: "Organization".into(),
        widths: vec![Constraint::Fill(1), Constraint::Fill(1)],
        rows,
        ..Default::default()
    }
}

impl Overlay {
    /// The table for `kind`, or `None` for the effort slider, which `PickerState` draws.
    pub fn open(kind: Picker, app: &App) -> Option<Overlay> {
        Some(match kind {
            Picker::Model => Overlay::Model(TableState::default()),
            Picker::Workspace => Overlay::Workspace(TableState::default()),
            Picker::Organization => Overlay::Organization(match app.org_id {
                Some(id) => TableState::with_selected(RowKey::Organization(id)),
                None => TableState::default(),
            }),
            Picker::Effort => return None,
        })
    }

    /// Whether the overlay covers the whole transcript area.
    pub fn full_height(&self) -> bool {
        false
    }

    /// Whether the overlay shows a spinner, so the screen keeps redrawing.
    pub fn animates(&self, _app: &App) -> bool {
        false
    }

    pub fn state(&self) -> &TableState {
        match self {
            Overlay::Model(s) | Overlay::Workspace(s) | Overlay::Organization(s) => s,
        }
    }

    fn state_mut(&mut self) -> &mut TableState {
        match self {
            Overlay::Model(s) | Overlay::Workspace(s) | Overlay::Organization(s) => s,
        }
    }

    pub fn view(&self, ctx: &ViewCtx) -> TableView {
        match self {
            Overlay::Model(s) => model_view(ctx.app, &s.filter),
            Overlay::Workspace(s) => workspace_view(ctx.app, &s.filter),
            Overlay::Organization(_) => organization_view(ctx),
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent, ctx: &ViewCtx) -> OverlayOutcome {
        let view = self.view(ctx);
        let state = self.state_mut();
        match state.handle_key(key, &view) {
            TableKey::Esc => OverlayOutcome::Close,
            TableKey::Enter => match state.selected_row(&view).map(|r| r.key.clone()) {
                Some(RowKey::Model(id)) => OverlayOutcome::CloseWith(Msg::ModelChosen(id)),
                Some(RowKey::Workspace(ws)) => OverlayOutcome::CloseWith(Msg::WorkspaceChosen(ws)),
                Some(RowKey::Organization(id)) => {
                    OverlayOutcome::CloseWith(Msg::OrganizationChosen(id))
                }
                _ => OverlayOutcome::Stay,
            },
            TableKey::Handled | TableKey::Unhandled => OverlayOutcome::Stay,
        }
    }
}
```

In `crates/scuttle-tui/src/main.rs`, add `mod overlay;` and `mod table;` in alphabetical order.
In `crates/scuttle-tui/src/picker.rs`, change the module doc comment to `//! The reasoning effort slider, drawn above the composer. The model, workspace, and organization pickers are table overlays in overlay.rs.`, and change nothing else, since M1.6's slider and its tests live there.
Its list branches stay compiled and tested but are no longer opened; removing them means reworking M1.6's slider plumbing, which is out of this task's scope.
In `crates/scuttle-tui/src/app.rs`:

- Add `use crate::overlay::{Overlay, OverlayOutcome, ViewCtx};` and `use crate::table;`.
- Add the field `pub overlay: Option<Overlay>,` after `picker`, with `overlay: None,` in `Tui::new`.
- In `apply_ui_effect`, replace the `Effect::ShowPicker(kind)` arm with:

```rust
            Effect::ShowPicker(kind) => match Overlay::open(*kind, &self.core) {
                Some(overlay) => self.overlay = Some(overlay),
                None => self.picker = Some(PickerState::open(*kind, &self.core)),
            },
```

- In the Ctrl+C branch of `key`, add `self.overlay = None;` after `self.picker = None;`.
- In `overlay_showing`, add `|| self.overlay.is_some()`.
- In `key`, right after the `if let Some(picker) = self.picker.as_mut() { .. }` block, add:

```rust
        if self.overlay.is_some() {
            return self.overlay_key(key);
        }
```

Add to `impl Tui`, after `key`:

```rust
    /// Hands a key to the open overlay and applies what it asks for.
    fn overlay_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        let ctx = ViewCtx {
            app: &self.core,
            theme: &self.theme,
        };
        let Some(overlay) = self.overlay.as_mut() else {
            return vec![];
        };
        match overlay.handle_key(key, &ctx) {
            OverlayOutcome::Stay => vec![],
            OverlayOutcome::Close => {
                self.overlay = None;
                vec![]
            }
            OverlayOutcome::CloseWith(msg) => {
                self.overlay = None;
                self.update(msg)
            }
        }
    }
```

In `draw_at`, add after the `if let Some(picker) = self.picker.as_ref() { .. }` block:

```rust
        if let Some(overlay) = self.overlay.as_ref() {
            let ctx = ViewCtx {
                app: &self.core,
                theme: &self.theme,
            };
            let view = overlay.view(&ctx);
            let h = if overlay.full_height() {
                transcript.height
            } else {
                10.min(transcript.height)
            };
            table::render(
                f,
                Rect {
                    y: transcript.y + transcript.height - h,
                    height: h,
                    ..transcript
                },
                &view,
                overlay.state(),
                &self.theme,
            );
        }
```

Replace `animation_deadline` with:

```rust
    /// When the next spinner frame is due, or `None` while nothing animates.
    pub fn animation_deadline(&self, now: Instant) -> Option<Instant> {
        let animating = self.core.activity().is_some()
            || self.overlay.as_ref().is_some_and(|o| o.animates(&self.core));
        animating.then(|| now + SPINNER_INTERVAL)
    }
```

In the test module of `crates/scuttle-tui/src/app.rs`, replace `t.picker` with `t.overlay` in the tests that open the model or organization picker (`slash_model_opens_the_picker_and_escape_closes_it`, `ctrl_c_closes_a_picker_and_a_second_press_quits`, `clicks_are_ignored_while_an_overlay_is_showing`, and `the_organization_picker_choice_reaches_the_core`); the effort slider's tests keep `t.picker`.

- [ ] **Step 9: Run the tests to verify they pass**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings`
Expected: PASS, with the model, workspace, and organization tests passing against the table and the slider's tests unchanged.

- [ ] **Step 10: Commit**

```bash
cargo fmt --all && git add crates/scuttle-core/src/fuzzy.rs crates/scuttle-core/src/lib.rs crates/scuttle-core/Cargo.toml Cargo.lock crates/scuttle-tui/src/table.rs crates/scuttle-tui/src/overlay.rs crates/scuttle-tui/src/picker.rs crates/scuttle-tui/src/main.rs crates/scuttle-tui/src/app.rs && git commit -m "feat: draw the model, workspace, and organization pickers as one filterable table

Assisted-by: AI"
```

---

### Task 8: `/chats`: the list, filters, paging, and opening a chat

Depends on M1.6: the activity spinner frames (interface 4).

`/chats` (alias `/resume`) opens a full-height overlay of root chats with their subagents nested one level below, collapsed by default, filtered fuzzily by title and by the tabs all, active, unread, and archived (design section 2).
Each row shows a status marker (the activity spinner while running or interrupting, as feedback item 18 asks, `?` for `requires_action`, `!` for `error`), an unread dot, the title, `+N` with the busiest subagent's marker while collapsed, `archived`, and the time since `updated_at`.
Enter opens the selected chat through Task 3's switch, the last loaded row loads the next page, and Ctrl+R opens the overlay from anywhere (decision "Ctrl+R for `/chats`"; `ratatui-textarea` 0.9.2 then no longer gets Ctrl+R for redo).

**Files:**
- Create: `crates/scuttle-core/src/time.rs`
- Modify: `crates/scuttle-core/src/lib.rs` (`pub mod time;`)
- Modify: `crates/scuttle-core/src/chat_list.rs` (`Filter`, `ChatRow`, `rows`, `any_running`, tests)
- Modify: `crates/scuttle-core/src/commands.rs` (`Command::Chats`, `/chats` with `/resume`, tests)
- Modify: `crates/scuttle-core/src/app.rs` (`Effect::ShowChats`, `command`, tests)
- Modify: `crates/scuttle-tui/src/table.rs` (`RowKey::Chat`)
- Modify: `crates/scuttle-tui/src/overlay.rs` (`ChatsState`, `Overlay::Chats`, rows and keys, tests)
- Modify: `crates/scuttle-tui/src/app.rs` (`ShowChats`, Ctrl+R, tests)
- Modify: `crates/scuttle-tui/src/runtime.rs` (ignore `ShowChats`)

**Interfaces:**
- Consumes: `ChatList`, `ListQuery`, `Load`, `chat_status`, `is_active` (Task 5); `Msg::OpenChat` (Task 3); `Msg::LoadChats` (Task 5); `table` and `overlay` (Task 7); `activity::spinner_frame`.
- Produces: `scuttle_core::time::relative(then_unix: i64, now_unix: i64) -> String`; `chat_list::Filter { All, Active, Unread, Archived }` with `ALL`, `next`, `label`, `query`; `chat_list::ChatRow { pub id, pub depth: u8, pub title, pub status: Option<ChatStatus>, pub unread: bool, pub archived: bool, pub pinned: bool, pub children: usize, pub busiest_child: Option<ChatStatus>, pub updated_unix: Option<i64> }`; `ChatList::rows(&self, Filter, &str, &HashSet<Uuid>) -> Vec<ChatRow>`; `ChatList::any_running(&self) -> bool`; `Command::Chats(Option<String>)`; `Effect::ShowChats(String)`; `overlay::ViewCtx::now_unix: i64` and `ViewCtx::elapsed: Duration`; `OverlayOutcome::Send(Msg)`; `app::now_unix() -> i64` in the TUI; `table::RowKey::Chat(Uuid)`; `overlay::ChatsState { pub table: TableState, pub filter: Filter, pub expanded: HashSet<Uuid> }`; `Overlay::Chats(ChatsState)` and `Overlay::chats(query: String, app: &App) -> Overlay`.
  Tasks 9 and 10 add keys and rows to `Overlay::Chats`.

- [ ] **Step 1: Write the failing core tests**

Create `crates/scuttle-core/src/time.rs` with:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_times_are_short() {
        let now = 1_000_000;
        assert_eq!(relative(now - 20, now), "now");
        assert_eq!(relative(now - 4 * 60, now), "4m");
        assert_eq!(relative(now - 3600, now), "1h");
        assert_eq!(relative(now - 3 * 86_400, now), "3d");
        assert_eq!(relative(now + 90, now), "now", "a clock ahead of ours is now");
    }
}
```

Add `pub mod time;` to `crates/scuttle-core/src/lib.rs`, and add to the test module of `crates/scuttle-core/src/chat_list.rs`:

```rust
    fn family() -> (ChatList, Uuid, Uuid) {
        let (root, child) = (Uuid::new_v4(), Uuid::new_v4());
        let mut parent = listed(root, "Fix the flaky watch reconnect test", "2026-09-30T10:00:00Z");
        let mut sub = with_status(listed(child, "explore", "2026-09-30T10:00:00Z"), "running");
        sub.parent_chat_id = Some(root);
        sub.has_unread = Some(true);
        parent.children = vec![sub];
        let mut archived = listed(Uuid::new_v4(), "Old reconnect spike", "2026-09-27T10:00:00Z");
        archived.archived = Some(true);
        let mut list = ChatList::default();
        list.apply_page(
            &ListQuery::Default,
            0,
            vec![parent, listed(Uuid::new_v4(), "Draft the M2 design", "2026-09-30T09:00:00Z")],
        );
        list.apply_page(&ListQuery::Archived, 0, vec![archived]);
        (list, root, child)
    }

    fn row_titles(rows: &[ChatRow]) -> Vec<&str> {
        rows.iter().map(|r| r.title.as_str()).collect()
    }

    #[test]
    fn filters_pick_all_active_unread_and_archived_chats() {
        let (list, _, _) = family();
        let none = HashSet::new();
        assert_eq!(
            row_titles(&list.rows(Filter::All, "", &none)),
            ["Fix the flaky watch reconnect test", "Draft the M2 design"]
        );
        assert_eq!(
            row_titles(&list.rows(Filter::Active, "", &none)),
            ["Fix the flaky watch reconnect test"],
            "a running subagent makes its root active"
        );
        assert_eq!(
            row_titles(&list.rows(Filter::Unread, "", &none)),
            ["Fix the flaky watch reconnect test"]
        );
        assert_eq!(
            row_titles(&list.rows(Filter::Archived, "", &none)),
            ["Old reconnect spike"]
        );
        assert_eq!(
            row_titles(&list.rows(Filter::All, "draft", &none)),
            ["Draft the M2 design"]
        );
        assert_eq!(Filter::Archived.next(), Filter::All);
        assert_eq!(Filter::Archived.query(), ListQuery::Archived);
    }

    #[test]
    fn a_collapsed_root_shows_its_count_and_busiest_subagent_and_expands_below() {
        let (list, root, child) = family();
        let rows = list.rows(Filter::All, "", &HashSet::new());
        assert_eq!(rows[0].children, 1);
        assert_eq!(rows[0].busiest_child, Some(ChatStatus::Running));
        let expanded = HashSet::from([root]);
        let rows = list.rows(Filter::All, "", &expanded);
        assert_eq!(rows[1].id, child);
        assert_eq!(rows[1].depth, 1);
        assert_eq!(rows[0].busiest_child, None, "an expanded root shows its subagents instead");
        assert!(list.any_running());
    }
```

Change the imports at the top of the `chat_list` test module to `use super::*;` and `use std::collections::HashSet;`.
In the test module of `crates/scuttle-core/src/commands.rs`, change the expected list in `completes_by_prefix` to `vec!["/chats", "/compact", "/clear", "/copy"]`, insert `"/chats",` after `"/new",` in `every_listed_command_parses`, and add:

```rust
    #[test]
    fn parses_chats_and_its_alias() {
        assert_eq!(parse("/chats"), Ok(Command::Chats(None)));
        assert_eq!(
            parse("/resume fix watch"),
            Ok(Command::Chats(Some("fix watch".into())))
        );
    }
```

Add to the test module of `crates/scuttle-core/src/app.rs`:

```rust
    #[test]
    fn chats_opens_the_overlay_and_loads_the_list_unless_it_is_live() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        assert_eq!(
            app.update(Msg::Command(Command::Chats(Some("fix".into())))),
            vec![
                Effect::ShowChats("fix".into()),
                Effect::FetchChats {
                    query: ListQuery::Default,
                    offset: 0
                }
            ]
        );
        app.update(Msg::ChatsLoaded {
            query: ListQuery::Default,
            offset: 0,
            chats: vec![],
        });
        app.update(Msg::WatchConnected);
        app.update(Msg::ChatsLoaded {
            query: ListQuery::Default,
            offset: 0,
            chats: vec![],
        });
        assert_eq!(
            app.update(Msg::Command(Command::Chats(None))),
            vec![Effect::ShowChats(String::new())]
        );
    }
```

- [ ] **Step 2: Run the core tests to verify they fail**

Run: `cargo test -p scuttle-core`
Expected: FAIL to compile, because `relative`, `Filter`, `ChatRow`, `Command::Chats`, and `Effect::ShowChats` do not exist.

- [ ] **Step 3: Implement the core side**

Put this above the test module in `crates/scuttle-core/src/time.rs`:

```rust
//! Times as the overlays show them.

/// The time from `then_unix` to `now_unix` in one short unit: `now`, `4m`, `1h`, or `3d`.
pub fn relative(then_unix: i64, now_unix: i64) -> String {
    let secs = now_unix.saturating_sub(then_unix);
    match secs {
        ..60 => "now".into(),
        60..3600 => format!("{}m", secs / 60),
        3600..86_400 => format!("{}h", secs / 3600),
        _ => format!("{}d", secs / 86_400),
    }
}
```

Add to `crates/scuttle-core/src/chat_list.rs`, after `ChatList`'s struct, and `use std::collections::HashSet;` at the top:

```rust
/// The tabs of `/chats`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Filter {
    #[default]
    All,
    Active,
    Unread,
    Archived,
}

impl Filter {
    pub const ALL: [Filter; 4] = [Filter::All, Filter::Active, Filter::Unread, Filter::Archived];

    pub fn next(self) -> Filter {
        match self {
            Filter::All => Filter::Active,
            Filter::Active => Filter::Unread,
            Filter::Unread => Filter::Archived,
            Filter::Archived => Filter::All,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Filter::All => "all",
            Filter::Active => "active",
            Filter::Unread => "unread",
            Filter::Archived => "archived",
        }
    }

    /// The server list the tab draws from; active and unread filter the default list locally.
    pub fn query(self) -> ListQuery {
        match self {
            Filter::Archived => ListQuery::Archived,
            _ => ListQuery::Default,
        }
    }
}

/// One row of `/chats`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatRow {
    pub id: Uuid,
    /// 0 for a root chat, 1 for a subagent.
    pub depth: u8,
    pub title: String,
    pub status: Option<ChatStatus>,
    pub unread: bool,
    pub archived: bool,
    pub pinned: bool,
    /// A root's subagent count.
    pub children: usize,
    /// The status of the busiest subagent, shown while the root is collapsed.
    pub busiest_child: Option<ChatStatus>,
    pub updated_unix: Option<i64>,
}

/// The status that says the most about a family: working, then waiting on the user, then
/// failed.
fn busiest(chats: &[types::CodersdkChat]) -> Option<ChatStatus> {
    let weight = |s: &ChatStatus| match s {
        ChatStatus::Running | ChatStatus::Interrupting => 3,
        ChatStatus::RequiresAction => 2,
        ChatStatus::Error => 1,
        _ => 0,
    };
    chats
        .iter()
        .filter_map(chat_status)
        .filter(|s| weight(s) > 0)
        .max_by_key(|s| weight(s))
}

fn row(chat: &types::CodersdkChat, id: Uuid, depth: u8) -> ChatRow {
    ChatRow {
        id,
        depth,
        title: chat
            .title
            .clone()
            .filter(|t| !t.trim().is_empty())
            .unwrap_or_else(|| "Untitled".into()),
        status: chat_status(chat),
        unread: chat.has_unread == Some(true),
        archived: chat.archived == Some(true),
        pinned: chat.pin_order.unwrap_or(0) > 0,
        children: 0,
        busiest_child: None,
        updated_unix: chat.updated_at.map(|t| t.timestamp()),
    }
}
```

Add to `impl ChatList`:

```rust
    /// The rows of `/chats` for `filter`, ranked by `query` on root titles, with the subagents
    /// of each root in `expanded` right below it.
    pub fn rows(&self, filter: Filter, query: &str, expanded: &HashSet<Uuid>) -> Vec<ChatRow> {
        let family = |c: &types::CodersdkChat| -> Vec<types::CodersdkChat> {
            std::iter::once(c.clone()).chain(c.children.iter().cloned()).collect()
        };
        let page = match filter {
            Filter::Archived => &self.archived,
            _ => &self.main,
        };
        let roots: Vec<&types::CodersdkChat> = page
            .chats
            .iter()
            .filter(|c| match filter {
                Filter::All => c.archived != Some(true),
                Filter::Active => {
                    c.archived != Some(true)
                        && family(c).iter().any(|m| is_active(chat_status(m).as_ref()))
                }
                Filter::Unread => {
                    c.archived != Some(true)
                        && family(c).iter().any(|m| m.has_unread == Some(true))
                }
                Filter::Archived => true,
            })
            .collect();
        let mut rows = Vec::new();
        for root in crate::fuzzy::rank(query, roots, |c| c.title.clone().unwrap_or_default()) {
            let Some(id) = root.id else {
                continue;
            };
            let open = expanded.contains(&id);
            rows.push(ChatRow {
                children: root.children.len(),
                busiest_child: (!open).then(|| busiest(&root.children)).flatten(),
                ..row(root, id, 0)
            });
            if open {
                rows.extend(
                    root.children
                        .iter()
                        .filter_map(|c| Some(row(c, c.id?, 1))),
                );
            }
        }
        rows
    }

    /// Whether any loaded chat or subagent is working, so the list's spinners must move.
    pub fn any_running(&self) -> bool {
        self.pages()
            .flat_map(|p| p.chats.iter())
            .flat_map(|root| std::iter::once(root).chain(root.children.iter()))
            .any(|c| {
                matches!(
                    chat_status(c),
                    Some(ChatStatus::Running | ChatStatus::Interrupting)
                )
            })
    }
```

In `crates/scuttle-core/src/commands.rs`, add `Chats(Option<String>),` to `Command` after `New`, add after the `/new` entry of `COMMANDS`:

```rust
    CommandInfo {
        name: "/chats",
        aliases: &["/resume"],
        usage: "/chats [query]",
        description: "Find and open a chat or subagent (Ctrl+R)",
    },
```

and add `"chats" => Ok(Command::Chats(arg.map(str::to_owned))),` to `parse` after the `"new"` arm.
In `crates/scuttle-core/src/app.rs`, add `ShowChats(String),` to `Effect` after `ShowHelp`, with the doc comment `/// Opens /chats with its filter already typed.`, and add this arm to `command` before `Command::New`:

```rust
            Command::Chats(query) => {
                let mut effects = vec![Effect::ShowChats(query.unwrap_or_default())];
                // The watch keeps a loaded list current; without it, refetch the first page.
                let stale = !matches!(self.chats.main.load, crate::chat_list::Load::Loaded);
                if stale || !self.chats.watch_live {
                    effects.extend(self.load_chats(ListQuery::Default, false));
                }
                effects
            }
```

- [ ] **Step 4: Run the core tests to verify they pass**

Run: `cargo test -p scuttle-core`
Expected: PASS.

- [ ] **Step 5: Write the failing TUI tests**

Add `Chat(Uuid),` to `RowKey` in `crates/scuttle-tui/src/table.rs`.
Add to the test module of `crates/scuttle-tui/src/app.rs`:

```rust
    fn chats_tui() -> (Tui, uuid::Uuid, uuid::Uuid) {
        use scuttle_core::chat_list::ListQuery;
        let mut t = tui();
        t.core.update(Msg::Started {
            org_id: uuid::Uuid::new_v4(),
            open_chat: None,
        });
        let (root, child) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let chats = serde_json::from_value(json!([
            {"id": root, "title": "Fix the flaky watch reconnect test", "status": "waiting",
             "updated_at": "2026-09-30T10:00:00Z", "files": [], "mcp_server_ids": [],
             "inline_mcp_servers": [], "labels": {},
             "children": [{"id": child, "title": "explore", "status": "running", "parent_chat_id": root,
                "updated_at": "2026-09-30T10:00:00Z", "children": [], "files": [],
                "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}}]},
            {"id": uuid::Uuid::new_v4(), "title": "Draft the M2 design", "status": "waiting",
             "updated_at": "2026-09-30T09:00:00Z", "children": [], "files": [],
             "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}}
        ]))
        .unwrap();
        t.core.update(Msg::ChatsLoaded {
            query: ListQuery::Default,
            offset: 0,
            chats,
        });
        (t, root, child)
    }

    fn show(t: &mut Tui, effects: Vec<Effect>) {
        for e in &effects {
            t.apply_ui_effect(e);
        }
    }

    #[test]
    fn slash_chats_lists_chats_with_its_query_and_enter_opens_one() {
        let (mut t, root, _) = chats_tui();
        let effects = t.update(Msg::Submit("/chats watch".into()));
        show(&mut t, effects);
        let shown = screen(&mut t, 80, 20);
        assert!(shown.contains("> watch"), "{shown}");
        assert!(shown.contains("Fix the flaky watch reconnect test"), "{shown}");
        assert!(!shown.contains("Draft the M2 design"), "{shown}");
        assert!(shown.contains("+1"), "{shown}");
        let effects = t.handle(key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(t.overlay.is_none());
        assert!(effects.contains(&Effect::LoadChat(root)), "{effects:?}");
    }

    #[test]
    fn ctrl_r_opens_chats_and_right_shows_the_subagents() {
        let (mut t, _, _) = chats_tui();
        let effects = t.handle(key(KeyCode::Char('r'), KeyModifiers::CONTROL));
        show(&mut t, effects);
        assert!(t.overlay.is_some());
        assert!(!screen(&mut t, 80, 20).contains("explore"));
        t.handle(key(KeyCode::Right, KeyModifiers::NONE));
        assert!(screen(&mut t, 80, 20).contains("explore"));
        t.handle(key(KeyCode::Left, KeyModifiers::NONE));
        assert!(!screen(&mut t, 80, 20).contains("explore"));
    }

    #[test]
    fn tab_cycles_the_filters_and_the_archived_tab_loads_its_page() {
        let (mut t, _, _) = chats_tui();
        show(&mut t, vec![Effect::ShowChats(String::new())]);
        t.handle(key(KeyCode::Tab, KeyModifiers::NONE));
        let shown = screen(&mut t, 80, 20);
        assert!(shown.contains("[active]"), "{shown}");
        assert!(shown.contains("Fix the flaky"), "a running subagent makes its root active");
        assert!(!shown.contains("Draft the M2 design"), "{shown}");
        t.handle(key(KeyCode::Tab, KeyModifiers::NONE));
        let effects = t.handle(key(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(
            effects,
            vec![Effect::FetchChats {
                query: scuttle_core::chat_list::ListQuery::Archived,
                offset: 0
            }]
        );
        assert!(screen(&mut t, 80, 20).contains("Loading chats…"));
    }

    #[test]
    fn running_chats_spin_and_the_header_says_when_updates_pause() {
        let (mut t, _, _) = chats_tui();
        show(&mut t, vec![Effect::ShowChats(String::new())]);
        let now = Instant::now();
        assert!(t.animation_deadline(now).is_some(), "a running subagent spins");
        assert!(screen(&mut t, 80, 20).contains("live updates paused"));
        t.update(Msg::WatchConnected);
        assert!(!screen(&mut t, 80, 20).contains("live updates paused"));
    }
```

- [ ] **Step 6: Run the TUI tests to verify they fail**

Run: `cargo test -p scuttle-tui`
Expected: FAIL: `Effect::ShowChats` opens nothing and Ctrl+R reaches the composer.

- [ ] **Step 7: Implement the overlay, the effect, and Ctrl+R**

In `crates/scuttle-tui/src/overlay.rs`, add imports `use std::collections::HashSet;`, `use std::time::Duration;`, `use coder_sdk::ChatStatus;`, `use crossterm::event::KeyCode;`, `use scuttle_core::chat_list::{ChatRow, Filter, Load};`, `use uuid::Uuid;`, and `use crate::activity::spinner_frame;`.
Add a variant to `OverlayOutcome`, which `/chats` is the first to need:

```rust
    /// Sends the message to the core and keeps the overlay open.
    Send(Msg),
```

and its arm to `Tui::overlay_key` in `crates/scuttle-tui/src/app.rs`: `OverlayOutcome::Send(msg) => self.update(msg),`.
Add two fields to `ViewCtx`:

```rust
    /// Seconds since the Unix epoch, for relative times.
    pub now_unix: i64,
    /// Time since the UI started, which picks spinner frames.
    pub elapsed: Duration,
```

and add `now_unix: 0, elapsed: Duration::ZERO,` to the three `ViewCtx` literals in the overlay tests (the `press` helper and the two tests that build a context).
Then add:

```rust
/// The state of `/chats` beyond its table: the tab and the expanded roots.
pub struct ChatsState {
    pub table: TableState,
    pub filter: Filter,
    pub expanded: HashSet<Uuid>,
}

fn marker(status: Option<&ChatStatus>, elapsed: Duration) -> String {
    match status {
        Some(ChatStatus::Running | ChatStatus::Interrupting) => spinner_frame(elapsed).to_owned(),
        Some(ChatStatus::RequiresAction) => "?".into(),
        Some(ChatStatus::Error) => "!".into(),
        _ => " ".into(),
    }
}

fn chat_cells(r: &ChatRow, ctx: &ViewCtx) -> Vec<Line<'static>> {
    let indent = if r.depth > 0 { "   " } else { "" };
    let family = if r.children > 0 {
        format!("+{} {}", r.children, marker(r.busiest_child.as_ref(), ctx.elapsed))
    } else {
        String::new()
    };
    let when = r
        .updated_unix
        .map(|t| scuttle_core::time::relative(t, ctx.now_unix))
        .unwrap_or_default();
    vec![
        Line::from(Span::styled(marker(r.status.as_ref(), ctx.elapsed), ctx.theme.accent)),
        Line::from(if r.unread { "•" } else { " " }),
        Line::from(format!("{indent}{}", r.title)),
        Line::from(Span::styled(family, ctx.theme.dim)),
        Line::from(Span::styled(if r.archived { "archived" } else { "" }, ctx.theme.dim)),
        Line::from(Span::styled(when, ctx.theme.dim)),
    ]
}

fn chats_view(state: &ChatsState, ctx: &ViewCtx) -> TableView {
    let list = &ctx.app.chats;
    let query = state.table.filter.as_str();
    let rows: Vec<Row> = list
        .rows(state.filter, query, &state.expanded)
        .iter()
        .map(|r| Row::item(RowKey::Chat(r.id), chat_cells(r, ctx)))
        .collect();
    let page = list.page(&state.filter.query());
    let load = page.map(|p| &p.load);
    let status = match load {
        Some(Load::Failed(message)) => Some(format!("{message} Press Tab to retry.")),
        _ if !rows.is_empty() => None,
        Some(Load::Idle | Load::Loading) => Some("Loading chats…".into()),
        _ if query.is_empty() && state.filter == Filter::All => {
            Some("No chats yet. Type a message to start one.".into())
        }
        _ => Some("No chats match.".into()),
    };
    let tabs = Filter::ALL
        .iter()
        .map(|f| {
            if *f == state.filter {
                format!("[{}]", f.label())
            } else {
                f.label().to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join(" | ");
    let paused = if list.watch_live { "" } else { "   live updates paused" };
    TableView {
        title: format!("Chats   {tabs}{paused}"),
        widths: vec![
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Fill(1),
            Constraint::Length(4),
            Constraint::Length(8),
            Constraint::Length(3),
        ],
        rows,
        status,
        hint: Some("Enter opens, Tab filters, Right and Left show subagents, Esc closes".into()),
        filterable: true,
    }
}

impl ChatsState {
    fn handle_key(&mut self, key: KeyEvent, ctx: &ViewCtx) -> OverlayOutcome {
        let view = chats_view(self, ctx);
        let selected = match self.table.selected_row(&view).map(|r| r.key.clone()) {
            Some(RowKey::Chat(id)) => Some(id),
            _ => None,
        };
        let outcome = match self.table.handle_key(key, &view) {
            TableKey::Esc => OverlayOutcome::Close,
            TableKey::Enter => match selected {
                Some(id) => OverlayOutcome::CloseWith(Msg::OpenChat(id)),
                None => OverlayOutcome::Stay,
            },
            TableKey::Handled => self.load_more(ctx),
            TableKey::Unhandled => self.chat_key(key, selected, ctx),
        };
        outcome
    }

    /// Loads the next page once the selection reaches the last loaded row.
    fn load_more(&self, ctx: &ViewCtx) -> OverlayOutcome {
        let view = chats_view(self, ctx);
        let last = view.rows.iter().rposition(Row::selectable);
        if last.is_some() && self.table.index(&view) == last {
            OverlayOutcome::Send(Msg::LoadChats {
                query: self.filter.query(),
                more: true,
            })
        } else {
            OverlayOutcome::Stay
        }
    }

    fn chat_key(&mut self, key: KeyEvent, selected: Option<Uuid>, ctx: &ViewCtx) -> OverlayOutcome {
        let root_of = |id: Uuid| {
            ctx.app
                .chats
                .find(id)
                .and_then(|c| c.parent_chat_id)
                .unwrap_or(id)
        };
        match key.code {
            KeyCode::Tab => {
                let query = self.filter.query();
                if let Some(Load::Failed(_)) = ctx.app.chats.page(&query).map(|p| &p.load) {
                    return OverlayOutcome::Send(Msg::LoadChats { query, more: false });
                }
                self.filter = self.filter.next();
                self.table.selected = None;
                let idle = ctx
                    .app
                    .chats
                    .page(&self.filter.query())
                    .is_some_and(|p| p.load == Load::Idle);
                if idle {
                    OverlayOutcome::Send(Msg::LoadChats {
                        query: self.filter.query(),
                        more: false,
                    })
                } else {
                    OverlayOutcome::Stay
                }
            }
            KeyCode::Right => {
                if let Some(id) = selected {
                    self.expanded.insert(root_of(id));
                }
                OverlayOutcome::Stay
            }
            KeyCode::Left => {
                if let Some(id) = selected {
                    let root = root_of(id);
                    self.expanded.remove(&root);
                    self.table.selected = Some(RowKey::Chat(root));
                }
                OverlayOutcome::Stay
            }
            _ => OverlayOutcome::Stay,
        }
    }
}
```

Add `Chats(ChatsState),` to `Overlay`, and a constructor to `impl Overlay`:

```rust
    /// `/chats` with `query` already typed, starting on the open chat when it is listed.
    pub fn chats(query: String, app: &App) -> Overlay {
        Overlay::Chats(ChatsState {
            table: TableState {
                filter: query,
                selected: app.chat_id.map(RowKey::Chat),
            },
            filter: Filter::All,
            expanded: HashSet::new(),
        })
    }
```

Then extend each method of `impl Overlay` with the new variant: `full_height` returns `matches!(self, Overlay::Chats(_))`; `animates` returns `matches!(self, Overlay::Chats(_)) && app.chats.any_running()`; `state` and `state_mut` add `Overlay::Chats(c) => &c.table` and `Overlay::Chats(c) => &mut c.table`; `view` adds `Overlay::Chats(c) => chats_view(c, ctx),`; and `handle_key` starts with:

```rust
        if let Overlay::Chats(chats) = self {
            return chats.handle_key(key, ctx);
        }
```

In `crates/scuttle-tui/src/app.rs`, add a free function after `web_copy_notice`:

```rust
/// Seconds since the Unix epoch, for relative times in overlays.
fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
```

Add `now_unix: now_unix(), elapsed: self.epoch.elapsed(),` to the `ViewCtx` literal in `overlay_key`, and `now_unix: now_unix(), elapsed: now.saturating_duration_since(self.epoch),` to the one in `draw_at`.
Add to `apply_ui_effect`:

```rust
            Effect::ShowChats(query) => {
                self.overlay = Some(Overlay::chats(query.clone(), &self.core));
            }
```

and in `key`, right after the `if self.overlay.is_some() { .. }` block, add:

```rust
        if key.code == KeyCode::Char('r') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return self.update(Msg::Command(scuttle_core::commands::Command::Chats(None)));
        }
```

In `crates/scuttle-tui/src/runtime.rs`, add `| Effect::ShowChats(_)` to the list of UI effects the runtime ignores.

- [ ] **Step 8: Run the tests to verify they pass**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings`
Expected: PASS.

- [ ] **Step 9: Commit**

```bash
cargo fmt --all && git add crates/scuttle-core/src/time.rs crates/scuttle-core/src/lib.rs crates/scuttle-core/src/chat_list.rs crates/scuttle-core/src/commands.rs crates/scuttle-core/src/app.rs crates/scuttle-tui/src/table.rs crates/scuttle-tui/src/overlay.rs crates/scuttle-tui/src/app.rs crates/scuttle-tui/src/runtime.rs && git commit -m "feat: find and open chats with /chats and Ctrl+R

Assisted-by: AI"
```

---

### Task 9: `/chats` actions: archive, rename, pin, and read state

In `/chats`, Ctrl+A archives or unarchives the selected root chat after a second press, Ctrl+E renames the selected chat in a one-line editor, Ctrl+P pins or unpins a root chat, and Ctrl+U marks the chat read or unread (design section 2).
All four use `PATCH /api/v2/chats/{chat}` (`codersdk/chats.go:710-738`); archive state changes only on a root (`coderd/exp_chats.go:2470-2475`), a child cannot be pinned (`:2543-2545`), and scuttle refuses to archive while the family is running, as the web UI does, leaving the server's refusal as the backstop (`site/src/pages/AgentsPage/components/ChatActionsMenuItems.tsx:26-43`).
The one-line editor lives in the core, so `/title` (Task 15) and free-text answers (Task 17) reuse it.

**Files:**
- Create: `crates/scuttle-core/src/line_edit.rs`
- Modify: `crates/scuttle-core/src/lib.rs` (`pub mod line_edit;`)
- Modify: `crates/scuttle-core/src/chat_list.rs` (`resort`)
- Modify: `crates/scuttle-core/src/app.rs` (`ChatAction`, `ChatChange`, `EditTarget`, `Editor`, `Msg`, `Effect::UpdateChat`, `chat_action`, `edit`, tests)
- Modify: `crates/scuttle-tui/src/overlay.rs` (Ctrl keys and the confirmation)
- Modify: `crates/scuttle-tui/src/app.rs` (`edit_key`, editor routing, `draw_editor`, tests)
- Modify: `crates/scuttle-tui/src/runtime.rs` (`UpdateChat`, tests)

**Interfaces:**
- Consumes: `ChatList::find`, `update_copies`, `set_read`, `family_running` (Task 5); `ChatsState` (Task 8).
- Produces: `line_edit::Edit { Char(char), Backspace, Delete, Left, Right, Home, End, Submit, Cancel }`; `line_edit::LineEdit` with `new(&str)`, `text(&self) -> &str`, `cursor(&self) -> usize`, `apply(&mut self, Edit) -> EditOutcome`; `line_edit::EditOutcome { Editing, Submitted(String), Cancelled }`; `app::ChatAction { ToggleArchive(Uuid), TogglePin(Uuid), ToggleRead(Uuid), Rename(Uuid) }`; `app::ChatChange { Archived(bool), Title(String), PinOrder(i64), Read(bool) }`; `app::EditTarget { Rename(Uuid) }`; `app::Editor { pub target: EditTarget, pub line: LineEdit, pub loading: bool }`; `App::editor: Option<Editor>`; `Msg::ChatAction(ChatAction)`; `Msg::Edit(Edit)`; `Msg::ChatUpdated { chat: Uuid, change: ChatChange }`; `Msg::ChatUpdateFailed { chat: Uuid, change: ChatChange, message: String }`; `Effect::UpdateChat { chat: Uuid, change: ChatChange }`; `ChatList::resort(&mut self)`; `ChatsState::confirm: Option<Uuid>`; `app::edit_key(KeyEvent) -> Option<Edit>` in the TUI.
  Task 15 adds `EditTarget::Title`, Task 17 adds `EditTarget::Other`, and Task 15 sends `Effect::UpdateChat` with `ChatChange::Title`.

- [ ] **Step 1: Write the failing core tests**

Create `crates/scuttle-core/src/line_edit.rs` with:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_happen_at_the_cursor_across_multibyte_characters() {
        let mut e = LineEdit::new("héllo");
        assert_eq!(e.cursor(), 5);
        e.apply(Edit::Left);
        e.apply(Edit::Left);
        e.apply(Edit::Backspace);
        assert_eq!(e.text(), "hélo");
        assert_eq!(e.cursor(), 2);
        e.apply(Edit::Char('ł'));
        assert_eq!(e.text(), "héłlo");
        e.apply(Edit::Home);
        e.apply(Edit::Delete);
        assert_eq!(e.text(), "éłlo");
        e.apply(Edit::Right);
        e.apply(Edit::End);
        assert_eq!(e.cursor(), 4);
    }

    #[test]
    fn submit_trims_and_cancel_discards() {
        let mut e = LineEdit::new("  Fix the watch test ");
        assert_eq!(
            e.apply(Edit::Submit),
            EditOutcome::Submitted("Fix the watch test".into())
        );
        assert_eq!(e.apply(Edit::Cancel), EditOutcome::Cancelled);
        assert_eq!(e.apply(Edit::Char('x')), EditOutcome::Editing);
    }
}
```

Add `pub mod line_edit;` to `crates/scuttle-core/src/lib.rs`, and add to the test module of `crates/scuttle-core/src/app.rs`:

```rust
    fn loaded_list(app: &mut App, chats: Vec<types::CodersdkChat>) {
        app.update(Msg::ChatsLoaded {
            query: ListQuery::Default,
            offset: 0,
            chats,
        });
    }

    #[test]
    fn archiving_waits_for_a_running_subagent_and_a_refusal_leaves_the_list() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let (root, child) = (Uuid::new_v4(), Uuid::new_v4());
        let mut family = listed(root, "root", "2026-09-30T10:00:00Z");
        let mut sub = listed(child, "explore", "2026-09-30T10:00:00Z");
        sub.parent_chat_id = Some(root);
        sub.status = Some(types::CodersdkChatStatus("running".into()));
        family.children = vec![sub];
        loaded_list(&mut app, vec![family]);
        assert!(
            app.update(Msg::ChatAction(ChatAction::ToggleArchive(root)))
                .is_empty()
        );
        assert_eq!(
            app.notices.last(),
            Some(&Notice::Error(
                "Wait for this chat and its subagents to stop, then archive it.".into()
            ))
        );
        let mut done = listed(child, "explore", "2026-09-30T10:05:00Z");
        done.status = Some(types::CodersdkChatStatus("waiting".into()));
        app.update(watch("status_change", done));
        assert_eq!(
            app.update(Msg::ChatAction(ChatAction::ToggleArchive(root))),
            vec![Effect::UpdateChat {
                chat: root,
                change: ChatChange::Archived(true)
            }]
        );
        // A subagent that starts again before the request lands makes the server refuse it.
        app.update(Msg::ChatUpdateFailed {
            chat: root,
            change: ChatChange::Archived(true),
            message: "Chat has a running subagent.".into(),
        });
        assert_eq!(app.chats.find(root).and_then(|c| c.archived), None);
        assert_eq!(
            app.notices.last(),
            Some(&Notice::Error(
                "Could not archive the chat: Chat has a running subagent.".into()
            ))
        );
        app.update(Msg::ChatUpdated {
            chat: root,
            change: ChatChange::Archived(true),
        });
        assert_eq!(app.chats.find(root).and_then(|c| c.archived), Some(true));
    }

    #[test]
    fn subagents_cannot_be_archived_or_pinned_and_pin_and_read_toggle() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let (root, child) = (Uuid::new_v4(), Uuid::new_v4());
        let mut family = listed(root, "root", "2026-09-30T10:00:00Z");
        family.has_unread = Some(true);
        let mut sub = listed(child, "explore", "2026-09-30T10:00:00Z");
        sub.parent_chat_id = Some(root);
        family.children = vec![sub];
        loaded_list(&mut app, vec![family]);
        assert!(app.update(Msg::ChatAction(ChatAction::ToggleArchive(child))).is_empty());
        assert!(app.update(Msg::ChatAction(ChatAction::TogglePin(child))).is_empty());
        assert_eq!(
            app.notices.last(),
            Some(&Notice::Error("A subagent cannot be pinned.".into()))
        );
        assert_eq!(
            app.update(Msg::ChatAction(ChatAction::TogglePin(root))),
            vec![Effect::UpdateChat {
                chat: root,
                change: ChatChange::PinOrder(1)
            }]
        );
        assert_eq!(
            app.update(Msg::ChatAction(ChatAction::ToggleRead(root))),
            vec![Effect::UpdateChat {
                chat: root,
                change: ChatChange::Read(true)
            }]
        );
        app.update(Msg::ChatUpdated {
            chat: root,
            change: ChatChange::Read(true),
        });
        assert_eq!(app.chats.find(root).and_then(|c| c.has_unread), Some(false));
    }

    #[test]
    fn rename_edits_the_title_in_the_core_and_saves_it() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        loaded_list(&mut app, vec![listed(id, "Untitled", "2026-09-30T10:00:00Z")]);
        app.update(Msg::ChatAction(ChatAction::Rename(id)));
        assert_eq!(
            app.editor.as_ref().map(|e| e.line.text()),
            Some("Untitled")
        );
        for _ in 0.."Untitled".len() {
            app.update(Msg::Edit(Edit::Backspace));
        }
        for c in "Watch fix".chars() {
            app.update(Msg::Edit(Edit::Char(c)));
        }
        assert_eq!(
            app.update(Msg::Edit(Edit::Submit)),
            vec![Effect::UpdateChat {
                chat: id,
                change: ChatChange::Title("Watch fix".into())
            }]
        );
        assert!(app.editor.is_none());
        app.update(Msg::ChatUpdated {
            chat: id,
            change: ChatChange::Title("Watch fix".into()),
        });
        assert_eq!(app.chats.find(id).and_then(|c| c.title.as_deref()), Some("Watch fix"));
        app.update(Msg::ChatAction(ChatAction::Rename(id)));
        app.update(Msg::Edit(Edit::Cancel));
        assert!(app.editor.is_none());
    }
```

Add `use crate::line_edit::Edit;` to the test module's imports.

- [ ] **Step 2: Run the core tests to verify they fail**

Run: `cargo test -p scuttle-core`
Expected: FAIL to compile, because `LineEdit`, `ChatAction`, `ChatChange`, and `Msg::Edit` do not exist.

- [ ] **Step 3: Implement the editor and the actions in the core**

Put this above the test module in `crates/scuttle-core/src/line_edit.rs`:

```rust
//! A one-line text editor the core owns, for renames, titles, and free-text answers.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edit {
    Char(char),
    Backspace,
    Delete,
    Left,
    Right,
    Home,
    End,
    Submit,
    Cancel,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditOutcome {
    Editing,
    /// The trimmed text.
    Submitted(String),
    Cancelled,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LineEdit {
    text: String,
    /// The cursor position in characters.
    cursor: usize,
}

impl LineEdit {
    /// An editor holding `text` with the cursor at its end.
    pub fn new(text: &str) -> LineEdit {
        LineEdit {
            text: text.to_owned(),
            cursor: text.chars().count(),
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    fn byte(&self, chars: usize) -> usize {
        self.text
            .char_indices()
            .nth(chars)
            .map_or(self.text.len(), |(i, _)| i)
    }

    pub fn apply(&mut self, edit: Edit) -> EditOutcome {
        let len = self.text.chars().count();
        match edit {
            Edit::Char(c) => {
                let at = self.byte(self.cursor);
                self.text.insert(at, c);
                self.cursor += 1;
            }
            Edit::Backspace if self.cursor > 0 => {
                let at = self.byte(self.cursor - 1);
                self.text.remove(at);
                self.cursor -= 1;
            }
            Edit::Delete if self.cursor < len => {
                let at = self.byte(self.cursor);
                self.text.remove(at);
            }
            Edit::Left => self.cursor = self.cursor.saturating_sub(1),
            Edit::Right => self.cursor = (self.cursor + 1).min(len),
            Edit::Home => self.cursor = 0,
            Edit::End => self.cursor = len,
            Edit::Submit => return EditOutcome::Submitted(self.text.trim().to_owned()),
            Edit::Cancel => return EditOutcome::Cancelled,
            Edit::Backspace | Edit::Delete => {}
        }
        EditOutcome::Editing
    }
}
```

In `crates/scuttle-core/src/chat_list.rs`, add to `impl ChatList`:

```rust
    /// Re-sorts the lists after a local change to pin order or activity.
    pub fn resort(&mut self) {
        sort(&mut self.main.chats);
        sort(&mut self.archived.chats);
    }
```

In `crates/scuttle-core/src/app.rs`, add `use crate::line_edit::{Edit, EditOutcome, LineEdit};` and, after `UserRef`:

```rust
/// A `/chats` row action on one chat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatAction {
    ToggleArchive(Uuid),
    TogglePin(Uuid),
    ToggleRead(Uuid),
    Rename(Uuid),
}

/// One field of `PATCH /chats/{chat}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatChange {
    Archived(bool),
    Title(String),
    /// 0 unpins; a positive value pins, and the server picks the position.
    PinOrder(i64),
    /// `true` marks every message read; `false` makes the chat read as unread.
    Read(bool),
}

impl ChatChange {
    fn verb(&self) -> &'static str {
        match self {
            ChatChange::Archived(true) => "archive the chat",
            ChatChange::Archived(false) => "unarchive the chat",
            ChatChange::Title(_) => "rename the chat",
            ChatChange::PinOrder(0) => "unpin the chat",
            ChatChange::PinOrder(_) => "pin the chat",
            ChatChange::Read(true) => "mark the chat read",
            ChatChange::Read(false) => "mark the chat unread",
        }
    }
}

/// What the one-line editor is editing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditTarget {
    Rename(Uuid),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Editor {
    pub target: EditTarget,
    pub line: LineEdit,
    /// Set while the starting text is still on its way, such as a proposed title.
    pub loading: bool,
}
```

Add to `Msg`, after `LoadChats`:

```rust
    ChatAction(ChatAction),
    /// A key for the one-line editor.
    Edit(Edit),
    ChatUpdated {
        chat: Uuid,
        change: ChatChange,
    },
    ChatUpdateFailed {
        chat: Uuid,
        change: ChatChange,
        message: String,
    },
```

Add `UpdateChat { chat: Uuid, change: ChatChange },` to `Effect`, after `SetPlanMode`.
Add `pub editor: Option<Editor>,` to `App`, after `plan_mode`.
Add these arms to `update`:

```rust
            Msg::ChatAction(action) => self.chat_action(action),
            Msg::Edit(edit) => self.edit(edit),
            Msg::ChatUpdated { chat, change } => {
                let open = self.chat_id == Some(chat);
                match &change {
                    ChatChange::Archived(archived) => {
                        self.chats.update_copies(chat, |c| c.archived = Some(*archived));
                        if open && let Some(c) = self.chat.as_mut() {
                            c.archived = Some(*archived);
                        }
                        self.info(if *archived { "Archived." } else { "Unarchived." });
                    }
                    ChatChange::Title(title) => {
                        self.chats.update_copies(chat, |c| c.title = Some(title.clone()));
                        if open && let Some(c) = self.chat.as_mut() {
                            c.title = Some(title.clone());
                        }
                        self.info(format!("Renamed to {title}."));
                    }
                    ChatChange::PinOrder(order) => {
                        self.chats.update_copies(chat, |c| c.pin_order = Some(*order));
                        self.chats.resort();
                    }
                    ChatChange::Read(read) => {
                        self.chats.set_read(chat, *read);
                        if open && !*read {
                            self.info("The open chat reads as read again while it stays open.");
                        }
                    }
                }
                vec![]
            }
            Msg::ChatUpdateFailed {
                change, message, ..
            } => {
                self.error(format!("Could not {}: {message}", change.verb()));
                vec![]
            }
```

Add to `impl App`:

```rust
    fn chat_action(&mut self, action: ChatAction) -> Vec<Effect> {
        let id = match &action {
            ChatAction::ToggleArchive(id)
            | ChatAction::TogglePin(id)
            | ChatAction::ToggleRead(id)
            | ChatAction::Rename(id) => *id,
        };
        let open = self.chat.as_deref().filter(|c| c.id == Some(id)).cloned();
        let Some(chat) = self.chats.find(id).cloned().or(open) else {
            self.error("That chat is not loaded.");
            return vec![];
        };
        let child = chat.parent_chat_id.is_some();
        let running = self.chats.family_running(id) || (self.chat_id == Some(id) && self.is_running());
        let change = match action {
            ChatAction::ToggleArchive(_) if child => {
                self.error("Only a root chat can be archived. Archive its parent.");
                return vec![];
            }
            ChatAction::ToggleArchive(_) if chat.archived == Some(true) => ChatChange::Archived(false),
            ChatAction::ToggleArchive(_) if running => {
                self.error("Wait for this chat and its subagents to stop, then archive it.");
                return vec![];
            }
            ChatAction::ToggleArchive(_) => ChatChange::Archived(true),
            ChatAction::TogglePin(_) if child => {
                self.error("A subagent cannot be pinned.");
                return vec![];
            }
            ChatAction::TogglePin(_) => {
                ChatChange::PinOrder(if chat.pin_order.unwrap_or(0) > 0 { 0 } else { 1 })
            }
            ChatAction::ToggleRead(_) => ChatChange::Read(chat.has_unread == Some(true)),
            ChatAction::Rename(_) => {
                self.editor = Some(Editor {
                    target: EditTarget::Rename(id),
                    line: LineEdit::new(chat.title.as_deref().unwrap_or_default()),
                    loading: false,
                });
                return vec![];
            }
        };
        vec![Effect::UpdateChat { chat: id, change }]
    }

    fn edit(&mut self, edit: Edit) -> Vec<Effect> {
        let Some(editor) = self.editor.as_mut() else {
            return vec![];
        };
        if editor.loading && edit != Edit::Cancel {
            return vec![];
        }
        let outcome = editor.line.apply(edit);
        let target = editor.target.clone();
        match outcome {
            EditOutcome::Editing => vec![],
            EditOutcome::Cancelled => {
                self.editor = None;
                vec![]
            }
            EditOutcome::Submitted(text) => {
                self.editor = None;
                self.finish_edit(target, text)
            }
        }
    }

    fn finish_edit(&mut self, target: EditTarget, text: String) -> Vec<Effect> {
        match target {
            EditTarget::Rename(_) if text.is_empty() => {
                self.error("A title cannot be empty.");
                vec![]
            }
            EditTarget::Rename(chat) => vec![Effect::UpdateChat {
                chat,
                change: ChatChange::Title(text),
            }],
        }
    }
```

- [ ] **Step 4: Run the core tests to verify they pass**

Run: `cargo test -p scuttle-core`
Expected: PASS.

- [ ] **Step 5: Write the failing TUI and runtime tests**

Add to the test module of `crates/scuttle-tui/src/app.rs`:

```rust
    #[test]
    fn ctrl_a_asks_before_archiving_and_any_other_key_cancels() {
        let (mut t, _, _) = chats_tui();
        show(&mut t, vec![Effect::ShowChats(String::new())]);
        t.handle(key(KeyCode::Down, KeyModifiers::NONE));
        let ctrl_a = || key(KeyCode::Char('a'), KeyModifiers::CONTROL);
        assert!(t.handle(ctrl_a()).is_empty());
        assert!(screen(&mut t, 80, 20).contains("Press Ctrl+A again"));
        t.handle(key(KeyCode::Up, KeyModifiers::NONE));
        t.handle(key(KeyCode::Down, KeyModifiers::NONE));
        assert!(t.handle(ctrl_a()).is_empty(), "the move cancelled the first press");
        let effects = t.handle(ctrl_a());
        assert!(
            matches!(effects.as_slice(), [Effect::UpdateChat { change: scuttle_core::app::ChatChange::Archived(true), .. }]),
            "{effects:?}"
        );
    }

    #[test]
    fn ctrl_e_renames_in_the_editor_and_enter_saves() {
        let (mut t, root, _) = chats_tui();
        show(&mut t, vec![Effect::ShowChats(String::new())]);
        t.handle(key(KeyCode::Char('e'), KeyModifiers::CONTROL));
        assert!(screen(&mut t, 80, 20).contains("Rename chat"));
        for _ in 0..4 {
            t.handle(key(KeyCode::Backspace, KeyModifiers::NONE));
        }
        t.handle(key(KeyCode::Char('!'), KeyModifiers::NONE));
        let effects = t.handle(key(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(
            effects,
            vec![Effect::UpdateChat {
                chat: root,
                change: scuttle_core::app::ChatChange::Title(
                    "Fix the flaky watch reconnect !".into()
                )
            }]
        );
        assert!(t.overlay.is_some(), "the list stays open after a rename");
    }
```

Add to the test module of `crates/scuttle-tui/src/runtime.rs`:

```rust
    #[tokio::test]
    async fn chat_updates_patch_one_field_and_report_the_result() {
        use scuttle_core::app::ChatChange;
        use wiremock::matchers::body_json;
        let server = MockServer::start().await;
        let chat = Uuid::new_v4();
        Mock::given(method("PATCH"))
            .and(path(format!("/api/v2/chats/{chat}")))
            .and(body_json(serde_json::json!({"pin_order": 1})))
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;
        Mock::given(method("PATCH"))
            .and(path(format!("/api/v2/chats/{chat}")))
            .and(body_json(serde_json::json!({"archived": true})))
            .respond_with(api_error(400, "Chat archive state can only be changed on the root chat."))
            .mount(&server)
            .await;
        let (mut rt, mut rx) = runtime(&server.uri());
        rt.run(Effect::UpdateChat {
            chat,
            change: ChatChange::PinOrder(1),
        });
        assert!(matches!(
            next(&mut rx).await,
            Msg::ChatUpdated { change: ChatChange::PinOrder(1), .. }
        ));
        rt.run(Effect::UpdateChat {
            chat,
            change: ChatChange::Archived(true),
        });
        match next(&mut rx).await {
            Msg::ChatUpdateFailed { message, .. } => assert!(message.contains("root chat"), "{message}"),
            other => panic!("expected ChatUpdateFailed, got {other:?}"),
        }
    }
```

- [ ] **Step 6: Run the TUI tests to verify they fail**

Run: `cargo test -p scuttle-tui`
Expected: FAIL: the Ctrl keys do nothing in `/chats`, no editor is drawn, and the runtime ignores `UpdateChat`.

- [ ] **Step 7: Implement the keys, the editor, and the request**

In `crates/scuttle-tui/src/overlay.rs`, add `use scuttle_core::app::ChatAction;`, change the crossterm import to `use crossterm::event::{KeyCode, KeyModifiers};`, add the field below to `ChatsState` and `confirm: None,` to `Overlay::chats`:

```rust
    /// The chat a first Ctrl+A asked to archive or unarchive, waiting for the second press.
    pub confirm: Option<Uuid>,
```

Then replace the `_ => OverlayOutcome::Stay,` arm at the end of `ChatsState::chat_key` with:

```rust
            KeyCode::Char(c) if key.modifiers.contains(KeyModifiers::CONTROL) => {
                let Some(id) = selected else {
                    return OverlayOutcome::Stay;
                };
                match c {
                    'a' if self.confirm == Some(id) => {
                        self.confirm = None;
                        OverlayOutcome::Send(Msg::ChatAction(ChatAction::ToggleArchive(id)))
                    }
                    'a' => {
                        self.confirm = Some(id);
                        OverlayOutcome::Stay
                    }
                    'e' => OverlayOutcome::Send(Msg::ChatAction(ChatAction::Rename(id))),
                    'p' => OverlayOutcome::Send(Msg::ChatAction(ChatAction::TogglePin(id))),
                    'u' => OverlayOutcome::Send(Msg::ChatAction(ChatAction::ToggleRead(id))),
                    _ => OverlayOutcome::Stay,
                }
            }
            _ => OverlayOutcome::Stay,
```

At the top of `ChatsState::handle_key`, after computing `selected`, add:

```rust
        let archive = key.code == KeyCode::Char('a') && key.modifiers.contains(KeyModifiers::CONTROL);
        if !archive {
            self.confirm = None;
        }
```

In `chats_view`, make the confirmation the status line while it is pending: before the `TableView` literal, add

```rust
    let status = match state.confirm.and_then(|id| list.find(id)) {
        Some(chat) => {
            let verb = if chat.archived == Some(true) { "Unarchive" } else { "Archive" };
            let title = chat.title.clone().unwrap_or_else(|| "Untitled".into());
            Some(format!("{verb} “{title}”? Press Ctrl+A again."))
        }
        None => status,
    };
```

and extend the hint to `"Enter opens, Tab filters, Right and Left show subagents, Ctrl+A archives, Ctrl+E renames, Ctrl+P pins, Ctrl+U marks read, Esc closes"`.
In `crates/scuttle-tui/src/app.rs`, add `use scuttle_core::line_edit::Edit;`, a free function:

```rust
/// The editor key for `key`, if the one-line editor uses it.
pub(crate) fn edit_key(key: KeyEvent) -> Option<Edit> {
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        return None;
    }
    Some(match key.code {
        KeyCode::Char(c) => Edit::Char(c),
        KeyCode::Backspace => Edit::Backspace,
        KeyCode::Delete => Edit::Delete,
        KeyCode::Left => Edit::Left,
        KeyCode::Right => Edit::Right,
        KeyCode::Home => Edit::Home,
        KeyCode::End => Edit::End,
        KeyCode::Enter => Edit::Submit,
        KeyCode::Esc => Edit::Cancel,
        _ => return None,
    })
}
```

and in `key`, right after the `if self.show_help { .. }` block, add:

```rust
        // The one-line editor takes every key it uses, even over an open overlay.
        if self.core.editor.is_some() {
            return match edit_key(key) {
                Some(edit) => self.update(Msg::Edit(edit)),
                None => vec![],
            };
        }
```

Add to `impl Tui`:

```rust
    /// Draws the one-line editor as a box over the composer.
    fn draw_editor(&self, f: &mut Frame, composer: Rect) {
        let Some(editor) = self.core.editor.as_ref() else {
            return;
        };
        let title = match editor.target {
            scuttle_core::app::EditTarget::Rename(_) => " Rename chat (Enter saves, Esc cancels) ",
        };
        let text = if editor.loading {
            Line::from(Span::styled("Loading…", self.theme.dim))
        } else {
            Line::from(editor.line.text().to_owned())
        };
        let area = Rect {
            height: 3.min(composer.height.max(3)),
            ..composer
        };
        f.render_widget(Clear, area);
        f.render_widget(
            Paragraph::new(text).block(Block::default().borders(Borders::ALL).title(title)),
            area,
        );
        if !editor.loading {
            let column: usize = editor
                .line
                .text()
                .chars()
                .take(editor.line.cursor())
                .map(|c| unicode_width::UnicodeWidthChar::width(c).unwrap_or(0))
                .sum();
            f.set_cursor_position((area.x + 1 + column as u16, area.y + 1));
        }
    }
```

and call `self.draw_editor(f, composer);` at the end of `draw_at`, after the help box.
In `crates/scuttle-tui/src/runtime.rs`, add `use scuttle_core::app::ChatChange;` and this arm to `run`:

```rust
            Effect::UpdateChat { chat, change } => self.spawn(Box::pin(async move {
                let mut body = types::CodersdkUpdateChatRequest::default();
                match &change {
                    ChatChange::Archived(archived) => body.archived = Some(*archived),
                    ChatChange::Title(title) => body.title = Some(title.clone()),
                    ChatChange::PinOrder(order) => body.pin_order = Some(*order),
                    ChatChange::Read(read) => body.read = Some(*read),
                }
                match client.api().update_chat(&chat, &body).await {
                    Ok(_) => Msg::ChatUpdated { chat, change },
                    Err(e) => Msg::ChatUpdateFailed {
                        chat,
                        change,
                        message: err(e).await,
                    },
                }
            })),
```

- [ ] **Step 8: Run the tests to verify they pass**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings`
Expected: PASS.

- [ ] **Step 9: Commit**

```bash
cargo fmt --all && git add crates/scuttle-core/src/line_edit.rs crates/scuttle-core/src/lib.rs crates/scuttle-core/src/chat_list.rs crates/scuttle-core/src/app.rs crates/scuttle-tui/src/overlay.rs crates/scuttle-tui/src/app.rs crates/scuttle-tui/src/runtime.rs && git commit -m "feat: archive, rename, pin, and mark chats read from /chats

Assisted-by: AI"
```

---

### Task 10: `/chats` server search row

With filter text typed, the last row is always "Search all chats for “<query>”", which sends `q=search:"<query>"` to `GET /chats` and shows the server's results in the same overlay until the filter text changes (design section 2, "Paging and search").
The server searches titles, PR titles, and message bodies, and a value with no searchable words returns an empty list (`coder-api-gen` doc comment on `list_chats`, from `coderd/exp_chats.go:416`).

**Files:**
- Modify: `crates/scuttle-core/src/chat_list.rs` (`search_rows`, tests)
- Modify: `crates/scuttle-core/src/app.rs` (`Msg::SearchChats`, tests)
- Modify: `crates/scuttle-tui/src/table.rs` (`RowKey::SearchAll`)
- Modify: `crates/scuttle-tui/src/overlay.rs` (the search row and results)
- Modify: `crates/scuttle-tui/src/app.rs` (tests)

**Interfaces:**
- Consumes: `ListQuery::Search`, `begin_load`, `page` (Task 5); `ChatList::rows` and `chat_cells` (Task 8).
- Produces: `ChatList::search_rows(&self, query: &str) -> Option<(Vec<ChatRow>, &Load)>`; `Msg::SearchChats(String)`; `RowKey::SearchAll`.

- [ ] **Step 1: Write the failing tests**

Add to the test module of `crates/scuttle-core/src/chat_list.rs`:

```rust
    #[test]
    fn search_rows_show_only_for_the_text_that_was_searched() {
        let mut list = ChatList::default();
        assert!(list.search_rows("watch").is_none());
        list.begin_load(&ListQuery::Search("watch".into()), false);
        let (rows, load) = list.search_rows("watch").unwrap();
        assert!(rows.is_empty());
        assert_eq!(*load, Load::Loading);
        list.apply_page(
            &ListQuery::Search("watch".into()),
            0,
            vec![listed(Uuid::new_v4(), "Mentions watch in a message", "2026-09-01T00:00:00Z")],
        );
        let (rows, _) = list.search_rows("watch").unwrap();
        assert_eq!(rows[0].title, "Mentions watch in a message");
        assert!(list.search_rows("watch it").is_none(), "new text drops the results");
    }
```

Add to the test module of `crates/scuttle-tui/src/app.rs`:

```rust
    #[test]
    fn the_last_row_searches_the_server_and_shows_its_results() {
        use scuttle_core::chat_list::ListQuery;
        let (mut t, _, _) = chats_tui();
        show(&mut t, vec![Effect::ShowChats("fix watch".into())]);
        assert!(screen(&mut t, 80, 20).contains("Search all chats for “fix watch”"));
        t.handle(key(KeyCode::Down, KeyModifiers::NONE));
        let effects = t.handle(key(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(
            effects,
            vec![Effect::FetchChats {
                query: ListQuery::Search("fix watch".into()),
                offset: 0
            }]
        );
        assert!(screen(&mut t, 80, 20).contains("Searching all chats…"));
        t.update(Msg::ChatsLoaded {
            query: ListQuery::Search("fix watch".into()),
            offset: 0,
            chats: serde_json::from_value(json!([{"id": uuid::Uuid::new_v4(), "title": "Body mentions it",
                "children": [], "files": [], "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}}]))
            .unwrap(),
        });
        let shown = screen(&mut t, 80, 20);
        assert!(shown.contains("Body mentions it"), "{shown}");
        assert!(!shown.contains("Fix the flaky"), "search results replace the local matches");
        t.handle(key(KeyCode::Backspace, KeyModifiers::NONE));
        assert!(screen(&mut t, 80, 20).contains("Fix the flaky"));
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --workspace`
Expected: FAIL to compile, because `search_rows`, `Msg::SearchChats`, and `RowKey::SearchAll` do not exist.

- [ ] **Step 3: Implement the search row**

Add to `impl ChatList` in `crates/scuttle-core/src/chat_list.rs`:

```rust
    /// The server search results and their load state, while `query` is the searched text.
    pub fn search_rows(&self, query: &str) -> Option<(Vec<ChatRow>, &Load)> {
        let (searched, page) = self.search.as_ref()?;
        if searched != query {
            return None;
        }
        let rows = page
            .chats
            .iter()
            .filter_map(|c| Some(row(c, c.id?, 0)))
            .collect();
        Some((rows, &page.load))
    }
```

In `crates/scuttle-core/src/app.rs`, add `SearchChats(String),` to `Msg`, after `LoadChats`, with the doc comment `/// Runs the server's full-text search from the /chats search row.`, and the arm:

```rust
            Msg::SearchChats(text) => self.load_chats(ListQuery::Search(text), false),
```

Add `SearchAll,` to `RowKey` in `crates/scuttle-tui/src/table.rs`.
In `crates/scuttle-tui/src/overlay.rs`, replace `chats_view` with this version, which keeps Task 9's confirmation and lets search results replace the local rows while the search row stays last:

```rust
fn chats_view(state: &ChatsState, ctx: &ViewCtx) -> TableView {
    let list = &ctx.app.chats;
    let query = state.table.filter.as_str();
    let searched = list.search_rows(query);
    let chat_rows = match searched.as_ref() {
        Some((rows, _)) => rows.clone(),
        None => list.rows(state.filter, query, &state.expanded),
    };
    let mut rows: Vec<Row> = chat_rows
        .iter()
        .map(|r| Row::item(RowKey::Chat(r.id), chat_cells(r, ctx)))
        .collect();
    if !query.trim().is_empty() {
        rows.push(Row::item(
            RowKey::SearchAll,
            vec![
                Line::default(),
                Line::default(),
                Line::from(Span::styled(
                    format!("Search all chats for “{query}”"),
                    ctx.theme.accent,
                )),
            ],
        ));
    }
    let local = match list.page(&state.filter.query()).map(|p| &p.load) {
        Some(Load::Failed(message)) => Some(format!("{message} Press Tab to retry.")),
        _ if !chat_rows.is_empty() => None,
        Some(Load::Idle | Load::Loading) => Some("Loading chats…".into()),
        _ if query.is_empty() && state.filter == Filter::All => {
            Some("No chats yet. Type a message to start one.".into())
        }
        _ => Some("No chats match.".into()),
    };
    let listing = match searched.as_ref() {
        Some((_, Load::Loading)) => Some("Searching all chats…".to_owned()),
        Some((_, Load::Failed(message))) => Some(message.to_string()),
        Some((found, _)) if found.is_empty() => Some(format!("No chats match “{query}”.")),
        Some(_) => None,
        None => local,
    };
    let status = match state.confirm.and_then(|id| list.find(id)) {
        Some(chat) => {
            let verb = if chat.archived == Some(true) { "Unarchive" } else { "Archive" };
            let title = chat.title.clone().unwrap_or_else(|| "Untitled".into());
            Some(format!("{verb} “{title}”? Press Ctrl+A again."))
        }
        None => listing,
    };
    let tabs = Filter::ALL
        .iter()
        .map(|f| {
            if *f == state.filter {
                format!("[{}]", f.label())
            } else {
                f.label().to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join(" | ");
    let paused = if list.watch_live { "" } else { "   live updates paused" };
    TableView {
        title: format!("Chats   {tabs}{paused}"),
        widths: vec![
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Fill(1),
            Constraint::Length(4),
            Constraint::Length(8),
            Constraint::Length(3),
        ],
        rows,
        status,
        hint: Some(
            "Enter opens, Tab filters, Right and Left show subagents, Ctrl+A archives, Ctrl+E renames, Ctrl+P pins, Ctrl+U marks read, Esc closes".into(),
        ),
        filterable: true,
    }
}
```

In `ChatsState::handle_key`, change the `TableKey::Enter` arm to:

```rust
            TableKey::Enter => match self.table.selected_row(&view).map(|r| r.key.clone()) {
                Some(RowKey::Chat(id)) => OverlayOutcome::CloseWith(Msg::OpenChat(id)),
                Some(RowKey::SearchAll) => {
                    OverlayOutcome::Send(Msg::SearchChats(self.table.filter.trim().to_owned()))
                }
                _ => OverlayOutcome::Stay,
            },
```

and change `load_more` so the search row does not count as the last loaded row: use `view.rows.iter().rposition(|r| matches!(r.key, RowKey::Chat(_)))` for `last`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all && git add crates/scuttle-core/src/chat_list.rs crates/scuttle-core/src/app.rs crates/scuttle-tui/src/table.rs crates/scuttle-tui/src/overlay.rs crates/scuttle-tui/src/app.rs && git commit -m "feat: search every chat on the server from the last /chats row

Assisted-by: AI"
```

---

### Task 11: Name the running tool while the live turn is empty

The activity row reads "Working…" while a tool runs (M1.5 final review Minor 7).
chatd persists the assistant message with its tool calls before the tools run (`coderd/x/chatd/generation.go:206-220`), which clears the live turn, and publishes each result only when its tool finishes (`coderd/x/chatd/chatloop/chatloop.go:539-568`).
So while running with an empty live turn, the activity names the tool calls in the last durable assistant message that have no result yet (design section 17).

**Files:**
- Modify: `crates/scuttle-core/src/transcript.rs` (`unresolved_tool_calls`, tests)
- Modify: `crates/scuttle-core/src/app.rs` (`activity`, tests)

**Interfaces:**
- Consumes: nothing new.
- Produces: `Transcript::unresolved_tool_calls(&self) -> Vec<String>`.

- [ ] **Step 1: Write the failing tests**

Add to the test module of `crates/scuttle-core/src/app.rs`:

```rust
    #[test]
    fn a_running_tool_is_named_once_its_message_lands() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let messages = serde_json::from_value(json!([
            {"id": 1, "role": "user", "content": [{"type": "text", "text": "list files"}]},
            {"id": 2, "role": "assistant", "content": [
                {"type": "tool-call", "tool_call_id": "c1", "tool_name": "execute", "args": {"command": "ls"}},
                {"type": "tool-call", "tool_call_id": "c2", "tool_name": "read_file", "args": {}}
            ]}
        ]))
        .unwrap();
        app.update(Msg::ChatLoaded {
            chat: chat(Uuid::new_v4()),
            messages,
        });
        app.update(running());
        assert_eq!(
            app.activity(),
            Some(Activity::Tool("execute, read_file".into()))
        );
        app.update(ev(json!({"type": "message", "message": {"id": 3, "role": "tool", "content": [
            {"type": "tool-result", "tool_call_id": "c1", "tool_name": "execute", "result": {"output": ""}}
        ]}})));
        assert_eq!(app.activity(), Some(Activity::Tool("read_file".into())));
        app.update(ev(json!({"type": "message", "message": {"id": 4, "role": "tool", "content": [
            {"type": "tool-result", "tool_call_id": "c2", "tool_name": "read_file", "result": {}}
        ]}})));
        assert_eq!(app.activity(), Some(Activity::Working));
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p scuttle-core a_running_tool_is_named`
Expected: FAIL: the activity is `Working` while the tools run.

- [ ] **Step 3: Implement the lookup**

Add to `impl Transcript` in `crates/scuttle-core/src/transcript.rs`, with `use std::collections::HashSet;` at the top:

```rust
    /// The tool names of calls in the last assistant message that have no result yet. chatd
    /// persists that message before its tools run, so these are the tools running now.
    pub fn unresolved_tool_calls(&self) -> Vec<String> {
        let kind = |p: &types::CodersdkChatMessagePart| p.type_.as_ref().map(|t| t.as_str().to_owned());
        let Some((&last_id, last)) = self
            .messages
            .iter()
            .rev()
            .find(|(_, m)| m.role.as_ref().map(|r| r.as_str()) == Some("assistant"))
        else {
            return Vec::new();
        };
        let resolved: HashSet<&str> = self
            .messages
            .range(last_id..)
            .flat_map(|(_, m)| m.content.iter())
            .filter(|p| kind(p).as_deref() == Some("tool-result"))
            .filter_map(|p| p.tool_call_id.as_deref())
            .collect();
        last.content
            .iter()
            .filter(|p| kind(p).as_deref() == Some("tool-call"))
            .filter(|p| p.tool_call_id.as_deref().is_none_or(|id| !resolved.contains(id)))
            .map(|p| p.tool_name.clone().unwrap_or_default())
            .collect()
    }
```

In `App::activity` in `crates/scuttle-core/src/app.rs`, replace the last two arms of the inner `match self.transcript.live.blocks.last()` (`_ if self.awaiting_reply => Activity::Waiting,` and `_ => Activity::Working,`) with:

```rust
                _ if self.awaiting_reply => Activity::Waiting,
                _ => match self.transcript.unresolved_tool_calls().as_slice() {
                    [] => Activity::Working,
                    names => Activity::Tool(names.join(", ")),
                },
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all && git add crates/scuttle-core/src/transcript.rs crates/scuttle-core/src/app.rs && git commit -m "fix(scuttle-core): name the running tool after its call lands in the history

Assisted-by: AI"
```

---

### Task 12: The subagent preview stream

The `/subagents` popup streams the selected subagent live, so the author can watch it work without leaving the parent (design section 3, decision "The subagent preview").
The core keeps a second, preview-only `Transcript` fed by the unchanged reducer, and preview messages arrive tagged `ForPreview { chat, generation }`, applied only when both match the current preview.
The runtime gains a second stream slot with its own atomic, closed when the preview changes or closes; every chat switch closes it first.
Opening the preview stream marks the child read on the server (`coderd/exp_chats.go:3298-3308`), which matches the author looking at it.

**Files:**
- Modify: `crates/scuttle-core/src/app.rs` (`Preview`, `Msg`, `Effect`, `preview_chat`, `close_preview`, `apply_preview`, `reset_chat_state`, tests)
- Modify: `crates/scuttle-tui/src/runtime.rs` (`Slot`, `tag_for`, `StreamSender::wrap`, `open_stream`, `close_slot`, the preview slot, tests)

**Interfaces:**
- Consumes: `App::reset_chat_state` (Task 3); `backoff`; `Runtime::open_stream` and `StreamSender` (Task 2).
- Produces: `app::Preview { pub chat: Uuid, pub transcript: Transcript, pub error: Option<String> }` (with private generation and attempt); `App::preview: Option<Preview>`; `Msg::PreviewChat(Option<Uuid>)`; `Msg::ForPreview { chat: Uuid, generation: u64, msg: Box<Msg> }`; `Effect::OpenPreview { chat: Uuid, after_id: Option<i64>, delay: Duration, generation: u64 }`; `Effect::ClosePreview`; private `App::preview_chat(&mut self, Option<Uuid>) -> Vec<Effect>` and `App::close_preview(&mut self) -> Vec<Effect>`; runtime `enum Slot { Main, Preview }`.
  Task 13 opens and draws the preview.

- [ ] **Step 1: Write the failing core tests**

Add to the test module of `crates/scuttle-core/src/app.rs`:

```rust
    fn opened_preview(effects: &[Effect]) -> u64 {
        match effects {
            [.., Effect::OpenPreview { generation, .. }] => *generation,
            other => panic!("expected OpenPreview last, got {other:?}"),
        }
    }

    #[test]
    fn the_preview_follows_the_selection_and_closes_with_the_popup() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let first = opened_preview(&app.update(Msg::PreviewChat(Some(a))));
        let reply = json!({"type": "message", "message": {"id": 3, "role": "assistant", "content": [{"type": "text", "text": "Found 3 callers"}]}});
        app.update(Msg::ForPreview {
            chat: a,
            generation: first,
            msg: Box::new(ev(reply.clone())),
        });
        assert_eq!(app.preview.as_ref().map(|p| p.transcript.messages().count()), Some(1));
        let effects = app.update(Msg::PreviewChat(Some(b)));
        assert_eq!(effects[0], Effect::ClosePreview);
        let second = opened_preview(&effects);
        assert!(app.update(Msg::ForPreview {
            chat: a,
            generation: first,
            msg: Box::new(ev(reply)),
        })
        .is_empty());
        assert_eq!(app.preview.as_ref().map(|p| p.transcript.messages().count()), Some(0));
        assert!(second > first);
        assert_eq!(app.update(Msg::PreviewChat(None)), vec![Effect::ClosePreview]);
        assert!(app.preview.is_none());
    }

    #[test]
    fn a_failed_preview_shows_its_error_and_retries_with_backoff() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let child = Uuid::new_v4();
        let generation = opened_preview(&app.update(Msg::PreviewChat(Some(child))));
        let effects = app.update(Msg::ForPreview {
            chat: child,
            generation,
            msg: Box::new(Msg::StreamEnded {
                error: Some("connection reset".into()),
            }),
        });
        assert!(matches!(
            effects.as_slice(),
            [Effect::OpenPreview { chat, delay, generation: g, .. }] if *chat == child && *delay == backoff(1) && *g > generation
        ));
        assert_eq!(
            app.preview.as_ref().and_then(|p| p.error.as_deref()),
            Some("connection reset")
        );
    }

    #[test]
    fn switching_with_a_live_preview_drops_both_old_streams() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let (a, child, b) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        app.update(Msg::ChatLoaded {
            chat: chat(a),
            messages: vec![message(1)],
        });
        let main = app.stream_generation();
        let preview = opened_preview(&app.update(Msg::PreviewChat(Some(child))));
        let effects = app.update(Msg::OpenChat(b));
        assert!(effects.contains(&Effect::ClosePreview), "{effects:?}");
        assert!(effects.contains(&Effect::CloseStream), "{effects:?}");
        assert!(app.preview.is_none());
        let late = || {
            ev(json!({"type": "message", "message": {"id": 9, "role": "assistant", "content": [{"type": "text", "text": "late"}]}}))
        };
        let from_old_streams = |app: &mut App| {
            app.update(Msg::ForStream {
                chat: a,
                generation: main,
                msg: Box::new(late()),
            });
            app.update(Msg::ForPreview {
                chat: child,
                generation: preview,
                msg: Box::new(late()),
            });
        };
        from_old_streams(&mut app);
        app.update(Msg::ChatLoaded {
            chat: chat(b),
            messages: vec![],
        });
        assert_eq!(app.transcript.messages().count(), 0);
        // Back on A with the same subagent previewed again, the first visit's events stay out.
        app.update(Msg::OpenChat(a));
        app.update(Msg::ChatLoaded {
            chat: chat(a),
            messages: vec![message(1)],
        });
        app.update(Msg::PreviewChat(Some(child)));
        from_old_streams(&mut app);
        assert_eq!(app.transcript.messages().count(), 1);
        assert_eq!(
            app.preview.as_ref().map(|p| p.transcript.messages().count()),
            Some(0)
        );
    }
```

- [ ] **Step 2: Run the core tests to verify they fail**

Run: `cargo test -p scuttle-core`
Expected: FAIL to compile, because `Preview`, `Msg::PreviewChat`, and `Effect::OpenPreview` do not exist.

- [ ] **Step 3: Implement the preview in the core**

Add after `Editor` in `crates/scuttle-core/src/app.rs`:

```rust
/// A read-only second stream on one subagent, shown live in the `/subagents` popup.
#[derive(Debug, Default)]
pub struct Preview {
    pub chat: Uuid,
    pub transcript: Transcript,
    /// Why the preview stream last ended, shown until it is live again.
    pub error: Option<String>,
    generation: u64,
    attempt: u32,
}
```

Add to `Msg`, after `ForStream`:

```rust
    /// Previews `Some(chat)` in place of any other preview, or closes the preview.
    PreviewChat(Option<Uuid>),
    /// A message from the preview stream opened with `generation`.
    ForPreview {
        chat: Uuid,
        generation: u64,
        msg: Box<Msg>,
    },
```

Add to `Effect`, after `CloseStream`:

```rust
    OpenPreview {
        chat: Uuid,
        after_id: Option<i64>,
        delay: Duration,
        generation: u64,
    },
    ClosePreview,
```

Add `pub preview: Option<Preview>,` and `preview_generation: u64,` to `App`, after `stream_generation`.
Add these arms to `update`:

```rust
            Msg::PreviewChat(target) => self.preview_chat(target),
            Msg::ForPreview {
                chat,
                generation,
                msg,
            } => {
                let current = self
                    .preview
                    .as_ref()
                    .is_some_and(|p| p.chat == chat && p.generation == generation);
                if current {
                    self.apply_preview(*msg)
                } else {
                    vec![]
                }
            }
```

Add to `impl App`:

```rust
    fn preview_chat(&mut self, target: Option<Uuid>) -> Vec<Effect> {
        if target.is_some() && self.preview.as_ref().map(|p| p.chat) == target {
            return vec![];
        }
        let mut effects = self.close_preview();
        if let Some(chat) = target {
            self.preview_generation += 1;
            self.preview = Some(Preview {
                chat,
                generation: self.preview_generation,
                ..Preview::default()
            });
            effects.push(Effect::OpenPreview {
                chat,
                after_id: None,
                delay: Duration::ZERO,
                generation: self.preview_generation,
            });
        }
        effects
    }

    fn close_preview(&mut self) -> Vec<Effect> {
        match self.preview.take() {
            Some(_) => {
                self.preview_generation += 1;
                vec![Effect::ClosePreview]
            }
            None => vec![],
        }
    }

    /// Applies a preview stream message, reconnecting the preview with the chat stream's
    /// backoff when its stream ends or skips.
    fn apply_preview(&mut self, msg: Msg) -> Vec<Effect> {
        let Some(preview) = self.preview.as_mut() else {
            return vec![];
        };
        let delay = match msg {
            Msg::Stream(ev) => match preview.transcript.apply(&ev) {
                Applied::Reconnect(_) => {
                    preview.attempt += 1;
                    if preview.attempt == 1 {
                        Duration::ZERO
                    } else {
                        backoff(preview.attempt)
                    }
                }
                _ => {
                    preview.error = None;
                    return vec![];
                }
            },
            Msg::StreamEnded { error } => {
                preview.error = Some(error.unwrap_or_else(|| "the stream closed".into()));
                preview.attempt += 1;
                backoff(preview.attempt)
            }
            Msg::StreamHealthy => {
                preview.attempt = 0;
                return vec![];
            }
            _ => return vec![],
        };
        preview.transcript.live.clear();
        self.preview_generation += 1;
        preview.generation = self.preview_generation;
        vec![Effect::OpenPreview {
            chat: preview.chat,
            after_id: preview.transcript.last_message_id(),
            delay,
            generation: self.preview_generation,
        }]
    }
```

In `reset_chat_state`, change the first statement to:

```rust
        let mut effects = self.close_preview();
        effects.extend([self.close_stream(), Effect::ClearView]);
```

The expected effects in `opening_a_chat_closes_the_old_one_and_loads_the_new_one` stay the same, because no preview is open there.

- [ ] **Step 4: Run the core tests to verify they pass**

Run: `cargo test -p scuttle-core`
Expected: PASS.

- [ ] **Step 5: Write the failing runtime tests**

In the test module of `crates/scuttle-tui/src/runtime.rs`, add `wrap: tag_for(Slot::Main),` to the `StreamSender` literal in `a_stale_stream_sender_delivers_nothing`, and add:

```rust
    #[tokio::test]
    async fn the_preview_has_its_own_slot_and_tag() {
        let url = serve_tagged_streams().await;
        let (mut rt, mut rx) = runtime(&url);
        let (main, child) = (Uuid::new_v4(), Uuid::new_v4());
        rt.run(Effect::OpenStream {
            chat: main,
            after_id: None,
            generation: 1,
        });
        rt.run(Effect::OpenPreview {
            chat: child,
            after_id: None,
            delay: Duration::ZERO,
            generation: 4,
        });
        let (mut saw_main, mut saw_preview) = (false, false);
        while !(saw_main && saw_preview) {
            match next(&mut rx).await {
                Msg::ForStream { chat, .. } => saw_main |= chat == main,
                Msg::ForPreview {
                    chat, generation, ..
                } => saw_preview |= chat == child && generation == 4,
                other => panic!("unexpected {other:?}"),
            }
        }
        rt.run(Effect::ClosePreview);
        let later: Vec<Msg> = {
            let mut all = Vec::new();
            for _ in 0..30 {
                all.push(next(&mut rx).await);
            }
            all.split_off(10)
        };
        assert!(
            later.iter().all(|m| matches!(m, Msg::ForStream { .. })),
            "the preview stopped and the main stream kept going"
        );
    }
```

- [ ] **Step 6: Run the runtime tests to verify they fail**

Run: `cargo test -p scuttle-tui runtime`
Expected: FAIL to compile, because `Slot`, `tag_for`, and `StreamSender::wrap` do not exist.

- [ ] **Step 7: Implement the second slot**

In `crates/scuttle-tui/src/runtime.rs`, add after `STREAM_HEALTHY_AFTER`:

```rust
/// Which stream slot a chat stream task fills.
#[derive(Debug, Clone, Copy)]
enum Slot {
    Main,
    Preview,
}

/// How a slot's messages are tagged for the core.
fn tag_for(slot: Slot) -> fn(Uuid, u64, Msg) -> Msg {
    match slot {
        Slot::Main => |chat, generation, msg| Msg::ForStream {
            chat,
            generation,
            msg: Box::new(msg),
        },
        Slot::Preview => |chat, generation, msg| Msg::ForPreview {
            chat,
            generation,
            msg: Box::new(msg),
        },
    }
}
```

Add `preview: Option<JoinHandle<()>>,` and `preview_generation: Arc<AtomicU64>,` to `Runtime`, with `preview: None,` and `preview_generation: Arc::new(AtomicU64::new(0)),` in `Runtime::new`.
Add `wrap: fn(Uuid, u64, Msg) -> Msg,` to `StreamSender`, and make its send `let _ = self.tx.send((self.wrap)(self.chat, self.tag, msg));`.
Change the head of `open_stream` to take the slot, keeping the spawned body from Task 2 unchanged:

```rust
    fn open_stream(
        &mut self,
        slot: Slot,
        chat: Uuid,
        after_id: Option<i64>,
        delay: Duration,
        tag: u64,
    ) {
        let client = self.client.clone();
        let tx = self.tx.clone();
        let healthy_after = self.healthy_after;
        let (task, counter) = match slot {
            Slot::Main => (&mut self.stream, self.stream_generation.clone()),
            Slot::Preview => (&mut self.preview, self.preview_generation.clone()),
        };
        // Bump first so the old task goes quiet even if it is mid-flight on another thread.
        let mine = counter.fetch_add(1, Ordering::SeqCst) + 1;
        if let Some(old) = task.take() {
            old.abort();
        }
        let out = StreamSender {
            tx,
            generation: counter,
            mine,
            chat,
            tag,
            wrap: tag_for(slot),
        };
        *task = Some(tokio::spawn(async move {
            if !delay.is_zero() {
                tokio::time::sleep(delay + jitter()).await;
            }
            let mut stream = match client.stream_chat(chat, after_id).await {
                Ok(s) => s,
                Err(e) => {
                    out.send(Msg::StreamEnded {
                        error: Some(e.to_string()),
                    });
                    return;
                }
            };
            let healthy = tokio::time::sleep(healthy_after);
            tokio::pin!(healthy);
            let mut reported = false;
            loop {
                let item = tokio::select! {
                    item = stream.next() => item,
                    () = &mut healthy, if !reported => {
                        reported = true;
                        if !out.send(Msg::StreamHealthy) {
                            return;
                        }
                        continue;
                    }
                };
                let Some(item) = item else {
                    break;
                };
                let msg = match item {
                    Ok(ev) => Msg::Stream(ev),
                    Err(coder_sdk::Error::Decode(_)) => continue,
                    Err(e) => Msg::StreamEnded {
                        error: Some(e.to_string()),
                    },
                };
                let ended = matches!(msg, Msg::StreamEnded { .. });
                if !out.send(msg) || ended {
                    return;
                }
            }
            out.send(Msg::StreamEnded { error: None });
        }));
    }
```

The spawned body is Task 2's, with Task 6's `jitter()`.
Add after `open_stream`:

```rust
    /// Stops the stream in `slot`; the bump silences a task that is mid-send.
    fn close_slot(&mut self, slot: Slot) {
        let (task, counter) = match slot {
            Slot::Main => (&mut self.stream, &self.stream_generation),
            Slot::Preview => (&mut self.preview, &self.preview_generation),
        };
        counter.fetch_add(1, Ordering::SeqCst);
        if let Some(old) = task.take() {
            old.abort();
        }
    }
```

In `run`, route the stream effects through the slots:

```rust
            Effect::OpenStream {
                chat,
                after_id,
                generation,
            } => self.open_stream(Slot::Main, chat, after_id, Duration::ZERO, generation),
            Effect::ReconnectAfter {
                chat,
                after_id,
                delay,
                generation,
            } => self.open_stream(Slot::Main, chat, after_id, delay, generation),
            Effect::CloseStream => self.close_slot(Slot::Main),
            Effect::OpenPreview {
                chat,
                after_id,
                delay,
                generation,
            } => self.open_stream(Slot::Preview, chat, after_id, delay, generation),
            Effect::ClosePreview => self.close_slot(Slot::Preview),
```

- [ ] **Step 8: Run the tests to verify they pass**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings`
Expected: PASS.

- [ ] **Step 9: Commit**

```bash
cargo fmt --all && git add crates/scuttle-core/src/app.rs crates/scuttle-tui/src/runtime.rs && git commit -m "feat: stream a subagent preview on its own tagged slot

Assisted-by: AI"
```

---

### Task 13: `/subagents`, `/parent`, and Esc back to the parent

`/subagents` opens a full-height popup listing the open chat's subagents with status, title, and last activity, with the selected one streaming live below; when the open chat is itself a subagent, it lists its siblings and names the parent (design section 3).
Enter opens the selected subagent full screen through Task 3's switch, which closes the preview first.
Depends on M1.6: `Ctx::activity` in `transcript_view.rs` (interface 4), which the preview builder must carry.
`/parent` (alias `/back`) opens the parent, and so does Esc while the subagent is idle and the composer is empty (decision "Returning from a subagent"); Esc still interrupts a running subagent.
The children come from the list cache, which the watch keeps current, falling back to the open chat's own `children` (`codersdk/chats.go:188-193`).

**Files:**
- Modify: `crates/scuttle-core/src/commands.rs` (`Subagents`, `Parent` with `/back`, tests)
- Modify: `crates/scuttle-core/src/app.rs` (`subagents`, `can_return_to_parent`, `Effect::ShowSubagents`, `command`, tests)
- Modify: `crates/scuttle-tui/src/transcript_view.rs` (`build_transcript`, `Ctx::prefs`)
- Modify: `crates/scuttle-tui/src/overlay.rs` (`SubagentsState`, `Overlay::Subagents`, `marker` reuse)
- Modify: `crates/scuttle-tui/src/app.rs` (`ShowSubagents`, `draw_preview`, the Esc rule, Ctrl+C, tests)
- Modify: `crates/scuttle-tui/src/runtime.rs` (ignore `ShowSubagents`)

**Interfaces:**
- Consumes: `Preview`, `Msg::PreviewChat`, `App::preview_chat` (Task 12); `Msg::OpenChat`, `App::open_chat` (Task 3); `ChatList::find` (Task 5); `marker`, `ViewCtx`, `RowKey::Chat` (Tasks 7 and 8).
- Produces: `Command::Subagents`; `Command::Parent`; `Effect::ShowSubagents`; `App::subagents(&self) -> (Option<String>, Vec<types::CodersdkChat>)`; `App::can_return_to_parent(&self) -> bool`; `transcript_view::TranscriptSource<'a> { pub transcript: &'a Transcript, pub prefs: &'a DisplayPrefs, pub activity: Option<Activity> }`; `transcript_view::build_transcript(source: &TranscriptSource, overrides: &BTreeMap<String, Density>, toggles: &HashSet<BlockId>, welcome: Option<&Welcome>, theme: &Theme, width: u16) -> View`; `overlay::SubagentsState { pub table: TableState, pub scroll: usize }`; `Overlay::Subagents(SubagentsState)` with `preview_scroll(&self) -> Option<usize>`.

- [ ] **Step 1: Write the failing core tests**

In the test module of `crates/scuttle-core/src/commands.rs`, insert `"/subagents",` and `"/parent",` after `"/chats",` in `every_listed_command_parses`, and add:

```rust
    #[test]
    fn parses_subagents_and_parent_with_its_alias() {
        assert_eq!(parse("/subagents"), Ok(Command::Subagents));
        assert_eq!(parse("/parent"), Ok(Command::Parent));
        assert_eq!(parse("/back"), Ok(Command::Parent));
    }
```

Add to the test module of `crates/scuttle-core/src/app.rs`:

```rust
    fn family_of_two(app: &mut App) -> (Uuid, Uuid, Uuid) {
        let (root, a, b) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let mut family = listed(root, "Fix the watch test", "2026-09-30T10:00:00Z");
        family.children = [a, b]
            .into_iter()
            .map(|id| types::CodersdkChat {
                parent_chat_id: Some(root),
                ..listed(id, "explore", "2026-09-30T10:00:00Z")
            })
            .collect();
        loaded_list(app, vec![family]);
        (root, a, b)
    }

    #[test]
    fn subagents_lists_children_or_siblings_and_previews_the_first() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let (root, a, b) = family_of_two(&mut app);
        app.update(Msg::ChatLoaded {
            chat: chat(root),
            messages: vec![],
        });
        let effects = app.update(Msg::Command(Command::Subagents));
        assert_eq!(effects[0], Effect::ShowSubagents);
        assert!(matches!(effects[1], Effect::OpenPreview { chat, .. } if chat == a));
        let (parent, children) = app.subagents();
        assert_eq!(parent, None);
        assert_eq!(children.iter().map(|c| c.id).collect::<Vec<_>>(), [Some(a), Some(b)]);
        app.update(Msg::OpenChat(a));
        app.update(Msg::ChatLoaded {
            chat: Box::new(types::CodersdkChat {
                parent_chat_id: Some(root),
                ..*chat(a)
            }),
            messages: vec![],
        });
        let (parent, children) = app.subagents();
        assert_eq!(parent.as_deref(), Some("Fix the watch test"));
        assert_eq!(children.len(), 2, "a subagent lists its siblings");
    }

    #[test]
    fn parent_returns_from_an_idle_subagent_and_esc_only_does_so_while_idle() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let (root, a, _) = family_of_two(&mut app);
        app.update(Msg::ChatLoaded {
            chat: Box::new(types::CodersdkChat {
                parent_chat_id: Some(root),
                ..*chat(a)
            }),
            messages: vec![],
        });
        assert!(app.can_return_to_parent());
        app.update(running());
        assert!(!app.can_return_to_parent(), "Esc interrupts a running subagent");
        let effects = app.update(Msg::Command(Command::Parent));
        assert!(effects.contains(&Effect::LoadChat(root)), "{effects:?}");
        app.update(Msg::ChatLoaded {
            chat: chat(root),
            messages: vec![],
        });
        assert!(app.update(Msg::Command(Command::Parent)).is_empty());
        assert_eq!(
            app.notices.last(),
            Some(&Notice::Info("This chat is not a subagent.".into()))
        );
    }
```

- [ ] **Step 2: Run the core tests to verify they fail**

Run: `cargo test -p scuttle-core`
Expected: FAIL to compile, because the commands, `subagents`, and `Effect::ShowSubagents` do not exist.

- [ ] **Step 3: Implement the core side**

In `crates/scuttle-core/src/commands.rs`, add `Subagents,` and `Parent,` to `Command` after `Chats`, these entries after `/chats` in `COMMANDS`:

```rust
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
```

and `"subagents" => Ok(Command::Subagents),` and `"parent" => Ok(Command::Parent),` to `parse`.
In `crates/scuttle-core/src/app.rs`, add `ShowSubagents,` to `Effect` after `ShowChats`, and to `impl App`:

```rust
    /// The open chat's subagents, or for a subagent its siblings, with the parent's title when
    /// the open chat is a subagent. The watch keeps the list cache current, so it wins over
    /// the open chat's own copy.
    pub fn subagents(&self) -> (Option<String>, Vec<types::CodersdkChat>) {
        let Some(open) = self.chat.as_deref() else {
            return (None, vec![]);
        };
        match open.parent_chat_id {
            Some(parent) => {
                let root = self.chats.find(parent);
                let title = root
                    .and_then(|r| r.title.clone())
                    .unwrap_or_else(|| "the parent chat".into());
                (Some(title), root.map(|r| r.children.clone()).unwrap_or_default())
            }
            None => {
                let listed = open.id.and_then(|id| self.chats.find(id));
                let children = match listed {
                    Some(root) if !root.children.is_empty() => root.children.clone(),
                    _ => open.children.clone(),
                };
                (None, children)
            }
        }
    }

    /// Whether Esc on an empty composer returns to the parent: the open chat is an idle subagent.
    pub fn can_return_to_parent(&self) -> bool {
        self.chat.as_ref().is_some_and(|c| c.parent_chat_id.is_some()) && !self.is_running()
    }
```

Add these arms to `command`, before `Command::New`:

```rust
            Command::Subagents => {
                if self.chat_id.is_none() {
                    self.error("Start a chat first.");
                    return vec![];
                }
                let first = self.subagents().1.first().and_then(|c| c.id);
                let mut effects = vec![Effect::ShowSubagents];
                effects.extend(self.preview_chat(first));
                effects
            }
            Command::Parent => match self.chat.as_ref().and_then(|c| c.parent_chat_id) {
                Some(parent) => self.open_chat(parent),
                None => {
                    self.info("This chat is not a subagent.");
                    vec![]
                }
            },
```

- [ ] **Step 4: Run the core tests to verify they pass**

Run: `cargo test -p scuttle-core`
Expected: PASS.

- [ ] **Step 5: Write the failing TUI tests**

Add to the test module of `crates/scuttle-tui/src/app.rs`:

```rust
    fn subagent_tui() -> (Tui, uuid::Uuid, uuid::Uuid) {
        let (mut t, root, child) = chats_tui();
        t.update(Msg::ChatLoaded {
            chat: Box::new(serde_json::from_value(json!({"id": root, "title": "Fix the flaky watch reconnect test",
                "children": [], "files": [], "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}})).unwrap()),
            messages: vec![],
        });
        (t, root, child)
    }

    #[test]
    fn slash_subagents_previews_the_selection_live_and_enter_opens_it() {
        let (mut t, _, child) = subagent_tui();
        let effects = t.update(Msg::Submit("/subagents".into()));
        let generation = match effects.as_slice() {
            [Effect::ShowSubagents, Effect::OpenPreview { chat, generation, .. }] if *chat == child => *generation,
            other => panic!("{other:?}"),
        };
        show(&mut t, effects);
        let shown = screen(&mut t, 80, 24);
        assert!(shown.contains("explore"), "{shown}");
        assert!(shown.contains("Connecting…"), "{shown}");
        t.update(Msg::ForPreview {
            chat: child,
            generation,
            msg: Box::new(stream(json!({"type": "message", "message": {"id": 3, "role": "assistant",
                "content": [{"type": "text", "text": "Found 3 callers"}]}}))),
        });
        assert!(screen(&mut t, 80, 24).contains("Found 3 callers"));
        let effects = t.handle(key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(t.overlay.is_none());
        assert!(effects.contains(&Effect::ClosePreview), "{effects:?}");
        assert!(effects.contains(&Effect::LoadChat(child)), "{effects:?}");
    }

    #[test]
    fn esc_on_an_idle_subagent_with_an_empty_composer_opens_the_parent() {
        let (mut t, root, child) = chats_tui();
        t.update(Msg::ChatLoaded {
            chat: Box::new(serde_json::from_value(json!({"id": child, "parent_chat_id": root,
                "children": [], "files": [], "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}})).unwrap()),
            messages: vec![],
        });
        t.composer.set_text("draft");
        let effects = t.handle(key(KeyCode::Esc, KeyModifiers::NONE));
        assert!(!effects.contains(&Effect::LoadChat(root)), "a draft keeps Esc as interrupt");
        t.composer.set_text("");
        let effects = t.handle(key(KeyCode::Esc, KeyModifiers::NONE));
        assert!(effects.contains(&Effect::LoadChat(root)), "{effects:?}");
    }
```

- [ ] **Step 6: Run the TUI tests to verify they fail**

Run: `cargo test -p scuttle-tui`
Expected: FAIL: `ShowSubagents` opens nothing, and Esc only interrupts.

- [ ] **Step 7: Build a transcript without the whole app**

In `crates/scuttle-tui/src/transcript_view.rs`, with `use scuttle_core::density::DisplayPrefs;`, `use scuttle_core::app::Activity;` (if M1.6 did not already import it), and `use scuttle_core::transcript::Transcript;`, add:

```rust
/// What a transcript is built from, so the subagent preview can build one without the app.
pub struct TranscriptSource<'a> {
    pub transcript: &'a Transcript,
    pub prefs: &'a DisplayPrefs,
    /// What the agent is doing, which decides whether markers animate; `None` keeps them still.
    pub activity: Option<Activity>,
}
```

Change `Ctx`'s field `app: &'c App` to `prefs: &'c DisplayPrefs`, replace both `&ctx.app.prefs` in `render_items` with `ctx.prefs`, and replace the head of `build` with:

```rust
pub fn build(
    app: &App,
    overrides: &BTreeMap<String, Density>,
    toggles: &HashSet<BlockId>,
    welcome: &Welcome,
    theme: &Theme,
    width: u16,
) -> View {
    let source = TranscriptSource {
        transcript: &app.transcript,
        prefs: &app.prefs,
        activity: app.activity(),
    };
    build_transcript(&source, overrides, toggles, welcome.show.then_some(welcome), theme, width)
}

/// The lines of a transcript at `width`, with `welcome` on a blank transcript.
pub fn build_transcript(
    source: &TranscriptSource,
    overrides: &BTreeMap<String, Density>,
    toggles: &HashSet<BlockId>,
    welcome: Option<&Welcome>,
    theme: &Theme,
    width: u16,
) -> View {
```

The body that follows is the old body of `build`, with three changes: every `app.transcript` becomes `source.transcript`; `if welcome.show { out.push(welcome_lines(welcome, theme), false); }` becomes `if let Some(welcome) = welcome { out.push(welcome_lines(welcome, theme), false); }`; and the `Ctx` literal takes `prefs: source.prefs,` and `activity: source.activity.clone(),` in place of `app,` and `activity: app.activity(),`.
The three transcript snapshots must not change.

- [ ] **Step 8: Implement the popup, the Esc rule, and the effect**

In `crates/scuttle-tui/src/overlay.rs`, add `use scuttle_core::chat_list::chat_status;` and:

```rust
/// The `/subagents` popup: the list, and how far the preview below it is scrolled up.
pub struct SubagentsState {
    pub table: TableState,
    /// Lines scrolled up from the bottom of the preview.
    pub scroll: usize,
}

fn subagents_view(ctx: &ViewCtx) -> TableView {
    let (parent, children) = ctx.app.subagents();
    let rows: Vec<Row> = children
        .iter()
        .filter_map(|c| {
            let status = chat_status(c);
            let when = c
                .updated_at
                .map(|t| scuttle_core::time::relative(t.timestamp(), ctx.now_unix))
                .unwrap_or_default();
            Some(Row::item(
                RowKey::Chat(c.id?),
                vec![
                    Line::from(Span::styled(marker(status.as_ref(), ctx.elapsed), ctx.theme.accent)),
                    Line::from(c.title.clone().unwrap_or_else(|| "Untitled".into())),
                    Line::from(Span::styled(when, ctx.theme.dim)),
                ],
            ))
        })
        .collect();
    TableView {
        title: match parent {
            Some(parent) => format!("Subagents of “{parent}”"),
            None => "Subagents".into(),
        },
        widths: vec![Constraint::Length(1), Constraint::Fill(1), Constraint::Length(3)],
        status: rows
            .is_empty()
            .then(|| "This chat has no subagents.".to_owned()),
        rows,
        hint: Some("Up and Down preview, Enter opens, PageUp and PageDown scroll, Esc closes".into()),
        filterable: false,
    }
}

impl SubagentsState {
    fn handle_key(&mut self, key: KeyEvent, ctx: &ViewCtx) -> OverlayOutcome {
        match key.code {
            KeyCode::PageUp => {
                self.scroll += 10;
                return OverlayOutcome::Stay;
            }
            KeyCode::PageDown => {
                self.scroll = self.scroll.saturating_sub(10);
                return OverlayOutcome::Stay;
            }
            _ => {}
        }
        let view = subagents_view(ctx);
        let before = self.table.selected_row(&view).map(|r| r.key.clone());
        match self.table.handle_key(key, &view) {
            TableKey::Esc => OverlayOutcome::CloseWith(Msg::PreviewChat(None)),
            TableKey::Enter => match before {
                Some(RowKey::Chat(id)) => OverlayOutcome::CloseWith(Msg::OpenChat(id)),
                _ => OverlayOutcome::Stay,
            },
            TableKey::Handled | TableKey::Unhandled => {
                match self.table.selected_row(&view).map(|r| r.key.clone()) {
                    Some(RowKey::Chat(id)) if before != Some(RowKey::Chat(id)) => {
                        self.scroll = 0;
                        OverlayOutcome::Send(Msg::PreviewChat(Some(id)))
                    }
                    _ => OverlayOutcome::Stay,
                }
            }
        }
    }
}
```

Add `Subagents(SubagentsState),` to `Overlay` and extend its methods: `full_height` is true for `Chats` and `Subagents`; `animates` adds `Overlay::Subagents(_) => app.subagents().1.iter().any(|c| matches!(chat_status(c), Some(ChatStatus::Running | ChatStatus::Interrupting)))` (turn the body into a `match`); `state` and `state_mut` add `Overlay::Subagents(s) => &s.table` and `&mut s.table`; `view` adds `Overlay::Subagents(_) => subagents_view(ctx),`; `handle_key` gains `if let Overlay::Subagents(s) = self { return s.handle_key(key, ctx); }` next to the `Chats` one; and add:

```rust
    /// How far the preview is scrolled, for the overlays that show one.
    pub fn preview_scroll(&self) -> Option<usize> {
        match self {
            Overlay::Subagents(s) => Some(s.scroll),
            _ => None,
        }
    }
```

In `crates/scuttle-tui/src/app.rs`, add to `apply_ui_effect`:

```rust
            Effect::ShowSubagents => {
                self.overlay = Some(Overlay::Subagents(crate::overlay::SubagentsState {
                    table: crate::table::TableState::default(),
                    scroll: 0,
                }));
            }
```

In `draw_at`, replace the `table::render(..)` call inside the overlay block with a split when the overlay has a preview:

```rust
            let area = Rect {
                y: transcript.y + transcript.height - h,
                height: h,
                ..transcript
            };
            match overlay.preview_scroll() {
                Some(scroll) => {
                    let list = (view.rows.len() as u16 + 4).clamp(5, (area.height / 2).max(5));
                    let [top, bottom] =
                        Layout::vertical([Constraint::Length(list), Constraint::Min(1)]).areas(area);
                    table::render(f, top, &view, overlay.state(), &self.theme);
                    self.draw_preview(f, bottom, scroll);
                }
                None => table::render(f, area, &view, overlay.state(), &self.theme),
            }
```

and add to `impl Tui`:

```rust
    /// Draws the subagent preview, newest lines at the bottom, `scroll` lines up from there.
    fn draw_preview(&self, f: &mut Frame, area: Rect, scroll: usize) {
        f.render_widget(Clear, area);
        let block = Block::default().borders(Borders::ALL).title(" Preview ");
        let inner = block.inner(area);
        f.render_widget(block, area);
        let Some(preview) = self.core.preview.as_ref() else {
            return;
        };
        let mut lines = Vec::new();
        if let Some(error) = preview.error.as_ref() {
            lines.push(Line::from(Span::styled(
                format!("The preview stream failed: {error}. Retrying."),
                self.theme.error,
            )));
        }
        let source = transcript_view::TranscriptSource {
            transcript: &preview.transcript,
            prefs: &self.core.prefs,
            activity: None,
        };
        let view = transcript_view::build_transcript(
            &source,
            &self.config.density,
            &HashSet::new(),
            None,
            &self.theme,
            inner.width,
        );
        if view.lines.is_empty() && preview.error.is_none() {
            lines.push(Line::from(Span::styled("Connecting…", self.theme.dim)));
        }
        lines.extend(view.lines);
        let height = inner.height as usize;
        let top = lines.len().saturating_sub(height + scroll);
        let shown: Vec<Line> = lines.into_iter().skip(top).take(height).collect();
        f.render_widget(Paragraph::new(shown), inner);
    }
```

In `key`, replace `ComposerAction::Interrupt => self.update(Msg::Interrupt),` with:

```rust
            ComposerAction::Interrupt
                if self.composer.text().trim().is_empty() && self.core.can_return_to_parent() =>
            {
                self.update(Msg::Command(scuttle_core::commands::Command::Parent))
            }
            ComposerAction::Interrupt => self.update(Msg::Interrupt),
```

In the Ctrl+C branch of `key`, close the preview with its popup: replace `self.overlay = None;` there with

```rust
            let previewing = matches!(self.overlay, Some(Overlay::Subagents(_)));
            self.overlay = None;
```

and its final `return vec![];` with `return if previewing { self.update(Msg::PreviewChat(None)) } else { vec![] };`.
In `crates/scuttle-tui/src/runtime.rs`, add `| Effect::ShowSubagents` to the ignored UI effects.

- [ ] **Step 9: Run the tests to verify they pass**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings`
Expected: PASS, with the transcript snapshots unchanged.

- [ ] **Step 10: Commit**

```bash
cargo fmt --all && git add crates/scuttle-core/src/commands.rs crates/scuttle-core/src/app.rs crates/scuttle-tui/src/transcript_view.rs crates/scuttle-tui/src/overlay.rs crates/scuttle-tui/src/app.rs crates/scuttle-tui/src/runtime.rs && git commit -m "feat: watch and open subagents with /subagents and return with /parent or Esc

Assisted-by: AI"
```

---

### Task 14: `/model` grouped by provider

`/model` shows a table grouped by provider with a fuzzy filter across the model name, display name, and provider name; each row shows the display name, a `current` or `default` tag, and the context window, and a provider that cannot be used shows why in its header and its models are listed but not selectable (design section 9).
`GET /organizations/{org}/chats/models` returns `models` and `providers` (`codersdk/chats.go:2355-2359`), which M1 fetched and partly discarded; grouping joins `models[].ai_provider_id` (`:1431`) to `providers[].id`, the header uses `providers[].display_name` (`:2338-2351`), the context window is `context_limit` (`:1436`), and an unusable provider has `available: false` with `unavailable_reason` `missing_api_key`, `fetch_failed`, or `user_api_key_required` (`:873-880`).
`unsupported_providers` (`:884-889`) becomes a dim status line.

**Files:**
- Modify: `crates/scuttle-core/src/usage.rs` (`short_tokens`, tests)
- Modify: `crates/scuttle-core/src/app.rs` (`providers`, `unsupported_providers`, `Msg::CatalogLoaded`, `ModelGroup`, `ModelRow`, `model_groups`, `load_lists_for`, tests)
- Modify: `crates/scuttle-tui/src/table.rs` (`RowKey::None`, `RowKind::Header`, `Row::header`, tests)
- Modify: `crates/scuttle-tui/src/overlay.rs` (`model_view`, tests)
- Modify: `crates/scuttle-tui/src/runtime.rs` (`FetchModels`, tests)

**Interfaces:**
- Consumes: `fuzzy::rank` (Task 7); `RowKind`, `Row` (Task 7).
- Produces in `table`: `RowKey::None`; `RowKind::Header`; `Row::header(cells: Vec<Line<'static>>) -> Row`.
- Produces: `usage::short_tokens(n: i64) -> String`; `App::providers: Vec<types::CodersdkChatModelProviderDescriptor>`; `App::unsupported_providers: Vec<types::CodersdkChatUnsupportedProvider>`; `Msg::CatalogLoaded(Box<types::CodersdkOrganizationChatModelsResponse>)`; `app::ModelGroup { pub provider_id: Option<Uuid>, pub provider: String, pub reason: Option<String>, pub models: Vec<ModelRow> }`; `app::ModelRow { pub id: Uuid, pub name: String, pub current: bool, pub default: bool, pub context: Option<String>, pub usable: bool }`; `App::model_groups(&self, query: &str) -> Vec<ModelGroup>`.

- [ ] **Step 1: Write the failing core tests**

Add to the test module of `crates/scuttle-core/src/usage.rs`:

```rust
    #[test]
    fn context_windows_read_short() {
        assert_eq!(short_tokens(200_000), "200k");
        assert_eq!(short_tokens(128_000), "128k");
        assert_eq!(short_tokens(32_768), "33k");
        assert_eq!(short_tokens(1_000_000), "1M");
        assert_eq!(short_tokens(1_500_000), "1.5M");
        assert_eq!(short_tokens(999), "999");
    }
```

Add to the test module of `crates/scuttle-core/src/app.rs`:

```rust
    #[test]
    fn models_group_by_provider_with_reasons_tags_and_context() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let org = started(&mut app);
        let (anthropic, openai) = (Uuid::new_v4(), Uuid::new_v4());
        let (sonnet, long, gpt) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let catalog = serde_json::from_value(json!({
            "models": [
                {"id": sonnet, "display_name": "Claude Sonnet", "ai_provider_id": anthropic, "enabled": true, "context_limit": 200000, "reasoning_efforts": []},
                {"id": long, "display_name": "Claude Sonnet (extended)", "ai_provider_id": anthropic, "enabled": true, "is_default": true, "context_limit": 1000000, "reasoning_efforts": []},
                {"id": gpt, "display_name": "GPT-5", "ai_provider_id": openai, "enabled": true, "context_limit": 400000, "reasoning_efforts": []}
            ],
            "providers": [
                {"id": anthropic, "display_name": "Anthropic", "available": true},
                {"id": openai, "display_name": "OpenAI", "available": false, "unavailable_reason": "user_api_key_required"}
            ],
            "unsupported_providers": [{"display_name": "Copilot", "provider": "copilot"}]
        }))
        .unwrap();
        app.update(Msg::ForOrg {
            org,
            msg: Box::new(Msg::CatalogLoaded(Box::new(catalog))),
        });
        app.update(Msg::ModelChosen(sonnet));
        let groups = app.model_groups("");
        let names: Vec<&str> = groups.iter().map(|g| g.provider.as_str()).collect();
        assert_eq!(names, ["Anthropic", "OpenAI"]);
        assert_eq!(groups[0].models[0].name, "Claude Sonnet");
        assert!(groups[0].models[0].current);
        assert!(groups[0].models[1].default);
        assert_eq!(groups[0].models[1].context.as_deref(), Some("1M"));
        assert_eq!(groups[1].reason.as_deref(), Some("needs your API key"));
        assert!(!groups[1].models[0].usable);
        assert_eq!(app.unsupported_providers.len(), 1);
        let filtered = app.model_groups("openai");
        assert_eq!(filtered.len(), 1, "the provider name is searchable");
        assert_eq!(filtered[0].models[0].name, "GPT-5");
    }
```

- [ ] **Step 2: Run the core tests to verify they fail**

Run: `cargo test -p scuttle-core`
Expected: FAIL to compile, because `short_tokens`, `Msg::CatalogLoaded`, and `model_groups` do not exist.

- [ ] **Step 3: Implement the grouping**

Add to `crates/scuttle-core/src/usage.rs`, after `format_tokens`:

```rust
/// A context window as the model table shows it: `200k`, `1M`, `1.5M`.
pub fn short_tokens(n: i64) -> String {
    if n >= 1_000_000 {
        let millions = format!("{:.1}", n as f64 / 1_000_000.0);
        format!("{}M", millions.trim_end_matches(".0"))
    } else if n >= 1_000 {
        format!("{}k", (n + 500) / 1_000)
    } else {
        n.to_string()
    }
}
```

In `crates/scuttle-core/src/app.rs`, add after `Preview`:

```rust
/// One provider's models in the `/model` table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelGroup {
    pub provider_id: Option<Uuid>,
    pub provider: String,
    /// Why the provider cannot be used, when it cannot.
    pub reason: Option<String>,
    pub models: Vec<ModelRow>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelRow {
    pub id: Uuid,
    pub name: String,
    pub current: bool,
    pub default: bool,
    pub context: Option<String>,
    pub usable: bool,
}

fn provider_reason(reason: Option<&str>) -> String {
    match reason {
        Some("missing_api_key") => "no API key is configured".into(),
        Some("user_api_key_required") => "needs your API key".into(),
        Some("fetch_failed") => "its models could not be loaded".into(),
        Some(other) if !other.is_empty() => other.replace('_', " "),
        _ => "unavailable".into(),
    }
}
```

Add to `Msg`, after `ModelsLoaded`:

```rust
    /// The whole model list response: models, providers, and unsupported providers.
    CatalogLoaded(Box<types::CodersdkOrganizationChatModelsResponse>),
```

Add `pub providers: Vec<types::CodersdkChatModelProviderDescriptor>,` and `pub unsupported_providers: Vec<types::CodersdkChatUnsupportedProvider>,` to `App`, after `models`.
In `load_lists_for`, after `self.models.clear();`, add `self.providers.clear();` and `self.unsupported_providers.clear();`.
Add the arm:

```rust
            Msg::CatalogLoaded(catalog) => {
                let catalog = *catalog;
                self.providers = catalog.providers;
                self.unsupported_providers = catalog.unsupported_providers;
                self.update(Msg::ModelsLoaded(catalog.models))
            }
```

and to `impl App`:

```rust
    /// The `/model` table: models ranked by `query` over their names and provider, grouped by
    /// provider in the order the best match of each appears.
    pub fn model_groups(&self, query: &str) -> Vec<ModelGroup> {
        let current = self.current_model().and_then(|m| m.id);
        let provider_of = |m: &types::CodersdkChatModel| {
            m.ai_provider_id
                .and_then(|id| self.providers.iter().find(|p| p.id == Some(id)))
        };
        let models: Vec<&types::CodersdkChatModel> =
            self.models.iter().filter(|m| m.id.is_some()).collect();
        let ranked = crate::fuzzy::rank(query, models, |m| {
            format!(
                "{} {} {}",
                m.display_name.as_deref().unwrap_or_default(),
                m.model.as_deref().unwrap_or_default(),
                provider_of(m).and_then(|p| p.display_name.as_deref()).unwrap_or_default()
            )
        });
        let mut groups: Vec<ModelGroup> = Vec::new();
        for m in ranked {
            let provider = provider_of(m);
            let reason = provider
                .filter(|p| p.available == Some(false))
                .map(|p| provider_reason(p.unavailable_reason.as_ref().map(|r| r.as_str())));
            let row = ModelRow {
                id: m.id.unwrap_or_default(),
                name: m
                    .display_name
                    .clone()
                    .or_else(|| m.model.clone())
                    .unwrap_or_default(),
                current: m.id == current,
                default: m.is_default == Some(true),
                context: m.context_limit.map(crate::usage::short_tokens),
                usable: reason.is_none(),
            };
            let key = provider.and_then(|p| p.id);
            match groups.iter_mut().find(|g| g.provider_id == key) {
                Some(group) => group.models.push(row),
                None => groups.push(ModelGroup {
                    provider_id: key,
                    provider: provider
                        .and_then(|p| p.display_name.clone())
                        .filter(|n| !n.is_empty())
                        .unwrap_or_else(|| "Other".into()),
                    reason,
                    models: vec![row],
                }),
            }
        }
        groups
    }
```

- [ ] **Step 4: Run the core tests to verify they pass**

Run: `cargo test -p scuttle-core`
Expected: PASS.

- [ ] **Step 5: Write the failing TUI and runtime tests**

In the test module of `crates/scuttle-tui/src/overlay.rs`, change the assertion in `disabled_models_are_not_offered` to `assert_eq!(view.rows.len(), 2, "the Other header and A; a disabled model is not offered");`, and add:

```rust
    #[test]
    fn the_model_table_groups_by_provider_and_cannot_pick_an_unusable_model() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let (anthropic, openai, sonnet, gpt) = (
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
        );
        app.update(Msg::CatalogLoaded(Box::new(serde_json::from_value(json!({
            "models": [
                {"id": gpt, "display_name": "GPT-5", "ai_provider_id": openai, "enabled": true, "reasoning_efforts": []},
                {"id": sonnet, "display_name": "Claude Sonnet", "ai_provider_id": anthropic, "enabled": true, "is_default": true, "context_limit": 200000, "reasoning_efforts": []}
            ],
            "providers": [
                {"id": openai, "display_name": "OpenAI", "available": false, "unavailable_reason": "missing_api_key"},
                {"id": anthropic, "display_name": "Anthropic", "available": true}
            ],
            "unsupported_providers": []
        })).unwrap())));
        let theme = Theme::terminal(true);
        let ctx = ViewCtx {
            app: &app,
            theme: &theme,
            now_unix: 0,
            elapsed: Duration::ZERO,
        };
        let mut o = Overlay::open(Picker::Model, &app);
        let view = o.view(&ctx);
        let text: Vec<String> = view
            .rows
            .iter()
            .map(|r| r.cells.iter().map(|c| c.to_string()).collect::<Vec<_>>().join("|"))
            .collect();
        assert_eq!(
            text,
            [
                "OpenAI|no API key is configured",
                "  GPT-5||",
                "Anthropic|",
                "  Claude Sonnet|current|200k"
            ]
        );
        assert!(
            matches!(o.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &ctx),
                OverlayOutcome::CloseWith(Msg::ModelChosen(id)) if id == sonnet),
            "the unusable model is skipped, and the default model is the current one"
        );
    }
```

Add to the test module of `crates/scuttle-tui/src/runtime.rs`:

```rust
    #[tokio::test]
    async fn the_model_catalog_keeps_its_providers() {
        let server = MockServer::start().await;
        let org = Uuid::new_v4();
        Mock::given(method("GET"))
            .and(path(format!("/api/v2/organizations/{org}/chats/models")))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "models": [], "providers": [{"id": Uuid::new_v4(), "display_name": "Anthropic"}],
                "unsupported_providers": []
            })))
            .mount(&server)
            .await;
        let (mut rt, mut rx) = runtime(&server.uri());
        rt.run(Effect::FetchModels(org));
        match next(&mut rx).await {
            Msg::ForOrg { msg, .. } => {
                assert!(matches!(*msg, Msg::CatalogLoaded(ref c) if c.providers.len() == 1), "{msg:?}")
            }
            other => panic!("expected a tagged catalog, got {other:?}"),
        }
    }
```

- [ ] **Step 6: Run the TUI tests to verify they fail**

Run: `cargo test -p scuttle-tui`
Expected: FAIL: the model table is flat, and the runtime still sends `ModelsLoaded`.

- [ ] **Step 7: Implement the table and the catalog reply**

In `crates/scuttle-tui/src/table.rs`, add `None,` to `RowKey` with the doc comment `/// A row that stands for nothing, such as a group header.`, add `Header,` to `RowKind` with the doc comment `/// A group title, never selected.`, draw it with `RowKind::Header => theme.accent,` in `render`, and add to `impl Row`:

```rust
    pub fn header(cells: Vec<Line<'static>>) -> Row {
        Row {
            key: RowKey::None,
            cells,
            kind: RowKind::Header,
        }
    }
```

Add this test to its test module:

```rust
    #[test]
    fn moving_skips_group_headers() {
        let a = Uuid::new_v4();
        let v = view(vec![
            Row::header(vec![Line::from("Anthropic")]),
            Row::item(RowKey::Model(a), vec![Line::from("A")]),
        ]);
        let mut s = TableState::default();
        press(&mut s, &v, KeyCode::Up);
        assert_eq!(selected(&s, &v), Some(RowKey::Model(a)));
    }
```

In `crates/scuttle-tui/src/overlay.rs`, replace `model_view` with:

```rust
fn model_view(app: &App, filter: &str) -> TableView {
    let mut rows = Vec::new();
    for group in app.model_groups(filter) {
        rows.push(Row::header(vec![
            Line::from(group.provider.clone()),
            Line::from(group.reason.clone().unwrap_or_default()),
        ]));
        for m in group.models {
            let tag = if m.current {
                "current"
            } else if m.default {
                "default"
            } else {
                ""
            };
            let cells = vec![
                Line::from(format!("  {}", m.name)),
                Line::from(tag),
                Line::from(m.context.clone().unwrap_or_default()),
            ];
            rows.push(if m.usable {
                Row::item(RowKey::Model(m.id), cells)
            } else {
                Row::disabled(RowKey::Model(m.id), cells)
            });
        }
    }
    let unsupported: Vec<String> = app
        .unsupported_providers
        .iter()
        .filter_map(|p| p.display_name.clone().or_else(|| p.provider.clone()))
        .collect();
    let status = if rows.is_empty() {
        Some("No models match.".to_owned())
    } else if !unsupported.is_empty() {
        Some(format!("Configured but not usable here: {}", unsupported.join(", ")))
    } else {
        None
    };
    TableView {
        title: "Model".into(),
        widths: vec![Constraint::Fill(1), Constraint::Length(8), Constraint::Length(5)],
        rows,
        status,
        hint: None,
        filterable: true,
    }
}
```

Delete `model_label`, which nothing uses now.
In `crates/scuttle-tui/src/runtime.rs`, in the `Effect::FetchModels` arm, replace `Ok(r) => Msg::ModelsLoaded(r.into_inner().models),` with `Ok(r) => Msg::CatalogLoaded(Box::new(r.into_inner())),`.

- [ ] **Step 8: Run the tests to verify they pass**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings`
Expected: PASS.

- [ ] **Step 9: Commit**

```bash
cargo fmt --all && git add crates/scuttle-core/src/usage.rs crates/scuttle-core/src/app.rs crates/scuttle-tui/src/table.rs crates/scuttle-tui/src/overlay.rs crates/scuttle-tui/src/runtime.rs && git commit -m "feat: group /model by provider with context windows and unusable providers

Assisted-by: AI"
```

---

### Task 15: `/title`

`/title <text>` renames the open chat, and `/title` alone asks the server for a proposed title, puts it in the one-line editor over the composer, and saves on Enter or cancels on Esc (design section 5).
Renaming is `PATCH /chats/{chat}` with `title` (`codersdk/chats.go:711`), which publishes `title_change` (`coderd/exp_chats.go:2282-2295`); proposing is `POST /chats/{chat}/title/propose` (`coderd/chat_routes.go:87`, `coderd/exp_chats.go:3722-3732`), which returns `{ title }` without saving it, and a timeout becomes `504` (`coderd/exp_chats.go:187`).

**Files:**
- Modify: `crates/scuttle-core/src/commands.rs` (`Title`, tests)
- Modify: `crates/scuttle-core/src/app.rs` (`EditTarget::Title`, `Msg`, `Effect::ProposeTitle`, `command`, `finish_edit`, tests)
- Modify: `crates/scuttle-tui/src/app.rs` (`draw_editor` title and loading text, tests)
- Modify: `crates/scuttle-tui/src/runtime.rs` (`ProposeTitle`, tests)

**Interfaces:**
- Consumes: `Editor`, `EditTarget`, `LineEdit`, `Effect::UpdateChat`, `ChatChange::Title` (Task 9).
- Produces: `Command::Title(Option<String>)`; `EditTarget::Title(Uuid)`; `Effect::ProposeTitle(Uuid)`; `Msg::TitleProposed(String)` and `Msg::TitleProposeFailed(String)`, both wrapped in `ForChat` by the runtime.

- [ ] **Step 1: Write the failing core tests**

In the test module of `crates/scuttle-core/src/commands.rs`, insert `"/title",` after `"/plan-mode",` in `every_listed_command_parses`, and add:

```rust
    #[test]
    fn parses_title() {
        assert_eq!(parse("/title"), Ok(Command::Title(None)));
        assert_eq!(
            parse("/title Fix the watch test"),
            Ok(Command::Title(Some("Fix the watch test".into())))
        );
    }
```

Add to the test module of `crates/scuttle-core/src/app.rs`:

```rust
    #[test]
    fn title_renames_directly_or_proposes_one_to_confirm() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        assert!(app.update(Msg::Command(Command::Title(None))).is_empty());
        assert_eq!(
            app.notices.last(),
            Some(&Notice::Error("Start a chat first.".into()))
        );
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            chat: chat(id),
            messages: vec![],
        });
        assert_eq!(
            app.update(Msg::Command(Command::Title(Some("Watch fix".into())))),
            vec![Effect::UpdateChat {
                chat: id,
                change: ChatChange::Title("Watch fix".into())
            }]
        );
        assert_eq!(
            app.update(Msg::Command(Command::Title(None))),
            vec![Effect::ProposeTitle(id)]
        );
        assert!(app.editor.as_ref().is_some_and(|e| e.loading));
        assert!(app.update(Msg::Edit(Edit::Char('x'))).is_empty(), "no typing while it loads");
        app.update(Msg::ForChat {
            chat: id,
            msg: Box::new(Msg::TitleProposed("Fix the flaky watch test".into())),
        });
        assert_eq!(
            app.editor.as_ref().map(|e| (e.loading, e.line.text())),
            Some((false, "Fix the flaky watch test"))
        );
        assert_eq!(
            app.update(Msg::Edit(Edit::Submit)),
            vec![Effect::UpdateChat {
                chat: id,
                change: ChatChange::Title("Fix the flaky watch test".into())
            }]
        );
    }

    #[test]
    fn a_failed_proposal_closes_the_editor_and_keeps_the_title() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            chat: chat(id),
            messages: vec![],
        });
        app.update(Msg::Command(Command::Title(None)));
        app.update(Msg::ForChat {
            chat: id,
            msg: Box::new(Msg::TitleProposeFailed("Title generation timed out.".into())),
        });
        assert!(app.editor.is_none());
        assert_eq!(app.chat.as_ref().and_then(|c| c.title.as_deref()), Some("t"));
        assert_eq!(
            app.notices.last(),
            Some(&Notice::Error(
                "Could not propose a title: Title generation timed out.".into()
            ))
        );
    }
```

- [ ] **Step 2: Run the core tests to verify they fail**

Run: `cargo test -p scuttle-core`
Expected: FAIL to compile, because `Command::Title`, `Effect::ProposeTitle`, and `Msg::TitleProposed` do not exist.

- [ ] **Step 3: Implement `/title` in the core**

In `crates/scuttle-core/src/commands.rs`, add `Title(Option<String>),` to `Command` after `PlanMode`, this entry after `/plan-mode` in `COMMANDS`:

```rust
    CommandInfo {
        name: "/title",
        aliases: &[],
        usage: "/title [text]",
        description: "Rename this chat, or edit a proposed title",
    },
```

and `"title" => Ok(Command::Title(arg.map(str::to_owned))),` to `parse`.
In `crates/scuttle-core/src/app.rs`, add `Title(Uuid),` to `EditTarget`, `ProposeTitle(Uuid),` to `Effect` after `UpdateChat`, and to `Msg`, after `ChatUpdateFailed`:

```rust
    /// The server's proposed title for the open chat, not yet saved.
    TitleProposed(String),
    TitleProposeFailed(String),
```

Add these arms to `update`:

```rust
            Msg::TitleProposed(title) => {
                if let Some(editor) = self.editor.as_mut()
                    && matches!(editor.target, EditTarget::Title(_))
                    && editor.loading
                {
                    editor.line = LineEdit::new(&title);
                    editor.loading = false;
                }
                vec![]
            }
            Msg::TitleProposeFailed(message) => {
                if matches!(self.editor.as_ref().map(|e| &e.target), Some(EditTarget::Title(_))) {
                    self.editor = None;
                }
                self.error(format!("Could not propose a title: {message}"));
                vec![]
            }
```

Add this arm to `command`, before `Command::New`:

```rust
            Command::Title(text) => {
                let Some(chat) = self.chat_id else {
                    self.error("Start a chat first.");
                    return vec![];
                };
                match text {
                    Some(title) => vec![Effect::UpdateChat {
                        chat,
                        change: ChatChange::Title(title),
                    }],
                    None => {
                        self.editor = Some(Editor {
                            target: EditTarget::Title(chat),
                            line: LineEdit::default(),
                            loading: true,
                        });
                        vec![Effect::ProposeTitle(chat)]
                    }
                }
            }
```

In `finish_edit`, change the two `EditTarget::Rename` arms to cover both targets:

```rust
            EditTarget::Rename(_) | EditTarget::Title(_) if text.is_empty() => {
                self.error("A title cannot be empty.");
                vec![]
            }
            EditTarget::Rename(chat) | EditTarget::Title(chat) => vec![Effect::UpdateChat {
                chat,
                change: ChatChange::Title(text),
            }],
```

- [ ] **Step 4: Run the core tests to verify they pass**

Run: `cargo test -p scuttle-core`
Expected: PASS.

- [ ] **Step 5: Write the failing TUI and runtime tests**

Add to the test module of `crates/scuttle-tui/src/app.rs`:

```rust
    #[test]
    fn the_title_editor_says_it_is_proposing_until_the_title_arrives() {
        let mut t = tui();
        t.core.update(Msg::Started {
            org_id: uuid::Uuid::new_v4(),
            open_chat: None,
        });
        let id = uuid::Uuid::new_v4();
        t.update(Msg::ChatLoaded {
            chat: Box::new(serde_json::from_value(json!({"id": id, "children": [], "files": [],
                "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}})).unwrap()),
            messages: vec![],
        });
        t.update(Msg::Submit("/title".into()));
        let shown = screen(&mut t, 60, 16);
        assert!(shown.contains("Title"), "{shown}");
        assert!(shown.contains("Proposing a title…"), "{shown}");
        t.update(Msg::ForChat {
            chat: id,
            msg: Box::new(Msg::TitleProposed("Watch fix".into())),
        });
        assert!(screen(&mut t, 60, 16).contains("Watch fix"));
    }
```

Add to the test module of `crates/scuttle-tui/src/runtime.rs`:

```rust
    #[tokio::test]
    async fn a_proposed_title_is_tagged_with_its_chat() {
        let server = MockServer::start().await;
        let chat = Uuid::new_v4();
        Mock::given(method("POST"))
            .and(path(format!("/api/v2/chats/{chat}/title/propose")))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"title": "Watch fix"})),
            )
            .mount(&server)
            .await;
        let (mut rt, mut rx) = runtime(&server.uri());
        rt.run(Effect::ProposeTitle(chat));
        match next(&mut rx).await {
            Msg::ForChat { chat: tagged, msg } => {
                assert_eq!(tagged, chat);
                assert!(matches!(*msg, Msg::TitleProposed(ref t) if t == "Watch fix"));
            }
            other => panic!("expected a tagged title, got {other:?}"),
        }
    }
```

- [ ] **Step 6: Run the TUI tests to verify they fail**

Run: `cargo test -p scuttle-tui`
Expected: FAIL: the editor has no title case for `Title`, and the runtime ignores `ProposeTitle`.

- [ ] **Step 7: Implement the editor text and the request**

In `draw_editor` in `crates/scuttle-tui/src/app.rs`, replace the `title` match and the loading line with:

```rust
        let (title, loading) = match editor.target {
            scuttle_core::app::EditTarget::Rename(_) => {
                (" Rename chat (Enter saves, Esc cancels) ", "Loading…")
            }
            scuttle_core::app::EditTarget::Title(_) => {
                (" Title (Enter saves, Esc cancels) ", "Proposing a title…")
            }
        };
        let text = if editor.loading {
            Line::from(Span::styled(loading, self.theme.dim))
        } else {
            Line::from(editor.line.text().to_owned())
        };
```

In `crates/scuttle-tui/src/runtime.rs`, add this arm to `run`:

```rust
            Effect::ProposeTitle(chat) => self.spawn(Box::pin(async move {
                let msg = match client.api().propose_chat_title(&chat).await {
                    Ok(r) => Msg::TitleProposed(r.into_inner().title.unwrap_or_default()),
                    Err(e) => Msg::TitleProposeFailed(err(e).await),
                };
                Msg::ForChat {
                    chat,
                    msg: Box::new(msg),
                }
            })),
```

- [ ] **Step 8: Run the tests to verify they pass**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings`
Expected: PASS.

- [ ] **Step 9: Commit**

```bash
cargo fmt --all && git add crates/scuttle-core/src/commands.rs crates/scuttle-core/src/app.rs crates/scuttle-tui/src/app.rs crates/scuttle-tui/src/runtime.rs && git commit -m "feat: rename a chat with /title or edit the server's proposed title

Assisted-by: AI"
```

---

### Task 16: `/queue`

Queued messages already render dimmed below the live turn; `/queue` opens an overlay listing them in order, where Enter promotes the selected message and Delete or Backspace removes it (design section 6).
Promoting is the web UI's "Send now" (`site/src/pages/AgentsPage/components/QueuedMessagesList.tsx:229-241`): when the chat is running, the server interrupts the turn, keeps the partial response, and then runs the promoted message (`coderd/x/chatd/chatd.go:2140-2164`), so the overlay labels Enter "Send now" and says it interrupts.
The send key on an empty composer while messages are queued promotes the first one, as the web UI does (`site/src/pages/AgentsPage/components/AgentChatInput.tsx:1130-1144`).
The list is the stream's `queue_update`, which the reducer keeps in `transcript.queued`, and the next `queue_update` is authoritative, so the overlay never edits the list itself.
Delete is `DELETE /chats/{chat}/queue/{queuedMessage}` (`coderd/chat_routes.go:94-95`, `coderd/exp_chats.go:3153`), and promote is `POST .../promote` (`coderd/chat_routes.go:96`, `coderd/exp_chats.go:3214`), which answers `202` (`:3293`) or `409` with the server's message.

**Files:**
- Modify: `crates/scuttle-core/src/commands.rs` (`Queue`, tests)
- Modify: `crates/scuttle-core/src/app.rs` (`QueueAction`, `Msg::QueueAction`, `Effect::PromoteQueued`, `Effect::DeleteQueued`, `Effect::ShowQueue`, tests)
- Modify: `crates/scuttle-tui/src/table.rs` (`RowKey::Queued`)
- Modify: `crates/scuttle-tui/src/overlay.rs` (`Overlay::Queue`, `queue_view`, keys, tests)
- Modify: `crates/scuttle-tui/src/app.rs` (`ShowQueue`, the send key on an empty composer, tests)
- Modify: `crates/scuttle-tui/src/runtime.rs` (the two requests, ignore `ShowQueue`, tests)

**Interfaces:**
- Consumes: `transcript.queued`; the table overlay (Task 7).
- Produces: `Command::Queue`; `app::QueueAction { Promote(i64), PromoteFirst, Remove(i64) }`; `Msg::QueueAction(QueueAction)`; `Effect::PromoteQueued { chat: Uuid, id: i64 }`; `Effect::DeleteQueued { chat: Uuid, id: i64 }`; `Effect::ShowQueue`; `RowKey::Queued(i64)`; `Overlay::Queue(TableState)`.

- [ ] **Step 1: Write the failing core tests**

In the test module of `crates/scuttle-core/src/commands.rs`, insert `"/queue",` after `"/title",` in `every_listed_command_parses`, and add `assert_eq!(parse("/queue"), Ok(Command::Queue));` to `parses_title`.
Add to the test module of `crates/scuttle-core/src/app.rs`:

```rust
    #[test]
    fn queue_actions_go_to_the_server_for_the_open_chat() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        assert!(app.update(Msg::Command(Command::Queue)).is_empty());
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            chat: chat(id),
            messages: vec![],
        });
        assert_eq!(app.update(Msg::Command(Command::Queue)), vec![Effect::ShowQueue]);
        assert_eq!(
            app.update(Msg::QueueAction(QueueAction::Promote(7))),
            vec![Effect::PromoteQueued { chat: id, id: 7 }]
        );
        assert_eq!(
            app.update(Msg::QueueAction(QueueAction::Remove(7))),
            vec![Effect::DeleteQueued { chat: id, id: 7 }]
        );
    }

    #[test]
    fn send_now_on_an_empty_composer_promotes_the_first_queued_message() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            chat: chat(id),
            messages: vec![],
        });
        assert!(
            app.update(Msg::QueueAction(QueueAction::PromoteFirst))
                .is_empty(),
            "nothing is queued"
        );
        app.update(ev(json!({"type": "queue_update", "queued_messages": [
            {"id": 7, "content": [{"type": "text", "text": "then run the tests"}]},
            {"id": 8, "content": [{"type": "text", "text": "and open a PR"}]}
        ]})));
        assert_eq!(
            app.update(Msg::QueueAction(QueueAction::PromoteFirst)),
            vec![Effect::PromoteQueued { chat: id, id: 7 }]
        );
    }
```

- [ ] **Step 2: Run the core tests to verify they fail**

Run: `cargo test -p scuttle-core`
Expected: FAIL to compile, because `Command::Queue` and `QueueAction` do not exist.

- [ ] **Step 3: Implement the core side**

In `crates/scuttle-core/src/commands.rs`, add `Queue,` to `Command` after `Title`, this entry after `/title`:

```rust
    CommandInfo {
        name: "/queue",
        aliases: &[],
        usage: "/queue",
        description: "Run a queued message next, or remove it",
    },
```

and `"queue" => Ok(Command::Queue),` to `parse`.
In `crates/scuttle-core/src/app.rs`, add after `ChatChange`:

```rust
/// A `/queue` action on one queued message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueAction {
    /// "Send now": runs the message next, interrupting a running turn.
    Promote(i64),
    /// "Send now" for the first queued message, from the send key on an empty composer.
    PromoteFirst,
    Remove(i64),
}
```

Add `QueueAction(QueueAction),` to `Msg`; add `PromoteQueued { chat: Uuid, id: i64 },` and `DeleteQueued { chat: Uuid, id: i64 },` to `Effect` after `ProposeTitle`, and `ShowQueue,` after `ShowSubagents`.
Add the arm:

```rust
            Msg::QueueAction(action) => {
                let first = self.transcript.queued.first().and_then(|q| q.id);
                match (self.chat_id, action) {
                    (Some(chat), QueueAction::Promote(id)) => {
                        vec![Effect::PromoteQueued { chat, id }]
                    }
                    (Some(chat), QueueAction::PromoteFirst) => match first {
                        Some(id) => vec![Effect::PromoteQueued { chat, id }],
                        None => vec![],
                    },
                    (Some(chat), QueueAction::Remove(id)) => vec![Effect::DeleteQueued { chat, id }],
                    (None, _) => vec![],
                }
            }
```

and to `command`, before `Command::New`:

```rust
            Command::Queue => match self.chat_id {
                Some(_) => vec![Effect::ShowQueue],
                None => {
                    self.error("Start a chat first.");
                    vec![]
                }
            },
```

- [ ] **Step 4: Run the core tests to verify they pass**

Run: `cargo test -p scuttle-core`
Expected: PASS.

- [ ] **Step 5: Write the failing TUI and runtime tests**

Add `Queued(i64),` to `RowKey` in `crates/scuttle-tui/src/table.rs`, and to the test module of `crates/scuttle-tui/src/overlay.rs`:

```rust
    #[test]
    fn the_queue_lists_messages_in_order_and_enter_sends_now_delete_removes() {
        let mut app = App::new(BusyBehavior::Queue, true);
        assert!(queue_view(&app).status.as_deref() == Some("Nothing is queued."));
        app.transcript.queued = serde_json::from_value(json!([
            {"id": 7, "content": [{"type": "text", "text": "then run the tests"}]},
            {"id": 8, "content": [{"type": "text", "text": "and open a PR"}]}
        ]))
        .unwrap();
        let view = queue_view(&app);
        assert_eq!(view.rows[0].cells[1].to_string(), "then run the tests");
        assert!(
            view.hint.as_deref().is_some_and(|h| h.contains("sends now, interrupting a running turn")),
            "{:?}",
            view.hint
        );
        let mut o = Overlay::Queue(TableState::default());
        press(&mut o, &app, KeyCode::Down);
        assert!(matches!(
            press(&mut o, &app, KeyCode::Enter),
            OverlayOutcome::Send(Msg::QueueAction(scuttle_core::app::QueueAction::Promote(8)))
        ));
        assert!(matches!(
            press(&mut o, &app, KeyCode::Delete),
            OverlayOutcome::Send(Msg::QueueAction(scuttle_core::app::QueueAction::Remove(8)))
        ));
    }
```

Add to the test module of `crates/scuttle-tui/src/runtime.rs`:

```rust
    #[tokio::test]
    async fn queue_requests_hit_their_paths_and_a_refusal_is_reported() {
        let server = MockServer::start().await;
        let chat = Uuid::new_v4();
        Mock::given(method("DELETE"))
            .and(path(format!("/api/v2/chats/{chat}/queue/7")))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(format!("/api/v2/chats/{chat}/queue/8/promote")))
            .respond_with(api_error(409, "The chat has no queued messages to promote."))
            .mount(&server)
            .await;
        let (mut rt, mut rx) = runtime(&server.uri());
        rt.run(Effect::DeleteQueued { chat, id: 7 });
        assert!(matches!(untag(next(&mut rx).await), Msg::Refresh));
        rt.run(Effect::PromoteQueued { chat, id: 8 });
        match untag(next(&mut rx).await) {
            Msg::ApiFailed { action, message } => {
                assert_eq!(action, "run the queued message next");
                assert!(message.contains("no queued messages"), "{message}");
            }
            other => panic!("expected ApiFailed, got {other:?}"),
        }
    }
```

Add to the test module of `crates/scuttle-tui/src/app.rs`:

```rust
    #[test]
    fn enter_on_an_empty_composer_sends_the_first_queued_message_now() {
        let mut t = tui();
        t.core.update(Msg::Started {
            org_id: uuid::Uuid::new_v4(),
            open_chat: None,
        });
        let id = uuid::Uuid::new_v4();
        t.update(Msg::ChatLoaded {
            chat: Box::new(serde_json::from_value(json!({"id": id, "children": [], "files": [],
                "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}})).unwrap()),
            messages: vec![],
        });
        assert!(t.handle(key(KeyCode::Enter, KeyModifiers::NONE)).is_empty());
        t.update(stream(json!({"type": "queue_update", "queued_messages": [
            {"id": 7, "content": [{"type": "text", "text": "then run the tests"}]}
        ]})));
        assert_eq!(
            t.handle(key(KeyCode::Enter, KeyModifiers::NONE)),
            vec![Effect::PromoteQueued { chat: id, id: 7 }]
        );
        t.composer.set_text("a new message");
        assert!(
            matches!(t.handle(key(KeyCode::Enter, KeyModifiers::NONE)).as_slice(), [Effect::SendMessage { .. }]),
            "with text in the composer, Enter sends it"
        );
    }
```

- [ ] **Step 6: Run the TUI tests to verify they fail**

Run: `cargo test -p scuttle-tui`
Expected: FAIL to compile, because `queue_view` and `Overlay::Queue` do not exist.

- [ ] **Step 7: Implement the overlay and the requests**

In `crates/scuttle-tui/src/overlay.rs`, add `use scuttle_core::app::QueueAction;` and:

```rust
fn queue_view(app: &App) -> TableView {
    let rows: Vec<Row> = app
        .transcript
        .queued
        .iter()
        .enumerate()
        .filter_map(|(n, q)| {
            let text = q
                .content
                .iter()
                .filter_map(|p| p.text.as_deref())
                .collect::<Vec<_>>()
                .join(" ");
            Some(Row::item(
                RowKey::Queued(q.id?),
                vec![Line::from(format!("{}.", n + 1)), Line::from(text)],
            ))
        })
        .collect();
    TableView {
        title: "Queue".into(),
        widths: vec![Constraint::Length(3), Constraint::Fill(1)],
        status: rows.is_empty().then(|| "Nothing is queued.".to_owned()),
        rows,
        hint: Some(
            "Enter sends now, interrupting a running turn; Delete removes; Esc closes".into(),
        ),
        filterable: false,
    }
}
```

Add `Queue(TableState),` to `Overlay`, add it to `state` and `state_mut` next to the other table-only variants, add `Overlay::Queue(_) => queue_view(ctx.app),` to `view`, and replace the `TableKey::Handled | TableKey::Unhandled => OverlayOutcome::Stay,` arm of the generic `handle_key` with:

```rust
            TableKey::Unhandled
                if matches!(key.code, KeyCode::Delete | KeyCode::Backspace) =>
            {
                match state.selected_row(&view).map(|r| r.key.clone()) {
                    Some(RowKey::Queued(id)) => {
                        OverlayOutcome::Send(Msg::QueueAction(QueueAction::Remove(id)))
                    }
                    _ => OverlayOutcome::Stay,
                }
            }
            TableKey::Handled | TableKey::Unhandled => OverlayOutcome::Stay,
```

and add `Some(RowKey::Queued(id)) => OverlayOutcome::Send(Msg::QueueAction(QueueAction::Promote(id))),` to its `TableKey::Enter` match.
In `crates/scuttle-tui/src/app.rs`, add `Effect::ShowQueue => self.overlay = Some(Overlay::Queue(crate::table::TableState::default())),` to `apply_ui_effect`, and in `key`, right before `match self.composer.handle_key(..)`, add:

```rust
        // The send key on an empty composer is "Send now" for the first queued message.
        let send = key.code == KeyCode::Enter
            && match self.core.prefs.send_shortcut {
                SendShortcut::Enter => !key
                    .modifiers
                    .intersects(KeyModifiers::SHIFT | KeyModifiers::CONTROL | KeyModifiers::ALT),
                SendShortcut::ModifierEnter => key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER),
            };
        if send && self.composer.text().trim().is_empty() && !self.core.transcript.queued.is_empty() {
            return self.update(Msg::QueueAction(scuttle_core::app::QueueAction::PromoteFirst));
        }
```
In `crates/scuttle-tui/src/runtime.rs`, add `| Effect::ShowQueue` to the ignored effects and these arms:

```rust
            Effect::DeleteQueued { chat, id } => self.spawn(Box::pin(async move {
                let msg = match client.api().delete_chat_queued_message(&chat, id).await {
                    Ok(_) => Msg::Refresh,
                    Err(e) => Msg::ApiFailed {
                        action: "remove the queued message",
                        message: err(e).await,
                    },
                };
                Msg::ForChat {
                    chat,
                    msg: Box::new(msg),
                }
            })),
            Effect::PromoteQueued { chat, id } => self.spawn(Box::pin(async move {
                let msg = match client.api().promote_chat_queued_message(&chat, id).await {
                    Ok(_) => Msg::Refresh,
                    Err(e) => Msg::ApiFailed {
                        action: "run the queued message next",
                        message: err(e).await,
                    },
                };
                Msg::ForChat {
                    chat,
                    msg: Box::new(msg),
                }
            })),
```

The generated `promote_chat_queued_message` accepts `202` and `delete_chat_queued_message` accepts `204` (`coder-api-gen/src/generated.rs:18370-18451`), so both successes arrive as `Ok`.

- [ ] **Step 8: Run the tests to verify they pass**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings`
Expected: PASS.

- [ ] **Step 9: Commit**

```bash
cargo fmt --all && git add crates/scuttle-core/src/commands.rs crates/scuttle-core/src/app.rs crates/scuttle-tui/src/table.rs crates/scuttle-tui/src/overlay.rs crates/scuttle-tui/src/app.rs crates/scuttle-tui/src/runtime.rs && git commit -m "feat: send a queued message now or remove it with /queue

Assisted-by: AI"
```

---

### Task 17: Plan-mode questions and "Implement the plan"

When the agent calls `ask_user_question`, the turn ends (it is a stop tool in plan mode, `coderd/x/chatd/chatd.go:3558-3567`), and scuttle shows its questions as a menu above the composer: each option with its description, plus "Other", which opens the one-line editor (design section 7).
Several questions are answered one after another, and the last answer sends one message: the chosen label or `Other: <text>` for one question, and `N. <header>: <answer>` lines for several, with `Question N` for an empty header (`site/src/pages/AgentsPage/components/ChatElements/tools/AskUserQuestionTool.tsx:48`, `:75-78`, `:108-120`).
The tool's arguments are `questions[].header`, `question`, and `options[].label` and `description` (`coderd/x/chatd/chattool/askuserquestion.go:23-35`).
When the agent calls `propose_plan`, Ctrl+Enter on an empty composer or `/implement` sends "Implement the plan." with `plan_mode: ""` on the same request, as the web UI does (`site/src/pages/AgentsPage/AgentChatPage.tsx:682-688`).
A user message from anywhere closes the menu, and a failed send puts the answer text back in the composer like any failed send.

**Files:**
- Create: `crates/scuttle-core/src/question.rs`
- Modify: `crates/scuttle-core/src/lib.rs` (`pub mod question;`)
- Modify: `crates/scuttle-core/src/commands.rs` (`Implement`, tests)
- Modify: `crates/scuttle-core/src/app.rs` (`Answering`, `QuestionKey`, `QuestionMenu`, `EditTarget::Other`, `question_menu`, `question_key`, `record_answer`, `plan_ready`, `Command::Implement`, tests)
- Modify: `crates/scuttle-tui/src/app.rs` (question keys, Ctrl+Enter, `draw_question_menu`, the plan hint, the editor title, tests)

**Interfaces:**
- Consumes: `Editor`, `LineEdit`, `finish_edit` (Task 9); `App::submit`, `TurnOptions`.
- Produces: `question::{Choice, Question, PendingQuestion, Answer}`; `question::pending(&Transcript) -> Option<PendingQuestion>`; `question::plan_ready(&Transcript) -> bool`; `question::answer_text(&[Question], &[Answer]) -> String`; `app::QuestionKey { Up, Down, Enter, Dismiss }`; `app::QuestionMenu { pub number: usize, pub count: usize, pub header: String, pub question: String, pub options: Vec<Choice>, pub selected: usize }`; `App::question_menu(&self) -> Option<QuestionMenu>`; `App::plan_ready(&self) -> bool`; `Msg::QuestionKey(QuestionKey)`; `EditTarget::Other`; `Command::Implement`.

- [ ] **Step 1: Write the failing module tests**

Create `crates/scuttle-core/src/question.rs` with this test module, which is `pub(crate)` so the app tests can reuse its fixture:

```rust
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
        answered.as_array_mut().unwrap().push(
            json!({"id": 3, "role": "user", "content": [{"type": "text", "text": "core"}]}),
        );
        assert_eq!(super::pending(&transcript(answered)), None);
        assert!(!plan_ready(&t));
    }

    #[test]
    fn answers_are_labels_or_other_text_and_several_are_numbered() {
        let t = transcript(asked());
        let questions = pending(&t).unwrap().questions;
        assert_eq!(answer_text(&questions[..1], &[Answer::Choice("tui".into())]), "tui");
        assert_eq!(
            answer_text(&questions[..1], &[Answer::Other("  both ".into())]),
            "Other: both"
        );
        assert_eq!(
            answer_text(
                &questions,
                &[Answer::Choice("tui".into()), Answer::Other("unit tests only".into())]
            ),
            "1. Scope: tui\n2. Question 2: Other: unit tests only"
        );
    }

    #[test]
    fn a_proposed_plan_is_ready_until_a_user_message_follows() {
        let t = transcript(json!([
            {"id": 1, "role": "assistant", "content": [
                {"type": "tool-call", "tool_call_id": "p1", "tool_name": "propose_plan", "args": {}}
            ]},
            {"id": 2, "role": "tool", "content": [
                {"type": "tool-result", "tool_call_id": "p1", "tool_name": "propose_plan", "result": {}}
            ]}
        ]));
        assert!(plan_ready(&t), "the tool result after the call does not count as a reply");
        assert_eq!(pending(&t), None);
    }
}
```

Add `pub mod question;` to `crates/scuttle-core/src/lib.rs`.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p scuttle-core question`
Expected: FAIL to compile, because the module has no items.

- [ ] **Step 3: Implement the module**

Put this above the test module in `crates/scuttle-core/src/question.rs`:

```rust
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

/// The last tool call of the newest assistant message, unless a user message follows it.
fn last_call(transcript: &Transcript) -> Option<&types::CodersdkChatMessagePart> {
    for m in transcript.messages().rev() {
        match m.role.as_ref().map(|r| r.as_str()) {
            Some("user") => return None,
            Some("assistant") => {
                return m
                    .content
                    .iter()
                    .rev()
                    .find(|p| part_type(p) == Some("tool-call"));
            }
            _ => continue,
        }
    }
    None
}

/// The questions the agent is waiting on: the newest assistant message ends with an
/// `ask_user_question` call and no user message follows. Options without a label are dropped.
pub fn pending(transcript: &Transcript) -> Option<PendingQuestion> {
    let call = last_call(transcript)
        .filter(|p| p.tool_name.as_deref() == Some("ask_user_question"))?;
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
                .filter(|o| !o.label.trim().is_empty())
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

/// Whether the agent proposed a plan with `propose_plan` and no user message follows it.
pub fn plan_ready(transcript: &Transcript) -> bool {
    last_call(transcript).is_some_and(|p| p.tool_name.as_deref() == Some("propose_plan"))
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
```

- [ ] **Step 4: Run the module tests to verify they pass**

Run: `cargo test -p scuttle-core question`
Expected: PASS.

- [ ] **Step 5: Write the failing app tests**

In the test module of `crates/scuttle-core/src/commands.rs`, insert `"/implement",` after `"/plan-mode",` in `every_listed_command_parses`, and add `assert_eq!(parse("/implement"), Ok(Command::Implement));` to `parses_plan_mode`.
Add to the test module of `crates/scuttle-core/src/app.rs`:

```rust
    fn asked(app: &mut App) -> Uuid {
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            chat: chat(id),
            messages: serde_json::from_value(crate::question::tests::asked()).unwrap(),
        });
        id
    }

    #[test]
    fn answering_every_question_sends_one_message_and_other_takes_free_text() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = asked(&mut app);
        let menu = app.question_menu().unwrap();
        assert_eq!((menu.number, menu.count, menu.header.as_str()), (1, 2, "Scope"));
        app.update(Msg::QuestionKey(QuestionKey::Down));
        assert!(app.update(Msg::QuestionKey(QuestionKey::Enter)).is_empty());
        assert_eq!(app.question_menu().map(|m| m.number), Some(2));
        app.update(Msg::QuestionKey(QuestionKey::Down));
        app.update(Msg::QuestionKey(QuestionKey::Enter));
        assert!(matches!(app.editor.as_ref().map(|e| &e.target), Some(EditTarget::Other)));
        for c in "unit tests only".chars() {
            app.update(Msg::Edit(Edit::Char(c)));
        }
        let effects = app.update(Msg::Edit(Edit::Submit));
        assert!(
            matches!(effects.as_slice(), [Effect::SendMessage { chat, text, .. }]
                if *chat == id && text == "1. Scope: tui\n2. Question 2: Other: unit tests only"),
            "{effects:?}"
        );
        assert_eq!(app.question_menu(), None, "the menu stays closed until the echo arrives");
    }

    #[test]
    fn esc_hides_the_menu_and_a_reply_from_elsewhere_closes_it() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        asked(&mut app);
        app.update(Msg::QuestionKey(QuestionKey::Dismiss));
        assert_eq!(app.question_menu(), None);
        let mut other = App::new(BusyBehavior::Queue, true);
        started(&mut other);
        asked(&mut other);
        assert!(other.question_menu().is_some());
        other.update(user_message(3));
        assert_eq!(other.question_menu(), None);
    }

    #[test]
    fn implement_leaves_plan_mode_on_the_same_request() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            chat: chat_with_plan(id, "plan"),
            messages: serde_json::from_value(json!([{"id": 1, "role": "assistant", "content": [
                {"type": "tool-call", "tool_call_id": "p1", "tool_name": "propose_plan", "args": {}}
            ]}]))
            .unwrap(),
        });
        assert!(app.plan_ready());
        assert_eq!(
            app.update(Msg::Command(Command::Implement)),
            vec![Effect::SendMessage {
                chat: id,
                text: "Implement the plan.".into(),
                model: None,
                busy: BusyBehavior::Queue,
                turn: TurnOptions {
                    plan_mode: Some(false),
                    ..app.turn()
                }
            }]
        );
        assert!(!app.plan_mode);
        app.update(user_message(2));
        assert!(app.update(Msg::Command(Command::Implement)).is_empty());
        assert_eq!(
            app.notices.last(),
            Some(&Notice::Info("There is no proposed plan to implement.".into()))
        );
    }
```


- [ ] **Step 6: Run the app tests to verify they fail**

Run: `cargo test -p scuttle-core`
Expected: FAIL to compile, because `QuestionKey`, `question_menu`, `plan_ready`, and `Command::Implement` do not exist.

- [ ] **Step 7: Implement the menu state and the plan action**

In `crates/scuttle-core/src/commands.rs`, add `Implement,` to `Command` after `PlanMode`, this entry after `/plan-mode`:

```rust
    CommandInfo {
        name: "/implement",
        aliases: &[],
        usage: "/implement",
        description: "Leave plan mode and implement the proposed plan (Ctrl+Enter)",
    },
```

and `"implement" => Ok(Command::Implement),` to `parse`.
In `crates/scuttle-core/src/app.rs`, add `use crate::question::{self, Answer, Choice};`, add `Other,` to `EditTarget` with the doc comment `/// The free-text "Other" answer to a plan-mode question.`, and add after `Preview`:

```rust
/// Progress through the pending question set.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Answering {
    call_id: String,
    /// The question being answered.
    index: usize,
    /// The highlighted option; `options.len()` is "Other".
    selected: usize,
    answers: Vec<Answer>,
    /// Set by Esc, and after the answers are sent, so the menu stays closed for this set.
    closed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuestionKey {
    Up,
    Down,
    Enter,
    Dismiss,
}

/// The question menu as the TUI draws it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuestionMenu {
    pub number: usize,
    pub count: usize,
    pub header: String,
    pub question: String,
    pub options: Vec<Choice>,
    /// The highlighted row; `options.len()` is "Other".
    pub selected: usize,
}
```

Add `QuestionKey(QuestionKey),` to `Msg`, `answering: Option<Answering>,` to `App`, and the arm `Msg::QuestionKey(key) => self.question_key(key),` to `update`.
Add to `impl App`:

```rust
    /// Whether the agent proposed a plan that `/implement` would start.
    pub fn plan_ready(&self) -> bool {
        question::plan_ready(&self.transcript)
    }

    /// The question menu to show, while a question set is pending and not closed.
    pub fn question_menu(&self) -> Option<QuestionMenu> {
        let pending = question::pending(&self.transcript)?;
        let state = self.answering.as_ref().filter(|a| a.call_id == pending.call_id);
        if state.is_some_and(|a| a.closed) {
            return None;
        }
        let index = state.map_or(0, |a| a.index);
        let q = pending.questions.get(index)?;
        Some(QuestionMenu {
            number: index + 1,
            count: pending.questions.len(),
            header: q.header.clone(),
            question: q.question.clone(),
            options: q.options.clone(),
            selected: state.map_or(0, |a| a.selected),
        })
    }

    fn question_key(&mut self, key: QuestionKey) -> Vec<Effect> {
        let Some(pending) = question::pending(&self.transcript) else {
            self.answering = None;
            return vec![];
        };
        if self
            .answering
            .as_ref()
            .is_none_or(|a| a.call_id != pending.call_id)
        {
            self.answering = Some(Answering {
                call_id: pending.call_id.clone(),
                index: 0,
                selected: 0,
                answers: vec![],
                closed: false,
            });
        }
        let Some(state) = self.answering.as_mut() else {
            return vec![];
        };
        let Some(q) = pending.questions.get(state.index) else {
            return vec![];
        };
        let other = q.options.len();
        match key {
            QuestionKey::Up => state.selected = state.selected.saturating_sub(1),
            QuestionKey::Down => state.selected = (state.selected + 1).min(other),
            QuestionKey::Dismiss => state.closed = true,
            QuestionKey::Enter if state.selected == other => {
                self.editor = Some(Editor {
                    target: EditTarget::Other,
                    line: LineEdit::default(),
                    loading: false,
                });
            }
            QuestionKey::Enter => {
                let label = q.options[state.selected].label.clone();
                return self.record_answer(Answer::Choice(label));
            }
        }
        vec![]
    }

    /// Records the answer to the current question, and sends every answer after the last.
    fn record_answer(&mut self, answer: Answer) -> Vec<Effect> {
        let Some(pending) = question::pending(&self.transcript) else {
            return vec![];
        };
        let Some(state) = self.answering.as_mut() else {
            return vec![];
        };
        state.answers.push(answer);
        state.index += 1;
        state.selected = 0;
        if state.index < pending.questions.len() {
            return vec![];
        }
        state.closed = true;
        let text = question::answer_text(&pending.questions, &state.answers);
        self.submit(text)
    }
```

Add this arm to `finish_edit`:

```rust
            EditTarget::Other if text.is_empty() => {
                self.error("Type an answer, or press Esc and pick an option.");
                vec![]
            }
            EditTarget::Other => self.record_answer(Answer::Other(text)),
```

Add this arm to `command`, before `Command::New`:

```rust
            Command::Implement => {
                let Some(chat) = self.chat_id else {
                    self.error("Start a chat first.");
                    return vec![];
                };
                if !self.plan_ready() {
                    self.info("There is no proposed plan to implement.");
                    return vec![];
                }
                // The plan mode change rides on the message, as the web UI sends it.
                self.plan_mode = false;
                self.start_wait();
                vec![Effect::SendMessage {
                    chat,
                    text: "Implement the plan.".into(),
                    model: self.selected_model,
                    busy: self.busy,
                    turn: TurnOptions {
                        plan_mode: Some(false),
                        ..self.turn()
                    },
                }]
            }
```

Make `turn` `pub(crate)` so the test can build the expected options.

- [ ] **Step 8: Run the core tests to verify they pass**

Run: `cargo test -p scuttle-core`
Expected: PASS.

- [ ] **Step 9: Write the failing TUI tests**

Add to the test module of `crates/scuttle-tui/src/app.rs`:

```rust
    #[test]
    fn the_question_menu_takes_arrows_and_enter_while_the_composer_is_empty() {
        let mut t = tui();
        t.core.update(Msg::Started {
            org_id: uuid::Uuid::new_v4(),
            open_chat: None,
        });
        loaded(
            &mut t,
            json!([{"id": 2, "role": "assistant", "content": [{"type": "tool-call", "tool_call_id": "q1",
                "tool_name": "ask_user_question", "args": {"questions": [{"header": "Scope", "question": "Which crate?",
                "options": [{"label": "core", "description": "state"}, {"label": "tui", "description": "drawing"}]}]}}]}]),
        );
        let shown = screen(&mut t, 70, 24);
        assert!(shown.contains("Question 1 of 1: Scope"), "{shown}");
        assert!(shown.contains("drawing"), "{shown}");
        assert!(shown.contains("Other…"), "{shown}");
        t.handle(key(KeyCode::Down, KeyModifiers::NONE));
        let effects = t.handle(key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(
            matches!(effects.as_slice(), [Effect::SendMessage { text, .. }] if text == "tui"),
            "{effects:?}"
        );
    }

    #[test]
    fn ctrl_enter_on_an_empty_composer_implements_a_proposed_plan() {
        let mut t = tui();
        t.core.update(Msg::Started {
            org_id: uuid::Uuid::new_v4(),
            open_chat: None,
        });
        loaded(
            &mut t,
            json!([{"id": 1, "role": "assistant", "content": [{"type": "tool-call", "tool_call_id": "p1",
                "tool_name": "propose_plan", "args": {}}]}]),
        );
        assert!(screen(&mut t, 70, 20).contains("Implement the plan: Ctrl+Enter or /implement"));
        let effects = t.handle(key(KeyCode::Enter, KeyModifiers::CONTROL));
        assert!(
            matches!(effects.as_slice(), [Effect::SendMessage { text, .. }] if text == "Implement the plan."),
            "{effects:?}"
        );
    }
```

- [ ] **Step 10: Run the TUI tests to verify they fail**

Run: `cargo test -p scuttle-tui`
Expected: FAIL: no menu or hint is drawn, and the keys reach the composer.

- [ ] **Step 11: Implement the keys and the drawing**

In `crates/scuttle-tui/src/app.rs`, add `use ratatui::style::Style;` and `use scuttle_core::app::QuestionKey;`.
In `key`, right after the Ctrl+R check, add:

```rust
        if self.composer.text().is_empty() {
            if self.core.question_menu().is_some() {
                let question = match key.code {
                    KeyCode::Up => Some(QuestionKey::Up),
                    KeyCode::Down => Some(QuestionKey::Down),
                    KeyCode::Enter => Some(QuestionKey::Enter),
                    KeyCode::Esc => Some(QuestionKey::Dismiss),
                    _ => None,
                };
                if let Some(question) = question {
                    return self.update(Msg::QuestionKey(question));
                }
            }
            if key.code == KeyCode::Enter
                && key.modifiers.contains(KeyModifiers::CONTROL)
                && self.core.plan_ready()
            {
                return self.update(Msg::Command(scuttle_core::commands::Command::Implement));
            }
        }
```

Add to `impl Tui`:

```rust
    /// Draws the pending question menu, or the plan hint, at the bottom of the transcript
    /// while the composer is empty.
    fn draw_question_menu(&self, f: &mut Frame, transcript: Rect) {
        if !self.composer.text().is_empty() {
            return;
        }
        let Some(menu) = self.core.question_menu() else {
            if self.core.plan_ready() {
                let area = Rect {
                    y: transcript.y + transcript.height.saturating_sub(1),
                    height: 1.min(transcript.height),
                    ..transcript
                };
                f.render_widget(
                    Paragraph::new(Span::styled(
                        "Implement the plan: Ctrl+Enter or /implement",
                        self.theme.accent,
                    )),
                    area,
                );
            }
            return;
        };
        let header = if menu.header.trim().is_empty() {
            "the agent asks".to_owned()
        } else {
            menu.header.clone()
        };
        let mut lines = vec![
            Line::from(Span::styled(
                format!("Question {} of {}: {header}", menu.number, menu.count),
                self.theme.accent,
            )),
            Line::from(menu.question.clone()),
        ];
        let row = |selected: bool, label: String, description: String| {
            let mark = if selected { "› " } else { "  " };
            let style = if selected { self.theme.accent } else { Style::default() };
            Line::from(vec![
                Span::styled(format!("{mark}{label}"), style),
                Span::styled(format!("  {description}"), self.theme.dim),
            ])
        };
        for (i, choice) in menu.options.iter().enumerate() {
            lines.push(row(i == menu.selected, choice.label.clone(), choice.description.clone()));
        }
        lines.push(row(
            menu.selected == menu.options.len(),
            "Other…".into(),
            "type your own answer".into(),
        ));
        let h = (lines.len() as u16 + 2).min(transcript.height);
        let area = Rect {
            y: transcript.y + transcript.height - h,
            height: h,
            ..transcript
        };
        f.render_widget(Clear, area);
        f.render_widget(
            Paragraph::new(lines).block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Up and Down choose, Enter answers, Esc hides "),
            ),
            area,
        );
    }
```

Call `self.draw_question_menu(f, transcript);` in `draw_at` right after the slash menu block.
In `draw_editor`, add the target:

```rust
            scuttle_core::app::EditTarget::Other => {
                (" Other answer (Enter sends, Esc cancels) ", "Loading…")
            }
```

- [ ] **Step 12: Run the tests to verify they pass**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings`
Expected: PASS.

- [ ] **Step 13: Commit**

```bash
cargo fmt --all && git add crates/scuttle-core/src/question.rs crates/scuttle-core/src/lib.rs crates/scuttle-core/src/commands.rs crates/scuttle-core/src/app.rs crates/scuttle-tui/src/app.rs && git commit -m "feat: answer plan-mode questions from a menu and implement a proposed plan

Assisted-by: AI"
```

---

### Task 18: One plan-mode request at a time, confirmed by a chat refresh

Overlapping plan-mode requests can leave the footer out of sync (M1.5 Task 8), and the planned fix assumed the watch socket reports plan mode, which it does not: `PATCH /chats/{chat}` with `plan_mode` writes the database and returns `204` without publishing (`coderd/exp_chats.go:2657-2683`).
So scuttle sends one plan-mode request at a time, remembers only the latest wanted state while one is in flight, and after each success refetches the chat and adopts the server's value (design sections 4 and 17).

**Files:**
- Modify: `crates/scuttle-core/src/app.rs` (`plan_request`, `plan_wanted`, `request_plan_mode`, `Msg::PlanModeApplied`, `ChatRefreshed`, `PlanModeFailed`, `reset_chat_state`, `ChatCreated`, tests)
- Modify: `crates/scuttle-tui/src/runtime.rs` (`SetPlanMode` reply, tests)

**Interfaces:**
- Consumes: `Effect::RefreshChat` and `Msg::ChatRefreshed` (Task 6).
- Produces: `Msg::PlanModeApplied { on: bool }`, which the runtime wraps in `ForChat`; private `App::request_plan_mode(&mut self, chat: Uuid, on: bool) -> Vec<Effect>`.

- [ ] **Step 1: Write the failing tests**

Add to the test module of `crates/scuttle-core/src/app.rs`:

```rust
    #[test]
    fn plan_mode_changes_go_one_at_a_time_and_the_servers_value_wins() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            chat: chat(id),
            messages: vec![],
        });
        let applied = |on| Msg::ForChat {
            chat: id,
            msg: Box::new(Msg::PlanModeApplied { on }),
        };
        let set = |on| Msg::Command(Command::PlanMode(Some(on)));
        assert_eq!(app.update(set(true)), vec![Effect::SetPlanMode { chat: id, on: true }]);
        assert!(app.update(set(false)).is_empty(), "a second change waits for the first");
        assert!(app.update(set(true)).is_empty());
        assert_eq!(
            app.update(applied(true)),
            vec![Effect::RefreshChat(id)],
            "only the latest wanted state goes next, and the server already has it"
        );
        assert_eq!(app.update(set(false)), vec![Effect::SetPlanMode { chat: id, on: false }]);
        assert!(app.update(set(true)).is_empty());
        assert_eq!(
            app.update(applied(false)),
            vec![Effect::SetPlanMode { chat: id, on: true }]
        );
        assert_eq!(app.update(applied(true)), vec![Effect::RefreshChat(id)]);
        assert!(app.plan_mode);
        // The refetched chat says plan mode is off, as another client set it, and scuttle agrees.
        app.update(Msg::ForChat {
            chat: id,
            msg: Box::new(Msg::ChatRefreshed(chat_with_plan(id, ""))),
        });
        assert!(!app.plan_mode);
    }
```

In `plan_mode_toggles_and_sets_on_an_existing_chat`, add this line right before the `PlanMode(Some(false))` assertion, because that change now waits for the first to be applied:

```rust
        app.update(Msg::ForChat {
            chat: id,
            msg: Box::new(Msg::PlanModeApplied { on: true }),
        });
```

In the test module of `crates/scuttle-tui/src/runtime.rs`, change the two assertions in `set_plan_mode_patches_the_chat` to `assert!(matches!(untag(next(&mut rx).await), Msg::PlanModeApplied { on: true }));` and `assert!(matches!(untag(next(&mut rx).await), Msg::PlanModeApplied { on: false }));`.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --workspace`
Expected: FAIL to compile, because `Msg::PlanModeApplied` does not exist.

- [ ] **Step 3: Implement the serialization**

In `crates/scuttle-core/src/app.rs`, add to `Msg`, after `PlanModeFailed`:

```rust
    /// The runtime sends this when `Effect::SetPlanMode` succeeds.
    PlanModeApplied {
        on: bool,
    },
```

Add to `App`, after `plan_mode`:

```rust
    /// The plan mode state of the `SetPlanMode` in flight, if any.
    plan_request: Option<bool>,
    /// The latest state asked for while a request was in flight, sent after it.
    plan_wanted: Option<bool>,
```

Add to `impl App`:

```rust
    /// Sends plan mode `on`, or, while a request is in flight, remembers it for afterwards.
    fn request_plan_mode(&mut self, chat: Uuid, on: bool) -> Vec<Effect> {
        if self.plan_request.is_some() {
            self.plan_wanted = Some(on);
            return vec![];
        }
        self.plan_request = Some(on);
        vec![Effect::SetPlanMode { chat, on }]
    }
```

In `plan_mode_command`, replace `vec![Effect::SetPlanMode { chat, on }]` in the `Some(chat)` arm with `self.request_plan_mode(chat, on)`.
In `Msg::ChatCreated`, replace `None if plan_mismatch => effects.push(Effect::SetPlanMode { chat: id, on: self.plan_mode }),` with `None if plan_mismatch => effects.extend(self.request_plan_mode(id, self.plan_mode)),`.
Add the arm:

```rust
            Msg::PlanModeApplied { on } => {
                self.plan_request = None;
                let Some(chat) = self.chat_id else {
                    return vec![];
                };
                match self.plan_wanted.take() {
                    Some(wanted) if wanted != on => self.request_plan_mode(chat, wanted),
                    // The PATCH publishes nothing, so read back what the server now holds.
                    _ => vec![Effect::RefreshChat(chat)],
                }
            }
```

At the top of the `Msg::PlanModeFailed` arm, add `self.plan_request = None;` and `self.plan_wanted = None;`.
In the `Msg::ChatRefreshed` arm, inside the `if`, add before `self.chat = Some(chat);`:

```rust
                    if self.plan_request.is_none() {
                        self.plan_mode = is_plan(&chat);
                    }
```

In `reset_chat_state`, add `self.plan_request = None;` and `self.plan_wanted = None;` next to `self.plan_mode = false;`.
In `crates/scuttle-tui/src/runtime.rs`, in the `Effect::SetPlanMode` arm, replace `Ok(_) => Msg::Refresh,` with `Ok(_) => Msg::PlanModeApplied { on },`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all && git add crates/scuttle-core/src/app.rs crates/scuttle-tui/src/runtime.rs && git commit -m "fix: send one plan-mode change at a time and adopt the server's value after it

Assisted-by: AI"
```

---

### Task 19: Attachments: `/attach`, chips, and upload

`/attach <path>` uploads a local file and shows it as a chip above the composer, the next message carries it, Backspace on an empty composer removes the last chip, and an image shows its name and size (design section 8).
Upload is `POST /api/v2/chats/files?organization=<org>` with the raw bytes and a `Content-Disposition` carrying the file name (`coderd/exp_chats.go:6423-6436`), answering `{ id }` (`codersdk/chats.go:818-820`); the message part is `{ "type": "file", "file_id": <id> }` (`codersdk/chats.go:611-620`).
The server takes at most 10 MiB (`codersdk/chats.go:57`, `coderd/exp_chats.go:6481-6490`) and classifies the bytes against its allowlist (PNG, JPEG, GIF, WebP, SVG, plain text, Markdown, CSV, JSON, PDF, `coderd/exp_chats.go:6427`, `:6510-6525`), which accepts source code as plain text.
So the core rejects, by extension, formats the server never accepts (archives, executables, audio, video, office files), and the runtime checks the size from file metadata before reading anything; both say why on the chip.

**Files:**
- Create: `crates/scuttle-core/src/attachments.rs`
- Modify: `crates/scuttle-core/src/lib.rs` (`pub mod attachments;`)
- Modify: `crates/scuttle-core/src/commands.rs` (`Attach`, tests)
- Modify: `crates/scuttle-core/src/app.rs` (`chips`, `TurnOptions::files`, `Msg`, `Effect::UploadFile`, `attach`, `submit`, tests)
- Modify: `crates/scuttle-tui/src/app.rs` (the chip row, Backspace, tests)
- Modify: `crates/scuttle-tui/src/runtime.rs` (`parts`, `UploadFile`, tests)

**Interfaces:**
- Consumes: `App::current_org`; `TurnOptions`.
- Produces: `attachments::MAX_FILE_BYTES: u64`; `attachments::check_type(name: &str) -> Result<(), String>`; `attachments::size_label(bytes: u64) -> String`; `attachments::{Chip, ChipState}` with `Chip { pub local: u64, pub name: String, pub size: Option<u64>, pub state: ChipState }`, `ChipState { Uploading, Ready(Uuid), Failed(String) }`, and `Chip::label(&self) -> String`; `App::chips: Vec<Chip>`; `TurnOptions::files: Vec<Uuid>`; `Command::Attach(String)`; `Effect::UploadFile { local: u64, path: String, org: Uuid }`; `Msg::FileUploaded { local: u64, file_id: Uuid, size: u64 }`; `Msg::UploadFailed { local: u64, message: String }`; `Msg::RemoveLastChip`; `runtime::parts(text: &str, files: &[Uuid]) -> Vec<types::CodersdkChatInputPart>`.
  Task 20 sends `Command::Attach` for `@path` tokens.

- [ ] **Step 1: Write the failing core tests**

Create `crates/scuttle-core/src/attachments.rs` with:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_the_server_never_takes_are_rejected_by_extension() {
        assert!(check_type("report.pdf").is_ok());
        assert!(check_type("main.rs").is_ok(), "source code uploads as plain text");
        assert!(check_type("Makefile").is_ok());
        let reason = check_type("build.ZIP").unwrap_err();
        assert_eq!(
            reason,
            "build.ZIP is a .zip file, which Coder does not accept. Attach images, text, Markdown, CSV, JSON, or PDF."
        );
    }

    #[test]
    fn sizes_and_labels_read_naturally() {
        assert_eq!(size_label(512), "512 B");
        assert_eq!(size_label(12 * 1024), "12 KiB");
        assert_eq!(size_label(1_258_291), "1.2 MiB");
        let chip = |state| Chip {
            local: 1,
            name: "shot.png".into(),
            size: Some(1_258_291),
            state,
        };
        assert_eq!(chip(ChipState::Ready(uuid::Uuid::nil())).label(), "shot.png 1.2 MiB");
        assert_eq!(chip(ChipState::Uploading).label(), "shot.png uploading");
        assert_eq!(chip(ChipState::Failed("too big".into())).label(), "shot.png: too big");
    }
}
```

Add `pub mod attachments;` to `crates/scuttle-core/src/lib.rs`.
In the test module of `crates/scuttle-core/src/commands.rs`, insert `"/attach",` after `"/queue",` in `every_listed_command_parses`, and add:

```rust
    #[test]
    fn parses_attach() {
        assert_eq!(
            parse("/attach ~/shots/login page.png"),
            Ok(Command::Attach("~/shots/login page.png".into()))
        );
        assert!(parse("/attach").unwrap_err().contains("path"));
    }
```

Add to the test module of `crates/scuttle-core/src/app.rs`, with `use crate::attachments::ChipState;`:

```rust
    #[test]
    fn an_attachment_uploads_and_the_next_message_carries_it() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let org = started(&mut app);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            chat: chat(id),
            messages: vec![],
        });
        assert_eq!(
            app.update(Msg::Command(Command::Attach("/tmp/shot.png".into()))),
            vec![Effect::UploadFile {
                local: 1,
                path: "/tmp/shot.png".into(),
                org
            }]
        );
        let file = Uuid::new_v4();
        app.update(Msg::FileUploaded {
            local: 1,
            file_id: file,
            size: 2048,
        });
        assert_eq!(app.chips[0].state, ChipState::Ready(file));
        let effects = app.update(Msg::Submit("what is wrong here?".into()));
        assert!(
            matches!(effects.as_slice(), [Effect::SendMessage { turn, .. }] if turn.files == vec![file]),
            "{effects:?}"
        );
        assert!(app.chips.is_empty(), "sent chips clear");
    }

    #[test]
    fn a_message_waits_for_uploads_and_a_failed_upload_hands_the_text_back() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        app.update(Msg::ChatLoaded {
            chat: chat(Uuid::new_v4()),
            messages: vec![],
        });
        app.update(Msg::Command(Command::Attach("/tmp/a.md".into())));
        app.update(Msg::Command(Command::Attach("/tmp/b.md".into())));
        assert!(app.update(Msg::Submit("read both".into())).is_empty());
        let file = Uuid::new_v4();
        assert!(app.update(Msg::FileUploaded { local: 1, file_id: file, size: 10 }).is_empty());
        assert_eq!(
            app.update(Msg::UploadFailed {
                local: 2,
                message: "b.md is 12 MiB; the limit is 10 MiB.".into()
            }),
            vec![Effect::RestoreComposer("read both".into())]
        );
        assert_eq!(app.chips[1].label(), "b.md: b.md is 12 MiB; the limit is 10 MiB.");
        app.update(Msg::RemoveLastChip);
        assert_eq!(app.chips.len(), 1);
        let effects = app.update(Msg::Submit("read a".into()));
        assert!(matches!(effects.as_slice(), [Effect::SendMessage { turn, .. }] if turn.files == vec![file]));
    }

    #[test]
    fn an_unsupported_type_is_rejected_before_upload() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        assert!(
            app.update(Msg::Command(Command::Attach("/tmp/logs.tar.gz".into())))
                .is_empty(),
            "nothing is uploaded"
        );
        assert!(matches!(&app.chips[0].state, ChipState::Failed(m) if m.contains(".gz file")));
    }
```

- [ ] **Step 2: Run the core tests to verify they fail**

Run: `cargo test -p scuttle-core`
Expected: FAIL to compile, because `attachments`, `Command::Attach`, and the upload messages do not exist.

- [ ] **Step 3: Implement the chips, the checks, and the send**

Put this above the test module in `crates/scuttle-core/src/attachments.rs`:

```rust
//! Files attached to the next message, and the checks made before any bytes are sent.

use uuid::Uuid;

/// The server's upload limit, `codersdk.MaxChatFileSizeBytes`.
pub const MAX_FILE_BYTES: u64 = 10 * 1024 * 1024;

/// Extensions of formats the server never accepts. Anything else uploads, and the server
/// classifies the bytes, which accepts source code as plain text.
const REJECTED: &[&str] = &[
    "zip", "tar", "gz", "tgz", "bz2", "xz", "7z", "rar", "exe", "dll", "so", "dylib", "bin",
    "iso", "dmg", "mp3", "mp4", "mov", "avi", "mkv", "wav", "doc", "docx", "xls", "xlsx", "ppt",
    "pptx", "heic", "tiff", "bmp", "ico",
];

/// Rejects `name` when its extension is a format the server never accepts.
pub fn check_type(name: &str) -> Result<(), String> {
    let Some((_, ext)) = name.rsplit_once('.') else {
        return Ok(());
    };
    let ext = ext.to_lowercase();
    if REJECTED.contains(&ext.as_str()) {
        return Err(format!(
            "{name} is a .{ext} file, which Coder does not accept. Attach images, text, Markdown, CSV, JSON, or PDF."
        ));
    }
    Ok(())
}

/// A byte count in binary units: `512 B`, `12 KiB`, `1.2 MiB`.
pub fn size_label(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = 1024 * 1024;
    match bytes {
        ..KIB => format!("{bytes} B"),
        KIB..MIB => format!("{} KiB", bytes / KIB),
        _ => format!("{:.1} MiB", bytes as f64 / MIB as f64),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChipState {
    Uploading,
    Ready(Uuid),
    Failed(String),
}

/// One attachment above the composer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chip {
    /// A local number that matches the upload's reply to its chip.
    pub local: u64,
    pub name: String,
    pub size: Option<u64>,
    pub state: ChipState,
}

impl Chip {
    /// The chip's text. The terminal cannot show an image, so every file shows its name and size.
    pub fn label(&self) -> String {
        match &self.state {
            ChipState::Uploading => format!("{} uploading", self.name),
            ChipState::Ready(_) => match self.size {
                Some(size) => format!("{} {}", self.name, size_label(size)),
                None => self.name.clone(),
            },
            ChipState::Failed(why) => format!("{}: {why}", self.name),
        }
    }
}
```

In `crates/scuttle-core/src/commands.rs`, add `Attach(String),` to `Command` after `Queue`, this entry after `/queue`:

```rust
    CommandInfo {
        name: "/attach",
        aliases: &[],
        usage: "/attach <path>",
        description: "Attach a file to the next message (or type @path)",
    },
```

and to `parse`:

```rust
        "attach" => arg
            .map(|p| Command::Attach(p.to_owned()))
            .ok_or_else(|| "/attach takes a file path".to_owned()),
```

In `crates/scuttle-core/src/app.rs`, add `use crate::attachments::{self, Chip, ChipState};`, add to `TurnOptions`:

```rust
    /// Uploaded files the message carries.
    pub files: Vec<Uuid>,
```

add to `Msg`:

```rust
    FileUploaded {
        local: u64,
        file_id: Uuid,
        size: u64,
    },
    UploadFailed {
        local: u64,
        message: String,
    },
    /// Backspace on an empty composer.
    RemoveLastChip,
```

add `UploadFile { local: u64, path: String, org: Uuid },` to `Effect`, and to `App`:

```rust
    pub chips: Vec<Chip>,
    next_chip: u64,
    /// Text submitted while an attachment was still uploading, sent once all are ready.
    waiting_send: Option<String>,
```

Add these arms to `update`:

```rust
            Msg::FileUploaded {
                local,
                file_id,
                size,
            } => {
                if let Some(chip) = self.chips.iter_mut().find(|c| c.local == local) {
                    chip.state = ChipState::Ready(file_id);
                    chip.size = Some(size);
                }
                let uploading = self.chips.iter().any(|c| c.state == ChipState::Uploading);
                match self.waiting_send.take() {
                    Some(text) if !uploading => self.submit(text),
                    waiting => {
                        self.waiting_send = waiting;
                        vec![]
                    }
                }
            }
            Msg::UploadFailed { local, message } => {
                if let Some(chip) = self.chips.iter_mut().find(|c| c.local == local) {
                    chip.state = ChipState::Failed(message);
                }
                match self.waiting_send.take() {
                    Some(text) => {
                        self.error("An attachment failed to upload. Remove it with Backspace on an empty composer, then send again.");
                        vec![Effect::RestoreComposer(text)]
                    }
                    None => vec![],
                }
            }
            Msg::RemoveLastChip => {
                self.chips.pop();
                vec![]
            }
```

Add to `impl App`:

```rust
    fn attach(&mut self, path: String) -> Vec<Effect> {
        let name = std::path::Path::new(&path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(&path)
            .to_owned();
        let Some(org) = self.current_org() else {
            self.error("Not connected to Coder yet.");
            return vec![];
        };
        self.next_chip += 1;
        let local = self.next_chip;
        match attachments::check_type(&name) {
            Err(reason) => {
                self.chips.push(Chip {
                    local,
                    name,
                    size: None,
                    state: ChipState::Failed(reason),
                });
                vec![]
            }
            Ok(()) => {
                self.chips.push(Chip {
                    local,
                    name,
                    size: None,
                    state: ChipState::Uploading,
                });
                vec![Effect::UploadFile { local, path, org }]
            }
        }
    }

    /// The uploaded files for a message about to go out; the chips clear with it.
    fn take_files(&mut self) -> Vec<Uuid> {
        let files = self
            .chips
            .iter()
            .filter_map(|c| match c.state {
                ChipState::Ready(id) => Some(id),
                _ => None,
            })
            .collect();
        self.chips.clear();
        files
    }
```

Add the arm `Command::Attach(path) => self.attach(path),` to `command`.
In `submit`, after the archived-chat guard, add:

```rust
        if self.chips.iter().any(|c| c.state == ChipState::Uploading) {
            self.waiting_send = Some(text);
            self.info("Waiting for attachments to upload; your message will follow.");
            return vec![];
        }
        if self.chips.iter().any(|c| matches!(c.state, ChipState::Failed(_))) {
            self.error("Remove the attachment that failed (Backspace on an empty composer), then send.");
            return vec![Effect::RestoreComposer(text)];
        }
```

Then in the two places `submit` returns a message effect, attach the files: in the `if let Some(chat) = self.chat_id` branch, build `turn: TurnOptions { files: self.take_files(), ..self.turn() }`, and in the final `Effect::CreateChat`, build `turn: TurnOptions { plan_mode: self.plan_mode.then_some(true), files: self.take_files(), ..self.turn() }`.

- [ ] **Step 4: Run the core tests to verify they pass**

Run: `cargo test -p scuttle-core`
Expected: PASS.

- [ ] **Step 5: Write the failing runtime and TUI tests**

Add to the test module of `crates/scuttle-tui/src/runtime.rs`:

```rust
    fn temp_file(name: &str, len: u64) -> String {
        let dir = std::env::temp_dir().join(format!("scuttle-attach-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(len).unwrap();
        path.to_string_lossy().into_owned()
    }

    #[tokio::test]
    async fn a_file_over_the_limit_is_rejected_before_upload() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v2/chats/files"))
            .respond_with(ResponseTemplate::new(201))
            .expect(0)
            .mount(&server)
            .await;
        let (mut rt, mut rx) = runtime(&server.uri());
        rt.run(Effect::UploadFile {
            local: 3,
            path: temp_file("huge.png", scuttle_core::attachments::MAX_FILE_BYTES + 1),
            org: Uuid::new_v4(),
        });
        match next(&mut rx).await {
            Msg::UploadFailed { local: 3, message } => {
                assert_eq!(message, "huge.png is 10.0 MiB; the limit is 10 MiB.");
            }
            other => panic!("expected UploadFailed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_file_uploads_with_its_name_and_messages_carry_file_parts() {
        use wiremock::matchers::{header, query_param};
        let server = MockServer::start().await;
        let (org, file) = (Uuid::new_v4(), Uuid::new_v4());
        Mock::given(method("POST"))
            .and(path("/api/v2/chats/files"))
            .and(query_param("organization", org.to_string()))
            .and(header("content-disposition", "attachment; filename=\"notes.md\""))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({"id": file})))
            .mount(&server)
            .await;
        let (mut rt, mut rx) = runtime(&server.uri());
        rt.run(Effect::UploadFile {
            local: 1,
            path: temp_file("notes.md", 5),
            org,
        });
        assert!(matches!(
            next(&mut rx).await,
            Msg::FileUploaded { local: 1, file_id, size: 5 } if file_id == file
        ));
        let parts = parts("see attached", &[file]);
        assert_eq!(
            serde_json::to_value(&parts).unwrap(),
            serde_json::json!([{"type": "text", "text": "see attached"}, {"type": "file", "file_id": file}])
        );
    }
```

Add to the test module of `crates/scuttle-tui/src/app.rs`:

```rust
    #[test]
    fn chips_show_above_the_composer_and_backspace_on_an_empty_composer_removes_one() {
        let mut t = tui();
        t.core.update(Msg::Started {
            org_id: uuid::Uuid::new_v4(),
            open_chat: None,
        });
        t.update(Msg::Submit("/attach /tmp/shot.png".into()));
        t.update(Msg::FileUploaded {
            local: 1,
            file_id: uuid::Uuid::new_v4(),
            size: 1_258_291,
        });
        assert!(screen(&mut t, 60, 16).contains("[shot.png 1.2 MiB]"));
        t.composer.set_text("x");
        t.handle(key(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(t.core.chips.len(), 1, "Backspace edits text first");
        t.handle(key(KeyCode::Backspace, KeyModifiers::NONE));
        assert!(t.core.chips.is_empty());
    }
```

- [ ] **Step 6: Run the TUI tests to verify they fail**

Run: `cargo test -p scuttle-tui`
Expected: FAIL to compile, because `parts` does not exist, and no chip row is drawn.

- [ ] **Step 7: Implement the upload, the parts, and the chip row**

In `crates/scuttle-tui/src/runtime.rs`, replace `text_part` with:

```rust
/// The content of a message: its text, then one part per uploaded file.
pub(crate) fn parts(text: &str, files: &[Uuid]) -> Vec<types::CodersdkChatInputPart> {
    let text = types::CodersdkChatInputPart {
        type_: Some(types::CodersdkChatInputPartType("text".into())),
        text: Some(text.to_owned()),
        ..Default::default()
    };
    std::iter::once(text)
        .chain(files.iter().map(|id| types::CodersdkChatInputPart {
            type_: Some(types::CodersdkChatInputPartType("file".into())),
            file_id: Some(*id),
            ..Default::default()
        }))
        .collect()
}
```

and change `content: vec![text_part(&text)],` in both `Effect::CreateChat` and `Effect::SendMessage` to `content: parts(&text, &turn.files),`.
Add this arm to `run`:

```rust
            Effect::UploadFile { local, path, org } => self.spawn(Box::pin(async move {
                let name = std::path::Path::new(&path)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or(&path)
                    .to_owned();
                let failed = |message: String| Msg::UploadFailed { local, message };
                // The size check comes before any read, so a huge file never leaves the disk.
                let size = match std::fs::metadata(&path) {
                    Ok(m) if m.is_file() => m.len(),
                    Ok(_) => return failed(format!("{name} is not a file.")),
                    Err(e) => return failed(format!("could not read {path}: {e}")),
                };
                if size > scuttle_core::attachments::MAX_FILE_BYTES {
                    return failed(format!(
                        "{name} is {}; the limit is 10 MiB.",
                        scuttle_core::attachments::size_label(size)
                    ));
                }
                let read = {
                    let path = path.clone();
                    tokio::task::spawn_blocking(move || std::fs::read(path)).await
                };
                let bytes = match read {
                    Ok(Ok(bytes)) => bytes,
                    Ok(Err(e)) => return failed(format!("could not read {path}: {e}")),
                    Err(e) => return failed(format!("could not read {path}: {e}")),
                };
                // A header value must be ASCII, and a quote would end the file name early.
                let safe: String = name
                    .chars()
                    .map(|c| if c.is_ascii() && !c.is_ascii_control() && c != '"' { c } else { '_' })
                    .collect();
                let disposition = format!("attachment; filename=\"{safe}\"");
                match client.api().upload_chat_file(&org, &disposition, bytes).await {
                    Ok(r) => match r.into_inner().id {
                        Some(file_id) => Msg::FileUploaded {
                            local,
                            file_id,
                            size,
                        },
                        None => failed("the server returned no file id".into()),
                    },
                    Err(e) => failed(err(e).await),
                }
            })),
```

In `crates/scuttle-tui/src/app.rs`, in `draw_at`, add a chip row to the layout between the activity row and the composer:

```rust
        let chips_height = u16::from(!self.core.chips.is_empty());
```

and replace the composer height and the layout with:

```rust
        let composer_height = self
            .composer
            .height(outer.width)
            .min(outer.height.saturating_sub(2 + activity_height + chips_height).max(3));
        let [transcript, activity_row, chips_row, composer, footer] = Layout::vertical([
            Constraint::Min(1),
            Constraint::Length(activity_height),
            Constraint::Length(chips_height),
            Constraint::Length(composer_height),
            Constraint::Length(1),
        ])
        .areas(outer);
```

Draw the row after the activity line:

```rust
        if chips_height > 0 {
            let spans: Vec<Span> = self
                .core
                .chips
                .iter()
                .flat_map(|chip| {
                    let style = match chip.state {
                        scuttle_core::attachments::ChipState::Failed(_) => self.theme.error,
                        _ => self.theme.accent,
                    };
                    [Span::styled(format!("[{}]", chip.label()), style), Span::raw(" ")]
                })
                .collect();
            f.render_widget(Paragraph::new(Line::from(spans)), chips_row);
        }
```

In `key`, right before `match self.composer.handle_key(..)`, add:

```rust
        if key.code == KeyCode::Backspace
            && self.composer.text().is_empty()
            && !self.core.chips.is_empty()
        {
            return self.update(Msg::RemoveLastChip);
        }
```

- [ ] **Step 8: Run the tests to verify they pass**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings`
Expected: PASS.

- [ ] **Step 9: Commit**

```bash
cargo fmt --all && git add crates/scuttle-core/src/attachments.rs crates/scuttle-core/src/lib.rs crates/scuttle-core/src/commands.rs crates/scuttle-core/src/app.rs crates/scuttle-tui/src/app.rs crates/scuttle-tui/src/runtime.rs && git commit -m "feat: attach files to the next message with /attach

Assisted-by: AI"
```

---

### Task 20: `@path` attachments with Tab completion

`@path` at a word boundary attaches like `/attach`, with path completion on Tab (design section 8).
On send, each `@<path>` token naming an existing file is attached and the token stays in the text, so the agent sees the file name; Tab on a last word that starts with `@` completes it to the longest shared path, adding a slash after a single directory.
Completion and the existence check read the filesystem in the TUI, which already writes the `$EDITOR` file there, so the core never touches the filesystem and no process starts outside `runtime.rs`.

**Files:**
- Create: `crates/scuttle-tui/src/paths.rs`
- Modify: `crates/scuttle-tui/src/main.rs` (`mod paths;`)
- Modify: `crates/scuttle-tui/src/composer.rs` (`ComposerAction::CompletePath`, `last_word`, `replace_last_word`, tests)
- Modify: `crates/scuttle-tui/src/app.rs` (completion and `@path` attachment on submit, tests)

**Interfaces:**
- Consumes: `Command::Attach` (Task 19).
- Produces: `paths::complete(partial: &str, home: Option<&Path>) -> Option<String>`; `paths::at_paths(text: &str, home: Option<&Path>) -> Vec<String>`; `ComposerAction::CompletePath(String)`; `Composer::replace_last_word(&mut self, with: &str)`.

- [ ] **Step 1: Write the failing tests**

Create `crates/scuttle-tui/src/paths.rs` with:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn tree() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("scuttle-paths-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("docs/specs")).unwrap();
        std::fs::write(dir.join("docs/design.md"), "x").unwrap();
        std::fs::write(dir.join("docs/deploy.md"), "x").unwrap();
        dir
    }

    #[test]
    fn completion_extends_to_the_shared_prefix_and_marks_a_lone_directory() {
        let dir = tree();
        let base = dir.to_string_lossy();
        assert_eq!(complete(&format!("{base}/do"), None), Some(format!("{base}/docs/")));
        assert_eq!(complete(&format!("{base}/docs/de"), None), Some(format!("{base}/docs/de")));
        assert_eq!(complete(&format!("{base}/docs/des"), None), Some(format!("{base}/docs/design.md")));
        assert_eq!(complete(&format!("{base}/nothing"), None), None);
        assert_eq!(complete("~/do", Some(&dir)), Some("~/docs/".into()));
    }

    #[test]
    fn at_tokens_that_name_files_are_attached() {
        let dir = tree();
        let base = dir.to_string_lossy();
        let text = format!("compare @{base}/docs/design.md with @{base}/docs/gone.md and mail@example.com");
        assert_eq!(at_paths(&text, None), [format!("{base}/docs/design.md")]);
        assert_eq!(at_paths("see @~/docs/deploy.md", Some(&dir)), [format!("{}/docs/deploy.md", base)]);
    }
}
```

Add `mod paths;` to `crates/scuttle-tui/src/main.rs`.
Add to the test module of `crates/scuttle-tui/src/composer.rs`:

```rust
    #[test]
    fn tab_on_an_at_word_asks_for_path_completion() {
        let mut c = Composer::new(10);
        type_str(&mut c, "look at @src/ma");
        assert_eq!(
            c.handle_key(key(KeyCode::Tab, KeyModifiers::NONE), SendShortcut::Enter),
            ComposerAction::CompletePath("src/ma".into())
        );
        c.replace_last_word("@src/main.rs");
        assert_eq!(c.text(), "look at @src/main.rs");
    }
```

Add to the test module of `crates/scuttle-tui/src/app.rs`:

```rust
    #[test]
    fn an_at_path_to_a_file_is_attached_when_the_message_is_sent() {
        let dir = std::env::temp_dir().join(format!("scuttle-at-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("notes.md");
        std::fs::write(&file, "x").unwrap();
        let mut t = tui();
        t.core.update(Msg::Started {
            org_id: uuid::Uuid::new_v4(),
            open_chat: None,
        });
        t.composer.set_text(&format!("read @{}", file.display()));
        let effects = t.handle(key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(
            matches!(effects.as_slice(), [Effect::UploadFile { path, .. }] if *path == file.to_string_lossy()),
            "{effects:?}"
        );
        assert_eq!(t.core.chips.len(), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p scuttle-tui`
Expected: FAIL to compile, because `complete`, `at_paths`, `ComposerAction::CompletePath`, and `replace_last_word` do not exist.

- [ ] **Step 3: Implement the paths and the composer hooks**

Put this above the test module in `crates/scuttle-tui/src/paths.rs`:

```rust
//! Local file paths for `@path`: completion and the files a message names.

use std::path::{Path, PathBuf};

/// `partial` with a leading `~/` resolved against `home`.
fn expand(partial: &str, home: Option<&Path>) -> PathBuf {
    match (partial.strip_prefix("~/"), home) {
        (Some(rest), Some(home)) => home.join(rest),
        _ => PathBuf::from(partial),
    }
}

/// Completes `partial` to the longest prefix every matching entry shares, with a slash after
/// a single matching directory. `None` when nothing matches. A `~/` prefix is kept as typed.
pub fn complete(partial: &str, home: Option<&Path>) -> Option<String> {
    let (dir_part, stem) = match partial.rfind('/') {
        Some(i) => (&partial[..=i], &partial[i + 1..]),
        None => ("", partial),
    };
    let dir = if dir_part.is_empty() {
        PathBuf::from(".")
    } else {
        expand(dir_part, home)
    };
    let mut names: Vec<(String, bool)> = std::fs::read_dir(&dir)
        .ok()?
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let is_dir = e.file_type().ok()?.is_dir();
            name.starts_with(stem).then_some((name, is_dir))
        })
        .collect();
    names.sort();
    let first = names.first()?.0.clone();
    let shared = names.iter().fold(first, |acc, (name, _)| {
        acc.chars()
            .zip(name.chars())
            .take_while(|(a, b)| a == b)
            .map(|(a, _)| a)
            .collect()
    });
    let slash = if names.len() == 1 && names[0].1 { "/" } else { "" };
    Some(format!("{dir_part}{shared}{slash}"))
}

/// The `@path` tokens in `text` that name existing files, resolved, in order. An `@` inside a
/// word, as in an email address, is not a path.
pub fn at_paths(text: &str, home: Option<&Path>) -> Vec<String> {
    text.split_whitespace()
        .filter_map(|word| word.strip_prefix('@'))
        .filter(|p| !p.is_empty())
        .map(|p| expand(p, home))
        .filter(|p| p.is_file())
        .map(|p| p.to_string_lossy().into_owned())
        .collect()
}
```

In `crates/scuttle-tui/src/composer.rs`, add `CompletePath(String),` to `ComposerAction` with the doc comment `/// Tab on a last word starting with @: the path typed after the @.`, and add to `impl Composer`:

```rust
    /// The last whitespace-separated word of the text, if the text ends inside it.
    fn last_word(&self) -> Option<String> {
        let text = self.text();
        if text.ends_with(char::is_whitespace) {
            return None;
        }
        text.split_whitespace().last().map(str::to_owned)
    }

    /// Replaces the last word of the text with `with`.
    pub fn replace_last_word(&mut self, with: &str) {
        let text = self.text();
        let keep = text.trim_end_matches(|c: char| !c.is_whitespace());
        self.set_text(&format!("{keep}{with}"));
    }
```

and change the `KeyCode::Tab` arm of `key_action` to:

```rust
            KeyCode::Tab => {
                if let Some(first) = self.slash_matches().first() {
                    self.set_text(&format!("{} ", first.name));
                } else if let Some(path) = self.last_word().and_then(|w| w.strip_prefix('@').map(str::to_owned)) {
                    return ComposerAction::CompletePath(path);
                }
            }
```

In `crates/scuttle-tui/src/app.rs`, add a helper and handle the two new cases in `key`:

```rust
/// The user's home directory, for `~/` in `@path`.
fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}
```

```rust
            ComposerAction::CompletePath(partial) => {
                if let Some(done) = crate::paths::complete(&partial, home().as_deref()) {
                    self.composer.replace_last_word(&format!("@{done}"));
                }
                vec![]
            }
```

and replace the `ComposerAction::Submit(text)` arm with:

```rust
            ComposerAction::Submit(text) => {
                self.scroll_from_bottom = 0;
                let mut effects = Vec::new();
                for path in crate::paths::at_paths(&text, home().as_deref()) {
                    effects.extend(self.update(Msg::Command(
                        scuttle_core::commands::Command::Attach(path),
                    )));
                }
                effects.extend(self.update(Msg::Submit(text)));
                effects
            }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings`
Expected: PASS; the message in the new app test waits for its upload, so the only effect is the upload.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all && git add crates/scuttle-tui/src/paths.rs crates/scuttle-tui/src/main.rs crates/scuttle-tui/src/composer.rs crates/scuttle-tui/src/app.rs && git commit -m "feat(scuttle-tui): attach @path files on send and complete paths with Tab

Assisted-by: AI"
```

---

### Task 21: Skills in the slash menu

The `/` menu lists scuttle's commands, the user's personal skills, and the open chat's workspace skills, each with its description, and choosing a skill inserts its trigger as message text for the agent (design section 10).
Built-in commands keep their names; a personal skill that shares a command's name is listed as `/<username>:<name>` (the author's decision) and sent as `/personal/<name>`, which the server resolves (`coderd/x/chatd/chattool/skill.go:113-118`, `coderd/x/skills/skills.go:190-225`) and which the author confirmed with the design's decisions.
A personal skill that shares a workspace skill's name also sends `/personal/<name>`, and a workspace skill that collides with anything is `/workspace/<name>`, as the web UI does (`site/src/pages/AgentsPage/components/ChatMessageInput/SkillsTriggerMenu.tsx:45-57`).
Personal skills come from `GET /api/experimental/users/me/skills` (`coderd/coderd.go:1343-1349`); workspace skills are the open chat's `context.resources` with `kind: "skill"` and `status: "ok"`, deduplicated first-wins (`site/src/pages/AgentsPage/components/ChatPageContent.tsx:88-109`), which only the single-chat GET fills (`codersdk/chats.go:199-251`).
The experimental API never blocks commands: its failure shows one dim line (spec section 8).

**Files:**
- Create: `crates/scuttle-core/src/skills.rs`
- Modify: `crates/scuttle-core/src/lib.rs` (`pub mod skills;`)
- Modify: `crates/scuttle-core/src/app.rs` (`SkillsLoad`, `personal_skills`, `slash_menu`, `workspace_skills`, `Msg`, `Effect::FetchSkills`, `submit`, `SessionStarted`, `ChatLoaded`, `new_chat`, tests)
- Modify: `crates/scuttle-tui/src/composer.rs` (`menu`, `set_menu`, `slash_matches`, tests)
- Modify: `crates/scuttle-tui/src/app.rs` (`Tui::update` sets the menu, the menu drawing, tests)
- Modify: `crates/scuttle-tui/src/runtime.rs` (`FetchSkills`, tests)

**Interfaces:**
- Consumes: `App::me` (Task 4); `ChatRefreshed` on `context_dirty` (Task 6).
- Produces: `skills::Skill { pub name: String, pub description: String }`; `skills::MenuKind { Command, Personal, Workspace, Note }`; `skills::MenuEntry { pub label: String, pub insert: String, pub description: String, pub kind: MenuKind, pub names: Vec<String> }`; `skills::menu(personal: &[Skill], workspace: &[Skill], username: Option<&str>, note: Option<&str>) -> Vec<MenuEntry>`; `skills::matches<'m>(menu: &'m [MenuEntry], prefix: &str) -> Vec<&'m MenuEntry>`; `skills::rewrite(text: &str, menu: &[MenuEntry]) -> Option<String>`; `app::SkillsLoad { Loading, Loaded(Vec<Skill>), Failed(String) }`; `App::personal_skills: SkillsLoad`; `App::slash_menu(&self) -> Vec<MenuEntry>`; `Effect::FetchSkills`; `Msg::SkillsLoaded(Vec<Skill>)`; `Msg::SkillsFailed(String)`; `Composer::set_menu(&mut self, Vec<MenuEntry>)`; `Composer::slash_matches(&self) -> Vec<&MenuEntry>`.

- [ ] **Step 1: Write the failing module tests**

Create `crates/scuttle-core/src/skills.rs` with:

```rust
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
        menu.iter().find(|e| e.label == label).unwrap_or_else(|| panic!("no {label}"))
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
        assert_eq!((shadow.insert.as_str(), &shadow.kind), ("/personal/new ", &MenuKind::Personal));
        assert_eq!(entry(&menu, "/deploy").insert, "/personal/deploy ", "a personal skill that shares a workspace skill's name");
        assert_eq!(entry(&menu, "/review").insert, "/review ");
        assert_eq!(entry(&menu, "/workspace/deploy").kind, MenuKind::Workspace);
        assert_eq!(entry(&menu, "/workspace/model").insert, "/workspace/model ");
        assert_eq!(entry(&menu, "/lint").insert, "/lint ");
        assert_eq!(rewrite("/nick:new ship it", &menu).as_deref(), Some("/personal/new ship it"));
        assert_eq!(rewrite("/personal/new", &menu).as_deref(), Some("/personal/new"));
        assert_eq!(rewrite("/lint", &menu).as_deref(), Some("/lint"));
        assert_eq!(rewrite("/nope", &menu), None);
    }

    #[test]
    fn matching_finds_commands_by_alias_and_skills_by_label_or_trigger() {
        let menu = menu(&[skill("new")], &[], Some("nick"), Some("Loading skills…"));
        let labels = |p: &str| matches(&menu, p).iter().map(|e| e.label.clone()).collect::<Vec<_>>();
        assert_eq!(labels("/ex"), ["/quit (/exit)"]);
        assert_eq!(labels("/nick"), ["/nick:new"]);
        assert_eq!(labels("/personal/"), ["/nick:new"]);
        assert!(labels("/").iter().any(|l| l == "Loading skills…"), "the note shows with every match");
        assert!(!labels("/q").iter().any(|l| l == "Loading skills…"));
    }
}
```

Add `pub mod skills;` to `crates/scuttle-core/src/lib.rs`.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p scuttle-core skills`
Expected: FAIL to compile, because the module has no items.

- [ ] **Step 3: Implement the merged menu**

Put this above the test module in `crates/scuttle-core/src/skills.rs`:

```rust
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

/// The whole menu. `note` is a status line for the skill groups, such as a load failure.
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
        entries.push(MenuEntry {
            label: note.to_owned(),
            insert: String::new(),
            description: String::new(),
            kind: MenuKind::Note,
            names: vec![],
        });
    }
    entries
}

/// The entries a typed `prefix` matches; the note shows while only `/` is typed.
pub fn matches<'m>(menu: &'m [MenuEntry], prefix: &str) -> Vec<&'m MenuEntry> {
    menu.iter()
        .filter(|e| match e.kind {
            MenuKind::Note => prefix == "/",
            _ => e.names.iter().any(|n| n.starts_with(prefix)),
        })
        .collect()
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
```

- [ ] **Step 4: Run the module tests to verify they pass**

Run: `cargo test -p scuttle-core skills`
Expected: PASS.

- [ ] **Step 5: Write the failing app, composer, and runtime tests**

Add to the test module of `crates/scuttle-core/src/app.rs`:

```rust
    #[test]
    fn a_shadowing_skill_is_sent_by_its_trigger_and_the_command_still_runs() {
        use crate::skills::Skill;
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        assert!(app.update(Msg::SessionStarted).contains(&Effect::FetchSkills));
        app.update(Msg::UserLoaded(UserRef {
            id: Uuid::new_v4(),
            username: "nick".into(),
        }));
        app.update(Msg::SkillsLoaded(vec![Skill {
            name: "new".into(),
            description: "Scaffold a crate".into(),
        }]));
        let id = Uuid::new_v4();
        let mut open = chat(id);
        open.context = Some(types::CodersdkChatContext {
            resources: vec![types::CodersdkChatContextResource {
                kind: Some(types::CodersdkChatContextResourceKind("skill".into())),
                status: Some(types::CodersdkChatContextResourceStatus("ok".into())),
                skill_name: Some("lint".into()),
                skill_description: Some("Run the linters".into()),
                ..Default::default()
            }],
            ..Default::default()
        });
        app.update(Msg::ChatLoaded {
            chat: open,
            messages: vec![],
        });
        let labels: Vec<String> = app.slash_menu().iter().map(|e| e.label.clone()).collect();
        assert!(labels.contains(&"/nick:new".to_owned()), "{labels:?}");
        assert!(labels.contains(&"/lint".to_owned()), "{labels:?}");
        let effects = app.update(Msg::Submit("/nick:new scuttle-mcp".into()));
        assert!(
            matches!(effects.as_slice(), [Effect::SendMessage { text, .. }] if text == "/personal/new scuttle-mcp"),
            "{effects:?}"
        );
        let effects = app.update(Msg::Submit("/new".into()));
        assert!(effects.contains(&Effect::ClearView), "the built-in /new still runs: {effects:?}");
    }

    #[test]
    fn a_skill_load_failure_leaves_the_commands_and_one_line() {
        let mut app = App::new(BusyBehavior::Queue, true);
        assert!(app.slash_menu().iter().any(|e| e.label == "Loading skills…"));
        app.update(Msg::SkillsFailed("HTTP 404".into()));
        let menu = app.slash_menu();
        assert!(menu.iter().any(|e| e.label == "/new"));
        assert!(menu.iter().any(|e| e.label == "Skills are unavailable: HTTP 404"));
    }
```

The composer's `tab_completes_the_first_slash_match` and `tab_completes_an_alias_to_its_command` stay as they are: `Composer::new` starts with the built-in menu, and Tab inserts `insert`, which is `"<name> "` for a command.
Add to the test module of `crates/scuttle-tui/src/runtime.rs`:

```rust
    #[tokio::test]
    async fn personal_skills_load_from_the_experimental_api() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/experimental/users/me/skills"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"name": "deploy", "description": "Ship it"}
            ])))
            .mount(&server)
            .await;
        let (mut rt, mut rx) = runtime(&server.uri());
        rt.run(Effect::FetchSkills);
        match next(&mut rx).await {
            Msg::SkillsLoaded(skills) => {
                assert_eq!((skills[0].name.as_str(), skills[0].description.as_str()), ("deploy", "Ship it"))
            }
            other => panic!("expected SkillsLoaded, got {other:?}"),
        }
    }
```

Add to the test module of `crates/scuttle-tui/src/app.rs`:

```rust
    #[test]
    fn the_slash_menu_lists_skills_with_their_group() {
        let mut t = tui();
        t.update(Msg::SkillsLoaded(vec![scuttle_core::skills::Skill {
            name: "deploy".into(),
            description: "Ship it".into(),
        }]));
        t.composer.set_text("/dep");
        let shown = screen(&mut t, 70, 16);
        assert!(shown.contains("/deploy"), "{shown}");
        assert!(shown.contains("Ship it"), "{shown}");
        assert!(shown.contains("personal skill"), "{shown}");
        t.handle(key(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(t.composer.text(), "/deploy ");
    }
```

- [ ] **Step 6: Run the tests to verify they fail**

Run: `cargo test --workspace`
Expected: FAIL to compile, because `slash_menu`, `Effect::FetchSkills`, `Msg::SkillsLoaded`, and `Composer::set_menu` do not exist.

- [ ] **Step 7: Implement the skill sources, the send rule, and the menu in the composer**

In `crates/scuttle-core/src/app.rs`, add `use crate::skills::{self, MenuEntry, Skill};` and:

```rust
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum SkillsLoad {
    #[default]
    Loading,
    Loaded(Vec<Skill>),
    Failed(String),
}
```

Add `pub personal_skills: SkillsLoad,` to `App`; `FetchSkills,` to `Effect`; and `SkillsLoaded(Vec<Skill>),` and `SkillsFailed(String),` to `Msg`, with the arms:

```rust
            Msg::SkillsLoaded(list) => {
                self.personal_skills = SkillsLoad::Loaded(list);
                vec![]
            }
            Msg::SkillsFailed(message) => {
                self.personal_skills = SkillsLoad::Failed(message);
                vec![]
            }
```

Add `Effect::FetchSkills` to the `SessionStarted` effects, to the end of `new_chat`'s effects, and in `Msg::ChatLoaded` push it when `chat.workspace_id.is_some()`, so workspace-bound chats and new chats see skills added since startup, as design section 10 says.
Because `/new` now also refetches skills, change the expected effects in `new_in_the_same_organization_keeps_its_lists` to `vec![Effect::CloseStream, Effect::ClearView, Effect::FetchSkills]`.
Add to `impl App`:

```rust
    /// The open chat's workspace skills that loaded cleanly, first of each name.
    fn workspace_skills(&self) -> Vec<Skill> {
        let mut out: Vec<Skill> = Vec::new();
        let resources = self
            .chat
            .as_ref()
            .and_then(|c| c.context.as_ref())
            .map(|c| c.resources.as_slice())
            .unwrap_or_default();
        for r in resources {
            let skill = r.kind.as_ref().is_some_and(|k| k.as_str() == "skill");
            let ok = r.status.as_ref().is_some_and(|s| s.as_str() == "ok");
            let Some(name) = r.skill_name.clone().filter(|_| skill && ok) else {
                continue;
            };
            if !out.iter().any(|s| s.name == name) {
                out.push(Skill {
                    name,
                    description: r.skill_description.clone().unwrap_or_default(),
                });
            }
        }
        out
    }

    /// The slash menu for the composer.
    pub fn slash_menu(&self) -> Vec<MenuEntry> {
        let (personal, note) = match &self.personal_skills {
            SkillsLoad::Loading => (Vec::new(), Some("Loading skills…".to_owned())),
            SkillsLoad::Loaded(list) => (list.clone(), None),
            SkillsLoad::Failed(message) => {
                (Vec::new(), Some(format!("Skills are unavailable: {message}")))
            }
        };
        skills::menu(
            &personal,
            &self.workspace_skills(),
            self.me.as_ref().map(|m| m.username.as_str()),
            note.as_deref(),
        )
    }
```

In `submit`, replace the `if text.starts_with('/') { return match commands::parse(&text) { .. }; }` block with:

```rust
        let text = if text.starts_with('/') {
            match commands::parse(&text) {
                // Built-in commands win over skills of the same name.
                Ok(cmd) => return self.command(cmd),
                Err(e) => match skills::rewrite(&text, &self.slash_menu()) {
                    Some(message) => message,
                    None => {
                        self.error(e);
                        return vec![];
                    }
                },
            }
        } else {
            text
        };
```

In `crates/scuttle-tui/src/runtime.rs`, add:

```rust
            Effect::FetchSkills => self.spawn(Box::pin(async move {
                match client.api().list_user_skills("me").await {
                    Ok(r) => Msg::SkillsLoaded(
                        r.into_inner()
                            .into_iter()
                            .filter_map(|s| {
                                Some(scuttle_core::skills::Skill {
                                    name: s.name?,
                                    description: s.description.unwrap_or_default(),
                                })
                            })
                            .collect(),
                    ),
                    Err(e) => Msg::SkillsFailed(err(e).await),
                }
            })),
```

In `crates/scuttle-tui/src/composer.rs`, replace `use scuttle_core::commands::{CommandInfo, completions};` with `use scuttle_core::skills::{self, MenuEntry};`, add `menu: Vec<MenuEntry>,` to `Composer` with `menu: skills::menu(&[], &[], None, None),` in `Composer::new`, and replace `slash_matches` with:

```rust
    /// Replaces the slash menu, which the app rebuilds when skills or the user load.
    pub fn set_menu(&mut self, menu: Vec<MenuEntry>) {
        self.menu = menu;
    }

    pub fn slash_matches(&self) -> Vec<&MenuEntry> {
        let text = self.text();
        if !text.starts_with('/') || text.contains(char::is_whitespace) {
            return Vec::new();
        }
        skills::matches(&self.menu, &text)
    }
```

Replace `key_action`'s Tab arm, as Task 20 left it, with:

```rust
            KeyCode::Tab => {
                let insert = self
                    .slash_matches()
                    .into_iter()
                    .find(|e| !e.insert.is_empty())
                    .map(|e| e.insert.clone());
                if let Some(insert) = insert {
                    self.set_text(&insert);
                } else if let Some(path) = self
                    .last_word()
                    .and_then(|w| w.strip_prefix('@').map(str::to_owned))
                {
                    return ComposerAction::CompletePath(path);
                }
            }
```
In `crates/scuttle-tui/src/app.rs`, in `Tui::update`, after the welcome update, add `self.composer.set_menu(self.core.slash_menu());`, and in the slash menu drawing replace the line builder with:

```rust
                .map(|e| {
                    let group = match e.kind {
                        scuttle_core::skills::MenuKind::Personal => "  personal skill",
                        scuttle_core::skills::MenuKind::Workspace => "  workspace skill",
                        _ => "",
                    };
                    let label = match e.kind {
                        scuttle_core::skills::MenuKind::Note => Span::styled(e.label.clone(), self.theme.dim),
                        _ => Span::styled(e.label.clone(), self.theme.accent),
                    };
                    Line::from(vec![
                        label,
                        Span::raw("  "),
                        Span::styled(e.description.clone(), self.theme.dim),
                        Span::styled(group, self.theme.dim),
                    ])
                })
```

- [ ] **Step 8: Run the tests to verify they pass**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings`
Expected: PASS, including the composer's Tab and Esc tests, which see the built-in entries.

- [ ] **Step 9: Commit**

```bash
cargo fmt --all && git add crates/scuttle-core/src/skills.rs crates/scuttle-core/src/lib.rs crates/scuttle-core/src/app.rs crates/scuttle-tui/src/composer.rs crates/scuttle-tui/src/app.rs crates/scuttle-tui/src/runtime.rs && git commit -m "feat: list personal and workspace skills in the slash menu without shadowing commands

Assisted-by: AI"
```

---

### Task 22: `/info`

`/info` (alias `/chat-info`) opens a read-only panel for the open chat: title and ID, summary, parent, organization and owner, model and effort, plan mode, workspace, created and updated times, context use, cost, changes, and warnings (design section 11).
Cost is `GET /api/v2/chats/{chat}/cost` (`coderd/chat_routes.go:77`), which covers the whole chat tree (`coderd/exp_chats.go:1790-1823`), so a subagent says so, as the web UI does (`site/src/pages/AgentsPage/exp/chatBoard/ChatInfo.tsx:180-185`); unpriced requests add "Excludes unpriced usage from N requests." (`:186-191`), and a `403` or `404` hides the row (spec section 8).
The panel refreshes the chat when it opens and refetches cost on each `status_change` in the chat's tree while it is open.

`chrono` becomes a direct dependency of `scuttle-core` for local times: it is already in the build through `coder-api-gen` (0.4.45 in `Cargo.lock`), and `features = ["clock", "std"]` is all `DateTime::with_timezone(&Local)` needs.

**Files:**
- Create: `crates/scuttle-core/src/panels.rs`
- Modify: `crates/scuttle-core/src/lib.rs` (`pub mod panels;`)
- Modify: `crates/scuttle-core/Cargo.toml` (`chrono`)
- Modify: `crates/scuttle-core/src/time.rs` (`local`, `ago`, tests)
- Modify: `crates/scuttle-core/src/commands.rs` (`Info` with `/chat-info`, tests)
- Modify: `crates/scuttle-core/src/app.rs` (`CostState`, `info_panel`, `Msg`, `Effect::FetchCost`, `Effect::ShowInfo`, `apply_watch`, tests)
- Modify: `crates/scuttle-tui/src/table.rs` (`RowKind::Text`)
- Modify: `crates/scuttle-tui/src/overlay.rs` (`Overlay::Info`, `info_view`)
- Modify: `crates/scuttle-tui/src/app.rs` (`ShowInfo`, tests)
- Modify: `crates/scuttle-tui/src/runtime.rs` (`FetchCost`, ignore `ShowInfo`, tests)

**Interfaces:**
- Consumes: `Effect::RefreshChat` (Task 6); `usage::context_usage`, `usage::format_tokens`; `time::relative` (Task 8).
- Produces: `time::local(when: chrono::DateTime<chrono::Utc>) -> String`; `time::ago(then_unix: i64, now_unix: i64) -> String`; `app::CostState { Loading, Loaded(types::CodersdkChatCost), Hidden, Failed(String) }`; `App::info_panel: Option<CostState>`; `Command::Info`; `Effect::ShowInfo`; `Effect::FetchCost(Uuid)`; `Msg::CostLoaded(types::CodersdkChatCost)`, `Msg::CostHidden`, `Msg::CostFailed(String)` (wrapped in `ForChat`); `Msg::InfoClosed`; `panels::info_lines(app: &App, now_unix: i64) -> Vec<(&'static str, String)>`; `table::RowKind::Text`.
  Tasks 24, 26, and 29 add their line builders to `panels` and reuse `RowKind::Text`.

- [ ] **Step 1: Write the failing core tests**

Add to the test module of `crates/scuttle-core/src/time.rs`:

```rust
    #[test]
    fn local_times_have_a_date_and_a_minute() {
        let shown = local("2026-09-30T14:05:00Z".parse().unwrap());
        assert_eq!(shown.len(), 16, "{shown}");
        assert_eq!(&shown[4..5], "-");
        assert_eq!(&shown[13..14], ":");
        assert_eq!(ago(100, 100), "now");
        assert_eq!(ago(100, 400), "5m ago");
    }
```

In the test module of `crates/scuttle-core/src/commands.rs`, insert `"/info",` after `"/attach",` in `every_listed_command_parses`, change the expected list in `completes_by_prefix` to `vec!["/chats", "/info", "/compact", "/clear", "/copy"]` (the alias `/chat-info` starts with `/c`), and add `assert_eq!(parse("/chat-info"), Ok(Command::Info));` to `parses_attach`.
Create `crates/scuttle-core/src/panels.rs` with:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{Msg, CostState};
    use crate::config::BusyBehavior;
    use serde_json::json;

    fn value<'l>(lines: &'l [(&'static str, String)], label: &str) -> Option<&'l str> {
        lines.iter().find(|(l, _)| *l == label).map(|(_, v)| v.as_str())
    }

    #[test]
    fn info_shows_the_chat_and_its_cost() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let parent = uuid::Uuid::new_v4();
        let chat = serde_json::from_value(json!({
            "id": uuid::Uuid::new_v4(), "title": "explore", "parent_chat_id": parent,
            "summary": "Found every watch caller.", "owner_username": "nick",
            "last_reasoning_effort": "high", "updated_at": "2026-09-30T14:00:00Z",
            "diff_status": {"pr_number": 42, "additions": 12, "deletions": 3},
            "warnings": ["The workspace is stopped."],
            "children": [], "files": [], "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}
        }))
        .unwrap();
        app.update(Msg::ChatLoaded {
            chat: Box::new(chat),
            messages: vec![],
        });
        app.info_panel = Some(CostState::Loaded(
            serde_json::from_value(json!({"total_cost_micros": 1230000, "request_count": 4, "unpriced_request_count": 1})).unwrap(),
        ));
        let now = "2026-09-30T14:05:00Z".parse::<chrono::DateTime<chrono::Utc>>().unwrap().timestamp();
        let lines = info_lines(&app, now);
        assert_eq!(value(&lines, "Title"), Some("explore"));
        assert_eq!(value(&lines, "Summary"), Some("Found every watch caller."));
        assert_eq!(value(&lines, "Parent"), Some(parent.to_string().as_str()));
        assert_eq!(value(&lines, "Owner"), Some("nick"));
        assert_eq!(value(&lines, "Model"), Some("unknown, high effort"));
        assert_eq!(value(&lines, "Workspace"), Some("none"));
        assert!(value(&lines, "Updated").unwrap().ends_with("(5m ago)"));
        assert_eq!(
            value(&lines, "Cost"),
            Some("$1.23 over 4 requests, for the whole chat tree. Excludes unpriced usage from 1 requests.")
        );
        assert_eq!(value(&lines, "Changes"), Some("#42, +12 -3"));
        assert_eq!(value(&lines, "Warnings"), Some("The workspace is stopped."));
        app.info_panel = Some(CostState::Hidden);
        assert_eq!(value(&info_lines(&app, now), "Cost"), None, "no cost row without access");
    }
}
```

Add `pub mod panels;` to `crates/scuttle-core/src/lib.rs`, and `chrono = { version = "0.4", default-features = false, features = ["clock", "std"] }` under `[dependencies]` in `crates/scuttle-core/Cargo.toml`.
Add to the test module of `crates/scuttle-core/src/app.rs`:

```rust
    #[test]
    fn info_fetches_cost_and_refetches_it_when_the_tree_changes() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let (root, child) = (Uuid::new_v4(), Uuid::new_v4());
        app.update(Msg::ChatLoaded {
            chat: chat(root),
            messages: vec![],
        });
        assert_eq!(
            app.update(Msg::Command(Command::Info)),
            vec![Effect::ShowInfo, Effect::RefreshChat(root), Effect::FetchCost(root)]
        );
        let mut sub = listed(child, "explore", "2026-09-30T10:00:00Z");
        sub.parent_chat_id = Some(root);
        assert_eq!(
            app.update(watch("status_change", sub.clone())),
            vec![Effect::FetchCost(root)],
            "a subagent's turn changes the tree's cost"
        );
        app.update(Msg::ForChat {
            chat: root,
            msg: Box::new(Msg::CostHidden),
        });
        assert!(matches!(app.info_panel, Some(CostState::Hidden)));
        app.update(Msg::InfoClosed);
        assert!(app.update(watch("status_change", sub)).is_empty());
    }
```

- [ ] **Step 2: Run the core tests to verify they fail**

Run: `cargo test -p scuttle-core`
Expected: FAIL to compile, because `local`, `ago`, `info_lines`, `CostState`, and `Command::Info` do not exist.

- [ ] **Step 3: Implement the panel data**

Add to `crates/scuttle-core/src/time.rs`, above the test module:

```rust
/// `when` in the local time zone, as `2026-09-30 14:05`.
pub fn local(when: chrono::DateTime<chrono::Utc>) -> String {
    when.with_timezone(&chrono::Local)
        .format("%Y-%m-%d %H:%M")
        .to_string()
}

/// `now` or `4m ago`.
pub fn ago(then_unix: i64, now_unix: i64) -> String {
    match relative(then_unix, now_unix).as_str() {
        "now" => "now".into(),
        short => format!("{short} ago"),
    }
}
```

In `crates/scuttle-core/src/commands.rs`, add `Info,` to `Command` after `Attach`, this entry after `/attach`:

```rust
    CommandInfo {
        name: "/info",
        aliases: &["/chat-info"],
        usage: "/info",
        description: "Show this chat's details, context, and cost",
    },
```

and `"info" => Ok(Command::Info),` to `parse`.
In `crates/scuttle-core/src/app.rs`, add after `QueueAction`:

```rust
/// The cost row of `/info`.
#[derive(Debug, Clone)]
pub enum CostState {
    Loading,
    Loaded(types::CodersdkChatCost),
    /// The server refused (`403` or `404`), as it does without AI Gateway data access.
    Hidden,
    Failed(String),
}
```

Add `pub info_panel: Option<CostState>,` to `App`; `ShowInfo,` and `FetchCost(Uuid),` to `Effect`; and `CostLoaded(types::CodersdkChatCost),`, `CostHidden,`, `CostFailed(String),`, and `InfoClosed,` to `Msg`, with the arms:

```rust
            Msg::CostLoaded(cost) => {
                if self.info_panel.is_some() {
                    self.info_panel = Some(CostState::Loaded(cost));
                }
                vec![]
            }
            Msg::CostHidden => {
                if self.info_panel.is_some() {
                    self.info_panel = Some(CostState::Hidden);
                }
                vec![]
            }
            Msg::CostFailed(message) => {
                if self.info_panel.is_some() {
                    self.info_panel = Some(CostState::Failed(message));
                }
                vec![]
            }
            Msg::InfoClosed => {
                self.info_panel = None;
                vec![]
            }
```

Add to `command`, before `Command::New`:

```rust
            Command::Info => match self.chat_id {
                Some(chat) => {
                    self.info_panel = Some(CostState::Loading);
                    vec![Effect::ShowInfo, Effect::RefreshChat(chat), Effect::FetchCost(chat)]
                }
                None => {
                    self.error("Start a chat first.");
                    vec![]
                }
            },
```

In `reset_chat_state`, add `self.info_panel = None;`.
Replace `apply_watch` with this version, which keeps Task 6's rules and adds the cost refetch:

```rust
    fn apply_watch(&mut self, ev: coder_sdk::WatchEvent) -> Vec<Effect> {
        let Some(chat) = ev.event.and_then(|e| e.chat) else {
            return vec![];
        };
        self.chats.apply_watch(&ev.kind, &chat, self.chat_id);
        let Some(open) = self.chat_id else {
            return vec![];
        };
        let mut effects = Vec::new();
        // Cost covers the whole chat tree, so any family member's turn changes it.
        let root = |c: &types::CodersdkChat| c.parent_chat_id.or(c.id);
        let open_root = self.chat.as_deref().and_then(root);
        if ev.kind == "status_change" && self.info_panel.is_some() && root(&chat) == open_root {
            effects.push(Effect::FetchCost(open));
        }
        if chat.id != Some(open) {
            return effects;
        }
        let Some(record) = self.chat.as_mut() else {
            return effects;
        };
        match ev.kind.as_str() {
            "title_change" => record.title = chat.title.clone(),
            "diff_status_change" => record.diff_status = chat.diff_status.clone(),
            "deleted" => record.archived = Some(true),
            "created" => record.archived = Some(false),
            "context_dirty" => effects.push(Effect::RefreshChat(open)),
            _ => {}
        }
        effects
    }
```

Put this above the test module in `crates/scuttle-core/src/panels.rs`:

```rust
//! The rows of the read-only panels: `/info`, and later `/workspace`, `/git`, and `/mcp`.

use crate::app::{App, CostState};
use crate::time;
use crate::usage;

/// The `/info` panel as label and value pairs, in the order shown.
pub fn info_lines(app: &App, now_unix: i64) -> Vec<(&'static str, String)> {
    let Some(chat) = app.chat.as_deref() else {
        return vec![("Chat", "Start a chat first.".into())];
    };
    let mut lines = vec![
        ("Title", chat.title.clone().unwrap_or_else(|| "Untitled".into())),
        ("ID", chat.id.map(|id| id.to_string()).unwrap_or_default()),
    ];
    if let Some(summary) = chat
        .summary
        .clone()
        .or_else(|| chat.last_turn_summary.clone())
        .filter(|s| !s.trim().is_empty())
    {
        lines.push(("Summary", summary));
    }
    if let Some(parent) = chat.parent_chat_id {
        let title = app.chats.find(parent).and_then(|p| p.title.clone());
        lines.push(("Parent", title.unwrap_or_else(|| parent.to_string())));
    }
    lines.push(("Organization", app.org_label(chat.organization_id)));
    lines.push(("Owner", chat.owner_username.clone().unwrap_or_default()));
    let model = chat
        .last_model_config_id
        .and_then(|id| app.models.iter().find(|m| m.id == Some(id)))
        .and_then(|m| m.display_name.clone().or_else(|| m.model.clone()))
        .unwrap_or_else(|| "unknown".into());
    let effort = chat
        .last_reasoning_effort
        .as_ref()
        .map(|e| format!(", {e} effort"))
        .unwrap_or_default();
    lines.push(("Model", format!("{model}{effort}")));
    lines.push(("Plan mode", if app.plan_mode { "on" } else { "off" }.into()));
    let workspace = chat.workspace_id.map(|id| {
        app.workspaces
            .iter()
            .find(|w| w.id == id)
            .map(|w| w.name.clone())
            .unwrap_or_else(|| id.to_string())
    });
    lines.push(("Workspace", workspace.unwrap_or_else(|| "none".into())));
    for (label, when) in [("Created", chat.created_at), ("Updated", chat.updated_at)] {
        if let Some(when) = when {
            let ago = time::ago(when.timestamp(), now_unix);
            lines.push((label, format!("{} ({ago})", time::local(when))));
        }
    }
    let context = match usage::context_usage(app.transcript.messages()) {
        Some(u) => match u.limit {
            Some(limit) => format!(
                "{} of {} tokens",
                usage::format_tokens(u.used),
                usage::format_tokens(limit)
            ),
            None => format!("{} tokens", usage::format_tokens(u.used)),
        },
        None => "unknown".into(),
    };
    lines.push(("Context", context));
    match app.info_panel.as_ref() {
        Some(CostState::Loading) => lines.push(("Cost", "…".into())),
        Some(CostState::Loaded(cost)) => {
            let dollars = cost.total_cost_micros.unwrap_or(0) as f64 / 1_000_000.0;
            let mut text = format!("${dollars:.2} over {} requests", cost.request_count.unwrap_or(0));
            if chat.parent_chat_id.is_some() {
                text.push_str(", for the whole chat tree");
            }
            if let Some(n) = cost.unpriced_request_count.filter(|n| *n > 0) {
                text.push_str(&format!(". Excludes unpriced usage from {n} requests."));
            }
            lines.push(("Cost", text));
        }
        Some(CostState::Failed(message)) => lines.push(("Cost", format!("unavailable: {message}"))),
        Some(CostState::Hidden) | None => {}
    }
    if let Some(diff) = chat.diff_status.as_ref() {
        let what = diff
            .pr_number
            .map(|n| format!("#{n}"))
            .unwrap_or_else(|| "branch".into());
        lines.push((
            "Changes",
            format!(
                "{what}, +{} -{}",
                diff.additions.unwrap_or(0),
                diff.deletions.unwrap_or(0)
            ),
        ));
    }
    if !chat.warnings.is_empty() {
        lines.push(("Warnings", chat.warnings.join("; ")));
    }
    lines
}
```

- [ ] **Step 4: Run the core tests to verify they pass**

Run: `cargo test -p scuttle-core`
Expected: PASS.

- [ ] **Step 5: Write the failing TUI and runtime tests**

Add to the test module of `crates/scuttle-tui/src/runtime.rs`:

```rust
    #[tokio::test]
    async fn cost_is_hidden_without_access_and_tagged_with_its_chat() {
        let server = MockServer::start().await;
        let (seen, hidden) = (Uuid::new_v4(), Uuid::new_v4());
        Mock::given(method("GET"))
            .and(path(format!("/api/v2/chats/{seen}/cost")))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "chat_id": seen, "total_cost_micros": 5, "request_count": 1, "unpriced_request_count": 0
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/api/v2/chats/{hidden}/cost")))
            .respond_with(api_error(403, "Forbidden."))
            .mount(&server)
            .await;
        let (mut rt, mut rx) = runtime(&server.uri());
        rt.run(Effect::FetchCost(seen));
        assert!(matches!(untag(next(&mut rx).await), Msg::CostLoaded(c) if c.request_count == Some(1)));
        rt.run(Effect::FetchCost(hidden));
        assert!(matches!(untag(next(&mut rx).await), Msg::CostHidden));
    }
```

Add to the test module of `crates/scuttle-tui/src/app.rs`:

```rust
    #[test]
    fn slash_info_shows_the_chat_and_esc_closes_it() {
        let mut t = tui();
        t.core.update(Msg::Started {
            org_id: uuid::Uuid::new_v4(),
            open_chat: None,
        });
        let id = uuid::Uuid::new_v4();
        t.update(Msg::ChatLoaded {
            chat: Box::new(serde_json::from_value(json!({"id": id, "title": "Watch fix", "children": [],
                "files": [], "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}})).unwrap()),
            messages: vec![],
        });
        let effects = t.update(Msg::Submit("/info".into()));
        show(&mut t, effects);
        let shown = screen(&mut t, 80, 24);
        assert!(shown.contains("Watch fix"), "{shown}");
        assert!(shown.contains("Cost"), "{shown}");
        let effects = t.handle(key(KeyCode::Esc, KeyModifiers::NONE));
        assert!(effects.is_empty());
        assert!(t.overlay.is_none());
        assert!(t.core.info_panel.is_none());
    }
```

- [ ] **Step 6: Run the TUI tests to verify they fail**

Run: `cargo test -p scuttle-tui`
Expected: FAIL: `ShowInfo` opens nothing, and the runtime ignores `FetchCost`.

- [ ] **Step 7: Implement the panel and the request**

In `crates/scuttle-tui/src/table.rs`, add `Text,` to `RowKind` with the doc comment `/// Plain text in a read-only panel, never selected.`, draw it with `RowKind::Item | RowKind::Text => Style::default(),`, and add:

```rust
    /// A read-only label and value.
    pub fn text(cells: Vec<Line<'static>>) -> Row {
        Row {
            key: RowKey::None,
            cells,
            kind: RowKind::Text,
        }
    }
```

In `crates/scuttle-tui/src/overlay.rs`, add:

```rust
fn info_view(ctx: &ViewCtx) -> TableView {
    let rows = scuttle_core::panels::info_lines(ctx.app, ctx.now_unix)
        .into_iter()
        .map(|(label, value)| {
            Row::text(vec![
                Line::from(Span::styled(label, ctx.theme.dim)),
                Line::from(value),
            ])
        })
        .collect();
    TableView {
        title: "Chat info".into(),
        widths: vec![Constraint::Length(13), Constraint::Fill(1)],
        rows,
        hint: Some("Esc closes".into()),
        ..Default::default()
    }
}
```

Add `Info(TableState),` to `Overlay`; add it to `state` and `state_mut` with the other table-only variants; make `full_height` true for it; and add `Overlay::Info(_) => info_view(ctx),` to `view`.
Panels that hold core state tell the core when they close, so add to `impl Overlay`:

```rust
    /// The message that tells the core this overlay closed, for overlays whose state lives there.
    pub fn close_msg(&self) -> Option<Msg> {
        match self {
            Overlay::Subagents(_) => Some(Msg::PreviewChat(None)),
            Overlay::Info(_) => Some(Msg::InfoClosed),
            _ => None,
        }
    }
```

In the generic `handle_key`, bind `let close = self.close_msg();` before `let state = self.state_mut();`, and change its `TableKey::Esc` arm to:

```rust
            TableKey::Esc => match close {
                Some(msg) => OverlayOutcome::CloseWith(msg),
                None => OverlayOutcome::Close,
            },
```

In `crates/scuttle-tui/src/app.rs`, make the Ctrl+C branch of `key` use it in place of Task 13's `previewing` check: replace `let previewing = matches!(self.overlay, Some(Overlay::Subagents(_)));` with `let closing = self.overlay.as_ref().and_then(Overlay::close_msg);`, and the branch's last line with `return match closing { Some(msg) => self.update(msg), None => vec![] };`.
In `crates/scuttle-tui/src/app.rs`, add `Effect::ShowInfo => self.overlay = Some(Overlay::Info(crate::table::TableState::default())),` to `apply_ui_effect`.
In `crates/scuttle-tui/src/runtime.rs`, add `| Effect::ShowInfo` to the ignored effects, and:

```rust
            Effect::FetchCost(chat) => self.spawn(Box::pin(async move {
                let msg = match client.api().get_chat_cost(&chat).await {
                    Ok(r) => Msg::CostLoaded(r.into_inner()),
                    Err(e) => match coder_sdk::Error::from_progenitor(e).await {
                        coder_sdk::Error::Api {
                            status: 403 | 404, ..
                        } => Msg::CostHidden,
                        other => Msg::CostFailed(other.to_string()),
                    },
                };
                Msg::ForChat {
                    chat,
                    msg: Box::new(msg),
                }
            })),
```

- [ ] **Step 8: Run the tests to verify they pass**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings`
Expected: PASS.

- [ ] **Step 9: Commit**

```bash
cargo fmt --all && git add crates/scuttle-core/src/panels.rs crates/scuttle-core/src/lib.rs crates/scuttle-core/Cargo.toml Cargo.lock crates/scuttle-core/src/time.rs crates/scuttle-core/src/commands.rs crates/scuttle-core/src/app.rs crates/scuttle-tui/src/table.rs crates/scuttle-tui/src/overlay.rs crates/scuttle-tui/src/app.rs crates/scuttle-tui/src/runtime.rs && git commit -m "feat: show chat details, context, and cost with /info

Assisted-by: AI"
```

---

### Task 23: `/workspace` table and `WorkspacesState`

Without a workspace, `/workspace` (new alias `/ws`) shows the M1 picker grown into a table of name, template display name, status, and last used, sorted by last used, with the fuzzy filter; Enter attaches and `none` detaches, as in M1 (design section 12).
The query stays `owner:me organization:<org>`, because attaching someone else's workspace needs SSH permission on it (`coderd/exp_chats.go:4446`), which is the approved decision.
A failed load now reads "Workspaces failed to load: <message>. /workspace retries." instead of "No workspace named …" (M1 final review Minor 8), and `/workspace` retries it.

**Files:**
- Modify: `crates/scuttle-core/src/commands.rs` (`/ws`, tests)
- Modify: `crates/scuttle-core/src/app.rs` (`WorkspaceRef` fields, `WorkspacesState`, `Msg::WorkspacesFailed`, `load_lists_for`, `command`, tests)
- Modify: `crates/scuttle-tui/src/overlay.rs` (`workspace_view`, tests)
- Modify: `crates/scuttle-tui/src/runtime.rs` (`FetchWorkspaces`, tests)

**Interfaces:**
- Consumes: `fuzzy::rank`, `time::relative`.
- Produces: `WorkspaceRef { pub id: Uuid, pub name: String, pub template: String, pub status: String, pub last_used: Option<i64> }` deriving `Default`; `app::WorkspacesState { Loading, Loaded, Failed(String) }`; `App::workspaces_state: WorkspacesState`; `Msg::WorkspacesFailed { message: String }`.
  Task 24 opens details instead of the table when the chat has a workspace.

- [ ] **Step 1: Write the failing tests**

In the test module of `crates/scuttle-core/src/commands.rs`, add `assert_eq!(parse("/ws dev"), Ok(Command::Workspace(Some("dev".into()))));` to `parses_commands_and_arguments`.
Add `..Default::default()` to the two `WorkspaceRef { .. }` literals in the core tests and the one in `crates/scuttle-tui/src/overlay.rs`'s tests.
Add to the test module of `crates/scuttle-core/src/app.rs`:

```rust
    #[test]
    fn a_failed_workspace_load_says_so_and_workspace_retries_it() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let org = started(&mut app);
        app.update(Msg::ForOrg {
            org,
            msg: Box::new(Msg::WorkspacesFailed {
                message: "HTTP 502".into(),
            }),
        });
        assert!(app.update(Msg::Command(Command::Workspace(Some("dev".into())))).is_empty());
        assert_eq!(
            app.notices.last(),
            Some(&Notice::Error(
                "Workspaces failed to load: HTTP 502. /workspace retries.".into()
            ))
        );
        assert_eq!(
            app.update(Msg::Command(Command::Workspace(None))),
            vec![Effect::ShowPicker(Picker::Workspace), Effect::FetchWorkspaces(org)]
        );
        assert_eq!(app.workspaces_state, WorkspacesState::Loading);
    }

    #[test]
    fn workspaces_sort_by_last_used() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let ws = |name: &str, last_used: Option<i64>| WorkspaceRef {
            id: Uuid::new_v4(),
            name: name.into(),
            last_used,
            ..Default::default()
        };
        app.update(Msg::WorkspacesLoaded(vec![ws("old", Some(1)), ws("never", None), ws("new", Some(9))]));
        let names: Vec<&str> = app.workspaces.iter().map(|w| w.name.as_str()).collect();
        assert_eq!(names, ["new", "old", "never"]);
        assert_eq!(app.workspaces_state, WorkspacesState::Loaded);
    }
```

Add to the test module of `crates/scuttle-tui/src/overlay.rs`:

```rust
    #[test]
    fn the_workspace_table_shows_template_status_and_last_used() {
        let mut app = App::new(BusyBehavior::Queue, true);
        app.update(Msg::WorkspacesLoaded(vec![WorkspaceRef {
            id: uuid::Uuid::new_v4(),
            name: "dev".into(),
            template: "Docker".into(),
            status: "running".into(),
            last_used: Some(1_000_000 - 7200),
        }]));
        let theme = Theme::terminal(true);
        let ctx = ViewCtx {
            app: &app,
            theme: &theme,
            now_unix: 1_000_000,
            elapsed: Duration::ZERO,
        };
        let view = Overlay::open(Picker::Workspace, &app).view(&ctx);
        let cells: Vec<String> = view.rows[1].cells.iter().map(|c| c.to_string()).collect();
        assert_eq!(cells, ["dev", "Docker", "running", "2h"]);
        let empty = App::new(BusyBehavior::Queue, true);
        let ctx = ViewCtx {
            app: &empty,
            ..ctx
        };
        let view = Overlay::open(Picker::Workspace, &empty).view(&ctx);
        assert_eq!(view.status.as_deref(), Some("Loading workspaces…"));
    }
```

In `crates/scuttle-tui/src/runtime.rs`, change `workspaces_are_listed_for_one_organization`'s response body to include the new fields and assert them:

```rust
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "workspaces": [{"id": Uuid::new_v4(), "name": "dev", "template_name": "docker",
                    "template_display_name": "Docker", "last_used_at": "2026-09-30T10:00:00Z",
                    "latest_build": {"status": "running", "resources": []}, "shared_with": []}],
                "count": 1
            })))
```

and its inner assertion to `matches!(*msg, Msg::WorkspacesLoaded(ref w) if w.len() == 1 && w[0].name == "dev" && w[0].template == "Docker" && w[0].status == "running" && w[0].last_used.is_some())`.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --workspace`
Expected: FAIL to compile, because the new `WorkspaceRef` fields, `WorkspacesState`, and `Msg::WorkspacesFailed` do not exist.

- [ ] **Step 3: Implement the state, the fields, and the table**

In `crates/scuttle-core/src/commands.rs`, change the `/workspace` entry's `aliases` to `&["/ws"]`.
In `crates/scuttle-core/src/app.rs`, replace `WorkspaceRef` with:

```rust
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WorkspaceRef {
    pub id: Uuid,
    pub name: String,
    /// The template's display name, else its name.
    pub template: String,
    /// The latest build's status, such as `running`.
    pub status: String,
    /// When the workspace was last used, in Unix seconds.
    pub last_used: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum WorkspacesState {
    #[default]
    Loading,
    Loaded,
    Failed(String),
}
```

Add `pub workspaces_state: WorkspacesState,` to `App` and `WorkspacesFailed { message: String },` to `Msg`.
In `load_lists_for`, add `self.workspaces_state = WorkspacesState::Loading;`.
Replace the `Msg::WorkspacesLoaded` arm and add the failure arm:

```rust
            Msg::WorkspacesLoaded(mut workspaces) => {
                workspaces.sort_by(|a, b| b.last_used.cmp(&a.last_used));
                self.workspaces = workspaces;
                self.workspaces_state = WorkspacesState::Loaded;
                vec![]
            }
            Msg::WorkspacesFailed { message } => {
                self.workspaces_state = WorkspacesState::Failed(message);
                vec![]
            }
```

In `command`, replace the three `Command::Workspace` arms with:

```rust
            Command::Workspace(None) => {
                let mut effects = vec![Effect::ShowPicker(Picker::Workspace)];
                if let WorkspacesState::Failed(_) = self.workspaces_state
                    && let Some(org) = self.lists_org
                {
                    self.workspaces_state = WorkspacesState::Loading;
                    effects.push(Effect::FetchWorkspaces(org));
                }
                effects
            }
            Command::Workspace(Some(name)) if name == "none" => self.set_workspace(None),
            Command::Workspace(Some(_)) if self.workspaces_state == WorkspacesState::Loading => {
                self.info("Workspaces are still loading.");
                vec![]
            }
            Command::Workspace(Some(_)) if matches!(self.workspaces_state, WorkspacesState::Failed(_)) => {
                if let WorkspacesState::Failed(message) = self.workspaces_state.clone() {
                    self.error(format!("Workspaces failed to load: {message}. /workspace retries."));
                }
                vec![]
            }
            Command::Workspace(Some(name)) => match self
                .workspaces
                .iter()
                .find(|w| w.name == name)
                .map(|w| w.id)
            {
                Some(id) => self.set_workspace(Some(id)),
                None => {
                    self.error(format!("No workspace named {name:?}"));
                    vec![]
                }
            },
```

In `crates/scuttle-tui/src/overlay.rs`, replace `workspace_view` with a version that takes the context:

```rust
fn workspace_view(ctx: &ViewCtx, filter: &str) -> TableView {
    let app = ctx.app;
    let mut rows = vec![Row::item(
        RowKey::Workspace(None),
        vec![Line::from("none (no workspace)")],
    )];
    rows.extend(
        fuzzy::rank(filter, app.workspaces.iter().collect(), |w| w.name.clone())
            .into_iter()
            .map(|w| {
                let used = w
                    .last_used
                    .map(|t| scuttle_core::time::relative(t, ctx.now_unix))
                    .unwrap_or_default();
                Row::item(
                    RowKey::Workspace(Some(w.id)),
                    vec![
                        Line::from(w.name.clone()),
                        Line::from(Span::styled(w.template.clone(), ctx.theme.dim)),
                        Line::from(w.status.clone()),
                        Line::from(Span::styled(used, ctx.theme.dim)),
                    ],
                )
            }),
    );
    let status = match &app.workspaces_state {
        scuttle_core::app::WorkspacesState::Loading => Some("Loading workspaces…".to_owned()),
        scuttle_core::app::WorkspacesState::Failed(message) => {
            Some(format!("Workspaces failed to load: {message}. /workspace retries."))
        }
        scuttle_core::app::WorkspacesState::Loaded if app.workspaces.is_empty() => Some(format!(
            "You have no workspaces in {}. The agent can create one.",
            app.org_label(app.current_org())
        )),
        scuttle_core::app::WorkspacesState::Loaded => None,
    };
    TableView {
        title: "Workspace".into(),
        widths: vec![
            Constraint::Fill(2),
            Constraint::Fill(1),
            Constraint::Length(9),
            Constraint::Length(3),
        ],
        rows,
        status,
        hint: None,
        filterable: true,
    }
}
```

and change its call in `view` to `Overlay::Workspace(s) => workspace_view(ctx, &s.filter),`.
In the "none" row's status case the table still shows the "none" row, so the status line appears below it.
In `crates/scuttle-tui/src/runtime.rs`, replace the `Ok(r) => Msg::WorkspacesLoaded(..)` and `Err(e) => Msg::ApiFailed { action: "load workspaces", .. }` arms of `Effect::FetchWorkspaces` with:

```rust
                    Ok(r) => Msg::WorkspacesLoaded(
                        r.into_inner()
                            .workspaces
                            .into_iter()
                            .filter_map(|w| {
                                let template = w
                                    .template_display_name
                                    .clone()
                                    .filter(|t| !t.trim().is_empty())
                                    .or(w.template_name.clone())
                                    .unwrap_or_default();
                                Some(WorkspaceRef {
                                    id: w.id?,
                                    name: w.name?,
                                    template,
                                    status: w
                                        .latest_build
                                        .and_then(|b| b.status)
                                        .map(|s| s.0)
                                        .unwrap_or_default(),
                                    last_used: w.last_used_at.map(|t| t.timestamp()),
                                })
                            })
                            .collect(),
                    ),
                    Err(e) => Msg::WorkspacesFailed {
                        message: err(e).await,
                    },
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all && git add crates/scuttle-core/src/commands.rs crates/scuttle-core/src/app.rs crates/scuttle-tui/src/overlay.rs crates/scuttle-tui/src/runtime.rs && git commit -m "feat: list workspaces as a table and say when they failed to load

Assisted-by: AI"
```

---

### Task 24: `/workspace` details and actions

With a workspace attached, `/workspace` shows its name and owner, template and whether it is outdated, status and health, and the chat's agent, with actions to copy the SSH command, open it in the web UI, detach, or switch (design section 12).
Details come from `GET /api/v2/workspaces/{workspace}` (generated `get_workspace_metadata_by_id`), fields from `codersdk/workspaces.go:38-68`, and the agent is the chat's `agent_id` (`codersdk/chats.go:148`) in `latest_build.resources[].agents[]`.
The SSH command is the web UI's `ssh <agent>.<workspace>.<owner>.<hostname_suffix>` (`site/src/pages/AgentsPage/AgentChatPage.tsx:585-588`) with the suffix from `GET /api/v2/deployment/ssh` (`coderd/coderd.go:1410`, `codersdk/deployment.go:5605-5613`), falling back to `coder ssh <owner>/<workspace>`; the web link is `<deployment>/@<owner>/<workspace>` (`site/src/router.tsx:700`), opened like `/web`.

**Files:**
- Modify: `crates/scuttle-core/src/panels.rs` (`Fetched`, `ssh_command`, `workspace_lines`, `workspace_agent`, tests)
- Modify: `crates/scuttle-core/src/app.rs` (`WorkspaceAction`, `workspace_panel`, `ssh_suffix`, `Msg`, `Effect`, `command`, `workspace_action`, tests)
- Modify: `crates/scuttle-tui/src/table.rs` (`RowKey::Action`)
- Modify: `crates/scuttle-tui/src/overlay.rs` (`Overlay::WorkspaceDetails`, `workspace_details_view`, keys, tests)
- Modify: `crates/scuttle-tui/src/app.rs` (`ShowWorkspace`, `CopyText`, `copied_notice`)
- Modify: `crates/scuttle-tui/src/runtime.rs` (`workspace_web_url`, `FetchWorkspaceDetails`, `FetchSshSuffix`, `OpenWorkspaceWeb`, tests)

**Interfaces:**
- Consumes: `Row::text` (Task 22); `set_workspace`; `Msg::WebOpened` and `Effect::CopyWebUrl` from M1.5 `/web`.
- Produces: `panels::Fetched<T> { Loading, Loaded(T), Failed(String) }`; `panels::ssh_command(agent: Option<&str>, workspace: &str, owner: &str, suffix: Option<&str>) -> String`; `panels::workspace_agent(ws: &types::CodersdkWorkspace, agent: Option<Uuid>) -> Option<String>`; `panels::workspace_lines(app: &App) -> Vec<(&'static str, String)>`; `app::WorkspaceAction { CopySsh, OpenWeb, Detach, Switch }`; `App::workspace_panel: Option<Fetched<Box<types::CodersdkWorkspace>>>`; `App::ssh_suffix: Option<Option<String>>`; `Effect::ShowWorkspace`; `Effect::FetchWorkspaceDetails { chat: Uuid, workspace: Uuid }`; `Effect::FetchSshSuffix`; `Effect::CopyText { text: String, what: &'static str }`; `Effect::OpenWorkspaceWeb { owner: String, workspace: String }`; `Msg::WorkspaceDetailsLoaded(Box<types::CodersdkWorkspace>)` and `Msg::WorkspaceDetailsFailed(String)` (in `ForChat`); `Msg::SshSuffixLoaded(Option<String>)`; `Msg::WorkspaceAction(WorkspaceAction)`; `Msg::WorkspaceClosed`; `RowKey::Action(&'static str)`; `runtime::workspace_web_url(base: &url::Url, owner: &str, workspace: &str) -> url::Url`.
  Tasks 26 and 29 reuse `Fetched` and `RowKey::Action`.

- [ ] **Step 1: Write the failing core tests**

Add to the test module of `crates/scuttle-core/src/panels.rs`:

```rust
    #[test]
    fn the_ssh_command_matches_the_web_ui_or_falls_back_to_coder_ssh() {
        assert_eq!(
            ssh_command(Some("main"), "dev", "nick", Some("coder")),
            "ssh main.dev.nick.coder"
        );
        assert_eq!(ssh_command(Some("main"), "dev", "nick", Some("")), "coder ssh nick/dev");
        assert_eq!(ssh_command(None, "dev", "nick", Some("coder")), "coder ssh nick/dev");
    }

    #[test]
    fn workspace_details_name_the_template_status_and_the_chats_agent() {
        let agent = uuid::Uuid::new_v4();
        let ws: coder_sdk::types::CodersdkWorkspace = serde_json::from_value(json!({
            "name": "dev", "owner_name": "nick", "template_display_name": "Docker", "outdated": true,
            "health": {"healthy": false, "failing_agents": []}, "shared_with": [],
            "latest_build": {"status": "running", "resources": [{"agents": [
                {"id": uuid::Uuid::new_v4(), "name": "other", "apps": [], "display_apps": [], "environment_variables": {}, "latency": {}, "log_sources": [], "metadata": [], "scripts": [], "subsystems": []},
                {"id": agent, "name": "main", "apps": [], "display_apps": [], "environment_variables": {}, "latency": {}, "log_sources": [], "metadata": [], "scripts": [], "subsystems": []}
            ], "metadata": []}]}
        }))
        .unwrap();
        assert_eq!(workspace_agent(&ws, Some(agent)).as_deref(), Some("main"));
        assert_eq!(workspace_agent(&ws, None).as_deref(), Some("other"), "the first agent without one set");
        let mut app = App::new(BusyBehavior::Queue, true);
        app.workspace_panel = Some(Fetched::Loaded(Box::new(ws)));
        let lines = workspace_lines(&app);
        assert_eq!(value(&lines, "Workspace"), Some("dev, owned by nick"));
        assert_eq!(value(&lines, "Template"), Some("Docker (outdated)"));
        assert_eq!(value(&lines, "Status"), Some("running, unhealthy"));
    }
```

Add to the test module of `crates/scuttle-core/src/app.rs`:

```rust
    #[test]
    fn workspace_with_one_attached_shows_details_and_its_actions() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let (id, ws) = (Uuid::new_v4(), Uuid::new_v4());
        let mut open = chat(id);
        open.workspace_id = Some(ws);
        app.update(Msg::ChatLoaded {
            chat: open,
            messages: vec![],
        });
        assert_eq!(
            app.update(Msg::Command(Command::Workspace(None))),
            vec![
                Effect::ShowWorkspace,
                Effect::FetchWorkspaceDetails { chat: id, workspace: ws },
                Effect::FetchSshSuffix
            ]
        );
        app.update(Msg::SshSuffixLoaded(Some("coder".into())));
        app.update(Msg::ForChat {
            chat: id,
            msg: Box::new(Msg::WorkspaceDetailsLoaded(Box::new(serde_json::from_value(json!({
                "name": "dev", "owner_name": "nick", "shared_with": [],
                "latest_build": {"resources": [{"agents": [{"name": "main", "apps": [], "display_apps": [],
                    "environment_variables": {}, "latency": {}, "log_sources": [], "metadata": [],
                    "scripts": [], "subsystems": []}], "metadata": []}]}
            })).unwrap()))),
        });
        assert_eq!(
            app.update(Msg::WorkspaceAction(WorkspaceAction::CopySsh)),
            vec![Effect::CopyText {
                text: "ssh main.dev.nick.coder".into(),
                what: "the SSH command"
            }]
        );
        assert_eq!(
            app.update(Msg::WorkspaceAction(WorkspaceAction::OpenWeb)),
            vec![Effect::OpenWorkspaceWeb {
                owner: "nick".into(),
                workspace: "dev".into()
            }]
        );
        assert_eq!(
            app.update(Msg::WorkspaceAction(WorkspaceAction::Switch)),
            vec![Effect::ShowPicker(Picker::Workspace)]
        );
        assert_eq!(
            app.update(Msg::WorkspaceAction(WorkspaceAction::Detach)),
            vec![Effect::SetWorkspace { chat: id, workspace: None }]
        );
        assert!(app.workspace_panel.is_none());
        assert_eq!(
            app.update(Msg::Command(Command::Workspace(None))),
            vec![Effect::ShowPicker(Picker::Workspace)],
            "after detaching, /workspace is the table again"
        );
    }
```

- [ ] **Step 2: Run the core tests to verify they fail**

Run: `cargo test -p scuttle-core`
Expected: FAIL to compile, because `Fetched`, `ssh_command`, `WorkspaceAction`, and the new effects do not exist.

- [ ] **Step 3: Implement the details in the core**

Add to `crates/scuttle-core/src/panels.rs`, with `use coder_sdk::types;` and `use uuid::Uuid;`:

```rust
/// A panel section fetched when the panel opens.
#[derive(Debug, Clone)]
pub enum Fetched<T> {
    Loading,
    Loaded(T),
    Failed(String),
}

/// The web UI's SSH command, or `coder ssh` when the agent or hostname suffix is unknown.
pub fn ssh_command(agent: Option<&str>, workspace: &str, owner: &str, suffix: Option<&str>) -> String {
    match (agent, suffix.filter(|s| !s.is_empty())) {
        (Some(agent), Some(suffix)) => format!("ssh {agent}.{workspace}.{owner}.{suffix}"),
        _ => format!("coder ssh {owner}/{workspace}"),
    }
}

/// The name of agent `agent` in the workspace's latest build, else of its first agent.
pub fn workspace_agent(ws: &types::CodersdkWorkspace, agent: Option<Uuid>) -> Option<String> {
    let agents: Vec<&types::CodersdkWorkspaceAgent> = ws
        .latest_build
        .iter()
        .flat_map(|b| b.resources.iter())
        .flat_map(|r| r.agents.iter())
        .collect();
    agent
        .and_then(|id| agents.iter().find(|a| a.id == Some(id)))
        .or(agents.first())
        .and_then(|a| a.name.clone())
}

/// The `/workspace` details as label and value pairs.
pub fn workspace_lines(app: &App) -> Vec<(&'static str, String)> {
    let ws = match app.workspace_panel.as_ref() {
        Some(Fetched::Loaded(ws)) => ws,
        Some(Fetched::Failed(message)) => {
            return vec![("Workspace", format!("unavailable: {message}"))];
        }
        _ => return vec![("Workspace", "Loading…".into())],
    };
    let name = ws.name.clone().unwrap_or_default();
    let owner = ws.owner_name.clone().unwrap_or_default();
    let template = ws
        .template_display_name
        .clone()
        .filter(|t| !t.trim().is_empty())
        .or(ws.template_name.clone())
        .unwrap_or_default();
    let outdated = if ws.outdated == Some(true) { " (outdated)" } else { "" };
    let status = ws
        .latest_build
        .as_ref()
        .and_then(|b| b.status.as_ref())
        .map(|s| s.0.clone())
        .unwrap_or_else(|| "unknown".into());
    let health = match ws.health.as_ref().and_then(|h| h.healthy) {
        Some(true) => ", healthy",
        Some(false) => ", unhealthy",
        None => "",
    };
    let agent = app.chat.as_ref().and_then(|c| c.agent_id);
    vec![
        ("Workspace", format!("{name}, owned by {owner}")),
        ("Template", format!("{template}{outdated}")),
        ("Status", format!("{status}{health}")),
        ("Agent", workspace_agent(ws, agent).unwrap_or_else(|| "none".into())),
    ]
}
```

In `crates/scuttle-core/src/app.rs`, add `use crate::panels::{self, Fetched};` and, after `CostState`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceAction {
    CopySsh,
    OpenWeb,
    Detach,
    Switch,
}
```

Add to `App`:

```rust
    /// The attached workspace's details while `/workspace` shows them.
    pub workspace_panel: Option<Fetched<Box<types::CodersdkWorkspace>>>,
    /// The deployment's SSH hostname suffix: `None` until loaded, `Some(None)` when it has none.
    pub ssh_suffix: Option<Option<String>>,
```

Add to `Effect`:

```rust
    ShowWorkspace,
    FetchWorkspaceDetails {
        chat: Uuid,
        workspace: Uuid,
    },
    FetchSshSuffix,
    /// Copies `text` and says it copied `what`.
    CopyText {
        text: String,
        what: &'static str,
    },
    /// Opens `<deployment>/@<owner>/<workspace>`, which the runtime builds from its base URL.
    OpenWorkspaceWeb {
        owner: String,
        workspace: String,
    },
```

Add to `Msg`: `WorkspaceDetailsLoaded(Box<types::CodersdkWorkspace>),`, `WorkspaceDetailsFailed(String),`, `SshSuffixLoaded(Option<String>),`, `WorkspaceAction(WorkspaceAction),`, and `WorkspaceClosed,`, with the arms:

```rust
            Msg::WorkspaceDetailsLoaded(ws) => {
                if self.workspace_panel.is_some() {
                    self.workspace_panel = Some(Fetched::Loaded(ws));
                }
                vec![]
            }
            Msg::WorkspaceDetailsFailed(message) => {
                if self.workspace_panel.is_some() {
                    self.workspace_panel = Some(Fetched::Failed(message));
                }
                vec![]
            }
            Msg::SshSuffixLoaded(suffix) => {
                self.ssh_suffix = Some(suffix);
                vec![]
            }
            Msg::WorkspaceAction(action) => self.workspace_action(action),
            Msg::WorkspaceClosed => {
                self.workspace_panel = None;
                vec![]
            }
```

Make the first `Command::Workspace(None)` arm (from Task 23) apply only without an attached workspace, by adding this arm before it:

```rust
            Command::Workspace(None) if self.chat_id.is_some() && self.selected_workspace.is_some() => {
                let (Some(chat), Some(workspace)) = (self.chat_id, self.selected_workspace) else {
                    return vec![];
                };
                self.workspace_panel = Some(Fetched::Loading);
                let mut effects = vec![
                    Effect::ShowWorkspace,
                    Effect::FetchWorkspaceDetails { chat, workspace },
                ];
                if self.ssh_suffix.is_none() {
                    effects.push(Effect::FetchSshSuffix);
                }
                effects
            }
```

Add to `impl App`:

```rust
    fn workspace_action(&mut self, action: WorkspaceAction) -> Vec<Effect> {
        let loaded = match self.workspace_panel.as_ref() {
            Some(Fetched::Loaded(ws)) => Some(ws.clone()),
            _ => None,
        };
        match action {
            WorkspaceAction::CopySsh | WorkspaceAction::OpenWeb => {
                let Some(ws) = loaded else {
                    self.info("The workspace details are still loading.");
                    return vec![];
                };
                let name = ws.name.clone().unwrap_or_default();
                let owner = ws.owner_name.clone().unwrap_or_default();
                if action == WorkspaceAction::OpenWeb {
                    return vec![Effect::OpenWorkspaceWeb {
                        owner,
                        workspace: name,
                    }];
                }
                let agent = panels::workspace_agent(&ws, self.chat.as_ref().and_then(|c| c.agent_id));
                let suffix = self.ssh_suffix.clone().flatten();
                vec![Effect::CopyText {
                    text: panels::ssh_command(agent.as_deref(), &name, &owner, suffix.as_deref()),
                    what: "the SSH command",
                }]
            }
            WorkspaceAction::Detach => {
                self.workspace_panel = None;
                self.set_workspace(None)
            }
            WorkspaceAction::Switch => {
                self.workspace_panel = None;
                vec![Effect::ShowPicker(Picker::Workspace)]
            }
        }
    }
```

In `reset_chat_state`, add `self.workspace_panel = None;`.

- [ ] **Step 4: Run the core tests to verify they pass**

Run: `cargo test -p scuttle-core`
Expected: PASS.

- [ ] **Step 5: Write the failing TUI and runtime tests**

Add `Action(&'static str),` to `RowKey` in `crates/scuttle-tui/src/table.rs`.
Add to the test module of `crates/scuttle-tui/src/overlay.rs`:

```rust
    #[test]
    fn workspace_details_list_the_actions_and_enter_runs_one() {
        let app = App::new(BusyBehavior::Queue, true);
        let mut o = Overlay::WorkspaceDetails(TableState::default());
        let theme = Theme::terminal(true);
        let ctx = ViewCtx {
            app: &app,
            theme: &theme,
            now_unix: 0,
            elapsed: Duration::ZERO,
        };
        let actions: Vec<String> = o
            .view(&ctx)
            .rows
            .iter()
            .filter(|r| r.selectable())
            .map(|r| r.cells[0].to_string())
            .collect();
        assert_eq!(actions, ["Copy SSH command", "Open in web", "Detach", "Switch workspace"]);
        press(&mut o, &app, KeyCode::Down);
        assert!(matches!(
            press(&mut o, &app, KeyCode::Enter),
            OverlayOutcome::CloseWith(Msg::WorkspaceAction(scuttle_core::app::WorkspaceAction::OpenWeb))
        ));
    }
```

Add to the test module of `crates/scuttle-tui/src/runtime.rs`:

```rust
    #[test]
    fn the_workspace_url_keeps_only_the_origin() {
        let base: url::Url = "https://user:pw@coder.example.com/x?y=1".parse().unwrap();
        assert_eq!(
            workspace_web_url(&base, "nick", "dev").as_str(),
            "https://coder.example.com/@nick/dev"
        );
    }

    #[tokio::test]
    async fn the_ssh_suffix_is_read_from_the_deployment() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/deployment/ssh"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "hostname_prefix": "coder.", "hostname_suffix": "coder", "ssh_config_options": {}
            })))
            .mount(&server)
            .await;
        let (mut rt, mut rx) = runtime(&server.uri());
        rt.run(Effect::FetchSshSuffix);
        assert!(matches!(next(&mut rx).await, Msg::SshSuffixLoaded(Some(ref s)) if s == "coder"));
    }
```

- [ ] **Step 6: Run the TUI tests to verify they fail**

Run: `cargo test -p scuttle-tui`
Expected: FAIL to compile, because `Overlay::WorkspaceDetails` and `workspace_web_url` do not exist.

- [ ] **Step 7: Implement the panel, the copy, and the requests**

In `crates/scuttle-tui/src/overlay.rs`, add `use scuttle_core::app::WorkspaceAction;` and:

```rust
const WORKSPACE_ACTIONS: [(&str, &str, WorkspaceAction); 4] = [
    ("ssh", "Copy SSH command", WorkspaceAction::CopySsh),
    ("web", "Open in web", WorkspaceAction::OpenWeb),
    ("detach", "Detach", WorkspaceAction::Detach),
    ("switch", "Switch workspace", WorkspaceAction::Switch),
];

fn workspace_details_view(ctx: &ViewCtx) -> TableView {
    let mut rows: Vec<Row> = scuttle_core::panels::workspace_lines(ctx.app)
        .into_iter()
        .map(|(label, value)| {
            Row::text(vec![
                Line::from(Span::styled(label, ctx.theme.dim)),
                Line::from(value),
            ])
        })
        .collect();
    rows.extend(
        WORKSPACE_ACTIONS
            .iter()
            .map(|(id, label, _)| Row::item(RowKey::Action(id), vec![Line::from(*label)])),
    );
    TableView {
        title: "Workspace".into(),
        widths: vec![Constraint::Length(10), Constraint::Fill(1)],
        rows,
        hint: Some("Enter runs the action, Esc closes".into()),
        ..Default::default()
    }
}
```

Add `WorkspaceDetails(TableState),` to `Overlay` with the other table-only variants in `state` and `state_mut`, add `Overlay::WorkspaceDetails(_) => workspace_details_view(ctx),` to `view`, add `Overlay::WorkspaceDetails(_) => Some(Msg::WorkspaceClosed),` to `close_msg`, and add to the generic `Enter` match:

```rust
                Some(RowKey::Action(id)) => match WORKSPACE_ACTIONS.iter().find(|(a, _, _)| *a == id) {
                    Some((_, _, action)) => OverlayOutcome::CloseWith(Msg::WorkspaceAction(*action)),
                    None => OverlayOutcome::Stay,
                },
```

In `crates/scuttle-tui/src/app.rs`, rename `web_copy_notice` to a general helper and keep its old call site (if M1.6's link copy already generalized it, reuse that helper under its M1.6 name instead):

```rust
/// The notice for copying `text`, which is `what`.
fn copied_notice(what: &str, text: &str, outcome: CopyOutcome) -> Notice {
    match outcome {
        CopyOutcome::Copied => Notice::Info(format!("Copied {what}: {text}")),
        CopyOutcome::CopiedWithWarning(w) => Notice::Info(format!("Copied {what}: {text}. {w}")),
        CopyOutcome::Failed(_) => Notice::Error(format!("Could not copy {what}: {text}")),
    }
}
```

with `Effect::CopyWebUrl` calling `copied_notice("the chat URL", url, outcome)`, and the test `the_web_url_copy_notice_names_the_url` calling `copied_notice("the chat URL", ..)`; the texts are unchanged.
Add to `apply_ui_effect`:

```rust
            Effect::ShowWorkspace => {
                self.overlay = Some(Overlay::WorkspaceDetails(crate::table::TableState::default()));
            }
            Effect::CopyText { text, what } => {
                if let Some(outcome) = self.write_clipboard(text.clone()) {
                    let notice = copied_notice(what, text, outcome);
                    self.notice(notice);
                }
            }
```

In `crates/scuttle-tui/src/runtime.rs`, add `| Effect::ShowWorkspace | Effect::CopyText { .. }` to the ignored effects, and add after `chat_web_url`:

```rust
/// The web UI page for a workspace, on the deployment's origin.
pub fn workspace_web_url(base: &url::Url, owner: &str, workspace: &str) -> url::Url {
    let mut url = base.clone();
    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.set_path(&format!("/@{owner}/{workspace}"));
    url.set_query(None);
    url.set_fragment(None);
    url
}
```

and these arms to `run`:

```rust
            Effect::FetchWorkspaceDetails { chat, workspace } => self.spawn(Box::pin(async move {
                let msg = match client
                    .api()
                    .get_workspace_metadata_by_id(&workspace, None, None)
                    .await
                {
                    Ok(w) => Msg::WorkspaceDetailsLoaded(Box::new(w.into_inner())),
                    Err(e) => Msg::WorkspaceDetailsFailed(err(e).await),
                };
                Msg::ForChat {
                    chat,
                    msg: Box::new(msg),
                }
            })),
            Effect::FetchSshSuffix => self.spawn(Box::pin(async move {
                let suffix = match client.api().ssh_config().await {
                    Ok(c) => c.into_inner().hostname_suffix.filter(|s| !s.is_empty()),
                    Err(_) => None,
                };
                Msg::SshSuffixLoaded(suffix)
            })),
            Effect::OpenWorkspaceWeb { owner, workspace } => {
                let url = workspace_web_url(client.base_url(), &owner, &workspace).to_string();
                self.spawn(Box::pin(async move {
                    let outcome = open_in_browser(&url).await;
                    Msg::WebOpened { url, outcome }
                }));
            }
```

- [ ] **Step 8: Run the tests to verify they pass**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings`
Expected: PASS.

- [ ] **Step 9: Commit**

```bash
cargo fmt --all && git add crates/scuttle-core/src/panels.rs crates/scuttle-core/src/app.rs crates/scuttle-tui/src/table.rs crates/scuttle-tui/src/overlay.rs crates/scuttle-tui/src/app.rs crates/scuttle-tui/src/runtime.rs && git commit -m "feat: show the attached workspace with SSH, web, detach, and switch actions

Assisted-by: AI"
```

---

### Task 25: Repin coder-sdk for the git stream

**Files:**
- Modify: `Cargo.toml` (the `coder-sdk` line)
- Modify: `Cargo.lock`

**Interfaces:**
- Consumes: the Task S2 commit.
- Produces: `coder_sdk::Client::watch_chat_git`, and upgrade errors that carry the server's message, for Task 26.

- [ ] **Step 1: Find the commit to pin**

The commit to pin is the tip of `m2-sdk` when this task runs, or the tip of the SDK's `main` if the author has merged `m2-sdk` into it.
Run: `SDK=~/git/nickvigilante/unofficial-coder-sdk-rs; REV=$(git -C $SDK rev-parse m2-sdk); git -C $SDK log --oneline $REV --grep "stream a chat's workspace git state"`
Expected: one line, which proves Task S2 is in `$REV`, called `<rev>` below.

- [ ] **Step 2: Repin**

In `Cargo.toml`, set the `coder-sdk` line's `rev` to `<rev>`, keeping `git = "https://github.com/nickvigilante/unofficial-coder-sdk-rs"`.
Run: `cargo update -p coder-sdk`
Expected: `Cargo.lock` names `<rev>`.

- [ ] **Step 3: Run the tests**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings`
Expected: PASS; a refused watch or stream now reports the server's message instead of `HTTP 4xx`, which no existing test asserts against.

- [ ] **Step 4: Commit**

```bash
cargo fmt --all --check && git add Cargo.toml Cargo.lock && git commit -m "build: repin coder-sdk for the chat git stream

Assisted-by: AI"
```

---

### Task 26: `/git` panel and the local-changes socket

Depends on M1.6: `Effect::OpenLink` from clickable links (interface 5).

`/git` shows the open chat's repository, provider, branch, pull request with its state, and size, with an action to open the PR, and the workspace's uncommitted changes per repository (design section 13).
Status is the chat's `diff_status` (`codersdk/chats.go:1724-1744`), contents are `GET /api/v2/chats/{chat}/diff` (`coderd/chat_routes.go:88`, `coderd/exp_chats.go:3781-3791`), and without a git token for the provider the server returns the metadata with an empty diff (`coderd/exp_chats.go:4093`), which scuttle explains.
Local changes stream from `GET /api/v2/chats/{chat}/stream/git` (`coderd/chat_routes.go:101`), opened only while the panel is open, as deltas keyed by `repo_root` with `removed` for a gone repository (`codersdk/workspaceagents.go:735-766`); a chat without a workspace gets `400` with a fixed message (`codersdk/chats.go:1756-1772`).
The panel refetches the contents on the watch's `diff_status_change` for the open chat.
When the local-changes socket ends, the panel says so and does not reconnect; reopening `/git` reconnects, because the socket serves only this open panel.

**Files:**
- Modify: `crates/scuttle-core/src/panels.rs` (`GitPanel`, `LocalGit`, `git_lines`, `diff_counts`, tests)
- Modify: `crates/scuttle-core/src/commands.rs` (`Git`, tests)
- Modify: `crates/scuttle-core/src/app.rs` (`git_panel`, `GitAction`, `Msg`, `Effect`, `command`, `apply_watch`, tests)
- Modify: `crates/scuttle-tui/src/overlay.rs` (`Overlay::Git`, `git_view`, keys)
- Modify: `crates/scuttle-tui/src/app.rs` (`ShowGit`)
- Modify: `crates/scuttle-tui/src/runtime.rs` (`git` slot, `FetchDiff`, `OpenGitWatch`, `CloseGitWatch`, tests)

**Interfaces:**
- Consumes: `Fetched`, `Row::text`, `RowKey::Action` (Tasks 22 and 24); `coder_sdk::Client::watch_chat_git` (Task S2 via Task 25); M1.6's `Effect::OpenLink(String)`.
- Produces: `panels::GitPanel { pub diff: Fetched<Box<types::CodersdkChatDiffContents>>, pub repos: BTreeMap<String, types::CodersdkWorkspaceAgentRepoChanges>, pub local: LocalGit }`; `panels::LocalGit { Connecting, Live, NoWorkspace, Ended(String) }`; `panels::diff_counts(diff: &str) -> (usize, usize)`; `panels::git_lines(app: &App) -> Vec<(&'static str, String)>`; `panels::diff_text(panel: &GitPanel) -> Option<String>`; `App::git_panel: Option<GitPanel>`; `app::GitAction { OpenPr }` (Task 27 adds `ViewDiff`); `Command::Git`; `Effect::ShowGit`, `Effect::FetchDiff(Uuid)`, `Effect::OpenGitWatch(Uuid)`, `Effect::CloseGitWatch`; `Msg::DiffLoaded(Box<types::CodersdkChatDiffContents>)`, `Msg::DiffFailed(String)`, `Msg::GitChanges(Box<types::CodersdkWorkspaceAgentGitServerMessage>)`, `Msg::GitWatchEnded(String)` (all in `ForChat`); `Msg::GitAction(GitAction)`; `Msg::GitClosed`.
  Task 27 adds and handles `GitAction::ViewDiff`.

- [ ] **Step 1: Write the failing core tests**

In the test module of `crates/scuttle-core/src/commands.rs`, insert `"/git",` after `"/info",` in `every_listed_command_parses`, and add `assert_eq!(parse("/git"), Ok(Command::Git));` to `parses_attach`.
Add to the test module of `crates/scuttle-core/src/panels.rs`:

```rust
    #[test]
    fn diff_counts_skip_the_file_headers() {
        let diff = "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1 +1,2 @@\n-old\n+new\n+more\n";
        assert_eq!(diff_counts(diff), (2, 1));
    }

    #[test]
    fn git_lines_explain_a_missing_token_and_list_local_changes() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let chat = serde_json::from_value(json!({
            "id": uuid::Uuid::new_v4(), "workspace_id": uuid::Uuid::new_v4(),
            "diff_status": {"pr_number": 7, "pull_request_title": "Fix the watch", "pull_request_state": "open",
                "pull_request_draft": true, "additions": 12, "deletions": 3, "changed_files": 2, "commits": 1,
                "head_branch": "m2", "base_branch": "main"},
            "children": [], "files": [], "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}
        }))
        .unwrap();
        app.update(Msg::ChatLoaded {
            chat: Box::new(chat),
            messages: vec![],
        });
        let mut repos = std::collections::BTreeMap::new();
        repos.insert(
            "/home/coder/scuttle".to_owned(),
            serde_json::from_value(json!({"repo_root": "/home/coder/scuttle", "branch": "m2",
                "unified_diff": "@@ -1 +1 @@\n-a\n+b\n"}))
            .unwrap(),
        );
        app.git_panel = Some(GitPanel {
            diff: Fetched::Loaded(Box::new(
                serde_json::from_value(json!({"provider": "github", "remote_origin": "https://github.com/x/scuttle",
                    "branch": "m2", "diff": ""}))
                .unwrap(),
            )),
            repos,
            local: LocalGit::Live,
        });
        let lines = git_lines(&app);
        assert_eq!(value(&lines, "Repository"), Some("https://github.com/x/scuttle"));
        assert_eq!(value(&lines, "Branch"), Some("m2 into main"));
        assert_eq!(value(&lines, "Pull request"), Some("#7 Fix the watch (open, draft)"));
        assert_eq!(value(&lines, "Size"), Some("+12 -3, 2 files, 1 commits"));
        assert_eq!(value(&lines, "Diff"), Some("Link your github account in Coder to see the diff."));
        assert_eq!(value(&lines, "Local changes"), Some("/home/coder/scuttle (m2): +1 -1"));
    }
```

Add to the test module of `crates/scuttle-core/src/app.rs`:

```rust
    #[test]
    fn git_fetches_the_diff_watches_local_changes_and_merges_their_deltas() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        let mut open = chat(id);
        open.workspace_id = Some(Uuid::new_v4());
        app.update(Msg::ChatLoaded {
            chat: open,
            messages: vec![],
        });
        assert_eq!(
            app.update(Msg::Command(Command::Git)),
            vec![Effect::ShowGit, Effect::FetchDiff(id), Effect::OpenGitWatch(id)]
        );
        let changes = |repos: serde_json::Value| Msg::ForChat {
            chat: id,
            msg: Box::new(Msg::GitChanges(Box::new(
                serde_json::from_value(json!({"type": "changes", "repositories": repos})).unwrap(),
            ))),
        };
        app.update(changes(json!([
            {"repo_root": "/a", "branch": "m2", "unified_diff": "+x\n"},
            {"repo_root": "/b", "branch": "main", "unified_diff": "+y\n"}
        ])));
        app.update(changes(json!([{"repo_root": "/b", "branch": "", "removed": true}])));
        let repos: Vec<&String> = app.git_panel.as_ref().unwrap().repos.keys().collect();
        assert_eq!(repos, ["/a"]);
        assert_eq!(
            app.update(watch("diff_status_change", listed(id, "t", "2026-09-30T10:00:00Z"))),
            vec![Effect::FetchDiff(id)]
        );
        assert_eq!(app.update(Msg::GitClosed), vec![Effect::CloseGitWatch]);
        assert!(app.git_panel.is_none());
    }

    #[test]
    fn git_without_a_workspace_skips_the_local_changes() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            chat: chat(id),
            messages: vec![],
        });
        assert_eq!(
            app.update(Msg::Command(Command::Git)),
            vec![Effect::ShowGit, Effect::FetchDiff(id)]
        );
        assert!(matches!(
            app.git_panel.as_ref().map(|p| &p.local),
            Some(crate::panels::LocalGit::NoWorkspace)
        ));
    }
```

- [ ] **Step 2: Run the core tests to verify they fail**

Run: `cargo test -p scuttle-core`
Expected: FAIL to compile, because `GitPanel`, `Command::Git`, and the git messages do not exist.

- [ ] **Step 3: Implement the panel data**

Add to `crates/scuttle-core/src/panels.rs`, with `use std::collections::BTreeMap;`:

```rust
/// The workspace's local changes, streamed while `/git` is open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalGit {
    Connecting,
    Live,
    NoWorkspace,
    /// The socket ended or was refused, with why.
    Ended(String),
}

#[derive(Debug, Clone)]
pub struct GitPanel {
    pub diff: Fetched<Box<types::CodersdkChatDiffContents>>,
    /// Local changes by repository root, merged from the socket's deltas.
    pub repos: BTreeMap<String, types::CodersdkWorkspaceAgentRepoChanges>,
    pub local: LocalGit,
}

/// Added and removed lines in a unified diff, not counting the `+++` and `---` headers.
pub fn diff_counts(diff: &str) -> (usize, usize) {
    diff.lines().fold((0, 0), |(add, del), line| {
        if line.starts_with("+++") || line.starts_with("---") {
            (add, del)
        } else if line.starts_with('+') {
            (add + 1, del)
        } else if line.starts_with('-') {
            (add, del + 1)
        } else {
            (add, del)
        }
    })
}

/// The diff to page: the server's diff, else the local changes, else nothing.
pub fn diff_text(panel: &GitPanel) -> Option<String> {
    let server = match &panel.diff {
        Fetched::Loaded(d) => d.diff.clone().filter(|d| !d.trim().is_empty()),
        _ => None,
    };
    server.or_else(|| {
        let local: String = panel
            .repos
            .values()
            .filter_map(|r| r.unified_diff.clone())
            .collect::<Vec<_>>()
            .join("\n");
        (!local.trim().is_empty()).then_some(local)
    })
}

/// The `/git` panel as label and value pairs.
pub fn git_lines(app: &App) -> Vec<(&'static str, String)> {
    let Some(panel) = app.git_panel.as_ref() else {
        return vec![];
    };
    let status = app.chat.as_ref().and_then(|c| c.diff_status.as_ref());
    let mut lines = Vec::new();
    let contents = match &panel.diff {
        Fetched::Loaded(d) => Some(d),
        Fetched::Loading => {
            lines.push(("Diff", "Loading…".into()));
            None
        }
        Fetched::Failed(message) => {
            lines.push(("Diff", format!("unavailable: {message}")));
            None
        }
    };
    if let Some(d) = contents {
        if let Some(origin) = d.remote_origin.clone().filter(|o| !o.is_empty()) {
            lines.push(("Repository", origin));
        }
        if let Some(provider) = d.provider.clone().filter(|p| !p.is_empty()) {
            lines.push(("Provider", provider));
        }
    }
    let head = status
        .and_then(|s| s.head_branch.clone())
        .or_else(|| contents.and_then(|d| d.branch.clone()))
        .filter(|b| !b.is_empty());
    if let Some(head) = head {
        let base = status.and_then(|s| s.base_branch.clone()).filter(|b| !b.is_empty());
        lines.push((
            "Branch",
            match base {
                Some(base) => format!("{head} into {base}"),
                None => head,
            },
        ));
    }
    if let Some(s) = status {
        if let Some(n) = s.pr_number {
            let mut marks: Vec<String> = s.pull_request_state.iter().cloned().collect();
            if s.pull_request_draft == Some(true) {
                marks.push("draft".into());
            }
            if s.approved == Some(true) {
                marks.push("approved".into());
            }
            if s.changes_requested == Some(true) {
                marks.push("changes requested".into());
            }
            let title = s.pull_request_title.clone().unwrap_or_default();
            lines.push(("Pull request", format!("#{n} {title} ({})", marks.join(", "))));
        }
        lines.push((
            "Size",
            format!(
                "+{} -{}, {} files, {} commits",
                s.additions.unwrap_or(0),
                s.deletions.unwrap_or(0),
                s.changed_files.unwrap_or(0),
                s.commits.unwrap_or(0)
            ),
        ));
    }
    if let Some(d) = contents
        && d.diff.as_deref().unwrap_or_default().trim().is_empty()
    {
        let changed = status.is_some_and(|s| s.additions.unwrap_or(0) + s.deletions.unwrap_or(0) > 0);
        let text = match d.provider.clone().filter(|p| !p.is_empty()) {
            Some(provider) if changed => format!("Link your {provider} account in Coder to see the diff."),
            _ => "No git changes for this chat yet.".into(),
        };
        lines.push(("Diff", text));
    }
    match &panel.local {
        LocalGit::NoWorkspace => {
            lines.push(("Local changes", "Attach a workspace to see local changes.".into()))
        }
        LocalGit::Connecting if panel.repos.is_empty() => {
            lines.push(("Local changes", "Connecting…".into()))
        }
        LocalGit::Ended(message) if panel.repos.is_empty() => {
            lines.push(("Local changes", message.clone()))
        }
        _ if panel.repos.is_empty() => lines.push(("Local changes", "none".into())),
        _ => {
            for (root, repo) in &panel.repos {
                let (add, del) = diff_counts(repo.unified_diff.as_deref().unwrap_or_default());
                let branch = repo.branch.clone().unwrap_or_default();
                lines.push(("Local changes", format!("{root} ({branch}): +{add} -{del}")));
            }
            if let LocalGit::Ended(message) = &panel.local {
                lines.push(("Local changes", format!("stopped updating: {message}")));
            }
        }
    }
    lines
}
```

In `crates/scuttle-core/src/commands.rs`, add `Git,` to `Command` after `Info`, this entry after `/info`:

```rust
    CommandInfo {
        name: "/git",
        aliases: &[],
        usage: "/git",
        description: "Show this chat's branch, pull request, and local changes",
    },
```

and `"git" => Ok(Command::Git),` to `parse`.
In `crates/scuttle-core/src/app.rs`, add `use crate::panels::{GitPanel, LocalGit};`, and after `WorkspaceAction`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitAction {
    OpenPr,
}
```

Add `pub git_panel: Option<GitPanel>,` to `App`; `ShowGit,`, `FetchDiff(Uuid),`, `OpenGitWatch(Uuid),`, and `CloseGitWatch,` to `Effect`; and `DiffLoaded(Box<types::CodersdkChatDiffContents>),`, `DiffFailed(String),`, `GitChanges(Box<types::CodersdkWorkspaceAgentGitServerMessage>),`, `GitWatchEnded(String),`, `GitAction(GitAction),`, and `GitClosed,` to `Msg`, with the arms:

```rust
            Msg::DiffLoaded(diff) => {
                if let Some(panel) = self.git_panel.as_mut() {
                    panel.diff = Fetched::Loaded(diff);
                }
                vec![]
            }
            Msg::DiffFailed(message) => {
                if let Some(panel) = self.git_panel.as_mut() {
                    panel.diff = Fetched::Failed(message);
                }
                vec![]
            }
            Msg::GitChanges(message) => {
                let Some(panel) = self.git_panel.as_mut() else {
                    return vec![];
                };
                match message.type_.as_ref().map(|t| t.0.as_str()) {
                    Some("error") => {
                        panel.local = LocalGit::Ended(message.message.clone().unwrap_or_default());
                    }
                    _ => {
                        panel.local = LocalGit::Live;
                        for repo in message.repositories {
                            let Some(root) = repo.repo_root.clone() else {
                                continue;
                            };
                            if repo.removed == Some(true) {
                                panel.repos.remove(&root);
                            } else {
                                panel.repos.insert(root, repo);
                            }
                        }
                    }
                }
                vec![]
            }
            Msg::GitWatchEnded(message) => {
                if let Some(panel) = self.git_panel.as_mut() {
                    panel.local = LocalGit::Ended(message);
                }
                vec![]
            }
            Msg::GitAction(action) => self.git_action(action),
            Msg::GitClosed => match self.git_panel.take() {
                Some(_) => vec![Effect::CloseGitWatch],
                None => vec![],
            },
```

Add to `command`, before `Command::New`:

```rust
            Command::Git => {
                let Some(chat) = self.chat_id else {
                    self.error("Start a chat first.");
                    return vec![];
                };
                let has_workspace = self.selected_workspace.is_some();
                self.git_panel = Some(GitPanel {
                    diff: Fetched::Loading,
                    repos: Default::default(),
                    local: if has_workspace {
                        LocalGit::Connecting
                    } else {
                        LocalGit::NoWorkspace
                    },
                });
                let mut effects = vec![Effect::ShowGit, Effect::FetchDiff(chat)];
                if has_workspace {
                    effects.push(Effect::OpenGitWatch(chat));
                }
                effects
            }
```

Add to `impl App`:

```rust
    fn git_action(&mut self, action: GitAction) -> Vec<Effect> {
        match action {
            GitAction::OpenPr => {
                let status = self.chat.as_ref().and_then(|c| c.diff_status.as_ref());
                let from_diff = match self.git_panel.as_ref().map(|p| &p.diff) {
                    Some(Fetched::Loaded(d)) => d.pull_request_url.clone(),
                    _ => None,
                };
                match from_diff
                    .or_else(|| status.and_then(|s| s.url.clone()))
                    .filter(|u| !u.is_empty())
                {
                    // M1.6's link opener: a browser, or a copy when none opens.
                    Some(url) => vec![Effect::OpenLink(url)],
                    None => {
                        self.info("This chat has no pull request yet.");
                        vec![]
                    }
                }
            }
        }
    }
```

In `reset_chat_state`, close the panel with the chat: add `if self.git_panel.take().is_some() { effects.push(Effect::CloseGitWatch); }` after the `ClearView` push.
In `apply_watch`, change the `"diff_status_change"` arm to:

```rust
            "diff_status_change" => {
                record.diff_status = chat.diff_status.clone();
                if self.git_panel.is_some() {
                    effects.push(Effect::FetchDiff(open));
                }
            }
```

- [ ] **Step 4: Run the core tests to verify they pass**

Run: `cargo test -p scuttle-core`
Expected: PASS.

- [ ] **Step 5: Write the failing runtime test**

Add to the test module of `crates/scuttle-tui/src/runtime.rs`:

```rust
    #[tokio::test]
    async fn the_git_watch_reports_changes_and_a_refusal_with_its_message() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
            let frame = serde_json::json!({"type": "changes", "repositories": [
                {"repo_root": "/a", "branch": "m2", "unified_diff": "+x\n"}
            ]});
            ws.send(Message::text(frame.to_string())).await.unwrap();
            while ws.next().await.is_some() {}
        });
        let (mut rt, mut rx) = runtime(&format!("http://{addr}"));
        let chat = Uuid::new_v4();
        rt.run(Effect::OpenGitWatch(chat));
        match next(&mut rx).await {
            Msg::ForChat { chat: tagged, msg } => {
                assert_eq!(tagged, chat);
                assert!(matches!(*msg, Msg::GitChanges(ref m) if m.repositories.len() == 1), "{msg:?}");
            }
            other => panic!("expected tagged changes, got {other:?}"),
        }
        rt.run(Effect::CloseGitWatch);

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(format!("/api/v2/chats/{chat}/stream/git")))
            .respond_with(api_error(400, "Chat has no workspace to watch."))
            .mount(&server)
            .await;
        let (mut rt, mut rx) = runtime(&server.uri());
        rt.run(Effect::OpenGitWatch(chat));
        assert!(matches!(
            untag(next(&mut rx).await),
            Msg::GitWatchEnded(ref m) if m == "Chat has no workspace to watch."
        ));
    }
```

- [ ] **Step 6: Run the runtime test to verify it fails**

Run: `cargo test -p scuttle-tui the_git_watch`
Expected: FAIL: the runtime ignores `OpenGitWatch`, so the channel times out.

- [ ] **Step 7: Implement the requests, the socket, and the panel**

In `crates/scuttle-tui/src/runtime.rs`, add `git: Option<JoinHandle<()>>,` to `Runtime` with `git: None,` in `new`, add `| Effect::ShowGit` to the ignored effects, and add these arms:

```rust
            Effect::FetchDiff(chat) => self.spawn(Box::pin(async move {
                let msg = match client.api().get_chat_diff_contents(&chat).await {
                    Ok(d) => Msg::DiffLoaded(Box::new(d.into_inner())),
                    Err(e) => Msg::DiffFailed(err(e).await),
                };
                Msg::ForChat {
                    chat,
                    msg: Box::new(msg),
                }
            })),
            Effect::OpenGitWatch(chat) => {
                if let Some(old) = self.git.take() {
                    old.abort();
                }
                let tx = self.tx.clone();
                let tagged = move |msg: Msg| Msg::ForChat {
                    chat,
                    msg: Box::new(msg),
                };
                self.git = Some(tokio::spawn(async move {
                    let mut stream = match client.watch_chat_git(chat).await {
                        Ok(s) => s,
                        Err(e) => {
                            let message = match e {
                                coder_sdk::Error::Api { message, .. } => message,
                                other => other.to_string(),
                            };
                            let _ = tx.send(tagged(Msg::GitWatchEnded(message)));
                            return;
                        }
                    };
                    while let Some(item) = stream.next().await {
                        match item {
                            Ok(message) => {
                                let _ = tx.send(tagged(Msg::GitChanges(Box::new(message))));
                            }
                            Err(coder_sdk::Error::Decode(_)) => continue,
                            Err(e) => {
                                let _ = tx.send(tagged(Msg::GitWatchEnded(e.to_string())));
                                return;
                            }
                        }
                    }
                    let _ = tx.send(tagged(Msg::GitWatchEnded("the connection closed".into())));
                }));
            }
            Effect::CloseGitWatch => {
                if let Some(old) = self.git.take() {
                    old.abort();
                }
            }
```

In `crates/scuttle-tui/src/overlay.rs`, add `use scuttle_core::app::GitAction;` and:

```rust
fn git_view(ctx: &ViewCtx) -> TableView {
    let mut rows: Vec<Row> = scuttle_core::panels::git_lines(ctx.app)
        .into_iter()
        .map(|(label, value)| {
            Row::text(vec![
                Line::from(Span::styled(label, ctx.theme.dim)),
                Line::from(value),
            ])
        })
        .collect();
    rows.push(Row::item(RowKey::Action("pr"), vec![Line::from("Open pull request")]));
    TableView {
        title: "Git".into(),
        widths: vec![Constraint::Length(14), Constraint::Fill(1)],
        rows,
        hint: Some("Enter runs the action, Esc closes".into()),
        ..Default::default()
    }
}
```

Add `Git(TableState),` to `Overlay` with the table-only variants, make it full height, add `Overlay::Git(_) => git_view(ctx),` to `view`, add `Overlay::Git(_) => Some(Msg::GitClosed),` to `close_msg` (so Esc and Ctrl+C both close the socket with the panel), and in the generic `Enter` match, before the workspace actions, add:

```rust
                Some(RowKey::Action("pr")) => OverlayOutcome::Send(Msg::GitAction(GitAction::OpenPr)),
```

In `crates/scuttle-tui/src/app.rs`, add `Effect::ShowGit => self.overlay = Some(Overlay::Git(crate::table::TableState::default())),` to `apply_ui_effect`.

- [ ] **Step 8: Run the tests to verify they pass**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings`
Expected: PASS.

- [ ] **Step 9: Commit**

```bash
cargo fmt --all && git add crates/scuttle-core/src/panels.rs crates/scuttle-core/src/commands.rs crates/scuttle-core/src/app.rs crates/scuttle-tui/src/overlay.rs crates/scuttle-tui/src/app.rs crates/scuttle-tui/src/runtime.rs && git commit -m "feat: show the chat's branch, pull request, and local changes with /git

Assisted-by: AI"
```

---

### Task 27: `/diff` and the pager handoff

`/git`'s "View diff" and `/diff` hand the diff to the user's pager: `$GIT_PAGER`, else `git config core.pager` in the current directory, else `$PAGER`, else `less -R`, which is git's own order, so delta applies when git uses it (design section 13, decision "Diff viewing").
scuttle hands the terminal over as it does for `$EDITOR`: it leaves the alternate screen and mouse capture, pipes the diff to the pager's standard input, waits, and restores; delta reads a unified diff on stdin, so no temporary file is needed.
The design puts the pager next to the editor handoff in the TUI; the terminal handoff stays there, and the pager resolution, `git config`, and the pager process live in `runtime.rs`, as the global constraint requires.
The diff paged is the server's diff, else the workspace's local changes; with neither, scuttle says "No git changes for this chat yet."

**Files:**
- Modify: `crates/scuttle-core/src/commands.rs` (`Diff`, tests)
- Modify: `crates/scuttle-core/src/app.rs` (`Effect::Page`, `page_when_loaded`, `Command::Diff`, `GitAction::ViewDiff`, `DiffLoaded`, tests)
- Modify: `crates/scuttle-tui/src/runtime.rs` (`pager_command`, `git_config_pager`, `run_pager`, tests)
- Modify: `crates/scuttle-tui/src/overlay.rs` (the "View diff" action)
- Modify: `crates/scuttle-tui/src/app.rs` (`page`, `last_paged`, `must_quit`, tests)
- Modify: `crates/scuttle-tui/src/main.rs` (quit when the terminal cannot be restored after the pager)

**Interfaces:**
- Consumes: `GitPanel`, `panels::diff_text`, `Effect::FetchDiff`, `GitAction` (Task 26); `editor_round_trip` and `finish_editor` in the TUI.
- Produces: `Command::Diff`; `GitAction::ViewDiff`; `Effect::Page(String)`; `runtime::pager_command(env: impl Fn(&str) -> Option<String>, git_pager: impl FnOnce() -> Option<String>) -> String`; `runtime::git_config_pager() -> Option<String>`; `runtime::run_pager(command: &str, text: &str) -> std::io::Result<()>`; `Tui::last_paged: Option<String>`; `Tui::must_quit(&self) -> bool`.

- [ ] **Step 1: Write the failing tests**

In the test module of `crates/scuttle-core/src/commands.rs`, insert `"/diff",` after `"/git",` in `every_listed_command_parses`, and add `assert_eq!(parse("/diff"), Ok(Command::Diff));` to `parses_attach`.
Add to the test module of `crates/scuttle-core/src/app.rs`:

```rust
    #[test]
    fn diff_pages_the_servers_diff_once_it_loads_or_says_there_is_none() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            chat: chat(id),
            messages: vec![],
        });
        assert_eq!(app.update(Msg::Command(Command::Diff)), vec![Effect::FetchDiff(id)]);
        let loaded = |diff: &str| Msg::ForChat {
            chat: id,
            msg: Box::new(Msg::DiffLoaded(Box::new(
                serde_json::from_value(json!({"diff": diff})).unwrap(),
            ))),
        };
        assert_eq!(
            app.update(loaded("diff --git a/x b/x\n+new\n")),
            vec![Effect::Page("diff --git a/x b/x\n+new\n".into())]
        );
        app.update(Msg::Command(Command::Diff));
        assert!(app.update(loaded("")).is_empty());
        assert_eq!(
            app.notices.last(),
            Some(&Notice::Info("No git changes for this chat yet.".into()))
        );
    }
```

Add to the test module of `crates/scuttle-tui/src/runtime.rs`:

```rust
    #[test]
    fn the_pager_follows_gits_order() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| pairs.iter().find(|(key, _)| *key == k).map(|(_, v)| v.to_string())
        };
        assert_eq!(pager_command(env(&[("GIT_PAGER", "delta"), ("PAGER", "more")]), || Some("bat".into())), "delta");
        assert_eq!(pager_command(env(&[("PAGER", "more")]), || Some("bat".into())), "bat");
        assert_eq!(pager_command(env(&[("PAGER", "more")]), || None), "more");
        assert_eq!(pager_command(env(&[]), || None), "less -R");
    }

    #[test]
    fn the_pager_gets_the_text_on_its_standard_input() {
        let out = std::env::temp_dir().join(format!("scuttle-pager-{}", Uuid::new_v4()));
        run_pager(&format!("cat > '{}'", out.display()), "+new line\n").unwrap();
        assert_eq!(std::fs::read_to_string(&out).unwrap(), "+new line\n");
        std::fs::remove_file(out).unwrap();
    }
```

Add to the test module of `crates/scuttle-tui/src/app.rs`:

```rust
    #[test]
    fn a_page_effect_hands_the_text_over_and_redraws() {
        let mut t = tui();
        assert!(t.apply_ui_effect(&Effect::Page("+x\n".into())));
        assert_eq!(t.last_paged.as_deref(), Some("+x\n"));
        assert!(t.take_full_redraw());
        assert!(!t.must_quit());
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --workspace`
Expected: FAIL to compile, because `Command::Diff`, `Effect::Page`, and the pager functions do not exist.

- [ ] **Step 3: Implement paging in the core**

In `crates/scuttle-core/src/commands.rs`, add `Diff,` to `Command` after `Git`, this entry after `/git`:

```rust
    CommandInfo {
        name: "/diff",
        aliases: &[],
        usage: "/diff",
        description: "Show this chat's diff in your pager",
    },
```

and `"diff" => Ok(Command::Diff),` to `parse`.
In `crates/scuttle-core/src/app.rs`, add `Page(String),` to `Effect` with the doc comment `/// Shows text in the user's pager, which takes over the terminal until it exits.`, and `page_when_loaded: bool,` to `App`.
Add to `command`, before `Command::New`:

```rust
            Command::Diff => {
                let Some(chat) = self.chat_id else {
                    self.error("Start a chat first.");
                    return vec![];
                };
                self.page_when_loaded = true;
                vec![Effect::FetchDiff(chat)]
            }
```

Change the `Msg::DiffLoaded` arm to page a diff that `/diff` asked for, whether or not the panel is open:

```rust
            Msg::DiffLoaded(diff) => {
                let wanted = std::mem::take(&mut self.page_when_loaded);
                let text = diff.diff.clone().filter(|d| !d.trim().is_empty());
                if let Some(panel) = self.git_panel.as_mut() {
                    panel.diff = Fetched::Loaded(diff);
                }
                if !wanted {
                    return vec![];
                }
                let text = text.or_else(|| self.git_panel.as_ref().and_then(crate::panels::diff_text));
                match text {
                    Some(text) => vec![Effect::Page(text)],
                    None => {
                        self.info("No git changes for this chat yet.");
                        vec![]
                    }
                }
            }
```

and replace the `Msg::DiffFailed` arm with:

```rust
            Msg::DiffFailed(message) => {
                if std::mem::take(&mut self.page_when_loaded) {
                    self.error(format!("Could not load the diff: {message}"));
                }
                if let Some(panel) = self.git_panel.as_mut() {
                    panel.diff = Fetched::Failed(message);
                }
                vec![]
            }
```

Add `ViewDiff,` to `GitAction`, and this arm to `git_action`:

```rust
            GitAction::ViewDiff => match self.git_panel.as_ref().and_then(crate::panels::diff_text) {
                Some(text) => vec![Effect::Page(text)],
                None => {
                    self.info("No git changes for this chat yet.");
                    vec![]
                }
            },
```

- [ ] **Step 4: Implement the pager in the runtime and the handoff in the TUI**

Add to `crates/scuttle-tui/src/runtime.rs`, after `open_in_browser`:

```rust
/// The pager for diffs, in git's order: `$GIT_PAGER`, then `git config core.pager`, then
/// `$PAGER`, then `less -R`. `env` returns only non-empty values.
pub fn pager_command(
    env: impl Fn(&str) -> Option<String>,
    git_pager: impl FnOnce() -> Option<String>,
) -> String {
    env("GIT_PAGER")
        .or_else(git_pager)
        .or_else(|| env("PAGER"))
        .unwrap_or_else(|| "less -R".into())
}

/// `git config core.pager` in the current directory, if git is installed and it is set.
pub fn git_config_pager() -> Option<String> {
    let out = std::process::Command::new("git")
        .args(["config", "core.pager"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    let pager = String::from_utf8(out.stdout).ok()?.trim().to_owned();
    (out.status.success() && !pager.is_empty()).then_some(pager)
}

/// Runs `command` through `sh` with `text` on its standard input, and waits for it. A pager
/// the user quits early closes its input, so a failed write is not an error.
pub fn run_pager(command: &str, text: &str) -> std::io::Result<()> {
    use std::io::Write;
    let mut child = std::process::Command::new("sh")
        .arg("-c")
        .arg(command)
        .stdin(std::process::Stdio::piped())
        .spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(text.as_bytes());
    }
    let status = child.wait()?;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!("the pager exited with {status}")))
    }
}
```

and add `| Effect::Page(_)` to the ignored effects.
In `crates/scuttle-tui/src/app.rs`, add `pub last_paged: Option<String>,` to `Tui` (initialized to `None`), and:

```rust
    /// Whether the app must quit now, for example because the terminal could not be restored.
    pub fn must_quit(&self) -> bool {
        self.fatal.is_some()
    }

    /// Hands the terminal to the pager with `text`, then takes it back. Tests only record it.
    fn page(&mut self, text: &str) {
        self.last_paged = Some(text.to_owned());
        self.needs_full_redraw = true;
        if cfg!(test) {
            return;
        }
        let command = crate::runtime::pager_command(
            |k| std::env::var(k).ok().filter(|v| !v.trim().is_empty()),
            crate::runtime::git_config_pager,
        );
        let mouse = self.core.mouse;
        let (paged, resumed) = editor_round_trip(
            crate::terminal::leave,
            || crate::runtime::run_pager(&command, text),
            || crate::terminal::resume(mouse),
        );
        self.set_keyboard_enhanced(crate::terminal::keyboard_enhanced());
        if let Err(e) = paged {
            self.notice(Notice::Error(format!("The pager failed: {e}")));
        }
        let _ = self.finish_editor(resumed);
    }
```

add `Effect::Page(text) => self.page(text),` to `apply_ui_effect`, and change the doc comment of `finish_editor` to say it serves the pager too.
In `crates/scuttle-tui/src/overlay.rs`, push `Row::item(RowKey::Action("diff"), vec![Line::from("View diff")])` after the "Open pull request" row in `git_view`, and add `Some(RowKey::Action("diff")) => OverlayOutcome::Send(Msg::GitAction(GitAction::ViewDiff)),` next to the `"pr"` arm.
In `crates/scuttle-tui/src/main.rs`, right after the `for effect in std::mem::take(&mut pending) { .. }` loop, add:

```rust
        if tui.must_quit() {
            break ExitCode::FAILURE;
        }
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
cargo fmt --all && git add crates/scuttle-core/src/commands.rs crates/scuttle-core/src/app.rs crates/scuttle-tui/src/runtime.rs crates/scuttle-tui/src/overlay.rs crates/scuttle-tui/src/app.rs crates/scuttle-tui/src/main.rs && git commit -m "feat: page the chat's diff through the user's git pager with /diff

Assisted-by: AI"
```

---

### Task 28: Repin coder-sdk for MCP connect outcomes

**Files:**
- Modify: `Cargo.toml` (the `coder-sdk` line)
- Modify: `Cargo.lock`

**Interfaces:**
- Consumes: the Task S3 commit.
- Produces: `coder_sdk::Client::latest_mcp_connect` and `coder_sdk::McpConnectOutcome` for Task 29.

- [ ] **Step 1: Find the commit to pin**

The commit to pin is the tip of `m2-sdk` when this task runs, or the tip of the SDK's `main` if the author has merged `m2-sdk` into it.
Run: `SDK=~/git/nickvigilante/unofficial-coder-sdk-rs; REV=$(git -C $SDK rev-parse m2-sdk); git -C $SDK log --oneline $REV --grep "latest MCP connect outcomes"`
Expected: one line, which proves Task S3 is in `$REV`, called `<rev>` below.

- [ ] **Step 2: Repin**

In `Cargo.toml`, set the `coder-sdk` line's `rev` to `<rev>`, keeping the `file://` URL.
Run: `cargo update -p coder-sdk`
Expected: `Cargo.lock` names `<rev>`.

- [ ] **Step 3: Run the tests**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings`
Expected: PASS with no code change.

- [ ] **Step 4: Commit**

```bash
cargo fmt --all --check && git add Cargo.toml Cargo.lock && git commit -m "build: repin coder-sdk for MCP connect outcomes

Assisted-by: AI"
```

---

### Task 29: `/mcp` list and health

`/mcp` lists the MCP servers the chat can use in three groups, with what scuttle can tell about each (design section 14): organization servers from `GET /api/v2/organizations/{org}/mcp-servers` (`coderd/chat_routes.go:147-148`, `coderd/mcp.go:144-155`) joined with the chat's `mcp_server_ids` (`codersdk/chats.go:168`), inline servers from the single-chat GET (`codersdk/chats.go:184-187`), and workspace servers from `context.resources` with `kind: "mcp_server"` (`codersdk/chats.go:217-251`).
Each row shows the display name or slug, the URL, whether it is on for the chat, and its health, with the tool allow and deny lists when set; a `force_on` server is always on (`site/src/pages/AgentsPage/utils/mcpSelection.ts:7-21`).
No chat, stream, or watch API reports whether a server connected (a server-side gap, design section 19), so health is: `auth_connected: false` as "needs reconnecting in the web UI" (`codersdk/mcp.go:86-103`), a workspace server's non-ok `status` and `error`, and, when debug logging is on (`GET /api/v2/chats/config/user-debug-logging`, `coderd/chat_routes.go:53`), the newest debug run's connect outcome per server; otherwise the panel says "Connection health is not reported by the server."

**Files:**
- Modify: `crates/scuttle-core/src/commands.rs` (`Mcp`, tests)
- Modify: `crates/scuttle-core/src/panels.rs` (`McpPanel`, `McpGroup`, `McpRow`, `mcp_groups`, tests)
- Modify: `crates/scuttle-core/src/app.rs` (`mcp_panel`, `Msg`, `Effect`, `command`, tests)
- Modify: `crates/scuttle-tui/src/table.rs` (`RowKey::Mcp`)
- Modify: `crates/scuttle-tui/src/overlay.rs` (`Overlay::Mcp`, `mcp_view`)
- Modify: `crates/scuttle-tui/src/app.rs` (`ShowMcp`)
- Modify: `crates/scuttle-tui/src/runtime.rs` (`FetchMcpServers`, `FetchMcpHealth`, tests)

**Interfaces:**
- Consumes: `Fetched`, `Row::text`, `close_msg` (Tasks 22 and 24); `coder_sdk::McpConnectOutcome` and `Client::latest_mcp_connect` (Task S3 via Task 28).
- Produces: `panels::McpPanel { pub servers: Fetched<Vec<types::CodersdkMcpServerConfig>>, pub health: Fetched<Option<Vec<coder_sdk::McpConnectOutcome>>> }`; `panels::McpRow { pub id: Option<Uuid>, pub name: String, pub url: String, pub state: String, pub detail: String }`; `panels::McpGroup { pub title: &'static str, pub rows: Vec<McpRow> }`; `panels::mcp_groups(app: &App) -> (Vec<McpGroup>, Option<String>)` (the groups and a status note); `App::mcp_panel: Option<McpPanel>`; `Command::Mcp`; `Effect::ShowMcp`, `Effect::FetchMcpServers { chat: Uuid, org: Uuid }`, `Effect::FetchMcpHealth(Uuid)`; `Msg::McpServersLoaded(Vec<types::CodersdkMcpServerConfig>)`, `Msg::McpServersFailed(String)`, `Msg::McpHealthLoaded(Option<Vec<coder_sdk::McpConnectOutcome>>)`, `Msg::McpHealthFailed(String)` (in `ForChat`); `Msg::McpClosed`; `RowKey::Mcp(Uuid)`.
  Task 31 toggles `RowKey::Mcp` rows.

- [ ] **Step 1: Write the failing core tests**

In the test module of `crates/scuttle-core/src/commands.rs`, insert `"/mcp",` after `"/diff",` in `every_listed_command_parses`, and add `assert_eq!(parse("/mcp"), Ok(Command::Mcp));` to `parses_attach`.
Add to the test module of `crates/scuttle-core/src/panels.rs`:

```rust
    #[test]
    fn mcp_groups_join_the_selection_with_health_and_list_every_source() {
        let (github, linear, docs) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let mut app = App::new(BusyBehavior::Queue, true);
        let chat = serde_json::from_value(json!({
            "id": uuid::Uuid::new_v4(), "mcp_server_ids": [github],
            "inline_mcp_servers": [{"slug": "local-tools", "url": "http://localhost:9000/mcp", "tool_allow_list": [], "tool_deny_list": []}],
            "context": {"resources": [{"kind": "mcp_server", "source": "playwright", "status": "error",
                "error": "spawn failed", "tools": []}]},
            "children": [], "files": [], "labels": {}
        }))
        .unwrap();
        app.update(Msg::ChatLoaded {
            chat: Box::new(chat),
            messages: vec![],
        });
        let server = |id, name: &str, availability: &str, auth: bool| -> coder_sdk::types::CodersdkMcpServerConfig {
            serde_json::from_value(json!({"id": id, "display_name": name, "url": format!("https://{name}.example/mcp"),
                "availability": availability, "enabled": true, "auth_connected": auth,
                "tool_allow_list": [], "tool_deny_list": ["delete_repo"]}))
            .unwrap()
        };
        app.mcp_panel = Some(McpPanel {
            servers: Fetched::Loaded(vec![
                server(github, "GitHub", "default_off", true),
                server(linear, "Linear", "default_off", false),
                server(docs, "Docs", "force_on", true),
            ]),
            health: Fetched::Loaded(Some(vec![coder_sdk::McpConnectOutcome {
                config_id: github,
                slug: "github".into(),
                outcome: "connected".into(),
                tool_count: 12,
                error: String::new(),
            }])),
        });
        let (groups, note) = mcp_groups(&app);
        assert_eq!(note, None);
        let titles: Vec<&str> = groups.iter().map(|g| g.title).collect();
        assert_eq!(titles, ["Organization servers", "Inline servers", "Workspace servers"]);
        let org: Vec<(&str, &str, &str)> = groups[0]
            .rows
            .iter()
            .map(|r| (r.name.as_str(), r.state.as_str(), r.detail.as_str()))
            .collect();
        assert_eq!(
            org,
            [
                ("GitHub", "on", "connected, 12 tools; deny: delete_repo"),
                ("Linear", "off", "needs reconnecting in the web UI; deny: delete_repo"),
                ("Docs", "on (required)", "deny: delete_repo"),
            ]
        );
        assert_eq!(groups[1].rows[0].url, "http://localhost:9000/mcp");
        assert_eq!(groups[2].rows[0].detail, "error: spawn failed");
        app.mcp_panel.as_mut().unwrap().health = Fetched::Loaded(None);
        assert_eq!(
            mcp_groups(&app).1.as_deref(),
            Some("Connection health is not reported by the server.")
        );
    }
```

Add to the test module of `crates/scuttle-core/src/app.rs`:

```rust
    #[test]
    fn mcp_fetches_the_servers_and_health_for_the_chats_organization() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let (id, org) = (Uuid::new_v4(), Uuid::new_v4());
        let mut open = chat(id);
        open.organization_id = Some(org);
        app.update(Msg::ChatLoaded {
            chat: open,
            messages: vec![],
        });
        assert_eq!(
            app.update(Msg::Command(Command::Mcp)),
            vec![
                Effect::ShowMcp,
                Effect::RefreshChat(id),
                Effect::FetchMcpServers { chat: id, org },
                Effect::FetchMcpHealth(id)
            ]
        );
        app.update(Msg::ForChat {
            chat: id,
            msg: Box::new(Msg::McpHealthLoaded(None)),
        });
        assert!(matches!(
            app.mcp_panel.as_ref().map(|p| &p.health),
            Some(Fetched::Loaded(None))
        ));
        app.update(Msg::McpClosed);
        assert!(app.mcp_panel.is_none());
    }
```

- [ ] **Step 2: Run the core tests to verify they fail**

Run: `cargo test -p scuttle-core`
Expected: FAIL to compile, because `McpPanel`, `mcp_groups`, and `Command::Mcp` do not exist.

- [ ] **Step 3: Implement the groups and the panel state**

Add to `crates/scuttle-core/src/panels.rs`:

```rust
#[derive(Debug, Clone)]
pub struct McpPanel {
    pub servers: Fetched<Vec<types::CodersdkMcpServerConfig>>,
    /// The newest debug run's connect outcomes; `Loaded(None)` when the server reports none.
    pub health: Fetched<Option<Vec<coder_sdk::McpConnectOutcome>>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpRow {
    /// Set for an organization server, which Task 31 can toggle.
    pub id: Option<Uuid>,
    pub name: String,
    pub url: String,
    pub state: String,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpGroup {
    pub title: &'static str,
    pub rows: Vec<McpRow>,
}

fn outcome_text(o: &coder_sdk::McpConnectOutcome) -> String {
    match o.outcome.as_str() {
        "connected" => format!("connected, {} tools", o.tool_count),
        "no_tools" => "connected, no tools".into(),
        "timeout" => "timed out connecting".into(),
        "error" => format!("failed: {}", o.error),
        other => other.replace('_', " "),
    }
}

fn tool_lists(allow: &[String], deny: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    if !allow.is_empty() {
        out.push(format!("allow: {}", allow.join(", ")));
    }
    if !deny.is_empty() {
        out.push(format!("deny: {}", deny.join(", ")));
    }
    out
}

/// The `/mcp` groups, empty groups left out, and a note for the status line.
pub fn mcp_groups(app: &App) -> (Vec<McpGroup>, Option<String>) {
    let (Some(chat), Some(panel)) = (app.chat.as_deref(), app.mcp_panel.as_ref()) else {
        return (vec![], None);
    };
    let outcomes = match &panel.health {
        Fetched::Loaded(Some(list)) => list.as_slice(),
        _ => &[],
    };
    let mut groups = Vec::new();
    let mut note = match &panel.health {
        Fetched::Loaded(None) => Some("Connection health is not reported by the server.".to_owned()),
        Fetched::Failed(message) => Some(format!("Connection health is unavailable: {message}")),
        _ => None,
    };
    match &panel.servers {
        Fetched::Loaded(servers) => {
            let rows: Vec<McpRow> = servers
                .iter()
                .filter(|s| s.enabled != Some(false))
                .map(|s| {
                    let required = s.availability.as_deref() == Some("force_on");
                    let on = required || s.id.is_some_and(|id| chat.mcp_server_ids.contains(&id));
                    let mut detail = Vec::new();
                    if s.auth_connected == Some(false) {
                        detail.push("needs reconnecting in the web UI".to_owned());
                    } else if let Some(o) = outcomes.iter().find(|o| Some(o.config_id) == s.id) {
                        detail.push(outcome_text(o));
                    }
                    detail.extend(tool_lists(&s.tool_allow_list, &s.tool_deny_list));
                    McpRow {
                        id: s.id,
                        name: s.display_name.clone().or(s.slug.clone()).unwrap_or_default(),
                        url: s.url.clone().unwrap_or_default(),
                        state: match (on, required) {
                            (true, true) => "on (required)".into(),
                            (true, false) => "on".into(),
                            (false, _) => "off".into(),
                        },
                        detail: detail.join("; "),
                    }
                })
                .collect();
            if !rows.is_empty() {
                groups.push(McpGroup {
                    title: "Organization servers",
                    rows,
                });
            }
        }
        Fetched::Loading => note = Some("Loading MCP servers…".into()),
        Fetched::Failed(message) => note = Some(format!("Organization servers failed to load: {message}")),
    }
    let inline: Vec<McpRow> = chat
        .inline_mcp_servers
        .iter()
        .map(|s| McpRow {
            id: None,
            name: s.slug.clone().unwrap_or_default(),
            url: s.url.clone().unwrap_or_default(),
            state: "on".into(),
            detail: tool_lists(&s.tool_allow_list, &s.tool_deny_list).join("; "),
        })
        .collect();
    if !inline.is_empty() {
        groups.push(McpGroup {
            title: "Inline servers",
            rows: inline,
        });
    }
    let workspace: Vec<McpRow> = chat
        .context
        .iter()
        .flat_map(|c| c.resources.iter())
        .filter(|r| r.kind.as_ref().is_some_and(|k| k.as_str() == "mcp_server"))
        .map(|r| {
            let status = r.status.as_ref().map(|s| s.as_str().to_owned()).unwrap_or_default();
            let tools: Vec<String> = r.tools.iter().filter_map(|t| t.name.clone()).collect();
            let detail = match r.error.clone().filter(|e| !e.is_empty()) {
                Some(error) => format!("{status}: {error}"),
                None if !tools.is_empty() => format!("{} tools: {}", tools.len(), tools.join(", ")),
                None => status,
            };
            McpRow {
                id: None,
                name: r.source.clone().unwrap_or_default(),
                url: String::new(),
                state: "on".into(),
                detail,
            }
        })
        .collect();
    if !workspace.is_empty() {
        groups.push(McpGroup {
            title: "Workspace servers",
            rows: workspace,
        });
    }
    if groups.is_empty() && matches!(panel.servers, Fetched::Loaded(_)) {
        note = Some("This chat uses no MCP servers.".into());
    }
    (groups, note)
}
```

In `crates/scuttle-core/src/commands.rs`, add `Mcp,` to `Command` after `Diff`, this entry after `/diff`:

```rust
    CommandInfo {
        name: "/mcp",
        aliases: &[],
        usage: "/mcp",
        description: "List this chat's MCP servers and what is known of their health",
    },
```

and `"mcp" => Ok(Command::Mcp),` to `parse`.
In `crates/scuttle-core/src/app.rs`, add `use crate::panels::McpPanel;`, add `pub mcp_panel: Option<McpPanel>,` to `App`, `ShowMcp,`, `FetchMcpServers { chat: Uuid, org: Uuid },`, and `FetchMcpHealth(Uuid),` to `Effect`, and to `Msg`:

```rust
    McpServersLoaded(Vec<types::CodersdkMcpServerConfig>),
    McpServersFailed(String),
    McpHealthLoaded(Option<Vec<coder_sdk::McpConnectOutcome>>),
    McpHealthFailed(String),
    McpClosed,
```

with the arms:

```rust
            Msg::McpServersLoaded(servers) => {
                if let Some(panel) = self.mcp_panel.as_mut() {
                    panel.servers = Fetched::Loaded(servers);
                }
                vec![]
            }
            Msg::McpServersFailed(message) => {
                if let Some(panel) = self.mcp_panel.as_mut() {
                    panel.servers = Fetched::Failed(message);
                }
                vec![]
            }
            Msg::McpHealthLoaded(outcomes) => {
                if let Some(panel) = self.mcp_panel.as_mut() {
                    panel.health = Fetched::Loaded(outcomes);
                }
                vec![]
            }
            Msg::McpHealthFailed(message) => {
                if let Some(panel) = self.mcp_panel.as_mut() {
                    panel.health = Fetched::Failed(message);
                }
                vec![]
            }
            Msg::McpClosed => {
                self.mcp_panel = None;
                vec![]
            }
```

and to `command`, before `Command::New`:

```rust
            Command::Mcp => {
                let (Some(chat), Some(org)) = (self.chat_id, self.current_org()) else {
                    self.error("Start a chat first.");
                    return vec![];
                };
                self.mcp_panel = Some(McpPanel {
                    servers: Fetched::Loading,
                    health: Fetched::Loading,
                });
                vec![
                    Effect::ShowMcp,
                    Effect::RefreshChat(chat),
                    Effect::FetchMcpServers { chat, org },
                    Effect::FetchMcpHealth(chat),
                ]
            }
```

In `reset_chat_state`, add `self.mcp_panel = None;`.

- [ ] **Step 4: Run the core tests to verify they pass**

Run: `cargo test -p scuttle-core`
Expected: PASS.

- [ ] **Step 5: Write the failing runtime test**

Add to the test module of `crates/scuttle-tui/src/runtime.rs`:

```rust
    #[tokio::test]
    async fn mcp_health_is_read_only_while_debug_logging_is_on() {
        let server = MockServer::start().await;
        let chat = Uuid::new_v4();
        Mock::given(method("GET"))
            .and(path("/api/v2/chats/config/user-debug-logging"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "debug_logging_enabled": false, "user_toggle_allowed": true, "forced_by_deployment": false
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/api/experimental/chats/{chat}/debug/runs")))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .expect(0)
            .mount(&server)
            .await;
        let (mut rt, mut rx) = runtime(&server.uri());
        rt.run(Effect::FetchMcpHealth(chat));
        assert!(matches!(untag(next(&mut rx).await), Msg::McpHealthLoaded(None)));
    }
```

- [ ] **Step 6: Run the runtime test to verify it fails**

Run: `cargo test -p scuttle-tui mcp_health`
Expected: FAIL: the runtime ignores `FetchMcpHealth`, so the channel times out.

- [ ] **Step 7: Implement the requests and the panel**

In `crates/scuttle-tui/src/runtime.rs`, add `| Effect::ShowMcp` to the ignored effects, and:

```rust
            Effect::FetchMcpServers { chat, org } => self.spawn(Box::pin(async move {
                let msg = match client.api().list_mcp_server_configs(&org.to_string()).await {
                    Ok(r) => Msg::McpServersLoaded(r.into_inner()),
                    Err(e) => Msg::McpServersFailed(err(e).await),
                };
                Msg::ForChat {
                    chat,
                    msg: Box::new(msg),
                }
            })),
            Effect::FetchMcpHealth(chat) => self.spawn(Box::pin(async move {
                let enabled = match client.api().get_user_chat_debug_logging_setting().await {
                    Ok(r) => r.into_inner().debug_logging_enabled == Some(true),
                    Err(_) => false,
                };
                let msg = if !enabled {
                    Msg::McpHealthLoaded(None)
                } else {
                    match client.latest_mcp_connect(chat).await {
                        Ok(outcomes) => Msg::McpHealthLoaded(outcomes),
                        Err(e) => Msg::McpHealthFailed(e.to_string()),
                    }
                };
                Msg::ForChat {
                    chat,
                    msg: Box::new(msg),
                }
            })),
```

Add `Mcp(Uuid),` to `RowKey` in `crates/scuttle-tui/src/table.rs`.
In `crates/scuttle-tui/src/overlay.rs`, add:

```rust
fn mcp_view(ctx: &ViewCtx) -> TableView {
    let (groups, note) = scuttle_core::panels::mcp_groups(ctx.app);
    let mut rows = Vec::new();
    for group in groups {
        rows.push(Row::header(vec![Line::from(group.title)]));
        for r in group.rows {
            let cells = vec![
                Line::from(format!("  {}", r.name)),
                Line::from(Span::styled(r.url.clone(), ctx.theme.dim)),
                Line::from(r.state.clone()),
                Line::from(Span::styled(r.detail.clone(), ctx.theme.dim)),
            ];
            rows.push(match r.id {
                Some(id) => Row::item(RowKey::Mcp(id), cells),
                None => Row::text(cells),
            });
        }
    }
    TableView {
        title: "MCP servers".into(),
        widths: vec![
            Constraint::Fill(1),
            Constraint::Fill(1),
            Constraint::Length(13),
            Constraint::Fill(2),
        ],
        rows,
        status: note,
        hint: Some("Esc closes".into()),
        filterable: false,
    }
}
```

Add `Mcp(TableState),` to `Overlay` with the table-only variants, make it full height, add `Overlay::Mcp(_) => mcp_view(ctx),` to `view`, and `Overlay::Mcp(_) => Some(Msg::McpClosed),` to `close_msg`.
In `crates/scuttle-tui/src/app.rs`, add `Effect::ShowMcp => self.overlay = Some(Overlay::Mcp(crate::table::TableState::default())),` to `apply_ui_effect`.

- [ ] **Step 8: Run the tests to verify they pass**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings`
Expected: PASS.

- [ ] **Step 9: Commit**

```bash
cargo fmt --all && git add crates/scuttle-core/src/commands.rs crates/scuttle-core/src/panels.rs crates/scuttle-core/src/app.rs crates/scuttle-tui/src/table.rs crates/scuttle-tui/src/overlay.rs crates/scuttle-tui/src/app.rs crates/scuttle-tui/src/runtime.rs && git commit -m "feat: list the chat's MCP servers and what is known of their health with /mcp

Assisted-by: AI"
```

---

### Task 30 (cuttable): Repin coder-sdk for the MCP selection send

Cut this task together with Tasks S4 and 31.

**Files:**
- Modify: `Cargo.toml` (the `coder-sdk` line)
- Modify: `Cargo.lock`

**Interfaces:**
- Consumes: the Task S4 commit.
- Produces: `coder_sdk::Client::send_chat_message_with_mcp_servers` for Task 31.

- [ ] **Step 1: Find the commit to pin**

The commit to pin is the tip of `m2-sdk` when this task runs, or the tip of the SDK's `main` if the author has merged `m2-sdk` into it.
Run: `SDK=~/git/nickvigilante/unofficial-coder-sdk-rs; REV=$(git -C $SDK rev-parse m2-sdk); git -C $SDK log --oneline $REV --grep "explicit, possibly empty, MCP selection"`
Expected: one line, which proves Task S4 is in `$REV`, called `<rev>` below.

- [ ] **Step 2: Repin**

In `Cargo.toml`, set the `coder-sdk` line's `rev` to `<rev>`, keeping the `file://` URL.
Run: `cargo update -p coder-sdk`
Expected: `Cargo.lock` names `<rev>`.

- [ ] **Step 3: Run the tests**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings`
Expected: PASS with no code change.

- [ ] **Step 4: Commit**

```bash
cargo fmt --all --check && git add Cargo.toml Cargo.lock && git commit -m "build: repin coder-sdk for sending an MCP selection

Assisted-by: AI"
```

---

### Task 31 (cuttable): `/mcp` toggling for the next message

In `/mcp`, Space turns an organization server on or off for the next message (design section 14, decision "`/mcp` toggling in M2"); a `force_on` server cannot be turned off.
There is no PATCH for the selection, so it rides on `mcp_server_ids` in the next `POST /chats/{chat}/messages` (`codersdk/chats.go:766`), as the web UI does, through Task S4's send, which can say "none".

**Files:**
- Modify: `crates/scuttle-core/src/app.rs` (`mcp_next`, `TurnOptions::mcp_servers`, `Msg::ToggleMcp`, `submit`, tests)
- Modify: `crates/scuttle-core/src/panels.rs` (pending state in `mcp_groups`, tests)
- Modify: `crates/scuttle-tui/src/overlay.rs` (Space, hint)
- Modify: `crates/scuttle-tui/src/runtime.rs` (`SendMessage` with a selection, tests)

**Interfaces:**
- Consumes: `McpPanel`, `mcp_groups`, `RowKey::Mcp` (Task 29); `Client::send_chat_message_with_mcp_servers` (Task S4 via Task 30).
- Produces: `App::mcp_next: Option<Vec<Uuid>>`; `TurnOptions::mcp_servers: Option<Vec<Uuid>>`; `Msg::ToggleMcp(Uuid)`.

- [ ] **Step 1: Write the failing tests**

Add to the test module of `crates/scuttle-core/src/app.rs`:

```rust
    #[test]
    fn toggling_a_server_rides_on_the_next_message() {
        use crate::panels::{Fetched, McpPanel};
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let (id, github, docs) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let mut open = chat(id);
        open.mcp_server_ids = vec![github];
        app.update(Msg::ChatLoaded {
            chat: open,
            messages: vec![],
        });
        app.mcp_panel = Some(McpPanel {
            servers: Fetched::Loaded(serde_json::from_value(json!([
                {"id": github, "display_name": "GitHub", "availability": "default_off", "tool_allow_list": [], "tool_deny_list": []},
                {"id": docs, "display_name": "Docs", "availability": "force_on", "tool_allow_list": [], "tool_deny_list": []}
            ])).unwrap()),
            health: Fetched::Loaded(None),
        });
        app.update(Msg::ToggleMcp(docs));
        assert_eq!(app.mcp_next, None, "a required server cannot be turned off");
        app.update(Msg::ToggleMcp(github));
        assert_eq!(app.mcp_next, Some(vec![]));
        let effects = app.update(Msg::Submit("no tools this time".into()));
        assert!(
            matches!(effects.as_slice(), [Effect::SendMessage { turn, .. }] if turn.mcp_servers == Some(vec![])),
            "{effects:?}"
        );
        assert_eq!(app.mcp_next, None, "the selection went with the message");
    }
```

Add to the test module of `crates/scuttle-core/src/panels.rs`, at the end of `mcp_groups_join_the_selection_with_health_and_list_every_source`:

```rust
        app.mcp_next = Some(vec![linear]);
        let (groups, _) = mcp_groups(&app);
        assert_eq!(groups[0].rows[0].state, "off (next message)");
        assert_eq!(groups[0].rows[1].state, "on (next message)");
```

Add to the test module of `crates/scuttle-tui/src/runtime.rs`:

```rust
    #[tokio::test]
    async fn a_message_with_an_mcp_selection_sends_it_even_when_empty() {
        use wiremock::matchers::body_partial_json;
        let server = MockServer::start().await;
        let chat = Uuid::new_v4();
        Mock::given(method("POST"))
            .and(path(format!("/api/v2/chats/{chat}/messages")))
            .and(body_partial_json(serde_json::json!({"mcp_server_ids": []})))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"queued": false})))
            .expect(1)
            .mount(&server)
            .await;
        let (mut rt, mut rx) = runtime(&server.uri());
        rt.run(Effect::SendMessage {
            chat,
            text: "no tools".into(),
            model: None,
            busy: scuttle_core::config::BusyBehavior::Queue,
            turn: TurnOptions {
                mcp_servers: Some(vec![]),
                ..Default::default()
            },
        });
        assert!(matches!(untag(next(&mut rx).await), Msg::Refresh));
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --workspace`
Expected: FAIL to compile, because `mcp_next`, `TurnOptions::mcp_servers`, and `Msg::ToggleMcp` do not exist.

- [ ] **Step 3: Implement the toggle**

In `crates/scuttle-core/src/app.rs`, add to `TurnOptions`:

```rust
    /// The organization MCP servers the chat uses from this message on; `None` leaves them.
    pub mcp_servers: Option<Vec<Uuid>>,
```

add `pub mcp_next: Option<Vec<Uuid>>,` to `App` with the doc comment `/// The MCP selection /mcp changed, sent with the next message.`, `ToggleMcp(Uuid),` to `Msg`, and the arm:

```rust
            Msg::ToggleMcp(server) => {
                let Some(chat) = self.chat.as_deref() else {
                    return vec![];
                };
                let config = match self.mcp_panel.as_ref().map(|p| &p.servers) {
                    Some(Fetched::Loaded(servers)) => servers.iter().find(|s| s.id == Some(server)).cloned(),
                    _ => None,
                };
                let Some(config) = config else {
                    return vec![];
                };
                let name = config.display_name.clone().or(config.slug.clone()).unwrap_or_default();
                if config.availability.as_deref() == Some("force_on") {
                    self.info(format!("{name} is required by your organization and stays on."));
                    return vec![];
                }
                let mut next = self.mcp_next.clone().unwrap_or_else(|| chat.mcp_server_ids.clone());
                let on = if let Some(i) = next.iter().position(|id| *id == server) {
                    next.remove(i);
                    false
                } else {
                    next.push(server);
                    true
                };
                self.mcp_next = Some(next);
                let word = if on { "on" } else { "off" };
                self.info(format!("{name} turns {word} with your next message."));
                vec![]
            }
```

In `submit`, in the `if let Some(chat) = self.chat_id` branch, build the options as `TurnOptions { files: self.take_files(), mcp_servers: self.mcp_next.take(), ..self.turn() }`, and in `reset_chat_state` add `self.mcp_next = None;`.
In `crates/scuttle-core/src/panels.rs`, in `mcp_groups`, compute the selection from the pending one when there is one: replace `let on = required || s.id.is_some_and(|id| chat.mcp_server_ids.contains(&id));` with

```rust
                    let selected = app.mcp_next.as_ref().unwrap_or(&chat.mcp_server_ids);
                    let pending = app.mcp_next.is_some();
                    let on = required || s.id.is_some_and(|id| selected.contains(&id));
```

and the `state` match with

```rust
                        state: match (on, required, pending) {
                            (true, true, _) => "on (required)".into(),
                            (true, false, true) => "on (next message)".into(),
                            (false, _, true) => "off (next message)".into(),
                            (true, false, false) => "on".into(),
                            (false, _, false) => "off".into(),
                        },
```

In `crates/scuttle-tui/src/overlay.rs`, change `mcp_view`'s hint to `"Space turns a server on or off for the next message, Esc closes"`, and add this arm to the generic `handle_key`, before the final `TableKey::Handled | TableKey::Unhandled` arm:

```rust
            TableKey::Unhandled if key.code == KeyCode::Char(' ') => {
                match state.selected_row(&view).map(|r| r.key.clone()) {
                    Some(RowKey::Mcp(id)) => OverlayOutcome::Send(Msg::ToggleMcp(id)),
                    _ => OverlayOutcome::Stay,
                }
            }
```

In `crates/scuttle-tui/src/runtime.rs`, in the `Effect::SendMessage` arm, replace the `client.api().send_chat_message(&chat, &body).await` call and its match with:

```rust
                let sent = match turn.mcp_servers.as_deref() {
                    Some(ids) => client
                        .send_chat_message_with_mcp_servers(chat, &body, ids)
                        .await
                        .map_err(|e| e.to_string()),
                    None => match client.api().send_chat_message(&chat, &body).await {
                        Ok(_) => Ok(()),
                        Err(e) => Err(err(e).await),
                    },
                };
                let msg = match sent {
                    Ok(()) => Msg::Refresh,
                    Err(message) => Msg::SendFailed {
                        text,
                        message,
                        plan_mode,
                    },
                };
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all && git add crates/scuttle-core/src/app.rs crates/scuttle-core/src/panels.rs crates/scuttle-tui/src/overlay.rs crates/scuttle-tui/src/runtime.rs && git commit -m "feat: turn organization MCP servers on or off for the next message from /mcp

Assisted-by: AI"
```

---

### Task 32 (cuttable): Older history and orphan tool results

M1 loads the newest 200 messages of a chat; chat switching opens long chats, so reaching the top of the transcript loads the 200 before them with `before_id` (`coderd/exp_chats.go:1688`, generated `list_chat_messages`), until the server says `has_more: false` (`codersdk/chats.go:849-853`) (design section 17).
A durable tool result whose call is outside the loaded window rendered nothing (M1 Task 10); it now renders as its own tool line with its tool name, which older-history loading also makes rare.
Depends on M1.6: `ToolResultInfo::summary` and `result_summary` (interface 3), and `Tui::mouse` returning `Vec<Effect>` (interface 5).

**Files:**
- Modify: `crates/scuttle-core/src/transcript.rs` (`first_message_id`)
- Modify: `crates/scuttle-core/src/app.rs` (`HISTORY_PAGE`, `history_more`, `history_loading`, `Msg`, `Effect::LoadOlder`, tests)
- Modify: `crates/scuttle-tui/src/transcript_view.rs` (orphan results, tests)
- Modify: `crates/scuttle-tui/src/app.rs` (load at the top, tests)
- Modify: `crates/scuttle-tui/src/runtime.rs` (`LoadOlder`, the page size, tests)

**Interfaces:**
- Consumes: `Transcript::load`.
- Produces: `app::HISTORY_PAGE: i64 = 200`; `Transcript::first_message_id(&self) -> Option<i64>`; `Msg::LoadOlder`; `Msg::OlderLoaded { messages: Vec<types::CodersdkChatMessage>, has_more: bool }` and `Msg::OlderFailed(String)` (in `ForChat`); `Effect::LoadOlder { chat: Uuid, before_id: i64 }`.

- [ ] **Step 1: Write the failing tests**

Add to the test module of `crates/scuttle-core/src/app.rs`:

```rust
    #[test]
    fn reaching_the_top_of_a_long_chat_loads_the_page_before_it_once() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            chat: chat(id),
            messages: (301..=500).map(message).collect(),
        });
        assert_eq!(
            app.update(Msg::LoadOlder),
            vec![Effect::LoadOlder { chat: id, before_id: 301 }]
        );
        assert!(app.update(Msg::LoadOlder).is_empty(), "one page at a time");
        app.update(Msg::ForChat {
            chat: id,
            msg: Box::new(Msg::OlderLoaded {
                messages: (101..=300).map(message).collect(),
                has_more: false,
            }),
        });
        assert_eq!(app.transcript.first_message_id(), Some(101));
        assert!(app.update(Msg::LoadOlder).is_empty(), "the server has nothing older");
        let short = Uuid::new_v4();
        app.update(Msg::OpenChat(short));
        app.update(Msg::ChatLoaded {
            chat: chat(short),
            messages: vec![message(1)],
        });
        assert!(app.update(Msg::LoadOlder).is_empty(), "a short chat is already whole");
    }
```

Add to the test module of `crates/scuttle-tui/src/transcript_view.rs`:

```rust
    #[test]
    fn a_result_whose_call_was_not_loaded_still_shows() {
        let app = app_with(json!([
            {"id": 5, "role": "tool", "content": [{"type": "tool-result", "tool_call_id": "c9",
                "tool_name": "execute", "result": {"output": "done"}}]}
        ]));
        let text: String = build_at(&app, 60).lines.iter().map(|l| l.to_string()).collect::<Vec<_>>().join("\n");
        assert!(text.contains("execute"), "{text}");
    }
```

Add to the test module of `crates/scuttle-tui/src/runtime.rs`:

```rust
    #[tokio::test]
    async fn older_messages_are_fetched_before_an_id_and_tagged() {
        use wiremock::matchers::query_param;
        let server = MockServer::start().await;
        let chat = Uuid::new_v4();
        Mock::given(method("GET"))
            .and(path(format!("/api/v2/chats/{chat}/messages")))
            .and(query_param("before_id", "301"))
            .and(query_param("limit", "200"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "messages": [{"id": 300, "role": "user", "content": []}], "queued_messages": [], "has_more": true
            })))
            .mount(&server)
            .await;
        let (mut rt, mut rx) = runtime(&server.uri());
        rt.run(Effect::LoadOlder { chat, before_id: 301 });
        assert!(matches!(
            untag(next(&mut rx).await),
            Msg::OlderLoaded { ref messages, has_more: true } if messages.len() == 1
        ));
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --workspace`
Expected: FAIL to compile, because `Msg::LoadOlder`, `Effect::LoadOlder`, and `first_message_id` do not exist.

- [ ] **Step 3: Implement the older pages and the orphan line**

In `crates/scuttle-core/src/transcript.rs`, add next to `last_message_id`:

```rust
    pub fn first_message_id(&self) -> Option<i64> {
        self.messages.keys().next().copied()
    }
```

In `crates/scuttle-core/src/app.rs`, add after `backoff`:

```rust
/// Messages loaded per history page, the most the server returns at once.
pub const HISTORY_PAGE: i64 = 200;
```

add `history_more: bool,` and `history_loading: bool,` to `App`; `LoadOlder { chat: Uuid, before_id: i64 },` to `Effect`; and `LoadOlder,`, `OlderLoaded { messages: Vec<types::CodersdkChatMessage>, has_more: bool },`, and `OlderFailed(String),` to `Msg`.
In `Msg::ChatLoaded`, before `self.transcript.load(messages);`, add `self.history_more = messages.len() as i64 >= HISTORY_PAGE;` and `self.history_loading = false;`.
Add the arms:

```rust
            Msg::LoadOlder => {
                let (Some(chat), Some(before_id)) = (self.chat_id, self.transcript.first_message_id()) else {
                    return vec![];
                };
                if !self.history_more || self.history_loading {
                    return vec![];
                }
                self.history_loading = true;
                vec![Effect::LoadOlder { chat, before_id }]
            }
            Msg::OlderLoaded { messages, has_more } => {
                self.history_loading = false;
                self.history_more = has_more;
                self.transcript.load(messages);
                vec![]
            }
            Msg::OlderFailed(message) => {
                self.history_loading = false;
                self.error(format!("Could not load older messages: {message}"));
                vec![]
            }
```

In `reset_chat_state`, add `self.history_more = false;` and `self.history_loading = false;`.
In `crates/scuttle-tui/src/transcript_view.rs`, give `items_for_message` the set of call IDs and make a `tool` message render results without a loaded call:

```rust
fn items_for_message<'a>(
    m: &'a types::CodersdkChatMessage,
    results: &BTreeMap<String, ToolResultInfo>,
    calls: &HashSet<String>,
) -> Vec<Item<'a>> {
    let role = m.role.as_ref().map(|r| r.as_str());
    // A "tool" message carries tool-result parts, already folded into `results` for the calls
    // they answer; a result whose call is outside the loaded history gets its own line.
    if role == Some("tool") {
        return m
            .content
            .iter()
            .filter(|p| p.type_.as_ref().map(|t| t.as_str()) == Some("tool-result"))
            .filter(|p| !calls.contains(p.tool_call_id.as_deref().unwrap_or_default()))
            .map(|p| Item::Tool {
                name: p.tool_name.as_deref().unwrap_or("tool"),
                args: result_summary(p.result.as_ref()).unwrap_or_default(),
                result: result_text(p.result.as_ref(), ""),
                is_error: p.is_error.unwrap_or(false),
                done: true,
            })
            .collect();
    }
```

keeping the rest of the function as it is, and pass `&calls` where `build_transcript` calls it: `.map(|m| (m.id, items_for_message(m, &results, &calls)))`.
In `crates/scuttle-tui/src/runtime.rs`, change the page size in `Effect::LoadChat` from `Some(200)` to `Some(scuttle_core::app::HISTORY_PAGE)`, and add:

```rust
            Effect::LoadOlder { chat, before_id } => self.spawn(Box::pin(async move {
                let msg = match client
                    .api()
                    .list_chat_messages(&chat, None, Some(before_id), Some(scuttle_core::app::HISTORY_PAGE))
                    .await
                {
                    Ok(r) => {
                        let r = r.into_inner();
                        Msg::OlderLoaded {
                            messages: r.messages,
                            has_more: r.has_more.unwrap_or(false),
                        }
                    }
                    Err(e) => Msg::OlderFailed(err(e).await),
                };
                Msg::ForChat {
                    chat,
                    msg: Box::new(msg),
                }
            })),
```

In `crates/scuttle-tui/src/app.rs`, add to `impl Tui`:

```rust
    /// Asks for older history once the view reaches the top; the core ignores it when the chat
    /// is whole or a page is already on its way.
    fn load_older_at_top(&mut self) -> Vec<Effect> {
        if self.top_line() == 0 {
            self.update(Msg::LoadOlder)
        } else {
            vec![]
        }
    }
```

In `key`, change the `KeyCode::PageUp` branch to `self.scroll_up(page); return self.load_older_at_top();`, and in `handle`, change the mouse arm to:

```rust
            Event::Mouse(m) => {
                let scrolled_up = m.kind == MouseEventKind::ScrollUp;
                let mut effects = self.mouse(m);
                if scrolled_up {
                    effects.extend(self.load_older_at_top());
                }
                effects
            }
```

Add to the test module of `crates/scuttle-tui/src/app.rs`:

```rust
    #[test]
    fn page_up_at_the_top_of_a_long_chat_asks_for_older_messages() {
        let mut t = tui();
        t.core.update(Msg::Started {
            org_id: uuid::Uuid::new_v4(),
            open_chat: None,
        });
        let messages: Vec<serde_json::Value> = (1..=200)
            .map(|i| json!({"id": i, "role": "user", "content": [{"type": "text", "text": format!("m{i}")}]}))
            .collect();
        loaded(&mut t, serde_json::Value::Array(messages));
        screen(&mut t, 60, 20);
        let mut asked = Vec::new();
        for _ in 0..100 {
            asked = t.handle(key(KeyCode::PageUp, KeyModifiers::NONE));
            if !asked.is_empty() {
                break;
            }
            screen(&mut t, 60, 20);
        }
        assert!(matches!(asked.as_slice(), [Effect::LoadOlder { before_id: 1, .. }]), "{asked:?}");
    }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings`
Expected: PASS, with the transcript snapshots unchanged.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all && git add crates/scuttle-core/src/transcript.rs crates/scuttle-core/src/app.rs crates/scuttle-tui/src/transcript_view.rs crates/scuttle-tui/src/app.rs crates/scuttle-tui/src/runtime.rs && git commit -m "feat: load older history at the top of a long chat and show orphan tool results

Assisted-by: AI"
```

---

### Task 33: Scroll anchoring while scrolled up

While scrolled up, new output at the bottom moves the text under the reader, because the offset counts from the bottom (M1 Task 14); subagent previews and chat switching make reading scrolled up common (design section 17).
The fix anchors the view to the durable message at the top of the screen and the number of its lines above the top, and restores that position after every rebuild, so new output below and older history above leave the text still.

**Files:**
- Modify: `crates/scuttle-tui/src/transcript_view.rs` (`View::starts`, tests)
- Modify: `crates/scuttle-tui/src/app.rs` (`anchor`, `anchor_at`, `remember_anchor`, `draw_at`, the scroll paths, tests)

**Interfaces:**
- Consumes: `View`, `Tui::top_line`, `Tui::max_scroll`.
- Produces: `View::starts: BTreeMap<i64, usize>` (the first line of each durable message); private `Tui::anchor: Option<(i64, usize)>`, `Tui::anchor_at(&self, top: usize) -> Option<(i64, usize)>`, and `Tui::remember_anchor(&mut self)`.

- [ ] **Step 1: Write the failing tests**

Add to the test module of `crates/scuttle-tui/src/transcript_view.rs`:

```rust
    #[test]
    fn each_durable_message_records_its_first_line() {
        let app = app_with(json!([
            {"id": 1, "role": "user", "content": [{"type": "text", "text": "first"}]},
            {"id": 2, "role": "assistant", "content": [{"type": "text", "text": "second"}]}
        ]));
        let view = build_at(&app, 60);
        let one = view.starts[&1];
        let two = view.starts[&2];
        assert!(one < two);
        assert!(view.lines[two..].iter().any(|l| l.to_string().contains("second")));
    }
```

Add to the test module of `crates/scuttle-tui/src/app.rs`:

```rust
    #[test]
    fn new_output_while_scrolled_up_keeps_the_view_still() {
        let mut t = tui();
        t.core.update(Msg::Started {
            org_id: uuid::Uuid::new_v4(),
            open_chat: None,
        });
        let messages: Vec<serde_json::Value> = (1..=30)
            .map(|i| {
                let role = if i % 2 == 0 { "assistant" } else { "user" };
                json!({"id": i, "role": role, "content": [{"type": "text", "text": format!("message {i}")}]})
            })
            .collect();
        loaded(&mut t, serde_json::Value::Array(messages));
        let now = Instant::now();
        screen_at(&mut t, 60, 20, now);
        t.handle(key(KeyCode::PageUp, KeyModifiers::NONE));
        let rows = |shown: String| shown.lines().take(10).map(str::to_owned).collect::<Vec<_>>();
        let before = rows(screen_at(&mut t, 60, 20, now));
        t.update(stream(json!({"type": "message", "message": {"id": 31, "role": "assistant",
            "content": [{"type": "text", "text": "a new answer\n\nwith two paragraphs"}]}})));
        let after = rows(screen_at(&mut t, 60, 20, now));
        assert_eq!(before, after, "the text under the reader did not move");
        t.handle(key(KeyCode::End, KeyModifiers::NONE));
        assert!(screen_at(&mut t, 60, 20, now).contains("with two paragraphs"));
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p scuttle-tui`
Expected: FAIL to compile, because `View::starts` does not exist.

- [ ] **Step 3: Record message starts and anchor the scroll**

In `crates/scuttle-tui/src/transcript_view.rs`, add to `View`:

```rust
    /// The first line of each durable message, which a scrolled-up view anchors to.
    pub starts: BTreeMap<i64, usize>,
```

and in `build_transcript`'s group loop, before `render_items`, add:

```rust
        if let Some(id) = owner {
            out.view.starts.insert(id, out.view.lines.len());
        }
```

In `crates/scuttle-tui/src/app.rs`, add `anchor: Option<(i64, usize)>,` to `Tui` (initialized to `None`) with the doc comment `/// While scrolled up: the durable message at the top of the screen and how many of its lines are above the top.`, and to `impl Tui`:

```rust
    /// The durable message at transcript line `top` and how far into it `top` is.
    fn anchor_at(&self, top: usize) -> Option<(i64, usize)> {
        self.view
            .starts
            .iter()
            .filter(|&(_, &line)| line <= top)
            .max_by_key(|&(_, &line)| line)
            .map(|(&id, &line)| (id, top - line))
    }

    /// Remembers what the top of the screen shows while scrolled up.
    fn remember_anchor(&mut self) {
        self.anchor = if self.scroll_from_bottom > 0 {
            self.anchor_at(self.top_line())
        } else {
            None
        };
    }
```

Call `self.remember_anchor();` at the end of the non-drag paths of `scroll_up` and `scroll_down`, after `self.scroll_from_bottom = 0;` in the `KeyCode::End` branch and in the `ComposerAction::Submit` arm, and set `self.anchor = None;` in the `Effect::ClearView` arm.
In `draw_at`, inside the `if !reuse { .. }` block, after `self.view_builds += 1;`, restore the anchor:

```rust
            if self.scroll_from_bottom > 0
                && self.drag.is_none()
                && let Some((id, offset)) = self.anchor
                && let Some(&start) = self.view.starts.get(&id)
            {
                let max = self.max_scroll();
                let top = (start + offset).min(max);
                self.scroll_from_bottom = max - top;
            }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings`
Expected: PASS, including M1.5's drag tests, whose pinned top still wins while a drag is held.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all && git add crates/scuttle-tui/src/transcript_view.rs crates/scuttle-tui/src/app.rs && git commit -m "fix(scuttle-tui): keep the text still while scrolled up as new output arrives

Assisted-by: AI"
```

---

### Task 34: The wheel with the mouse off, and `/help` for every new command

With mouse capture off, terminals in the alternate screen turn the wheel into arrow keys (alternate scroll mode), so the wheel recalls composer history (M1 final review Minor 3); scuttle turns the mode off (`CSI ? 1007 l`) while capture is off and back on (`CSI ? 1007 h`) when capture returns and on exit (design section 17).
`/help` already lists every command in `COMMANDS` with its aliases, so the new commands show; this task adds the new keys to `KEYS` (design section 16, feedback item 8).

**Files:**
- Modify: `crates/scuttle-tui/src/terminal.rs` (`alternate_scroll`, `set_mouse`, `leave`, tests)
- Modify: `crates/scuttle-tui/src/help.rs` (`KEYS`, tests)
- Modify: `crates/scuttle-tui/tests/pty.rs` (`exit_restores_terminal_modes`, a mouse test)

**Interfaces:**
- Consumes: every key added by Tasks 8, 9, 13, 16, 17, 19, 20.
- Produces: `terminal::alternate_scroll(on: bool) -> &'static str`.

- [ ] **Step 1: Write the failing tests**

Add to the test module of `crates/scuttle-tui/src/terminal.rs`:

```rust
    #[test]
    fn alternate_scroll_is_turned_off_with_the_mouse_and_back_on_after() {
        assert_eq!(super::alternate_scroll(false), "\x1b[?1007l");
        assert_eq!(super::alternate_scroll(true), "\x1b[?1007h");
    }
```

Add to the test module of `crates/scuttle-tui/src/help.rs`:

```rust
    #[test]
    fn help_names_the_m2_keys_and_commands() {
        let theme = Theme::terminal(true);
        let shown = text(&help_lines(&theme, 200)).join("\n");
        for needle in [
            "Ctrl+R",
            "/chats [query] (/resume)",
            "/parent (/back)",
            "/info (/chat-info)",
            "Return to the parent chat",
            "Send the first queued message now",
            "Implement a proposed plan",
            "Remove the last attachment",
            "Complete a file path to attach",
            "Ctrl+A archives",
        ] {
            assert!(shown.contains(needle), "{needle} is missing from:\n{shown}");
        }
    }
```

In `crates/scuttle-tui/tests/pty.rs`, add to `exit_restores_terminal_modes`, after the existing mode assertions:

```rust
    assert!(tail.contains("\x1b[?1007h"), "alternate scroll mode restored");
```

and add:

```rust
#[tokio::test(flavor = "multi_thread")]
async fn turning_the_mouse_off_stops_the_wheel_from_recalling_history() {
    let server = fake_coder().await;
    let mut s = spawn(
        "mouse-off-alternate-scroll",
        &[
            ("CODER_URL", server.uri()),
            ("CODER_SESSION_TOKEN", "test-token-not-real".into()),
        ],
    );
    s.wait_for("scuttle");
    s.writer.write_all(b"/mouse\r").unwrap();
    s.wait_for("\x1b[?1007l");
    s.writer.write_all(b"/quit\r").unwrap();
    assert_eq!(s.exit_code(), 0);
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p scuttle-tui`
Expected: FAIL: `alternate_scroll` does not exist, and help lacks the new keys.

- [ ] **Step 3: Implement the mode and the keys**

In `crates/scuttle-tui/src/terminal.rs`, add:

```rust
/// Alternate scroll mode (`CSI ? 1007`): while it is on and mouse capture is off, terminals
/// turn the wheel into arrow keys, which would recall composer history.
pub fn alternate_scroll(on: bool) -> &'static str {
    if on { "\x1b[?1007h" } else { "\x1b[?1007l" }
}
```

replace `set_mouse` with:

```rust
pub fn set_mouse(enabled: bool) -> std::io::Result<()> {
    let mut out = stdout();
    if enabled {
        execute!(out, EnableMouseCapture)?;
    } else {
        execute!(out, DisableMouseCapture)?;
    }
    out.write_all(alternate_scroll(enabled).as_bytes())?;
    out.flush()
}
```

and in `leave`, add `let _ = out.write_all(alternate_scroll(true).as_bytes());` before `out.flush()`, which restores the mode most terminals start with.
In `crates/scuttle-tui/src/help.rs`, add these entries to `KEYS`, before the `Ctrl+C twice` entry:

```rust
    KeyInfo {
        keys: "Ctrl+R",
        action: "Open /chats to find and open a chat",
    },
    KeyInfo {
        keys: "Esc in an idle subagent",
        action: "Return to the parent chat, with an empty composer",
    },
    KeyInfo {
        keys: "Enter on an empty composer",
        action: "Send the first queued message now, interrupting a running turn",
    },
    KeyInfo {
        keys: "Ctrl+Enter on an empty composer",
        action: "Implement a proposed plan",
    },
    KeyInfo {
        keys: "Up, Down, Enter, Esc",
        action: "Answer a plan-mode question while the composer is empty",
    },
    KeyInfo {
        keys: "Backspace on an empty composer",
        action: "Remove the last attachment",
    },
    KeyInfo {
        keys: "Tab after @",
        action: "Complete a file path to attach",
    },
    KeyInfo {
        keys: "In /chats",
        action: "Tab filters, Right and Left show subagents, Ctrl+A archives, Ctrl+E renames, Ctrl+P pins, Ctrl+U marks read",
    },
    KeyInfo {
        keys: "In /subagents",
        action: "Up and Down preview, Enter opens, PageUp and PageDown scroll the preview",
    },
    KeyInfo {
        keys: "In /queue",
        action: "Enter sends now, Delete removes",
    },
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all && git add crates/scuttle-tui/src/terminal.rs crates/scuttle-tui/src/help.rs crates/scuttle-tui/tests/pty.rs && git commit -m "fix(scuttle-tui): stop the wheel recalling history with the mouse off and list the M2 keys in /help

Assisted-by: AI"
```

---

## Self-review notes

- **Coverage of the design.** Section 2 (`/chats`) is Tasks 5, 6, 8, 9, and 10; section 3 (`/subagents`) is Tasks 12 and 13; section 4 (switching, the generation, the watch) is Tasks 2, 3, 5, and 6; section 5 (`/title`) is Task 15; section 6 (`/queue`, with "Send now") is Task 16; section 7 (questions and the plan) is Task 17, with plan-mode serialization from section 17 in Task 18; section 8 (attachments) is Tasks 19 and 20; section 9 (`/model`) is Task 14; section 10 (skills) is Task 21; section 11 (`/info`) is Task 22; section 12 (`/workspace`) is Tasks 23 and 24; section 13 (`/git`) is Tasks 26 and 27; section 14 (`/mcp`) is Tasks 29 and 31; section 16 (the signed-in user, the table overlay, `/help`) is Tasks 4, 7, and 34.
- **Deferred items kept in M2 (section 17).** Channel staleness is Task 2; the drifting view is Task 33; the organization lookup retry and the welcome user are Task 4; the wheel is Task 34; Minor 8 is Task 23; older history and orphan results are Task 32; the drain cap is Task 6; the upgrade timeout and the watchdog message are Task S1; the reconnect counter and the `CloseStream` during `ReconnectAfter` test are Task 2; plan-mode requests are Task 18; the running-tool label is Task 11.
- **Commands and their order.** After every task, `every_listed_command_parses` lists `/new`, `/chats`, `/subagents`, `/parent`, `/model`, `/effort`, `/workspace`, `/organization`, `/plan-mode`, `/implement`, `/title`, `/queue`, `/attach`, `/info`, `/git`, `/diff`, `/mcp`, `/compact`, `/clear`, `/copy`, `/web`, `/mouse`, `/help`, `/quit`, restricted to the commands that exist so far; each task names its insertion point.
- **Names across tasks.** `open_stream`, `reconnect`, `close_stream` (Task 2) are used by Tasks 3 and 12; `reset_chat_state` and `open_chat` (Task 3) are extended by Tasks 5, 12, 18, 22, 24, 26, 29, 31, and 32; `load_chats` (Task 5) by Tasks 6, 8, and 10; `apply_watch` (Task 6) is replaced whole in Task 22 and extended in Task 26; `Row::text` (Task 22), `Fetched` and `RowKey::Action` (Task 24), and `close_msg` (Task 22) are reused by Tasks 26, 27, and 29; `Editor`, `EditTarget`, and `LineEdit` (Task 9) gain targets in Tasks 15 and 17; `TurnOptions` gains `files` (Task 19) and `mcp_servers` (Task 31), and every literal in tests ends with `..Default::default()` or `..self.turn()`.
- **Existing tests changed on purpose.** `stream_event_after_reconnect_resets_backoff` becomes `only_a_healthy_stream_resets_backoff`, and every stream effect literal gains `generation` (Task 2); `a_stale_stream_sender_delivers_nothing`, `conn_of`, `untag`, and `stream_messages_are_tagged_with_their_chat` follow `ForStream` (Tasks 2 and 12); `completes_by_prefix` gains `/chats` (Task 8) and `/info` (Task 22); the model, workspace, and organization picker tests use `t.overlay` (Task 7); `disabled_models_are_not_offered` counts the group header (Task 14); `plan_mode_toggles_and_sets_on_an_existing_chat` and `set_plan_mode_patches_the_chat` wait for `PlanModeApplied` (Task 18); `workspaces_are_listed_for_one_organization` checks the new fields, and `WorkspaceRef` literals gain `..Default::default()` (Task 23); `the_web_url_copy_notice_names_the_url` calls `copied_notice` (Task 24); `new_in_the_same_organization_keeps_its_lists` also expects `FetchSkills` (Task 21).
  No insta snapshot changes.
- **Dead code under `-D warnings`.** Fields and helpers are added in the task that first reads them: `ViewCtx::now_unix` and `elapsed` in Task 8, `ChatsState::confirm` in Task 9, `GitAction::ViewDiff` in Task 27.
