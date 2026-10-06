# M1: Daily-usable single chat Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship `scuttle`, a full-screen Rust terminal client where the author can open a blank or existing Coder Agents chat, watch it stream, reply, interrupt, and copy output, well enough to use it every day.

**Architecture:** Two small SDK changes land first on a branch stacked on `m0-sdk`. The `scuttle` repo then gets a Cargo workspace with `scuttle-core`, a headless Elm-style state machine (`App::update(Msg) -> Vec<Effect>`) plus the stream reducer, and `scuttle-tui`, which renders core state with Ratatui, turns keys and mouse input into `Msg`s, and executes `Effect`s with `coder-sdk`. Core never touches the terminal and the TUI never mutates chat state directly.

**Tech Stack:** Rust 2024, tokio, coder-sdk (git dependency on the local SDK repo), ratatui 0.30, crossterm 0.29 (`event-stream`), ratatui-textarea 0.9, pulldown-cmark 0.13, syntect 5.3 with two-face 0.5, arboard 3.6, terminal-colorsaurus 1.0, toml and toml_edit, serde, insta, portable-pty, vt100, wiremock.

**Spec:** `~/git/nickvigilante/scuttle/docs/specs/2026-09-28-scuttle-design.md`, sections 2, 3, 4 (M1 commands), 5 (local settings, session), 6 (footer fields `model`, `context`, `status`), 8, 9, and the M1 row of section 10.

## Global Constraints

- SDK work happens on branch `m1-sdk`, created from `m0-sdk`, in the worktree `~/git/nickvigilante/unofficial-coder-sdk-rs/.worktrees/m1-sdk`.
- scuttle work happens on branch `m1` in the worktree `~/git/nickvigilante/scuttle/.worktrees/m1`.
- Never commit on `main`, never push, never add a remote, never create a GitHub repo.
- `scuttle` depends on `coder-sdk` by git revision: `coder-sdk = { git = "file://<unofficial-coder-sdk-rs checkout>", rev = "<full sha>" }`.
- `scuttle-core` has no terminal dependencies (no ratatui, crossterm, arboard); `scuttle-tui` never calls the API outside `runtime.rs`.
- The local config file is `$XDG_CONFIG_HOME/scuttle/config.toml`, defaulting to `~/.config/scuttle/config.toml` on every platform, and never contains secrets.
- The session token is never printed, logged, rendered, or written anywhere by scuttle.
- Every crate sets `publish = false`. Rust edition 2024. Toolchain pinned to `1.98` in `rust-toolchain.toml`.
- Commit messages use Conventional Commits, end with the trailer `Assisted-by: AI`, and never name an AI model or vendor.
- Markdown files use one sentence per line and no em dashes, en dashes, or spaced double hyphens as punctuation.
- Crate APIs in this plan were written against documentation without compiling. When a signature differs in the resolved version, adapt the implementation and test setup code, keep every test assertion, and report the adaptation.

## Decisions this plan makes that the spec left open

- The Codernaut artwork is pending a brand question for a Coder maintainer (spec open question 7), so the welcome screen shows a text wordmark by default and renders art only from a user-supplied file named by `welcome.art_file` in the local config.
- `/new`, `/chats`, `/title`, `/queue`, `/plan`, attachments, `/statusline`, `/usage`, `/settings`, `/provider-keys`, `/theme`, and `/keybindings` belong to later milestones per spec section 10. M1 adds `/help` and `/quit` so the app is usable, because the spec's command table lists both.
- M1 loads the most recent 200 messages of an existing chat; older history is out of scope until M2.
- Typing an unknown `/name` shows an error instead of sending it to the agent; skills in the `/` menu arrive with M4.
- After a `history_reset`, the reducer drops every message whose ID is at least the smallest ID among the replacement messages, then inserts the replacements.

## Review Focus

1. The chat stream drops, the server restarts, or the laptop sleeps mid-turn: scuttle reconnects with backoff and `after_id`, shows "reconnecting" in the footer, never duplicates messages, and never loses composer text. Pinned by `stream_end_schedules_backoff_reconnect_with_after_id` and `stream_event_after_reconnect_resets_backoff` in Task 8 and `snapshot_replay_does_not_duplicate_messages` in Task 5.
1. scuttle panics, or `$EDITOR` exits with an error: the terminal is restored (raw mode off, mouse capture off, alternate screen left). Pinned by `exit_restores_terminal_modes` in Task 15 and `editor_failure_keeps_composer_text` in Task 14.
1. A narrow terminal (40 columns) or a huge tool result (10,000 lines): nothing panics, every rendered line fits the width, and a summarized tool call stays at most two lines. Pinned by `summary_mode_bounds_huge_tool_output` and `narrow_terminal_welcome` in Task 10.
1. Wide characters and emoji in messages: wrapping never splits a character or exceeds the width. Pinned by `wraps_wide_characters_within_width` in Task 9.
1. Pressing Enter twice quickly on a blank chat: exactly one chat is created, and the second message waits instead of creating a second chat. Pinned by `second_submit_while_creating_does_not_create_twice` in Task 8.

---

## File Structure

```text
unofficial-coder-sdk-rs (branch m1-sdk)
  crates/coder-sdk/src/session.rs     host gate compares parsed host and port too
  crates/coder-sdk/src/lib.rs         GENERATED_FROM constant
  .github/workflows/regenerate.yml    change detection includes untracked files

scuttle (branch m1)
  Cargo.toml                          workspace
  rust-toolchain.toml
  crates/scuttle-core/
    Cargo.toml
    src/lib.rs                        module list and re-exports
    src/live.rs                       in-progress turn assembled from message parts
    src/transcript.rs                 durable messages plus stream state; the reducer
    src/density.rs                    server display prefs and per-block density
    src/usage.rs                      context usage and token formatting
    src/skew.rs                       server version skew warning
    src/commands.rs                   slash command parsing and the command list
    src/config.rs                     local TOML config, secret rejection, write-back
    src/app.rs                        App state machine: Msg in, Effect out
  crates/scuttle-tui/
    Cargo.toml                        [[bin]] scuttle
    src/main.rs                       startup: args, session, skew, run
    src/terminal.rs                   enter and restore terminal modes, panic hook
    src/runtime.rs                    executes API effects with coder-sdk
    src/app.rs                        Tui: event handling and drawing
    src/theme.rs                      terminal palette
    src/wrap.rs                       width-aware wrapping of styled lines
    src/highlight.rs                  lazy syntect highlighting
    src/markdown.rs                   markdown to styled lines with code block ranges
    src/transcript_view.rs            transcript lines, welcome block, click targets
    src/composer.rs                   input box, history, slash completion
    src/footer.rs                     status footer text
    src/picker.rs                     model and workspace pickers
    src/clipboard.rs                  arboard, OSC 52, tmux
    tests/pty.rs                      end-to-end tests in a pseudo terminal
```

---

### Task 1: Harden session host matching in coder-sdk

The M0 final review parked this: `host_key_from_text` reads the authority from raw text, but the `url` crate treats `\` as `/` in `https` URLs, so `https://evil.example\@dev.coder.com` connects to `evil.example` while its raw key reads `dev.coder.com`.
That lets a stored token go to the wrong host, through both the keychain lookup and the session-file fallback.

**Files:**
- Modify: `crates/coder-sdk/src/session.rs`
- Modify: `.github/workflows/regenerate.yml`

**Interfaces:**
- Consumes: the existing `host_key_from_text(raw_text: &str, url: &Url) -> Option<String>`, `token_from_keychain_for_host`, and `discover_with`.
- Produces: private `fn host_keys_agree(raw_text: &str, url: &Url) -> bool`; `discover_with` refuses keychain and session-file tokens whenever it returns false.

- [ ] **Step 1: Create the branch and worktree**

```bash
cd ~/git/nickvigilante/unofficial-coder-sdk-rs && git worktree add .worktrees/m1-sdk -b m1-sdk m0-sdk
```

- [ ] **Step 2: Write the failing tests**

Add to the test module in `crates/coder-sdk/src/session.rs`:

```rust
    #[test]
    fn backslash_userinfo_never_selects_another_hosts_keychain_token() {
        let mut env = FakeEnv::default();
        env.vars.insert("CODER_URL".into(), r"https://evil.example\@dev.coder.com".into());
        env.keychain = Some(keychain_blob("dev.coder.com", "test-token-keychain"));
        let err = discover_with(&env).unwrap_err();
        assert!(err.to_string().contains("coder login"), "{err}");
    }

    #[test]
    fn backslash_userinfo_never_selects_another_hosts_session_file() {
        let mut env = FakeEnv::default();
        env.vars.insert("CODER_URL".into(), r"https://evil.example\@dev.coder.com".into());
        env.files.insert("/cfg/url".into(), "https://dev.coder.com".into());
        env.files.insert("/cfg/session".into(), "test-token-file".into());
        let err = discover_with(&env).unwrap_err();
        assert!(err.to_string().contains("coder login"), "{err}");
    }

    #[test]
    fn host_keys_agree_matches_default_ports_and_rejects_mismatch() {
        let ok: Url = "https://dev.coder.com:443/".parse().unwrap();
        assert!(host_keys_agree("https://dev.coder.com:443/", &ok));
        let bad: Url = r"https://evil.example\@dev.coder.com".parse().unwrap();
        assert!(!host_keys_agree(r"https://evil.example\@dev.coder.com", &bad));
    }

    #[test]
    fn missing_host_keys_are_not_a_match() {
        let url: Url = "file:///tmp/x".parse().unwrap();
        assert!(!host_keys_agree("file:///tmp/x", &url));
    }
```

- [ ] **Step 3: Run them to verify they fail**

Run: `cargo test -p coder-sdk session`
Expected: FAIL; the two `backslash_*` tests return a session, and `host_keys_agree` is undefined.

- [ ] **Step 4: Implement the check**

Add to `crates/coder-sdk/src/session.rs` next to `host_key_from_text`:

```rust
/// True when the raw-text host key and the parsed URL name the same host and port.
/// The raw key matches the `coder` CLI's storage; the parsed URL is where requests really go.
fn host_keys_agree(raw_text: &str, url: &Url) -> bool {
    let Some(raw_key) = host_key_from_text(raw_text, url) else {
        return false;
    };
    let Some(host) = url.host_str() else {
        return false;
    };
    let Some(port) = url.port_or_known_default() else {
        return false;
    };
    let (raw_host, raw_port) = match raw_key.rsplit_once(':') {
        Some((h, p)) if !h.ends_with(']') || raw_key.starts_with('[') => match p.parse::<u16>() {
            Ok(p) => (h.to_owned(), p),
            Err(_) => (raw_key.clone(), port),
        },
        _ => (raw_key.clone(), port),
    };
    raw_host.eq_ignore_ascii_case(host) && raw_port == port
}
```

In `discover_with`, compute `let agree = host_keys_agree(trimmed_url_text, &url);` right after parsing the URL, and gate both token sources on it: the keychain branch runs only `if agree`, and the session-file branch requires `agree` in addition to its existing condition. `CODER_SESSION_TOKEN` stays unconditional. In the existing session-file host comparison, a `None` key on either side no longer counts as a match.

- [ ] **Step 5: Fix change detection in the regeneration workflow**

In `.github/workflows/regenerate.yml`, replace `if git diff --quiet; then` with `if [ -z "$(git status --porcelain)" ]; then` so a regeneration that only adds files still opens a PR.

- [ ] **Step 6: Run the tests and lint**

Run: `cargo test -p coder-sdk && cargo clippy -p coder-sdk --all-targets -- -D warnings && docker run --rm -v "$PWD:/repo" -w /repo rhysd/actionlint:latest -color`
Expected: all session tests pass, including the existing ones; clippy and actionlint are clean.

- [ ] **Step 7: Commit**

```bash
git add -A && git commit -m "fix(coder-sdk): require the parsed host to match the stored host key

Assisted-by: AI"
```

---

### Task 2: Expose the generation source in coder-sdk

**Files:**
- Modify: `crates/coder-sdk/src/lib.rs`

**Interfaces:**
- Produces: `pub const coder_sdk::GENERATED_FROM: &str`, the contents of `spec/coder-ref.txt` (for example `local (d1597a583b)\n`).

- [ ] **Step 1: Write the failing test**

Add to `crates/coder-sdk/src/lib.rs`:

```rust
#[cfg(test)]
mod tests {
    #[test]
    fn generated_from_names_a_coder_ref() {
        let value = super::GENERATED_FROM.trim();
        assert!(value.contains('(') && value.ends_with(')'), "{value}");
    }
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p coder-sdk --lib generated_from`
Expected: FAIL with `cannot find value GENERATED_FROM`.

- [ ] **Step 3: Implement**

Add to `crates/coder-sdk/src/lib.rs`:

```rust
/// The coder/coder ref and commit this SDK was generated from, as written by `scripts/regenerate.sh`.
pub const GENERATED_FROM: &str = include_str!("../../../spec/coder-ref.txt");
```

- [ ] **Step 4: Run it to verify it passes**

Run: `cargo test -p coder-sdk --lib generated_from`
Expected: PASS.

- [ ] **Step 5: Commit and record the revision**

```bash
git add -A && git commit -m "feat(coder-sdk): expose the coder ref the sdk was generated from

Assisted-by: AI" && git rev-parse HEAD
```

Record the full SHA in the task report; Task 3 pins it.

---

### Task 3: Scaffold the scuttle workspace

**Files:**
- Create: `Cargo.toml`, `rust-toolchain.toml`, `.gitignore`
- Create: `crates/scuttle-core/Cargo.toml`, `crates/scuttle-core/src/lib.rs`
- Create: `crates/scuttle-tui/Cargo.toml`, `crates/scuttle-tui/src/main.rs`

**Interfaces:**
- Consumes: the SDK revision recorded in Task 2.
- Produces: crates `scuttle-core` (lib) and `scuttle-tui` (bin `scuttle`), and every dependency later tasks use.

- [ ] **Step 1: Create the branch and worktree**

```bash
cd ~/git/nickvigilante/scuttle && git worktree add .worktrees/m1 -b m1 main
```

- [ ] **Step 2: Write the manifests**

`Cargo.toml`:

```toml
[workspace]
resolver = "3"
members = ["crates/scuttle-core", "crates/scuttle-tui"]

[workspace.package]
edition = "2024"
publish = false

[workspace.dependencies]
coder-sdk = { git = "file://<unofficial-coder-sdk-rs checkout>", rev = "REPLACE_WITH_TASK_2_SHA" }
futures = "0.3"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
thiserror = "2"
tokio = { version = "1", features = ["macros", "rt-multi-thread", "sync", "time", "process"] }
toml = "0.9"
toml_edit = "0.23"
uuid = { version = "1", features = ["serde", "v4"] }
```

Replace `REPLACE_WITH_TASK_2_SHA` with the full SHA from Task 2's report.

`rust-toolchain.toml`:

```toml
[toolchain]
channel = "1.98"
components = ["rustfmt", "clippy"]
```

`.gitignore`:

```text
/target
```

`crates/scuttle-core/Cargo.toml`:

```toml
[package]
name = "scuttle-core"
version = "0.0.0"
edition.workspace = true
publish.workspace = true

[dependencies]
coder-sdk.workspace = true
serde.workspace = true
serde_json.workspace = true
thiserror.workspace = true
toml.workspace = true
toml_edit.workspace = true
uuid.workspace = true
```

`crates/scuttle-core/src/lib.rs`:

```rust
//! Headless chat engine for scuttle: state, the stream reducer, commands, and settings.
```

`crates/scuttle-tui/Cargo.toml`:

```toml
[package]
name = "scuttle-tui"
version = "0.0.0"
edition.workspace = true
publish.workspace = true

[[bin]]
name = "scuttle"
path = "src/main.rs"

[dependencies]
arboard = "3.6"
base64 = "0.22"
coder-sdk.workspace = true
crossterm = { version = "0.29", features = ["event-stream"] }
futures.workspace = true
pulldown-cmark = "0.13"
ratatui = "0.30"
ratatui-textarea = "0.9"
scuttle-core = { path = "../scuttle-core" }
serde_json.workspace = true
syntect = "5.3"
terminal-colorsaurus = "1.0"
tokio.workspace = true
two-face = "0.5"
unicode-width = "0.2"
uuid.workspace = true

[dev-dependencies]
insta = "1"
portable-pty = "0.9"
tokio-tungstenite = "0.30"
vt100 = "0.15"
wiremock = "0.6"
```

`crates/scuttle-tui/src/main.rs`:

```rust
fn main() {}
```

- [ ] **Step 3: Verify the workspace resolves and builds**

Run: `cargo check --workspace`
Expected: finishes with no errors. If a crate version above does not exist, use the newest compatible release (`cargo search <name>`) and report it.

- [ ] **Step 4: Commit**

```bash
git add -A && git commit -m "chore: scaffold the scuttle workspace

Assisted-by: AI"
```

---

### Task 4: The live turn reducer

**Files:**
- Create: `crates/scuttle-core/src/live.rs`
- Modify: `crates/scuttle-core/src/lib.rs`

**Interfaces:**
- Consumes: `coder_sdk::types::CodersdkChatStreamMessagePart` and `CodersdkChatMessagePart` (string-newtype enums; use `.as_str()` through `Deref`).
- Produces: `scuttle_core::live::{Applied, LiveBlock, LiveTurn, parse_partial_json}` with `LiveTurn::{apply, clear, reset_preview, set_idle, is_empty}` and `pub blocks: Vec<LiveBlock>`.

- [ ] **Step 1: Write the failing tests**

Create `crates/scuttle-core/src/live.rs` containing only this test module, then add the implementation above it in Step 3:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use coder_sdk::types::CodersdkChatStreamMessagePart;
    use serde_json::json;

    fn mp(hv: i64, ga: i64, seq: i64, part: serde_json::Value) -> CodersdkChatStreamMessagePart {
        serde_json::from_value(json!({
            "role": "assistant", "history_version": hv, "generation_attempt": ga, "seq": seq, "part": part
        }))
        .unwrap()
    }

    fn text(t: &str) -> serde_json::Value {
        json!({"type": "text", "text": t})
    }

    #[test]
    fn text_deltas_append_to_one_block() {
        let mut live = LiveTurn::default();
        assert_eq!(live.apply(&mp(1, 1, 1, text("Hel"))), Applied::Changed);
        assert_eq!(live.apply(&mp(1, 1, 2, text("lo"))), Applied::Changed);
        assert_eq!(live.blocks, vec![LiveBlock::Text("Hello".into())]);
    }

    #[test]
    fn whitespace_only_delta_is_skipped() {
        let mut live = LiveTurn::default();
        live.apply(&mp(1, 1, 1, text("   ")));
        assert!(live.is_empty());
    }

    #[test]
    fn reasoning_then_text_are_separate_blocks() {
        let mut live = LiveTurn::default();
        live.apply(&mp(1, 1, 1, json!({"type": "reasoning", "text": "think"})));
        live.apply(&mp(1, 1, 2, text("answer")));
        assert_eq!(live.blocks, vec![LiveBlock::Reasoning("think".into()), LiveBlock::Text("answer".into())]);
    }

    #[test]
    fn newer_generation_resets_and_older_is_ignored() {
        let mut live = LiveTurn::default();
        live.apply(&mp(1, 1, 1, text("old")));
        live.apply(&mp(1, 2, 1, text("new")));
        assert_eq!(live.blocks, vec![LiveBlock::Text("new".into())]);
        assert_eq!(live.apply(&mp(1, 1, 2, text("stale"))), Applied::Unchanged);
        assert_eq!(live.blocks, vec![LiveBlock::Text("new".into())]);
    }

    #[test]
    fn duplicate_seq_is_ignored_and_gap_requests_reconnect() {
        let mut live = LiveTurn::default();
        live.apply(&mp(1, 1, 1, text("a")));
        assert_eq!(live.apply(&mp(1, 1, 1, text("a"))), Applied::Unchanged);
        assert!(matches!(live.apply(&mp(1, 1, 3, text("c"))), Applied::Reconnect(_)));
    }

    #[test]
    fn tool_call_args_stream_as_partial_json() {
        let mut live = LiveTurn::default();
        let call = |seq, delta: &str| mp(1, 1, seq, json!({"type": "tool-call", "tool_call_id": "t1", "tool_name": "read_file", "args_delta": delta}));
        live.apply(&call(1, r#"{"path": "/tm"#));
        match &live.blocks[0] {
            LiveBlock::ToolCall { name, args, .. } => {
                assert_eq!(name, "read_file");
                assert_eq!(args.as_ref().unwrap()["path"], "/tm");
            }
            other => panic!("unexpected {other:?}"),
        }
        live.apply(&mp(1, 1, 2, json!({"type": "tool-call", "tool_call_id": "t1", "args": {"path": "/tmp/x"}})));
        match &live.blocks[0] {
            LiveBlock::ToolCall { args, .. } => assert_eq!(args.as_ref().unwrap()["path"], "/tmp/x"),
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(live.blocks.len(), 1);
    }

    #[test]
    fn provider_executed_tool_calls_are_skipped() {
        let mut live = LiveTurn::default();
        live.apply(&mp(1, 1, 1, json!({"type": "tool-call", "tool_call_id": "t1", "tool_name": "web", "provider_executed": true})));
        assert!(live.is_empty());
    }

    #[test]
    fn tool_result_deltas_reset_and_finalize() {
        let mut live = LiveTurn::default();
        let res = |seq, v: serde_json::Value| mp(1, 1, seq, v);
        live.apply(&res(1, json!({"type": "tool-result", "tool_call_id": "t1", "tool_name": "execute", "result_delta": "line1\n", "reasoning_delta": "hmm"})));
        live.apply(&res(2, json!({"type": "tool-result", "tool_call_id": "t1", "result_reset": true})));
        live.apply(&res(3, json!({"type": "tool-result", "tool_call_id": "t1", "result_delta": "again\n"})));
        live.apply(&res(4, json!({"type": "tool-result", "tool_call_id": "t1", "result": {"output": "done"}, "is_error": false})));
        match &live.blocks[0] {
            LiveBlock::ToolResult { result, result_raw, reasoning, done, is_error, .. } => {
                assert!(*done);
                assert!(!*is_error);
                assert_eq!(result.as_ref().unwrap()["output"], "done");
                assert!(result_raw.contains("done"));
                assert!(reasoning.is_empty());
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn sources_are_deduplicated_by_url() {
        let mut live = LiveTurn::default();
        live.apply(&mp(1, 1, 1, json!({"type": "source", "url": "https://a", "title": "A"})));
        live.apply(&mp(1, 1, 2, json!({"type": "source", "url": "https://a"})));
        assert_eq!(live.blocks.len(), 1);
    }

    #[test]
    fn idle_ignores_same_generation_until_a_new_one_starts() {
        let mut live = LiveTurn::default();
        live.apply(&mp(1, 1, 1, text("a")));
        live.set_idle(true);
        assert_eq!(live.apply(&mp(1, 1, 2, text("late"))), Applied::Unchanged);
        assert_eq!(live.apply(&mp(2, 1, 1, text("next"))), Applied::Changed);
        assert_eq!(live.blocks, vec![LiveBlock::Text("next".into())]);
    }

    #[test]
    fn preview_reset_allows_replay_from_seq_one() {
        let mut live = LiveTurn::default();
        live.apply(&mp(1, 1, 1, text("a")));
        live.apply(&mp(1, 1, 2, text("b")));
        live.reset_preview();
        assert_eq!(live.apply(&mp(1, 1, 1, text("a"))), Applied::Changed);
        assert_eq!(live.blocks, vec![LiveBlock::Text("a".into())]);
    }

    #[test]
    fn partial_json_closes_open_strings_and_brackets() {
        assert_eq!(parse_partial_json(r#"{"a": [1, 2"#), Some(json!({"a": [1, 2]})));
        assert_eq!(parse_partial_json(r#"{"a": "b"#), Some(json!({"a": "b"})));
        assert_eq!(parse_partial_json("not json"), None);
    }
}
```

Add `pub mod live;` to `crates/scuttle-core/src/lib.rs`.

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p scuttle-core live`
Expected: FAIL with unresolved names `LiveTurn`, `LiveBlock`, `Applied`, `parse_partial_json`.

- [ ] **Step 3: Implement**

Insert above the test module in `crates/scuttle-core/src/live.rs`:

```rust
//! The in-progress assistant turn, assembled from `message_part` stream events.

use coder_sdk::types;
use serde_json::Value;

/// The result of applying one stream event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Applied {
    Changed,
    Unchanged,
    /// The stream is inconsistent, for example after a `seq` gap, and must be reopened.
    Reconnect(String),
}

/// One piece of the in-progress turn.
#[derive(Debug, Clone, PartialEq)]
pub enum LiveBlock {
    Text(String),
    Reasoning(String),
    ToolCall { id: String, name: String, args_raw: String, args: Option<Value> },
    ToolResult {
        id: String,
        name: String,
        result_raw: String,
        result: Option<Value>,
        reasoning: String,
        is_error: bool,
        done: bool,
    },
    Source { url: String, title: Option<String> },
}

/// Parts of the current generation, applied in `seq` order.
#[derive(Debug, Default)]
pub struct LiveTurn {
    pub blocks: Vec<LiveBlock>,
    cursor: Option<(i64, i64)>,
    last_seq: i64,
    idle: bool,
}

impl LiveTurn {
    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }

    pub fn clear(&mut self) {
        self.blocks.clear();
    }

    /// Discards in-progress parts and accepts a replay of the current generation from `seq` 1.
    pub fn reset_preview(&mut self) {
        self.blocks.clear();
        self.last_seq = 0;
    }

    /// While idle, late parts of the current generation are dropped; a newer generation clears it.
    pub fn set_idle(&mut self, idle: bool) {
        self.idle = idle;
    }

    pub fn apply(&mut self, mp: &types::CodersdkChatStreamMessagePart) -> Applied {
        let key = (mp.history_version.unwrap_or(0), mp.generation_attempt.unwrap_or(0));
        match self.cursor {
            Some(current) if key < current => return Applied::Unchanged,
            Some(current) if key == current => {}
            _ => {
                self.cursor = Some(key);
                self.blocks.clear();
                self.last_seq = 0;
                self.idle = false;
            }
        }
        if self.idle {
            return Applied::Unchanged;
        }
        let seq = mp.seq.unwrap_or(0);
        if seq <= self.last_seq {
            return Applied::Unchanged;
        }
        if seq != self.last_seq + 1 {
            return Applied::Reconnect(format!("stream gap: expected seq {}, got {seq}", self.last_seq + 1));
        }
        self.last_seq = seq;
        if let Some(part) = mp.part.as_ref() {
            self.apply_part(part);
        }
        Applied::Changed
    }

    fn apply_part(&mut self, part: &types::CodersdkChatMessagePart) {
        let kind = part.type_.as_ref().map(|t| t.as_str()).unwrap_or_default();
        match kind {
            "text" => append(&mut self.blocks, false, part.text.as_deref().unwrap_or_default()),
            "reasoning" => append(&mut self.blocks, true, part.text.as_deref().unwrap_or_default()),
            "tool-call" => self.apply_tool_call(part),
            "tool-result" => self.apply_tool_result(part),
            "source" => {
                let Some(url) = part.url.clone() else { return };
                let seen = self.blocks.iter().any(|b| matches!(b, LiveBlock::Source { url: u, .. } if *u == url));
                if !seen {
                    self.blocks.push(LiveBlock::Source { url, title: part.title.clone() });
                }
            }
            _ => {}
        }
    }

    fn apply_tool_call(&mut self, part: &types::CodersdkChatMessagePart) {
        if part.provider_executed == Some(true) {
            return;
        }
        let id = part.tool_call_id.clone().unwrap_or_default();
        let name = part.tool_name.clone().unwrap_or_default();
        let idx = match self.blocks.iter().position(|b| matches!(b, LiveBlock::ToolCall { id: i, .. } if *i == id)) {
            Some(i) => i,
            None => {
                self.blocks.push(LiveBlock::ToolCall { id, name: String::new(), args_raw: String::new(), args: None });
                self.blocks.len() - 1
            }
        };
        if let LiveBlock::ToolCall { name: block_name, args_raw, args, .. } = &mut self.blocks[idx] {
            if block_name.is_empty() {
                *block_name = name;
            }
            if let Some(full) = part.args.as_ref() {
                *args_raw = full.to_string();
                *args = Some(full.clone());
            } else if let Some(delta) = part.args_delta.as_deref() {
                args_raw.push_str(delta);
                *args = parse_partial_json(args_raw);
            }
        }
    }

    fn apply_tool_result(&mut self, part: &types::CodersdkChatMessagePart) {
        let id = part.tool_call_id.clone().unwrap_or_default();
        let name = part.tool_name.clone().unwrap_or_default();
        let idx = match self.blocks.iter().position(|b| matches!(b, LiveBlock::ToolResult { id: i, .. } if *i == id)) {
            Some(i) => i,
            None => {
                self.blocks.push(LiveBlock::ToolResult {
                    id,
                    name: String::new(),
                    result_raw: String::new(),
                    result: None,
                    reasoning: String::new(),
                    is_error: false,
                    done: false,
                });
                self.blocks.len() - 1
            }
        };
        if let LiveBlock::ToolResult { name: block_name, result_raw, result, reasoning, is_error, done, .. } = &mut self.blocks[idx] {
            if block_name.is_empty() {
                *block_name = name;
            }
            if part.result_reset == Some(true) {
                result_raw.clear();
                *result = None;
                *done = false;
            }
            if part.result.is_some() || part.is_error.is_some() {
                *result = part.result.clone();
                *result_raw = part.result.as_ref().map(Value::to_string).unwrap_or_default();
                *is_error = part.is_error.unwrap_or(false);
                *done = true;
                reasoning.clear();
            } else {
                if let Some(delta) = part.result_delta.as_deref() {
                    result_raw.push_str(delta);
                }
                if let Some(delta) = part.reasoning_delta.as_deref() {
                    reasoning.push_str(delta);
                }
            }
        }
    }
}

fn append(blocks: &mut Vec<LiveBlock>, reasoning: bool, delta: &str) {
    if delta.trim().is_empty() {
        return;
    }
    match (blocks.last_mut(), reasoning) {
        (Some(LiveBlock::Reasoning(s)), true) | (Some(LiveBlock::Text(s)), false) => s.push_str(delta),
        (_, true) => blocks.push(LiveBlock::Reasoning(delta.to_owned())),
        (_, false) => blocks.push(LiveBlock::Text(delta.to_owned())),
    }
}

/// Parses JSON that may be cut off mid-stream by closing any open string, array, or object.
pub fn parse_partial_json(raw: &str) -> Option<Value> {
    if let Ok(v) = serde_json::from_str(raw) {
        return Some(v);
    }
    let mut closers = Vec::new();
    let mut in_string = false;
    let mut escaped = false;
    for c in raw.chars() {
        if in_string {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '"' => in_string = true,
            '{' => closers.push('}'),
            '[' => closers.push(']'),
            '}' | ']' => {
                closers.pop();
            }
            _ => {}
        }
    }
    let mut fixed = raw.trim_end().trim_end_matches(',').to_owned();
    if in_string {
        if escaped {
            fixed.pop();
        }
        fixed.push('"');
    }
    while let Some(c) = closers.pop() {
        fixed.push(c);
    }
    serde_json::from_str(&fixed).ok()
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p scuttle-core live`
Expected: all 12 tests pass.

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat(scuttle-core): assemble the live turn from stream parts

Assisted-by: AI"
```

---

### Task 5: The transcript reducer

**Files:**
- Create: `crates/scuttle-core/src/transcript.rs`
- Modify: `crates/scuttle-core/src/lib.rs`

**Interfaces:**
- Consumes: `live::{Applied, LiveTurn}`; `coder_sdk::{StreamEvent, StreamEventType, ChatStatus, types}`.
- Produces: `scuttle_core::transcript::{Transcript, RetryInfo}` with `Transcript::{load, apply, messages, last_message_id}` and public fields `live`, `status: Option<ChatStatus>`, `last_error: Option<String>`, `retry: Option<RetryInfo>`, `queued: Vec<types::CodersdkChatQueuedMessage>`, `action_required: Vec<String>`; `RetryInfo { attempt: i64, delay_ms: i64, error: String }`.

- [ ] **Step 1: Write the failing tests**

Create `crates/scuttle-core/src/transcript.rs` with this test module; the implementation goes above it in Step 3:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::live::LiveBlock;
    use serde_json::json;

    fn ev(v: serde_json::Value) -> StreamEvent {
        StreamEvent {
            kind: StreamEventType::parse(v["type"].as_str().unwrap_or_default()),
            event: serde_json::from_value(v.clone()).ok(),
            raw: v,
        }
    }

    fn message(id: i64, role: &str, text: &str) -> serde_json::Value {
        json!({"type": "message", "message": {"id": id, "role": role, "content": [{"type": "text", "text": text}]}})
    }

    fn part(seq: i64, text: &str) -> serde_json::Value {
        json!({"type": "message_part", "message_part": {"history_version": 1, "generation_attempt": 1, "seq": seq, "role": "assistant", "part": {"type": "text", "text": text}}})
    }

    fn ids(t: &Transcript) -> Vec<i64> {
        t.messages().filter_map(|m| m.id).collect()
    }

    #[test]
    fn messages_are_upserted_in_id_order() {
        let mut t = Transcript::default();
        t.apply(&ev(message(2, "assistant", "b")));
        t.apply(&ev(message(1, "user", "a")));
        t.apply(&ev(message(2, "assistant", "b2")));
        assert_eq!(ids(&t), vec![1, 2]);
        assert_eq!(t.last_message_id(), Some(2));
    }

    #[test]
    fn snapshot_replay_does_not_duplicate_messages() {
        let mut t = Transcript::default();
        for id in 1..=3 {
            t.apply(&ev(message(id, "user", "x")));
        }
        for id in 1..=3 {
            t.apply(&ev(message(id, "user", "x")));
        }
        assert_eq!(ids(&t), vec![1, 2, 3]);
    }

    #[test]
    fn assistant_message_clears_live_but_user_message_does_not() {
        let mut t = Transcript::default();
        t.apply(&ev(part(1, "streaming")));
        t.apply(&ev(message(1, "user", "q")));
        assert!(!t.live.is_empty());
        t.apply(&ev(message(2, "assistant", "final")));
        assert!(t.live.is_empty());
    }

    #[test]
    fn history_reset_replaces_messages_from_the_first_replacement_onward() {
        let mut t = Transcript::default();
        for id in 1..=4 {
            t.apply(&ev(message(id, "user", "old")));
        }
        t.apply(&ev(json!({"type": "history_reset"})));
        t.apply(&ev(message(3, "user", "edited")));
        assert_eq!(ids(&t), vec![1, 2, 3, 4], "buffered until the next non-message event");
        t.apply(&ev(json!({"type": "status", "status": {"status": "running"}})));
        assert_eq!(ids(&t), vec![1, 2, 3]);
        let edited = t.messages().last().unwrap();
        assert_eq!(edited.content[0].text.as_deref(), Some("edited"));
    }

    #[test]
    fn waiting_status_makes_live_idle() {
        let mut t = Transcript::default();
        t.apply(&ev(part(1, "a")));
        t.apply(&ev(json!({"type": "status", "status": {"status": "waiting"}})));
        assert_eq!(t.status, Some(ChatStatus::Waiting));
        assert_eq!(t.apply(&ev(part(2, "late"))), Applied::Unchanged);
    }

    #[test]
    fn error_and_retry_clear_live_and_record_details() {
        let mut t = Transcript::default();
        t.apply(&ev(part(1, "a")));
        t.apply(&ev(json!({"type": "retry", "retry": {"attempt": 2, "delay_ms": 1500, "error": "rate limited"}})));
        assert!(t.live.is_empty());
        assert_eq!(t.retry, Some(RetryInfo { attempt: 2, delay_ms: 1500, error: "rate limited".into() }));
        t.apply(&ev(json!({"type": "error", "error": {"message": "boom"}})));
        assert_eq!(t.last_error.as_deref(), Some("boom"));
    }

    #[test]
    fn queue_update_replaces_the_queue() {
        let mut t = Transcript::default();
        t.apply(&ev(json!({"type": "queue_update", "queued_messages": [{"id": 1, "content": []}, {"id": 2, "content": []}]})));
        assert_eq!(t.queued.len(), 2);
        t.apply(&ev(json!({"type": "queue_update", "queued_messages": []})));
        assert!(t.queued.is_empty());
    }

    #[test]
    fn unknown_event_is_unchanged_and_gap_propagates_reconnect() {
        let mut t = Transcript::default();
        assert_eq!(t.apply(&ev(json!({"type": "from_the_future"}))), Applied::Unchanged);
        t.apply(&ev(part(1, "a")));
        assert!(matches!(t.apply(&ev(part(5, "e"))), Applied::Reconnect(_)));
    }

    #[test]
    fn load_then_live_part_keeps_both() {
        let mut t = Transcript::default();
        let m: types::CodersdkChatMessage = serde_json::from_value(json!({"id": 7, "role": "user", "content": []})).unwrap();
        t.load(vec![m]);
        t.apply(&ev(part(1, "hi")));
        assert_eq!(ids(&t), vec![7]);
        assert_eq!(t.live.blocks, vec![LiveBlock::Text("hi".into())]);
    }
}
```

Add `pub mod transcript;` to `crates/scuttle-core/src/lib.rs`.

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p scuttle-core transcript`
Expected: FAIL with unresolved `Transcript` and `RetryInfo`.

- [ ] **Step 3: Implement**

Insert above the test module in `crates/scuttle-core/src/transcript.rs`:

```rust
//! Durable messages plus stream state, updated by the chat stream reducer.

use std::collections::BTreeMap;

use coder_sdk::{ChatStatus, StreamEvent, StreamEventType, types};

pub use crate::live::Applied;
use crate::live::LiveTurn;

/// A provider retry the server reported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetryInfo {
    pub attempt: i64,
    pub delay_ms: i64,
    pub error: String,
}

/// Everything shown for one chat: durable history and the in-progress turn.
#[derive(Debug, Default)]
pub struct Transcript {
    messages: BTreeMap<i64, types::CodersdkChatMessage>,
    pub live: LiveTurn,
    pub status: Option<ChatStatus>,
    pub last_error: Option<String>,
    pub retry: Option<RetryInfo>,
    pub queued: Vec<types::CodersdkChatQueuedMessage>,
    pub action_required: Vec<String>,
    pending_history: Option<Vec<types::CodersdkChatMessage>>,
}

impl Transcript {
    pub fn messages(&self) -> impl DoubleEndedIterator<Item = &types::CodersdkChatMessage> {
        self.messages.values()
    }

    pub fn last_message_id(&self) -> Option<i64> {
        self.messages.keys().next_back().copied()
    }

    pub fn load(&mut self, messages: Vec<types::CodersdkChatMessage>) {
        for m in messages {
            self.upsert(m);
        }
    }

    fn upsert(&mut self, m: types::CodersdkChatMessage) {
        if let Some(id) = m.id {
            self.messages.insert(id, m);
        }
    }

    fn flush_history(&mut self) {
        let Some(buffer) = self.pending_history.take() else { return };
        if let Some(first) = buffer.iter().filter_map(|m| m.id).min() {
            self.messages.retain(|id, _| *id < first);
        }
        for m in buffer {
            self.upsert(m);
        }
    }

    /// Applies one stream event. Returns `Applied::Reconnect` when the stream must be reopened.
    pub fn apply(&mut self, ev: &StreamEvent) -> Applied {
        if self.pending_history.is_some() && ev.kind != StreamEventType::Message {
            self.flush_history();
        }
        let Some(e) = ev.event.as_ref() else {
            return match ev.kind {
                StreamEventType::PreviewReset => {
                    self.live.reset_preview();
                    Applied::Changed
                }
                StreamEventType::HistoryReset => {
                    self.pending_history = Some(Vec::new());
                    Applied::Changed
                }
                _ => Applied::Unchanged,
            };
        };
        match &ev.kind {
            StreamEventType::Message => {
                let Some(m) = e.message.clone() else { return Applied::Unchanged };
                if let Some(buffer) = self.pending_history.as_mut() {
                    buffer.push(m);
                    return Applied::Changed;
                }
                let assistant = m.role.as_ref().map(|r| r.as_str()) == Some("assistant");
                self.upsert(m);
                self.retry = None;
                if assistant {
                    self.live.clear();
                }
                Applied::Changed
            }
            StreamEventType::MessagePart => match e.message_part.as_ref() {
                Some(mp) => self.live.apply(mp),
                None => Applied::Unchanged,
            },
            StreamEventType::Status => {
                let status = e.status.as_ref().and_then(|s| s.status.as_ref()).map(|s| ChatStatus::parse(s.as_str()));
                match status {
                    Some(ChatStatus::Waiting) => {
                        self.live.set_idle(true);
                        self.retry = None;
                    }
                    Some(ChatStatus::Running) => self.live.set_idle(false),
                    _ => {}
                }
                self.status = status;
                Applied::Changed
            }
            StreamEventType::Error => {
                let message = e.error.as_ref().and_then(|x| x.message.clone()).unwrap_or_else(|| "unknown error".into());
                self.last_error = Some(message);
                self.live.clear();
                Applied::Changed
            }
            StreamEventType::Retry => {
                if let Some(r) = e.retry.as_ref() {
                    self.retry = Some(RetryInfo {
                        attempt: r.attempt.unwrap_or(0),
                        delay_ms: r.delay_ms.unwrap_or(0),
                        error: r.error.clone().unwrap_or_default(),
                    });
                }
                self.live.clear();
                Applied::Changed
            }
            StreamEventType::QueueUpdate => {
                self.queued = e.queued_messages.clone();
                Applied::Changed
            }
            StreamEventType::ActionRequired => {
                self.action_required = e
                    .action_required
                    .as_ref()
                    .map(|a| a.tool_calls.iter().filter_map(|c| c.tool_name.clone()).collect())
                    .unwrap_or_default();
                Applied::Changed
            }
            StreamEventType::PreviewReset => {
                self.live.reset_preview();
                Applied::Changed
            }
            StreamEventType::HistoryReset => {
                self.pending_history = Some(Vec::new());
                Applied::Changed
            }
            StreamEventType::Unknown(_) => Applied::Unchanged,
        }
    }
}
```

Check the generated field names used above (`CodersdkChatError::message`, `CodersdkChatStreamRetry::{attempt, delay_ms, error}`, `CodersdkChatStreamActionRequired::tool_calls` and each call's `tool_name`) in `coder-api-gen`'s `generated.rs` in the pinned SDK checkout under `~/.cargo/git/checkouts`, and adapt if they differ.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p scuttle-core transcript`
Expected: all 9 tests pass.

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat(scuttle-core): add the transcript stream reducer

Assisted-by: AI"
```

---

### Task 6: Display preferences, context usage, and version skew

**Files:**
- Create: `crates/scuttle-core/src/density.rs`, `crates/scuttle-core/src/usage.rs`, `crates/scuttle-core/src/skew.rs`
- Modify: `crates/scuttle-core/src/lib.rs`

**Interfaces:**
- Produces:
  - `density::{Density, DisplayPrefs, SendShortcut, BlockKind, density_for}`. `Density` is `Expanded | Summary | Hidden`, derives `Deserialize` with lowercase names, and is `Copy`. `DisplayPrefs { thinking: String, shell: String, diff: String, collapse_steps: bool, send_shortcut: SendShortcut }` with `Default` and `From<&types::CodersdkUserPreferenceSettings>`. `SendShortcut` is `Enter | ModifierEnter`. `BlockKind<'a>` is `Reasoning | Tool(&'a str)`. `density_for(kind: BlockKind, prefs: &DisplayPrefs, overrides: &BTreeMap<String, Density>) -> Density`.
  - `usage::{ContextUsage, context_usage, format_tokens}`. `ContextUsage { used: i64, limit: i64 }`. `context_usage<'a>(messages: impl DoubleEndedIterator<Item = &'a types::CodersdkChatMessage>) -> Option<ContextUsage>`. `format_tokens(n: i64) -> String`.
  - `skew::skew_warning(server: &str, generated_from: &str) -> Option<String>`.

- [ ] **Step 1: Write the failing tests**

`crates/scuttle-core/src/density.rs`, tests only for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn web_defaults_collapse_shell_and_summarize_everything() {
        let prefs = DisplayPrefs::default();
        let none = BTreeMap::new();
        assert_eq!(density_for(BlockKind::Tool("execute"), &prefs, &none), Density::Summary);
        assert_eq!(density_for(BlockKind::Tool("edit_files"), &prefs, &none), Density::Summary);
        assert_eq!(density_for(BlockKind::Reasoning, &prefs, &none), Density::Summary);
    }

    #[test]
    fn server_prefs_expand_shell_and_diff_tools() {
        let prefs = DisplayPrefs { shell: "always_expanded".into(), diff: "always_expanded".into(), ..DisplayPrefs::default() };
        let none = BTreeMap::new();
        assert_eq!(density_for(BlockKind::Tool("process_output"), &prefs, &none), Density::Expanded);
        assert_eq!(density_for(BlockKind::Tool("write_file"), &prefs, &none), Density::Expanded);
    }

    #[test]
    fn local_overrides_apply_only_to_tools_the_server_does_not_cover() {
        let prefs = DisplayPrefs::default();
        let mut overrides = BTreeMap::new();
        overrides.insert("read_file".to_string(), Density::Hidden);
        overrides.insert("execute".to_string(), Density::Hidden);
        assert_eq!(density_for(BlockKind::Tool("read_file"), &prefs, &overrides), Density::Hidden);
        assert_eq!(density_for(BlockKind::Tool("execute"), &prefs, &overrides), Density::Summary);
    }

    #[test]
    fn prefs_convert_from_server_settings() {
        let settings: coder_sdk::types::CodersdkUserPreferenceSettings = serde_json::from_value(serde_json::json!({
            "thinking_display_mode": "always_expanded",
            "shell_tool_display_mode": "auto",
            "code_diff_display_mode": "always_collapsed",
            "collapse_assistant_steps": true,
            "agent_chat_send_shortcut": "modifier_enter"
        }))
        .unwrap();
        let prefs = DisplayPrefs::from(&settings);
        assert_eq!(prefs.thinking, "always_expanded");
        assert_eq!(prefs.send_shortcut, SendShortcut::ModifierEnter);
        assert!(prefs.collapse_steps);
    }
}
```

`crates/scuttle-core/src/usage.rs`, tests only for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn msgs(v: serde_json::Value) -> Vec<types::CodersdkChatMessage> {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn uses_the_latest_message_with_usage() {
        let m = msgs(json!([
            {"id": 1, "role": "assistant", "content": [], "usage": {"input_tokens": 10, "output_tokens": 5, "context_limit": 1000}},
            {"id": 2, "role": "user", "content": []},
            {"id": 3, "role": "assistant", "content": [], "usage": {"input_tokens": 100, "output_tokens": 20, "cache_read_tokens": 30, "cache_creation_tokens": 4, "reasoning_tokens": 6, "context_limit": 2000}}
        ]));
        assert_eq!(context_usage(m.iter()), Some(ContextUsage { used: 160, limit: 2000 }));
    }

    #[test]
    fn cleared_boundary_means_no_usage_and_summarized_uses_the_estimate() {
        let cleared = msgs(json!([
            {"id": 1, "role": "assistant", "content": [], "usage": {"input_tokens": 10, "context_limit": 1000}},
            {"id": 2, "role": "tool", "content": [{"type": "tool-result", "tool_name": "chat_cleared", "result": {}}]}
        ]));
        assert_eq!(context_usage(cleared.iter()), None);
        let summarized = msgs(json!([
            {"id": 1, "role": "assistant", "content": [], "usage": {"input_tokens": 10, "context_limit": 1000}},
            {"id": 2, "role": "tool", "content": [{"type": "tool-result", "tool_name": "chat_summarized", "result": {"estimated_context_tokens": 42, "context_limit_tokens": 1000}}]}
        ]));
        assert_eq!(context_usage(summarized.iter()), Some(ContextUsage { used: 42, limit: 1000 }));
    }

    #[test]
    fn formats_tokens_compactly() {
        assert_eq!(format_tokens(950), "950");
        assert_eq!(format_tokens(12_345), "12.3k");
        assert_eq!(format_tokens(1_200_000), "1.2M");
    }
}
```

`crates/scuttle-core/src/skew.rs`, tests only for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warns_only_when_more_than_one_minor_ahead() {
        assert_eq!(skew_warning("v2.38.0+abc", "v2.37.3 (abc)"), None);
        assert!(skew_warning("v2.39.1", "v2.37.3 (abc)").is_some());
        assert!(skew_warning("v3.0.0", "v2.37.3 (abc)").is_some());
    }

    #[test]
    fn unparseable_versions_never_warn() {
        assert_eq!(skew_warning("v2.40.0", "local (d1597a583b)"), None);
        assert_eq!(skew_warning("devel", "v2.37.3 (abc)"), None);
    }
}
```

Add `pub mod density; pub mod skew; pub mod usage;` to `lib.rs`.

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p scuttle-core`
Expected: FAIL with unresolved names in all three modules.

- [ ] **Step 3: Implement**

Above the tests in `density.rs`:

```rust
//! How much of each reasoning or tool block to show, following the web UI's display preferences.

use std::collections::BTreeMap;

use coder_sdk::types;
use serde::Deserialize;

/// How a block renders.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Density {
    Expanded,
    Summary,
    Hidden,
}

/// Which key sends a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendShortcut {
    Enter,
    ModifierEnter,
}

/// The server-stored display preferences shared with the web UI.
#[derive(Debug, Clone, PartialEq)]
pub struct DisplayPrefs {
    pub thinking: String,
    pub shell: String,
    pub diff: String,
    pub collapse_steps: bool,
    pub send_shortcut: SendShortcut,
}

impl Default for DisplayPrefs {
    fn default() -> Self {
        DisplayPrefs {
            thinking: "auto".into(),
            shell: "always_collapsed".into(),
            diff: "auto".into(),
            collapse_steps: false,
            send_shortcut: SendShortcut::Enter,
        }
    }
}

impl From<&types::CodersdkUserPreferenceSettings> for DisplayPrefs {
    fn from(s: &types::CodersdkUserPreferenceSettings) -> Self {
        let d = DisplayPrefs::default();
        DisplayPrefs {
            thinking: s.thinking_display_mode.as_ref().map(|v| v.to_string()).unwrap_or(d.thinking),
            shell: s.shell_tool_display_mode.as_ref().map(|v| v.to_string()).unwrap_or(d.shell),
            diff: s.code_diff_display_mode.as_ref().map(|v| v.to_string()).unwrap_or(d.diff),
            collapse_steps: s.collapse_assistant_steps.unwrap_or(false),
            send_shortcut: match s.agent_chat_send_shortcut.as_ref().map(|v| v.as_str()) {
                Some("modifier_enter") => SendShortcut::ModifierEnter,
                _ => SendShortcut::Enter,
            },
        }
    }
}

/// The kind of block being rendered.
#[derive(Debug, Clone, Copy)]
pub enum BlockKind<'a> {
    Reasoning,
    Tool(&'a str),
}

const SHELL_TOOLS: &[&str] = &["execute", "process_output"];
const DIFF_TOOLS: &[&str] = &["write_file", "edit_files"];

fn from_mode(mode: &str) -> Density {
    match mode {
        "always_expanded" => Density::Expanded,
        _ => Density::Summary,
    }
}

/// The density for a block, before any per-block toggle the user applied this session.
pub fn density_for(kind: BlockKind, prefs: &DisplayPrefs, overrides: &BTreeMap<String, Density>) -> Density {
    match kind {
        BlockKind::Reasoning => from_mode(&prefs.thinking),
        BlockKind::Tool(name) if SHELL_TOOLS.contains(&name) => from_mode(&prefs.shell),
        BlockKind::Tool(name) if DIFF_TOOLS.contains(&name) => from_mode(&prefs.diff),
        BlockKind::Tool(name) => overrides.get(name).copied().unwrap_or(Density::Summary),
    }
}
```

The generated preference newtypes deref to `String`, so `v.to_string()` and `v.as_str()` work through `Deref`; adapt if the resolved type differs.

Above the tests in `usage.rs`:

```rust
//! Context window usage, computed the way the web UI does.

use coder_sdk::types;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextUsage {
    pub used: i64,
    pub limit: i64,
}

/// Walks messages newest first: a `chat_cleared` result means no usage, a `chat_summarized`
/// result supplies an estimate, otherwise the latest message with `usage` is used.
pub fn context_usage<'a>(messages: impl DoubleEndedIterator<Item = &'a types::CodersdkChatMessage>) -> Option<ContextUsage> {
    for m in messages.rev() {
        for part in m.content.iter().rev() {
            let is_result = part.type_.as_ref().map(|t| t.as_str()) == Some("tool-result");
            match (is_result, part.tool_name.as_deref()) {
                (true, Some("chat_cleared")) => return None,
                (true, Some("chat_summarized")) => {
                    let result = part.result.as_ref()?;
                    return Some(ContextUsage {
                        used: result["estimated_context_tokens"].as_i64()?,
                        limit: result["context_limit_tokens"].as_i64()?,
                    });
                }
                _ => {}
            }
        }
        if let Some(u) = m.usage.as_ref() {
            let used = [u.input_tokens, u.output_tokens, u.cache_read_tokens, u.cache_creation_tokens, u.reasoning_tokens]
                .iter()
                .map(|v| v.unwrap_or(0))
                .sum();
            return Some(ContextUsage { used, limit: u.context_limit? });
        }
    }
    None
}

pub fn format_tokens(n: i64) -> String {
    match n {
        n if n >= 1_000_000 => format!("{:.1}M", n as f64 / 1_000_000.0),
        n if n >= 1_000 => format!("{:.1}k", n as f64 / 1_000.0),
        n => n.to_string(),
    }
}
```

Above the tests in `skew.rs`:

```rust
//! Warns when the server is much newer than the SDK scuttle was built with.

fn major_minor(s: &str) -> Option<(u64, u64)> {
    let rest = s.trim().strip_prefix('v')?;
    let mut parts = rest.split(|c: char| !c.is_ascii_digit());
    Some((parts.next()?.parse().ok()?, parts.next()?.parse().ok()?))
}

/// A warning when the server is more than one minor version ahead of `generated_from`.
pub fn skew_warning(server: &str, generated_from: &str) -> Option<String> {
    let (s_major, s_minor) = major_minor(server)?;
    let (g_major, g_minor) = major_minor(generated_from)?;
    let ahead = s_major > g_major || (s_major == g_major && s_minor > g_minor + 1);
    ahead.then(|| format!("Server {} is newer than scuttle's SDK ({}); some features may not work.", server.trim(), generated_from.trim()))
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p scuttle-core`
Expected: all tests in `density`, `usage`, `skew`, `live`, and `transcript` pass.

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat(scuttle-core): add display prefs, context usage, and version skew

Assisted-by: AI"
```

---

### Task 7: Slash commands and local config

**Files:**
- Create: `crates/scuttle-core/src/commands.rs`, `crates/scuttle-core/src/config.rs`
- Modify: `crates/scuttle-core/src/lib.rs`

**Interfaces:**
- Consumes: `density::Density`.
- Produces:
  - `commands::{Command, CommandInfo, COMMANDS, parse, completions}`. `Command` is `Model(Option<String>) | Workspace(Option<String>) | Compact | Clear | Copy(Option<usize>) | Mouse | Help | Quit`. `CommandInfo { name: &'static str, usage: &'static str, description: &'static str }`. `parse(input: &str) -> Result<Command, String>`. `completions(prefix: &str) -> Vec<&'static CommandInfo>`.
  - `config::{LocalConfig, WelcomeConfig, BusyBehavior, ConfigError, config_path, load, load_from_str, set_mouse}`. `LocalConfig { mouse: bool, busy_behavior: BusyBehavior, composer_max_lines: u16, welcome: WelcomeConfig, density: BTreeMap<String, Density> }` with `Default` (mouse true, queue, 10, show true, no art). `WelcomeConfig { show: bool, art_file: Option<PathBuf> }`. `BusyBehavior` is `Queue | Interrupt` with `as_str()`. `ConfigError` is `Secret(String) | Parse(String) | Io(String)`. `config_path(env: &dyn Fn(&str) -> Option<String>) -> Option<PathBuf>`. `load(path: &Path) -> Result<LocalConfig, ConfigError>`. `load_from_str(text: &str) -> Result<LocalConfig, ConfigError>`. `set_mouse(path: &Path, enabled: bool) -> Result<(), ConfigError>`.

- [ ] **Step 1: Write the failing tests**

`crates/scuttle-core/src/commands.rs`, tests only for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_commands_and_arguments() {
        assert_eq!(parse("/model  Claude Sonnet "), Ok(Command::Model(Some("Claude Sonnet".into()))));
        assert_eq!(parse("/model"), Ok(Command::Model(None)));
        assert_eq!(parse("/workspace none"), Ok(Command::Workspace(Some("none".into()))));
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
    fn completes_by_prefix() {
        let names: Vec<_> = completions("/c").iter().map(|c| c.name).collect();
        assert_eq!(names, vec!["/compact", "/clear", "/copy"]);
        assert!(completions("/zzz").is_empty());
    }
}
```

`crates/scuttle-core/src/config.rs`, tests only for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_apply_to_an_empty_file() {
        assert_eq!(load_from_str("").unwrap(), LocalConfig::default());
        assert!(LocalConfig::default().mouse);
    }

    #[test]
    fn reads_every_supported_key() {
        let cfg = load_from_str(
            "mouse = false\nbusy_behavior = \"interrupt\"\ncomposer_max_lines = 6\n[welcome]\nshow = false\nart_file = \"/tmp/art.txt\"\n[density]\nread_file = \"hidden\"\n",
        )
        .unwrap();
        assert!(!cfg.mouse);
        assert_eq!(cfg.busy_behavior, BusyBehavior::Interrupt);
        assert_eq!(cfg.composer_max_lines, 6);
        assert!(!cfg.welcome.show);
        assert_eq!(cfg.density.get("read_file"), Some(&Density::Hidden));
    }

    #[test]
    fn rejects_secret_looking_keys_anywhere() {
        for text in ["token = \"x\"", "api_key = \"x\"", "[welcome]\nsession_token = \"x\"", "coder_password = \"x\""] {
            match load_from_str(text) {
                Err(ConfigError::Secret(key)) => assert!(!key.is_empty()),
                other => panic!("{text:?} gave {other:?}"),
            }
        }
    }

    #[test]
    fn config_path_prefers_xdg_then_home() {
        let xdg = |k: &str| match k {
            "XDG_CONFIG_HOME" => Some("/x".to_string()),
            "HOME" => Some("/h".to_string()),
            _ => None,
        };
        assert_eq!(config_path(&xdg), Some(PathBuf::from("/x/scuttle/config.toml")));
        let home = |k: &str| (k == "HOME").then(|| "/h".to_string());
        assert_eq!(config_path(&home), Some(PathBuf::from("/h/.config/scuttle/config.toml")));
    }

    #[test]
    fn set_mouse_preserves_comments_and_creates_the_file() {
        let dir = std::env::temp_dir().join(format!("scuttle-cfg-{}", uuid::Uuid::new_v4()));
        let path = dir.join("scuttle/config.toml");
        set_mouse(&path, false).unwrap();
        assert!(!load(&path).unwrap().mouse);
        std::fs::write(&path, "# keep me\nmouse = false\n").unwrap();
        set_mouse(&path, true).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# keep me"));
        assert!(load(&path).unwrap().mouse);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn missing_file_loads_defaults() {
        let path = std::env::temp_dir().join(format!("scuttle-missing-{}.toml", uuid::Uuid::new_v4()));
        assert_eq!(load(&path).unwrap(), LocalConfig::default());
    }
}
```

Add `pub mod commands; pub mod config;` to `lib.rs`.

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p scuttle-core commands config`
Expected: FAIL with unresolved names.

- [ ] **Step 3: Implement**

Above the tests in `commands.rs`:

```rust
//! Slash commands available in M1.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Model(Option<String>),
    Workspace(Option<String>),
    Compact,
    Clear,
    Copy(Option<usize>),
    Mouse,
    Help,
    Quit,
}

#[derive(Debug)]
pub struct CommandInfo {
    pub name: &'static str,
    pub usage: &'static str,
    pub description: &'static str,
}

pub const COMMANDS: &[CommandInfo] = &[
    CommandInfo { name: "/model", usage: "/model [name]", description: "Pick the model for the next message" },
    CommandInfo { name: "/workspace", usage: "/workspace [name|none]", description: "Attach or detach a workspace" },
    CommandInfo { name: "/compact", usage: "/compact", description: "Summarize the conversation to free context" },
    CommandInfo { name: "/clear", usage: "/clear", description: "Reset the model context and keep the transcript" },
    CommandInfo { name: "/copy", usage: "/copy [n]", description: "Copy the last message, or its nth code block" },
    CommandInfo { name: "/mouse", usage: "/mouse", description: "Toggle mouse capture" },
    CommandInfo { name: "/help", usage: "/help", description: "Show commands and keys" },
    CommandInfo { name: "/quit", usage: "/quit", description: "Exit scuttle" },
];

pub fn parse(input: &str) -> Result<Command, String> {
    let input = input.trim();
    let Some(rest) = input.strip_prefix('/') else {
        return Err("not a command".into());
    };
    let (name, arg) = match rest.split_once(char::is_whitespace) {
        Some((name, arg)) => (name, Some(arg.trim()).filter(|a| !a.is_empty())),
        None => (rest, None),
    };
    match name {
        "model" => Ok(Command::Model(arg.map(str::to_owned))),
        "workspace" => Ok(Command::Workspace(arg.map(str::to_owned))),
        "compact" => Ok(Command::Compact),
        "clear" => Ok(Command::Clear),
        "copy" => match arg {
            None => Ok(Command::Copy(None)),
            Some(n) => n.parse().map(|n| Command::Copy(Some(n))).map_err(|_| format!("/copy takes a code block number, got {n:?}")),
        },
        "mouse" => Ok(Command::Mouse),
        "help" => Ok(Command::Help),
        "quit" => Ok(Command::Quit),
        other => Err(format!("unknown command /{other}; type /help")),
    }
}

pub fn completions(prefix: &str) -> Vec<&'static CommandInfo> {
    COMMANDS.iter().filter(|c| c.name.starts_with(prefix)).collect()
}
```

Above the tests in `config.rs`:

```rust
//! The local, TUI-only config file. It never holds secrets.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::density::Density;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum BusyBehavior {
    #[default]
    Queue,
    Interrupt,
}

impl BusyBehavior {
    pub fn as_str(self) -> &'static str {
        match self {
            BusyBehavior::Queue => "queue",
            BusyBehavior::Interrupt => "interrupt",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct WelcomeConfig {
    pub show: bool,
    pub art_file: Option<PathBuf>,
}

impl Default for WelcomeConfig {
    fn default() -> Self {
        WelcomeConfig { show: true, art_file: None }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct LocalConfig {
    pub mouse: bool,
    pub busy_behavior: BusyBehavior,
    pub composer_max_lines: u16,
    pub welcome: WelcomeConfig,
    pub density: BTreeMap<String, Density>,
}

impl Default for LocalConfig {
    fn default() -> Self {
        LocalConfig {
            mouse: true,
            busy_behavior: BusyBehavior::Queue,
            composer_max_lines: 10,
            welcome: WelcomeConfig::default(),
            density: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    #[error("config key {0:?} looks like a secret; scuttle's config never holds secrets")]
    Secret(String),
    #[error("could not parse the config file: {0}")]
    Parse(String),
    #[error("could not read or write the config file: {0}")]
    Io(String),
}

fn looks_secret(key: &str) -> bool {
    let k = key.to_ascii_lowercase();
    ["token", "secret", "password", "api_key", "apikey"].contains(&k.as_str())
        || ["_token", "_secret", "_password", "_key"].iter().any(|s| k.ends_with(s))
}

fn check_secrets(table: &toml::Table, path: &str) -> Result<(), ConfigError> {
    for (key, value) in table {
        let full = if path.is_empty() { key.clone() } else { format!("{path}.{key}") };
        if looks_secret(key) {
            return Err(ConfigError::Secret(full));
        }
        if let toml::Value::Table(inner) = value {
            check_secrets(inner, &full)?;
        }
    }
    Ok(())
}

/// `$XDG_CONFIG_HOME/scuttle/config.toml`, else `$HOME/.config/scuttle/config.toml`.
pub fn config_path(env: &dyn Fn(&str) -> Option<String>) -> Option<PathBuf> {
    let base = env("XDG_CONFIG_HOME").map(PathBuf::from).or_else(|| env("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("scuttle").join("config.toml"))
}

pub fn load_from_str(text: &str) -> Result<LocalConfig, ConfigError> {
    let table: toml::Table = text.parse().map_err(|e: toml::de::Error| ConfigError::Parse(e.to_string()))?;
    check_secrets(&table, "")?;
    toml::from_str(text).map_err(|e| ConfigError::Parse(e.to_string()))
}

pub fn load(path: &Path) -> Result<LocalConfig, ConfigError> {
    match std::fs::read_to_string(path) {
        Ok(text) => load_from_str(&text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(LocalConfig::default()),
        Err(e) => Err(ConfigError::Io(e.to_string())),
    }
}

/// Sets `mouse` in the file, creating it if needed and preserving comments and formatting.
pub fn set_mouse(path: &Path, enabled: bool) -> Result<(), ConfigError> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(ConfigError::Io(e.to_string())),
    };
    let mut doc: toml_edit::DocumentMut = text.parse().map_err(|e: toml_edit::TomlError| ConfigError::Parse(e.to_string()))?;
    doc["mouse"] = toml_edit::value(enabled);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| ConfigError::Io(e.to_string()))?;
    }
    std::fs::write(path, doc.to_string()).map_err(|e| ConfigError::Io(e.to_string()))
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p scuttle-core`
Expected: all tests pass.

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat(scuttle-core): add slash commands and the local config file

Assisted-by: AI"
```

---

### Task 8: The App state machine

**Files:**
- Create: `crates/scuttle-core/src/app.rs`
- Modify: `crates/scuttle-core/src/lib.rs`

**Interfaces:**
- Consumes: `transcript::Transcript`, `live::Applied`, `commands::Command`, `config::BusyBehavior`, `density::DisplayPrefs`; `coder_sdk::{types, ChatStatus, StreamEvent}`.
- Produces: `scuttle_core::app::{App, Msg, Effect, Notice, Connection, Picker, CopyTarget, WorkspaceRef, backoff}` exactly as defined in Step 3, including `Msg::Refresh` for background actions whose result arrives on the stream. `App::new(busy: BusyBehavior, mouse: bool) -> App`; `App::update(&mut self, msg: Msg) -> Vec<Effect>`; `App::is_running(&self) -> bool`; `App::model_name(&self) -> Option<String>`; `backoff(attempt: u32) -> Duration`.

- [ ] **Step 1: Write the failing tests**

Create `crates/scuttle-core/src/app.rs` with this test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::time::Duration;

    fn chat(id: Uuid) -> Box<types::CodersdkChat> {
        Box::new(serde_json::from_value(json!({"id": id, "title": "t", "children": [], "files": [], "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}})).unwrap())
    }

    fn message(id: i64) -> types::CodersdkChatMessage {
        serde_json::from_value(json!({"id": id, "role": "user", "content": []})).unwrap()
    }

    fn started(app: &mut App) -> Uuid {
        let org = Uuid::new_v4();
        app.update(Msg::Started { org_id: org, open_chat: None });
        org
    }

    fn ev(v: serde_json::Value) -> Msg {
        Msg::Stream(StreamEvent {
            kind: coder_sdk::StreamEventType::parse(v["type"].as_str().unwrap_or_default()),
            event: serde_json::from_value(v.clone()).ok(),
            raw: v,
        })
    }

    #[test]
    fn start_fetches_settings_and_opens_a_requested_chat() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let org = Uuid::new_v4();
        let id = Uuid::new_v4();
        let effects = app.update(Msg::Started { org_id: org, open_chat: Some(id) });
        assert!(effects.contains(&Effect::FetchPrefs));
        assert!(effects.contains(&Effect::FetchModels(org)));
        assert!(effects.contains(&Effect::FetchWorkspaces));
        assert!(effects.contains(&Effect::LoadChat(id)));
        assert_eq!(app.connection, Connection::Connecting);
    }

    #[test]
    fn loaded_chat_opens_the_stream_after_the_last_message() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        let effects = app.update(Msg::ChatLoaded { chat: chat(id), messages: vec![message(3), message(9)] });
        assert_eq!(effects, vec![Effect::OpenStream { chat: id, after_id: Some(9) }]);
    }

    #[test]
    fn submit_on_blank_chat_creates_one_chat() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let org = started(&mut app);
        let effects = app.update(Msg::Submit("hello".into()));
        assert_eq!(effects, vec![Effect::CreateChat { org, text: "hello".into(), model: None, workspace: None }]);
    }

    #[test]
    fn second_submit_while_creating_does_not_create_twice() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        app.update(Msg::Submit("one".into()));
        let effects = app.update(Msg::Submit("two".into()));
        assert!(effects.is_empty());
        assert!(matches!(app.notices.last(), Some(Notice::Info(_))));
        let id = Uuid::new_v4();
        let effects = app.update(Msg::ChatCreated(chat(id)));
        assert!(effects.contains(&Effect::OpenStream { chat: id, after_id: None }));
        assert!(effects.contains(&Effect::SendMessage { chat: id, text: "two".into(), model: None, busy: BusyBehavior::Queue }));
    }

    #[test]
    fn submit_with_a_chat_sends_with_the_configured_busy_behavior() {
        let mut app = App::new(BusyBehavior::Interrupt, true);
        started(&mut app);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded { chat: chat(id), messages: vec![] });
        let effects = app.update(Msg::Submit("  hi  ".into()));
        assert_eq!(effects, vec![Effect::SendMessage { chat: id, text: "hi".into(), model: None, busy: BusyBehavior::Interrupt }]);
        assert!(app.update(Msg::Submit("   ".into())).is_empty());
    }

    #[test]
    fn stream_end_schedules_backoff_reconnect_with_after_id() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded { chat: chat(id), messages: vec![message(5)] });
        let first = app.update(Msg::StreamEnded { error: None });
        assert_eq!(first, vec![Effect::ReconnectAfter { chat: id, after_id: Some(5), delay: Duration::from_millis(500) }]);
        let second = app.update(Msg::StreamEnded { error: Some("reset".into()) });
        assert_eq!(second, vec![Effect::ReconnectAfter { chat: id, after_id: Some(5), delay: Duration::from_millis(1000) }]);
        assert_eq!(app.connection, Connection::Reconnecting { attempt: 2 });
    }

    #[test]
    fn stream_event_after_reconnect_resets_backoff() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded { chat: chat(id), messages: vec![] });
        app.update(Msg::StreamEnded { error: None });
        app.update(ev(json!({"type": "status", "status": {"status": "waiting"}})));
        assert_eq!(app.connection, Connection::Live);
        let effects = app.update(Msg::StreamEnded { error: None });
        assert_eq!(effects, vec![Effect::ReconnectAfter { chat: id, after_id: None, delay: Duration::from_millis(500) }]);
    }

    #[test]
    fn stream_gap_reconnects_immediately() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded { chat: chat(id), messages: vec![] });
        let part = |seq| json!({"type": "message_part", "message_part": {"history_version": 1, "generation_attempt": 1, "seq": seq, "part": {"type": "text", "text": "x"}}});
        app.update(ev(part(1)));
        let effects = app.update(ev(part(4)));
        assert_eq!(effects, vec![Effect::ReconnectAfter { chat: id, after_id: None, delay: Duration::ZERO }]);
    }

    #[test]
    fn commands_need_a_chat_and_resolve_names() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        assert!(app.update(Msg::Command(Command::Compact)).is_empty());
        assert!(matches!(app.notices.last(), Some(Notice::Error(_))));
        let model_id = Uuid::new_v4();
        app.update(Msg::ModelsLoaded(vec![serde_json::from_value(json!({"id": model_id, "display_name": "Big Model", "model": "big", "enabled": true, "reasoning_efforts": []})).unwrap()]));
        app.update(Msg::Command(Command::Model(Some("big model".into()))));
        assert_eq!(app.selected_model, Some(model_id));
        assert_eq!(app.update(Msg::Command(Command::Model(None))), vec![Effect::ShowPicker(Picker::Model)]);
        let ws = Uuid::new_v4();
        app.update(Msg::WorkspacesLoaded(vec![WorkspaceRef { id: ws, name: "dev".into() }]));
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded { chat: chat(id), messages: vec![] });
        assert_eq!(app.update(Msg::Command(Command::Workspace(Some("dev".into())))), vec![Effect::SetWorkspace { chat: id, workspace: Some(ws) }]);
        assert_eq!(app.update(Msg::Command(Command::Workspace(Some("none".into())))), vec![Effect::SetWorkspace { chat: id, workspace: None }]);
        assert_eq!(app.update(Msg::Command(Command::Compact)), vec![Effect::Compact(id)]);
        assert_eq!(app.update(Msg::Command(Command::Clear)), vec![Effect::Clear(id)]);
        assert_eq!(app.update(Msg::Command(Command::Copy(Some(2)))), vec![Effect::Copy(CopyTarget::CodeBlock(2))]);
        assert_eq!(app.update(Msg::Command(Command::Mouse)), vec![Effect::SetMouse(false)]);
        assert!(!app.mouse);
        assert_eq!(app.update(Msg::Command(Command::Quit)), vec![Effect::Quit]);
    }

    #[test]
    fn submit_parses_slash_commands() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        assert_eq!(app.update(Msg::Submit("/help".into())), vec![Effect::ShowHelp]);
        assert!(app.update(Msg::Submit("/bogus".into())).is_empty());
        assert!(matches!(app.notices.last(), Some(Notice::Error(m)) if m.contains("/help")));
    }

    #[test]
    fn interrupt_only_while_running() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded { chat: chat(id), messages: vec![] });
        assert!(app.update(Msg::Interrupt).is_empty());
        app.update(ev(json!({"type": "status", "status": {"status": "running"}})));
        assert_eq!(app.update(Msg::Interrupt), vec![Effect::Interrupt(id)]);
    }

    #[test]
    fn api_failure_becomes_an_error_notice() {
        let mut app = App::new(BusyBehavior::Queue, true);
        app.update(Msg::ApiFailed { action: "send message", message: "HTTP 409".into() });
        assert!(matches!(app.notices.last(), Some(Notice::Error(m)) if m.contains("send message") && m.contains("409")));
    }

    #[test]
    fn refresh_changes_nothing() {
        let mut app = App::new(BusyBehavior::Queue, true);
        assert!(app.update(Msg::Refresh).is_empty());
    }

    #[test]
    fn backoff_doubles_and_caps() {
        assert_eq!(backoff(1), Duration::from_millis(500));
        assert_eq!(backoff(3), Duration::from_millis(2000));
        assert_eq!(backoff(20), Duration::from_secs(10));
    }
}
```

Add `pub mod app;` to `lib.rs`.

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p scuttle-core app`
Expected: FAIL with unresolved `App`, `Msg`, `Effect`.

- [ ] **Step 3: Implement**

Insert above the tests in `crates/scuttle-core/src/app.rs`:

```rust
//! The headless application: every input is a `Msg`, every side effect is an `Effect`.

use std::time::Duration;

use coder_sdk::{ChatStatus, StreamEvent, types};
use uuid::Uuid;

use crate::commands::{self, Command};
use crate::config::BusyBehavior;
use crate::density::DisplayPrefs;
use crate::live::Applied;
use crate::transcript::Transcript;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceRef {
    pub id: Uuid,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Notice {
    Info(String),
    Error(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Connection {
    #[default]
    Idle,
    Connecting,
    Live,
    Reconnecting { attempt: u32 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Picker {
    Model,
    Workspace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyTarget {
    LastMessage,
    CodeBlock(usize),
}

#[derive(Debug)]
pub enum Msg {
    Started { org_id: Uuid, open_chat: Option<Uuid> },
    ChatLoaded { chat: Box<types::CodersdkChat>, messages: Vec<types::CodersdkChatMessage> },
    ChatCreated(Box<types::CodersdkChat>),
    Stream(StreamEvent),
    StreamEnded { error: Option<String> },
    PrefsLoaded(DisplayPrefs),
    ModelsLoaded(Vec<types::CodersdkChatModel>),
    WorkspacesLoaded(Vec<WorkspaceRef>),
    ModelChosen(Uuid),
    WorkspaceChosen(Option<Uuid>),
    ApiFailed { action: &'static str, message: String },
    Submit(String),
    Command(Command),
    Interrupt,
    /// A background action finished with nothing to update; the next stream event carries the result.
    Refresh,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    LoadChat(Uuid),
    OpenStream { chat: Uuid, after_id: Option<i64> },
    ReconnectAfter { chat: Uuid, after_id: Option<i64>, delay: Duration },
    CreateChat { org: Uuid, text: String, model: Option<Uuid>, workspace: Option<Uuid> },
    SendMessage { chat: Uuid, text: String, model: Option<Uuid>, busy: BusyBehavior },
    Interrupt(Uuid),
    Compact(Uuid),
    Clear(Uuid),
    SetWorkspace { chat: Uuid, workspace: Option<Uuid> },
    FetchPrefs,
    FetchModels(Uuid),
    FetchWorkspaces,
    ShowPicker(Picker),
    ShowHelp,
    Copy(CopyTarget),
    SetMouse(bool),
    Quit,
}

/// Reconnect delay: 500 ms doubling per attempt, capped at 10 s. The runtime adds jitter.
pub fn backoff(attempt: u32) -> Duration {
    let exp = attempt.saturating_sub(1).min(10);
    Duration::from_millis((500u64 << exp).min(10_000))
}

#[derive(Debug, Default)]
pub struct App {
    pub org_id: Option<Uuid>,
    pub chat_id: Option<Uuid>,
    pub chat: Option<Box<types::CodersdkChat>>,
    pub transcript: Transcript,
    pub prefs: DisplayPrefs,
    pub models: Vec<types::CodersdkChatModel>,
    pub workspaces: Vec<WorkspaceRef>,
    pub selected_model: Option<Uuid>,
    pub selected_workspace: Option<Uuid>,
    pub notices: Vec<Notice>,
    pub connection: Connection,
    pub busy: BusyBehavior,
    pub mouse: bool,
    creating: bool,
    pending_text: Option<String>,
    reconnect_attempt: u32,
}

impl App {
    pub fn new(busy: BusyBehavior, mouse: bool) -> App {
        App { busy, mouse, ..App::default() }
    }

    pub fn is_running(&self) -> bool {
        matches!(self.transcript.status, Some(ChatStatus::Running | ChatStatus::Interrupting | ChatStatus::RequiresAction))
    }

    /// The display name of the model the next message will use.
    pub fn model_name(&self) -> Option<String> {
        let id = self.selected_model.or_else(|| self.models.iter().find(|m| m.is_default == Some(true)).and_then(|m| m.id))?;
        let model = self.models.iter().find(|m| m.id == Some(id))?;
        model.display_name.clone().or_else(|| model.model.clone())
    }

    fn info(&mut self, text: impl Into<String>) {
        self.notices.push(Notice::Info(text.into()));
    }

    fn error(&mut self, text: impl Into<String>) {
        self.notices.push(Notice::Error(text.into()));
    }

    pub fn update(&mut self, msg: Msg) -> Vec<Effect> {
        match msg {
            Msg::Started { org_id, open_chat } => {
                self.org_id = Some(org_id);
                let mut effects = vec![Effect::FetchPrefs, Effect::FetchModels(org_id), Effect::FetchWorkspaces];
                if let Some(id) = open_chat {
                    self.connection = Connection::Connecting;
                    effects.push(Effect::LoadChat(id));
                }
                effects
            }
            Msg::ChatLoaded { chat, messages } => {
                let Some(id) = chat.id else { return vec![] };
                self.selected_model = self.selected_model.or(chat.last_model_config_id);
                self.selected_workspace = chat.workspace_id;
                self.chat_id = Some(id);
                self.chat = Some(chat);
                self.transcript.load(messages);
                self.connection = Connection::Connecting;
                vec![Effect::OpenStream { chat: id, after_id: self.transcript.last_message_id() }]
            }
            Msg::ChatCreated(chat) => {
                let Some(id) = chat.id else { return vec![] };
                self.creating = false;
                self.chat_id = Some(id);
                self.chat = Some(chat);
                self.connection = Connection::Connecting;
                let mut effects = vec![Effect::OpenStream { chat: id, after_id: None }];
                if let Some(text) = self.pending_text.take() {
                    effects.push(Effect::SendMessage { chat: id, text, model: self.selected_model, busy: self.busy });
                }
                effects
            }
            Msg::Stream(ev) => {
                self.connection = Connection::Live;
                self.reconnect_attempt = 0;
                match self.transcript.apply(&ev) {
                    Applied::Reconnect(_) => match self.chat_id {
                        Some(chat) => vec![Effect::ReconnectAfter { chat, after_id: self.transcript.last_message_id(), delay: Duration::ZERO }],
                        None => vec![],
                    },
                    _ => vec![],
                }
            }
            Msg::StreamEnded { .. } => {
                let Some(chat) = self.chat_id else { return vec![] };
                self.reconnect_attempt += 1;
                self.transcript.live.clear();
                self.connection = Connection::Reconnecting { attempt: self.reconnect_attempt };
                vec![Effect::ReconnectAfter { chat, after_id: self.transcript.last_message_id(), delay: backoff(self.reconnect_attempt) }]
            }
            Msg::PrefsLoaded(prefs) => {
                self.prefs = prefs;
                vec![]
            }
            Msg::ModelsLoaded(models) => {
                self.models = models.into_iter().filter(|m| m.enabled != Some(false)).collect();
                vec![]
            }
            Msg::WorkspacesLoaded(workspaces) => {
                self.workspaces = workspaces;
                vec![]
            }
            Msg::ModelChosen(id) => {
                self.selected_model = Some(id);
                if let Some(name) = self.model_name() {
                    self.info(format!("Model set to {name}"));
                }
                vec![]
            }
            Msg::WorkspaceChosen(ws) => self.set_workspace(ws),
            Msg::ApiFailed { action, message } => {
                self.creating = false;
                self.error(format!("Could not {action}: {message}"));
                vec![]
            }
            Msg::Submit(text) => self.submit(text),
            Msg::Command(cmd) => self.command(cmd),
            Msg::Interrupt => match self.chat_id {
                Some(chat) if self.is_running() => vec![Effect::Interrupt(chat)],
                _ => vec![],
            },
            Msg::Refresh => vec![],
        }
    }

    fn submit(&mut self, text: String) -> Vec<Effect> {
        let text = text.trim().to_owned();
        if text.is_empty() {
            return vec![];
        }
        if text.starts_with('/') {
            return match commands::parse(&text) {
                Ok(cmd) => self.command(cmd),
                Err(e) => {
                    self.error(e);
                    vec![]
                }
            };
        }
        if let Some(chat) = self.chat_id {
            return vec![Effect::SendMessage { chat, text, model: self.selected_model, busy: self.busy }];
        }
        if self.creating {
            self.pending_text = Some(match self.pending_text.take() {
                Some(prev) => format!("{prev}\n\n{text}"),
                None => text,
            });
            self.info("Waiting for the chat to be created; your message will follow.");
            return vec![];
        }
        let Some(org) = self.org_id else {
            self.error("Not connected to Coder yet.");
            return vec![];
        };
        self.creating = true;
        vec![Effect::CreateChat { org, text, model: self.selected_model, workspace: self.selected_workspace }]
    }

    fn set_workspace(&mut self, ws: Option<Uuid>) -> Vec<Effect> {
        self.selected_workspace = ws;
        match self.chat_id {
            Some(chat) => vec![Effect::SetWorkspace { chat, workspace: ws }],
            None => vec![],
        }
    }

    fn command(&mut self, cmd: Command) -> Vec<Effect> {
        match cmd {
            Command::Model(None) => vec![Effect::ShowPicker(Picker::Model)],
            Command::Model(Some(name)) => {
                let wanted = name.to_lowercase();
                let found = self.models.iter().find(|m| {
                    m.display_name.as_deref().map(str::to_lowercase) == Some(wanted.clone()) || m.model.as_deref().map(str::to_lowercase) == Some(wanted.clone())
                });
                match found.and_then(|m| m.id) {
                    Some(id) => self.update(Msg::ModelChosen(id)),
                    None => {
                        self.error(format!("No enabled model named {name:?}"));
                        vec![]
                    }
                }
            }
            Command::Workspace(None) => vec![Effect::ShowPicker(Picker::Workspace)],
            Command::Workspace(Some(name)) if name == "none" => self.set_workspace(None),
            Command::Workspace(Some(name)) => match self.workspaces.iter().find(|w| w.name == name).map(|w| w.id) {
                Some(id) => self.set_workspace(Some(id)),
                None => {
                    self.error(format!("No workspace named {name:?}"));
                    vec![]
                }
            },
            Command::Compact | Command::Clear => match self.chat_id {
                Some(chat) => vec![if cmd == Command::Compact { Effect::Compact(chat) } else { Effect::Clear(chat) }],
                None => {
                    self.error("Start a chat first.");
                    vec![]
                }
            },
            Command::Copy(None) => vec![Effect::Copy(CopyTarget::LastMessage)],
            Command::Copy(Some(n)) => vec![Effect::Copy(CopyTarget::CodeBlock(n))],
            Command::Mouse => {
                self.mouse = !self.mouse;
                vec![Effect::SetMouse(self.mouse)]
            }
            Command::Help => vec![Effect::ShowHelp],
            Command::Quit => vec![Effect::Quit],
        }
    }
}
```

The test helper `chat()` builds a `CodersdkChat` from JSON; if deserialization fails because the generated struct marks other fields as required, add those fields to the helper's JSON with neutral values and report which ones.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p scuttle-core && cargo clippy -p scuttle-core --all-targets -- -D warnings`
Expected: every test passes and clippy is clean.

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat(scuttle-core): add the app state machine

Assisted-by: AI"
```

---

### Task 9: Wrapping, highlighting, and markdown

**Files:**
- Create: `crates/scuttle-tui/src/wrap.rs`, `crates/scuttle-tui/src/highlight.rs`, `crates/scuttle-tui/src/markdown.rs`
- Modify: `crates/scuttle-tui/src/main.rs` (module declarations only)

**Interfaces:**
- Produces:
  - `wrap::wrap_line(line: &Line<'static>, width: u16) -> Vec<Line<'static>>` and `wrap::wrap_lines(lines: &[Line<'static>], width: u16) -> Vec<Line<'static>>`; every output line's display width is at most `width` (for `width >= 2`).
  - `highlight::{warm, highlight}`. `warm()` starts loading syntaxes on a background thread. `highlight(code: &str, lang: &str) -> Option<Vec<Line<'static>>>` returns `None` until loading has finished or when the language is unknown.
  - `markdown::{Rendered, CodeBlock, render}`. `render(text: &str) -> Rendered`; `Rendered { lines: Vec<Line<'static>>, code_blocks: Vec<CodeBlock> }`; `CodeBlock { start: usize, end: usize, code: String }`, where `start..end` are indices into `lines`.

- [ ] **Step 1: Declare the modules**

Replace `crates/scuttle-tui/src/main.rs` with:

```rust
mod highlight;
mod markdown;
mod wrap;

fn main() {}
```

- [ ] **Step 2: Write the failing tests**

`crates/scuttle-tui/src/wrap.rs`, tests only for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::{Style, Stylize};
    use ratatui::text::{Line, Span};
    use unicode_width::UnicodeWidthStr;

    fn width(line: &Line) -> usize {
        line.spans.iter().map(|s| s.content.width()).sum()
    }

    #[test]
    fn wraps_at_word_boundaries_and_keeps_styles() {
        let line = Line::from(vec![Span::raw("hello "), Span::styled("bold world", Style::new().bold())]);
        let out = wrap_line(&line, 8);
        assert!(out.iter().all(|l| width(l) <= 8));
        let text: String = out.iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>()).collect::<Vec<_>>().join("|");
        assert_eq!(text, "hello |bold |world");
        assert!(out[1].spans.iter().any(|s| s.style == Style::new().bold()));
    }

    #[test]
    fn wraps_wide_characters_within_width() {
        let line = Line::from("日本語のテキストと絵文字🦀🦀🦀が混ざっています");
        for w in [2u16, 3, 5, 10] {
            for l in wrap_line(&line, w) {
                assert!(width(&l) <= w as usize, "width {w}: {:?}", l);
            }
        }
    }

    #[test]
    fn long_words_are_split_and_empty_lines_survive() {
        let out = wrap_line(&Line::from("abcdefghij"), 4);
        assert_eq!(out.len(), 3);
        assert_eq!(wrap_line(&Line::from(""), 10).len(), 1);
    }
}
```

`crates/scuttle-tui/src/markdown.rs`, tests only for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Modifier;

    fn plain(r: &Rendered) -> Vec<String> {
        r.lines.iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect()).collect()
    }

    #[test]
    fn renders_headings_emphasis_and_lists() {
        let r = render("# Title\n\nSome **bold** and `code`.\n\n- one\n- two\n");
        let text = plain(&r);
        assert_eq!(text[0], "Title");
        assert!(r.lines[0].spans[0].style.add_modifier.contains(Modifier::BOLD));
        assert!(text.iter().any(|l| l == "Some bold and code."));
        assert!(text.iter().any(|l| l == "• one"));
        assert!(text.iter().any(|l| l == "• two"));
    }

    #[test]
    fn records_code_block_ranges_and_source() {
        let r = render("before\n\n```rust\nfn main() {}\nlet x = 1;\n```\n\nafter\n");
        assert_eq!(r.code_blocks.len(), 1);
        let block = &r.code_blocks[0];
        assert_eq!(block.code, "fn main() {}\nlet x = 1;\n");
        assert_eq!(block.end - block.start, 2);
        assert_eq!(plain(&r)[block.start], "fn main() {}");
    }

    #[test]
    fn unterminated_code_fence_while_streaming_is_still_a_block() {
        let r = render("```\npartial");
        assert_eq!(r.code_blocks.len(), 1);
        assert_eq!(r.code_blocks[0].code.trim_end(), "partial");
    }
}
```

`crates/scuttle-tui/src/highlight.rs`, tests only for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn highlights_known_languages_once_warm() {
        warm();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let lines = loop {
            if let Some(lines) = highlight("fn main() {}\n", "rust") {
                break lines;
            }
            assert!(std::time::Instant::now() < deadline, "syntaxes never finished loading");
            std::thread::sleep(std::time::Duration::from_millis(20));
        };
        assert_eq!(lines.len(), 1);
        assert!(highlight("x", "no-such-language-xyz").is_none());
    }
}
```

- [ ] **Step 3: Run them to verify they fail**

Run: `cargo test -p scuttle-tui`
Expected: FAIL with unresolved `wrap_line`, `render`, `warm`, `highlight`.

- [ ] **Step 4: Implement**

Above the tests in `wrap.rs`:

```rust
//! Wraps styled lines to a width, counting display columns so wide characters never overflow.

use ratatui::style::Style;
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthChar;

/// Wraps at spaces where possible, and splits words longer than the width.
pub fn wrap_line(line: &Line<'static>, width: u16) -> Vec<Line<'static>> {
    let width = width.max(2) as usize;
    let cells: Vec<(char, Style)> = line.spans.iter().flat_map(|s| s.content.chars().map(move |c| (c, s.style))).collect();
    if cells.is_empty() {
        return vec![Line::default().style(line.style)];
    }
    let mut out = Vec::new();
    let mut start = 0;
    while start < cells.len() {
        let mut used = 0;
        let mut end = start;
        let mut last_space = None;
        while end < cells.len() {
            let w = cells[end].0.width().unwrap_or(0);
            if used + w > width {
                break;
            }
            if cells[end].0 == ' ' {
                last_space = Some(end);
            }
            used += w;
            end += 1;
        }
        if end < cells.len() {
            if let Some(space) = last_space.filter(|s| *s > start) {
                end = space + 1;
            }
        }
        if end == start {
            end = start + 1;
        }
        out.push(to_line(&cells[start..end], line.style));
        start = end;
    }
    out
}

pub fn wrap_lines(lines: &[Line<'static>], width: u16) -> Vec<Line<'static>> {
    lines.iter().flat_map(|l| wrap_line(l, width)).collect()
}

fn to_line(cells: &[(char, Style)], line_style: Style) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    for (c, style) in cells {
        match spans.last_mut() {
            Some(last) if last.style == *style => last.content.to_mut().push(*c),
            _ => spans.push(Span::styled(c.to_string(), *style)),
        }
    }
    Line::from(spans).style(line_style)
}
```

The wide-character test uses widths as small as 2, so a single double-width character always fits. The `end == start` fallback only triggers for a zero-width budget, which `max(2)` rules out.

Above the tests in `highlight.rs`:

```rust
//! Syntax highlighting, loaded lazily on a background thread so startup is not delayed.

use std::sync::OnceLock;

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use syntect::easy::HighlightLines;
use syntect::highlighting::Theme;
use syntect::parsing::SyntaxSet;

struct Assets {
    syntaxes: SyntaxSet,
    theme: Theme,
}

static ASSETS: OnceLock<Assets> = OnceLock::new();

/// Starts loading syntaxes and the theme; safe to call more than once.
pub fn warm() {
    if ASSETS.get().is_some() {
        return;
    }
    std::thread::spawn(|| {
        let syntaxes = two_face::syntax::extra_newlines();
        let themes = two_face::theme::extra();
        let theme = themes.get(two_face::theme::EmbeddedThemeName::Base16OceanDark).clone();
        let _ = ASSETS.set(Assets { syntaxes, theme });
    });
}

/// Highlighted lines, or `None` if assets are still loading or the language is unknown.
pub fn highlight(code: &str, lang: &str) -> Option<Vec<Line<'static>>> {
    let assets = ASSETS.get()?;
    let syntax = assets.syntaxes.find_syntax_by_token(lang)?;
    let mut h = HighlightLines::new(syntax, &assets.theme);
    let mut out = Vec::new();
    for line in syntect::util::LinesWithEndings::from(code) {
        let ranges = h.highlight_line(line, &assets.syntaxes).ok()?;
        let spans = ranges
            .into_iter()
            .map(|(style, text)| {
                let fg = style.foreground;
                Span::styled(text.trim_end_matches('\n').to_owned(), Style::new().fg(Color::Rgb(fg.r, fg.g, fg.b)))
            })
            .collect::<Vec<_>>();
        out.push(Line::from(spans));
    }
    Some(out)
}
```

Above the tests in `markdown.rs`:

```rust
//! Markdown to styled terminal lines, recording where code blocks are for copying.

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::highlight;

#[derive(Debug, Clone, PartialEq)]
pub struct CodeBlock {
    pub start: usize,
    pub end: usize,
    pub code: String,
}

#[derive(Debug, Clone, Default)]
pub struct Rendered {
    pub lines: Vec<Line<'static>>,
    pub code_blocks: Vec<CodeBlock>,
}

struct Builder {
    out: Rendered,
    current: Vec<Span<'static>>,
    styles: Vec<Style>,
    list_depth: usize,
    quote_depth: usize,
    code: Option<(String, String)>,
}

impl Builder {
    fn style(&self) -> Style {
        self.styles.iter().fold(Style::new(), |acc, s| acc.patch(*s))
    }

    fn flush(&mut self) {
        if self.current.is_empty() {
            return;
        }
        let mut spans = Vec::new();
        if self.quote_depth > 0 {
            spans.push(Span::styled("│ ".repeat(self.quote_depth), Style::new().fg(Color::DarkGray)));
        }
        spans.append(&mut self.current);
        self.out.lines.push(Line::from(spans));
    }

    fn blank(&mut self) {
        self.flush();
        if self.out.lines.last().is_some_and(|l| !l.spans.is_empty()) {
            self.out.lines.push(Line::default());
        }
    }

    fn finish_code(&mut self) {
        let Some((lang, code)) = self.code.take() else { return };
        let start = self.out.lines.len();
        let lines = highlight::highlight(&code, &lang).unwrap_or_else(|| {
            code.lines().map(|l| Line::from(Span::styled(l.to_owned(), Style::new().fg(Color::Gray)))).collect()
        });
        self.out.lines.extend(lines);
        let end = self.out.lines.len();
        self.out.code_blocks.push(CodeBlock { start, end, code });
    }
}

pub fn render(text: &str) -> Rendered {
    let mut b = Builder { out: Rendered::default(), current: Vec::new(), styles: Vec::new(), list_depth: 0, quote_depth: 0, code: None };
    for event in Parser::new_ext(text, Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TABLES) {
        if let Some((_, code)) = b.code.as_mut() {
            match event {
                Event::Text(t) => {
                    code.push_str(&t);
                    continue;
                }
                Event::End(TagEnd::CodeBlock) => {
                    b.finish_code();
                    b.blank();
                    continue;
                }
                _ => continue,
            }
        }
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                b.blank();
                let color = if level == HeadingLevel::H1 { Color::Cyan } else { Color::Blue };
                b.styles.push(Style::new().fg(color).add_modifier(Modifier::BOLD));
            }
            Event::End(TagEnd::Heading(_)) => {
                b.styles.pop();
                b.blank();
            }
            Event::Start(Tag::Paragraph) => {}
            Event::End(TagEnd::Paragraph) => b.blank(),
            Event::Start(Tag::Strong) => b.styles.push(Style::new().add_modifier(Modifier::BOLD)),
            Event::Start(Tag::Emphasis) => b.styles.push(Style::new().add_modifier(Modifier::ITALIC)),
            Event::Start(Tag::Strikethrough) => b.styles.push(Style::new().add_modifier(Modifier::CROSSED_OUT)),
            Event::End(TagEnd::Strong | TagEnd::Emphasis | TagEnd::Strikethrough) => {
                b.styles.pop();
            }
            Event::Start(Tag::BlockQuote(_)) => {
                b.flush();
                b.quote_depth += 1;
            }
            Event::End(TagEnd::BlockQuote(_)) => {
                b.flush();
                b.quote_depth -= 1;
            }
            Event::Start(Tag::List(_)) => {
                b.flush();
                b.list_depth += 1;
            }
            Event::End(TagEnd::List(_)) => {
                b.flush();
                b.list_depth -= 1;
                if b.list_depth == 0 {
                    b.blank();
                }
            }
            Event::Start(Tag::Item) => {
                b.flush();
                b.current.push(Span::raw(format!("{}• ", "  ".repeat(b.list_depth.saturating_sub(1)))));
            }
            Event::End(TagEnd::Item) => b.flush(),
            Event::Start(Tag::CodeBlock(kind)) => {
                b.flush();
                let lang = match kind {
                    CodeBlockKind::Fenced(l) => l.split_whitespace().next().unwrap_or_default().to_owned(),
                    CodeBlockKind::Indented => String::new(),
                };
                b.code = Some((lang, String::new()));
            }
            Event::Code(c) => b.current.push(Span::styled(c.to_string(), Style::new().fg(Color::Yellow))),
            Event::Text(t) => {
                let style = b.style();
                b.current.push(Span::styled(t.to_string(), style));
            }
            Event::SoftBreak => b.current.push(Span::raw(" ")),
            Event::HardBreak => b.flush(),
            Event::Rule => {
                b.flush();
                b.out.lines.push(Line::from(Span::styled("─".repeat(20), Style::new().fg(Color::DarkGray))));
            }
            _ => {}
        }
    }
    b.finish_code();
    b.flush();
    while b.out.lines.last().is_some_and(|l| l.spans.is_empty()) {
        b.out.lines.pop();
    }
    b.out
}
```

Tests merge spans into plain strings, so adjacent spans with different styles are fine.

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p scuttle-tui`
Expected: all wrap, markdown, and highlight tests pass. If `two_face` names differ in 0.5 (`extra_newlines`, `theme::extra`, `EmbeddedThemeName::Base16OceanDark`), adapt and report.

- [ ] **Step 6: Commit**

```bash
git add -A && git commit -m "feat(scuttle-tui): add wrapping, highlighting, and markdown rendering

Assisted-by: AI"
```

---

### Task 10: Theme and transcript view

**Files:**
- Create: `crates/scuttle-tui/src/theme.rs`, `crates/scuttle-tui/src/transcript_view.rs`
- Modify: `crates/scuttle-tui/src/main.rs` (module declarations)
- Create (generated by insta): `crates/scuttle-tui/src/snapshots/*.snap`

**Interfaces:**
- Consumes: `scuttle_core::{app::App, live::LiveBlock, density::{density_for, BlockKind, Density}}`, `markdown::render`, `wrap::wrap_lines`.
- Produces:
  - `theme::Theme { accent, dim, user, error, ok, warn: Style }` with `Theme::terminal(dark: bool) -> Theme`.
  - `transcript_view::{BlockId, HitTarget, Hit, View, Welcome, build}`. `BlockId` is `(Option<i64>, usize)` meaning (message ID or `None` for live, block index). `HitTarget` is `Toggle(BlockId) | CopyCode(String)`. `Hit { lines: std::ops::Range<usize>, target: HitTarget }`. `View { lines: Vec<Line<'static>>, hits: Vec<Hit>, last_code_blocks: Vec<String> }`. `Welcome { url: String, user: String, art: Vec<String>, show: bool }`. `build(app: &App, overrides: &BTreeMap<String, Density>, toggles: &HashSet<BlockId>, welcome: &Welcome, theme: &Theme, width: u16) -> View`.

- [ ] **Step 1: Write the failing tests**

`crates/scuttle-tui/src/transcript_view.rs`, tests only for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::widgets::Paragraph;
    use ratatui::Terminal;
    use scuttle_core::config::BusyBehavior;
    use serde_json::json;
    use unicode_width::UnicodeWidthStr;

    fn app_with(messages: serde_json::Value) -> App {
        let mut app = App::new(BusyBehavior::Queue, true);
        let chat = serde_json::from_value(json!({"id": uuid::Uuid::new_v4(), "children": [], "files": [], "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}})).unwrap();
        app.update(scuttle_core::app::Msg::ChatLoaded { chat: Box::new(chat), messages: serde_json::from_value(messages).unwrap() });
        app
    }

    fn welcome() -> Welcome {
        Welcome { url: "https://dogfood.example".into(), user: "nick".into(), art: vec![], show: true }
    }

    fn draw(view: &View, w: u16, h: u16) -> Terminal<TestBackend> {
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        t.draw(|f| f.render_widget(Paragraph::new(view.lines.clone()), f.area())).unwrap();
        t
    }

    fn texts(view: &View) -> Vec<String> {
        view.lines.iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect()).collect()
    }

    #[test]
    fn welcome_shows_on_a_blank_chat() {
        let app = App::new(BusyBehavior::Queue, true);
        let view = build(&app, &Default::default(), &Default::default(), &welcome(), &Theme::terminal(true), 80);
        insta::assert_snapshot!(draw(&view, 80, 12).backend());
    }

    #[test]
    fn narrow_terminal_welcome() {
        let app = App::new(BusyBehavior::Queue, true);
        let view = build(&app, &Default::default(), &Default::default(), &welcome(), &Theme::terminal(true), 40);
        assert!(view.lines.iter().all(|l| l.spans.iter().map(|s| s.content.width()).sum::<usize>() <= 40));
        insta::assert_snapshot!(draw(&view, 40, 12).backend());
    }

    #[test]
    fn conversation_renders_user_markdown_and_tool_summary() {
        let app = app_with(json!([
            {"id": 1, "role": "user", "content": [{"type": "text", "text": "list files"}]},
            {"id": 2, "role": "assistant", "content": [
                {"type": "text", "text": "Here you go:\n\n```sh\nls -la\n```"},
                {"type": "tool-call", "tool_call_id": "a", "tool_name": "execute", "args": {"command": "ls -la"}},
                {"type": "tool-result", "tool_call_id": "a", "tool_name": "execute", "result": {"output": "total 0\nfile.txt"}}
            ]}
        ]));
        let view = build(&app, &Default::default(), &Default::default(), &welcome(), &Theme::terminal(true), 60);
        insta::assert_snapshot!(draw(&view, 60, 14).backend());
        assert_eq!(view.last_code_blocks, vec!["ls -la\n".to_string()]);
        assert!(view.hits.iter().any(|h| matches!(&h.target, HitTarget::CopyCode(c) if c == "ls -la\n")));
        assert!(view.hits.iter().any(|h| matches!(h.target, HitTarget::Toggle((Some(2), _)))));
    }

    #[test]
    fn summary_mode_bounds_huge_tool_output() {
        let huge: String = (0..10_000).map(|i| format!("line {i}\n")).collect();
        let app = app_with(json!([
            {"id": 1, "role": "assistant", "content": [
                {"type": "tool-call", "tool_call_id": "a", "tool_name": "read_file", "args": {"path": "/big"}},
                {"type": "tool-result", "tool_call_id": "a", "tool_name": "read_file", "result": {"content": huge}}
            ]}
        ]));
        let view = build(&app, &Default::default(), &Default::default(), &welcome(), &Theme::terminal(true), 40);
        let tool_lines: Vec<_> = texts(&view).into_iter().filter(|l| !l.trim().is_empty()).collect();
        assert!(tool_lines.len() <= 2, "{tool_lines:?}");
        assert!(view.lines.iter().all(|l| l.spans.iter().map(|s| s.content.width()).sum::<usize>() <= 40));
    }

    #[test]
    fn queued_messages_show_dimmed_after_the_conversation() {
        let mut app = app_with(json!([{"id": 1, "role": "user", "content": [{"type": "text", "text": "first"}]}]));
        app.transcript.queued = serde_json::from_value(json!([{"id": 5, "content": [{"type": "text", "text": "next question"}]}])).unwrap();
        let view = build(&app, &Default::default(), &Default::default(), &welcome(), &Theme::terminal(true), 60);
        let last = texts(&view).into_iter().rev().find(|l| !l.trim().is_empty()).unwrap();
        assert!(last.contains("queued") && last.contains("next question"), "{last}");
    }

    #[test]
    fn toggling_expands_and_hidden_tools_are_counted() {
        let app = app_with(json!([
            {"id": 1, "role": "assistant", "content": [
                {"type": "tool-call", "tool_call_id": "a", "tool_name": "read_file", "args": {"path": "/x"}},
                {"type": "tool-result", "tool_call_id": "a", "tool_name": "read_file", "result": {"content": "one\ntwo\nthree"}}
            ]}
        ]));
        let collapsed = build(&app, &Default::default(), &Default::default(), &welcome(), &Theme::terminal(true), 60);
        let id = collapsed.hits.iter().find_map(|h| match h.target { HitTarget::Toggle(id) => Some(id), _ => None }).unwrap();
        let mut toggles = HashSet::new();
        toggles.insert(id);
        let expanded = build(&app, &Default::default(), &toggles, &welcome(), &Theme::terminal(true), 60);
        assert!(texts(&expanded).iter().any(|l| l.contains("three")));
        let mut overrides = BTreeMap::new();
        overrides.insert("read_file".to_string(), Density::Hidden);
        let hidden = build(&app, &overrides, &Default::default(), &welcome(), &Theme::terminal(true), 60);
        assert!(texts(&hidden).iter().any(|l| l.contains("1 hidden tool call")));
    }
}
```

Add `mod theme; mod transcript_view;` to `main.rs`.

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p scuttle-tui transcript_view`
Expected: FAIL with unresolved `build`, `Welcome`, `Theme`.

- [ ] **Step 3: Implement the theme**

`crates/scuttle-tui/src/theme.rs`:

```rust
//! The terminal palette: ANSI colors, so the user's terminal color scheme applies.

use ratatui::style::{Color, Modifier, Style};

#[derive(Debug, Clone, Copy)]
pub struct Theme {
    pub accent: Style,
    pub dim: Style,
    pub user: Style,
    pub error: Style,
    pub ok: Style,
    pub warn: Style,
}

impl Theme {
    pub fn terminal(dark: bool) -> Theme {
        let dim = if dark { Color::DarkGray } else { Color::Gray };
        Theme {
            accent: Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            dim: Style::new().fg(dim),
            user: Style::new().fg(Color::Blue).add_modifier(Modifier::BOLD),
            error: Style::new().fg(Color::Red),
            ok: Style::new().fg(Color::Green),
            warn: Style::new().fg(Color::Yellow),
        }
    }
}
```

- [ ] **Step 4: Implement the transcript view**

Above the tests in `transcript_view.rs`:

```rust
//! Turns core state into transcript lines, the welcome block, and click targets.

use std::collections::{BTreeMap, HashSet};
use std::ops::Range;

use coder_sdk::types;
use ratatui::text::{Line, Span};
use scuttle_core::app::App;
use scuttle_core::density::{BlockKind, Density, density_for};
use scuttle_core::live::LiveBlock;

use crate::markdown;
use crate::theme::Theme;
use crate::wrap::wrap_lines;

/// A block: (message ID, or `None` for the live turn; index of the block within it).
pub type BlockId = (Option<i64>, usize);

#[derive(Debug, Clone, PartialEq)]
pub enum HitTarget {
    Toggle(BlockId),
    CopyCode(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub lines: Range<usize>,
    pub target: HitTarget,
}

#[derive(Debug, Clone, Default)]
pub struct View {
    pub lines: Vec<Line<'static>>,
    pub hits: Vec<Hit>,
    /// Code blocks of the last assistant message, for `/copy <n>`.
    pub last_code_blocks: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Welcome {
    pub url: String,
    pub user: String,
    pub art: Vec<String>,
    pub show: bool,
}

/// A renderable unit before wrapping.
enum Item<'a> {
    UserText(&'a str),
    AssistantText(&'a str),
    Reasoning(&'a str),
    Tool { name: &'a str, args: String, result: String, is_error: bool, done: bool },
}

struct Out<'t> {
    view: View,
    width: u16,
    theme: &'t Theme,
    hidden: usize,
}

impl Out<'_> {
    fn push(&mut self, lines: Vec<Line<'static>>) -> Range<usize> {
        self.flush_hidden();
        let start = self.view.lines.len();
        self.view.lines.extend(wrap_lines(&lines, self.width));
        start..self.view.lines.len()
    }

    fn flush_hidden(&mut self) {
        if self.hidden == 0 {
            return;
        }
        let n = std::mem::take(&mut self.hidden);
        let label = if n == 1 { "1 hidden tool call".to_string() } else { format!("{n} hidden tool calls") };
        let line = Line::from(Span::styled(format!("  ({label})"), self.theme.dim));
        self.view.lines.extend(wrap_lines(&[line], self.width));
    }

    fn gap(&mut self) {
        self.flush_hidden();
        if self.view.lines.last().is_some_and(|l| !l.spans.is_empty()) {
            self.view.lines.push(Line::default());
        }
    }
}

fn one_line(text: &str, max: usize) -> String {
    let first = text.lines().find(|l| !l.trim().is_empty()).unwrap_or_default().trim();
    let mut out: String = first.chars().take(max).collect();
    if first.chars().count() > max || text.lines().filter(|l| !l.trim().is_empty()).count() > 1 {
        out.push('…');
    }
    out
}

fn args_summary(args: &serde_json::Value) -> String {
    match args {
        serde_json::Value::Object(map) => map.values().filter_map(|v| v.as_str()).next().map(str::to_owned).unwrap_or_else(|| args.to_string()),
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn result_text(result: Option<&serde_json::Value>, raw: &str) -> String {
    match result {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Object(map)) => map
            .values()
            .filter_map(|v| v.as_str())
            .next()
            .map(str::to_owned)
            .unwrap_or_else(|| serde_json::to_string_pretty(result.unwrap()).unwrap_or_default()),
        Some(other) => other.to_string(),
        None => raw.to_owned(),
    }
}

fn items_for_message(m: &types::CodersdkChatMessage) -> Vec<Item<'_>> {
    let user = m.role.as_ref().map(|r| r.as_str()) == Some("user");
    let mut items = Vec::new();
    let mut results = BTreeMap::new();
    for p in &m.content {
        if p.type_.as_ref().map(|t| t.as_str()) == Some("tool-result") {
            results.insert(p.tool_call_id.clone().unwrap_or_default(), p);
        }
    }
    for p in &m.content {
        match p.type_.as_ref().map(|t| t.as_str()).unwrap_or_default() {
            "text" if user => items.push(Item::UserText(p.text.as_deref().unwrap_or_default())),
            "text" => items.push(Item::AssistantText(p.text.as_deref().unwrap_or_default())),
            "reasoning" => items.push(Item::Reasoning(p.text.as_deref().unwrap_or_default())),
            "tool-call" => {
                let id = p.tool_call_id.clone().unwrap_or_default();
                let result = results.get(&id);
                items.push(Item::Tool {
                    name: p.tool_name.as_deref().unwrap_or("tool"),
                    args: p.args.as_ref().map(args_summary).unwrap_or_default(),
                    result: result.map(|r| result_text(r.result.as_ref(), "")).unwrap_or_default(),
                    is_error: result.and_then(|r| r.is_error).unwrap_or(false),
                    done: result.is_some(),
                });
            }
            _ => {}
        }
    }
    items
}

fn items_for_live(blocks: &[LiveBlock]) -> Vec<Item<'_>> {
    let mut items = Vec::new();
    for b in blocks {
        match b {
            LiveBlock::Text(t) => items.push(Item::AssistantText(t)),
            LiveBlock::Reasoning(t) => items.push(Item::Reasoning(t)),
            LiveBlock::ToolCall { id, name, args, args_raw } => {
                let result = blocks.iter().find_map(|r| match r {
                    LiveBlock::ToolResult { id: rid, result, result_raw, is_error, done, .. } if rid == id => Some((result_text(result.as_ref(), result_raw), *is_error, *done)),
                    _ => None,
                });
                let (result, is_error, done) = result.unwrap_or_default();
                items.push(Item::Tool {
                    name,
                    args: args.as_ref().map(args_summary).unwrap_or_else(|| args_raw.clone()),
                    result,
                    is_error,
                    done,
                });
            }
            _ => {}
        }
    }
    items
}

fn render_items(out: &mut Out, owner: Option<i64>, items: Vec<Item>, app: &App, overrides: &BTreeMap<String, Density>, toggles: &HashSet<BlockId>, last_assistant: bool) {
    let width = out.width as usize;
    for (index, item) in items.into_iter().enumerate() {
        let id: BlockId = (owner, index);
        match item {
            Item::UserText(text) => {
                out.gap();
                let lines = text.lines().map(|l| Line::from(vec![Span::styled("› ", out.theme.user), Span::raw(l.to_owned())])).collect();
                out.push(lines);
            }
            Item::AssistantText(text) => {
                out.gap();
                let rendered = markdown::render(text);
                let base = out.view.lines.len();
                for block in &rendered.code_blocks {
                    let start = base + wrap_lines(&rendered.lines[..block.start], out.width).len();
                    let end = base + wrap_lines(&rendered.lines[..block.end], out.width).len();
                    out.view.hits.push(Hit { lines: start..end, target: HitTarget::CopyCode(block.code.clone()) });
                }
                if last_assistant {
                    out.view.last_code_blocks.extend(rendered.code_blocks.iter().map(|b| b.code.clone()));
                }
                out.push(rendered.lines);
            }
            Item::Reasoning(text) => {
                let mut density = density_for(BlockKind::Reasoning, &app.prefs, overrides);
                if toggles.contains(&id) {
                    density = if density == Density::Expanded { Density::Summary } else { Density::Expanded };
                }
                let lines = match density {
                    Density::Expanded => text.lines().map(|l| Line::from(Span::styled(l.to_owned(), out.theme.dim))).collect(),
                    _ => vec![Line::from(Span::styled("∴ Thinking", out.theme.dim))],
                };
                let range = out.push(lines);
                out.view.hits.push(Hit { lines: range, target: HitTarget::Toggle(id) });
            }
            Item::Tool { name, args, result, is_error, done } => {
                let mut density = density_for(BlockKind::Tool(name), &app.prefs, overrides);
                if toggles.contains(&id) {
                    density = if density == Density::Expanded { Density::Summary } else { Density::Expanded };
                }
                if density == Density::Hidden {
                    out.hidden += 1;
                    continue;
                }
                let marker = match (done, is_error) {
                    (false, _) => Span::styled("◌ ", out.theme.warn),
                    (true, true) => Span::styled("✗ ", out.theme.error),
                    (true, false) => Span::styled("⏺ ", out.theme.ok),
                };
                let head_budget = width.saturating_sub(name.chars().count() + 6).max(8);
                let head = Line::from(vec![marker, Span::styled(name.to_owned(), out.theme.accent), Span::raw(format!("({})", one_line(&args, head_budget)))]);
                let mut lines = vec![head];
                match density {
                    Density::Expanded => lines.extend(result.lines().map(|l| Line::from(Span::styled(format!("  {l}"), out.theme.dim)))),
                    _ if !result.is_empty() => lines.push(Line::from(Span::styled(format!("  ⎿ {}", one_line(&result, width.saturating_sub(6).max(8))), out.theme.dim))),
                    _ => {}
                }
                let mut wrapped = Vec::new();
                for (i, l) in lines.into_iter().enumerate() {
                    let w = wrap_lines(&[l], out.width);
                    if density == Density::Expanded || i > 0 || w.len() == 1 {
                        wrapped.extend(w);
                    } else {
                        wrapped.push(w.into_iter().next().unwrap_or_default());
                    }
                }
                if density != Density::Expanded {
                    wrapped.truncate(2);
                }
                out.flush_hidden();
                let start = out.view.lines.len();
                out.view.lines.extend(wrapped);
                out.view.hits.push(Hit { lines: start..out.view.lines.len(), target: HitTarget::Toggle(id) });
            }
        }
    }
}

fn welcome_lines(w: &Welcome, theme: &Theme) -> Vec<Line<'static>> {
    let mut lines = vec![Line::default()];
    if w.art.is_empty() {
        lines.push(Line::from(Span::styled("scuttle", theme.accent)));
    } else {
        lines.extend(w.art.iter().map(|l| Line::from(Span::styled(l.clone(), theme.accent))));
    }
    lines.push(Line::from(Span::styled("An unofficial terminal client for Coder Agents", theme.dim)));
    lines.push(Line::default());
    if !w.user.is_empty() {
        lines.push(Line::from(vec![Span::styled("Signed in as ", theme.dim), Span::raw(w.user.clone())]));
    }
    lines.push(Line::from(vec![Span::styled("Deployment ", theme.dim), Span::raw(w.url.clone())]));
    lines.push(Line::default());
    lines.push(Line::from(Span::styled("Type a message to start. /help lists commands.", theme.dim)));
    lines
}

pub fn build(app: &App, overrides: &BTreeMap<String, Density>, toggles: &HashSet<BlockId>, welcome: &Welcome, theme: &Theme, width: u16) -> View {
    let mut out = Out { view: View::default(), width: width.max(2), theme, hidden: 0 };
    let messages: Vec<_> = app.transcript.messages().collect();
    if messages.is_empty() && app.transcript.live.is_empty() {
        if welcome.show {
            out.push(welcome_lines(welcome, theme));
        }
        return out.view;
    }
    let last_assistant = messages.iter().rposition(|m| m.role.as_ref().map(|r| r.as_str()) == Some("assistant"));
    for (i, m) in messages.iter().enumerate() {
        let is_last = Some(i) == last_assistant && app.transcript.live.is_empty();
        render_items(&mut out, m.id, items_for_message(m), app, overrides, toggles, is_last);
    }
    if !app.transcript.live.is_empty() {
        render_items(&mut out, None, items_for_live(&app.transcript.live.blocks), app, overrides, toggles, true);
    }
    for queued in &app.transcript.queued {
        let text = queued.content.iter().filter_map(|p| p.text.as_deref()).collect::<Vec<_>>().join(" ");
        out.push(vec![Line::from(Span::styled(format!("  queued · {}", one_line(&text, (width as usize).saturating_sub(12).max(8))), theme.dim))]);
    }
    if let Some(err) = app.transcript.last_error.as_ref() {
        out.gap();
        out.push(vec![Line::from(Span::styled(format!("Error: {err}"), theme.error))]);
    }
    out.flush_hidden();
    out.view
}
```

- [ ] **Step 5: Accept the snapshots**

Run: `cargo test -p scuttle-tui transcript_view`, then `cargo insta review` (install with `cargo install cargo-insta --root ~/.cargo` only if it is missing), and accept the three snapshots after checking them by eye: the welcome block, the 40-column welcome, and the conversation with a one-line `execute` summary and its `⎿` result line.
Expected: all six tests pass.

- [ ] **Step 6: Commit**

```bash
git add -A && git commit -m "feat(scuttle-tui): render the transcript, tool density, and welcome screen

Assisted-by: AI"
```

---

### Task 11: The composer

**Files:**
- Create: `crates/scuttle-tui/src/composer.rs`
- Modify: `crates/scuttle-tui/src/main.rs` (module declaration)

**Interfaces:**
- Consumes: `scuttle_core::density::SendShortcut`, `scuttle_core::commands::{completions, CommandInfo}`.
- Produces: `composer::{Composer, ComposerAction}`. `Composer::new(max_lines: u16) -> Composer`; `Composer::handle_key(&mut self, key: crossterm::event::KeyEvent, shortcut: SendShortcut) -> ComposerAction`; `Composer::paste(&mut self, text: &str)`; `Composer::text(&self) -> String`; `Composer::set_text(&mut self, text: &str)`; `Composer::height(&self) -> u16`; `Composer::slash_matches(&self) -> Vec<&'static CommandInfo>`; `Composer::widget(&self) -> &ratatui_textarea::TextArea<'static>`. `ComposerAction` is `None | Submit(String) | OpenEditor | Interrupt`.

- [ ] **Step 1: Write the failing tests**

`crates/scuttle-tui/src/composer.rs`, tests only for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    fn type_str(c: &mut Composer, s: &str) {
        for ch in s.chars() {
            c.handle_key(key(KeyCode::Char(ch), KeyModifiers::NONE), SendShortcut::Enter);
        }
    }

    #[test]
    fn enter_sends_and_shift_enter_inserts_a_newline() {
        let mut c = Composer::new(10);
        type_str(&mut c, "a");
        assert_eq!(c.handle_key(key(KeyCode::Enter, KeyModifiers::SHIFT), SendShortcut::Enter), ComposerAction::None);
        type_str(&mut c, "b");
        assert_eq!(c.handle_key(key(KeyCode::Enter, KeyModifiers::NONE), SendShortcut::Enter), ComposerAction::Submit("a\nb".into()));
        assert_eq!(c.text(), "");
    }

    #[test]
    fn modifier_enter_mode_swaps_the_keys() {
        let mut c = Composer::new(10);
        type_str(&mut c, "a");
        assert_eq!(c.handle_key(key(KeyCode::Enter, KeyModifiers::NONE), SendShortcut::ModifierEnter), ComposerAction::None);
        assert_eq!(c.handle_key(key(KeyCode::Enter, KeyModifiers::CONTROL), SendShortcut::ModifierEnter), ComposerAction::Submit("a\n".into()));
    }

    #[test]
    fn alt_enter_and_ctrl_j_always_insert_newlines() {
        let mut c = Composer::new(10);
        type_str(&mut c, "a");
        c.handle_key(key(KeyCode::Enter, KeyModifiers::ALT), SendShortcut::Enter);
        c.handle_key(key(KeyCode::Char('j'), KeyModifiers::CONTROL), SendShortcut::Enter);
        assert_eq!(c.text(), "a\n\n");
    }

    #[test]
    fn empty_enter_does_not_submit() {
        let mut c = Composer::new(10);
        assert_eq!(c.handle_key(key(KeyCode::Enter, KeyModifiers::NONE), SendShortcut::Enter), ComposerAction::None);
    }

    #[test]
    fn history_recalls_previous_messages_and_restores_the_draft() {
        let mut c = Composer::new(10);
        type_str(&mut c, "first");
        c.handle_key(key(KeyCode::Enter, KeyModifiers::NONE), SendShortcut::Enter);
        type_str(&mut c, "second");
        c.handle_key(key(KeyCode::Enter, KeyModifiers::NONE), SendShortcut::Enter);
        type_str(&mut c, "draft");
        c.handle_key(key(KeyCode::Up, KeyModifiers::NONE), SendShortcut::Enter);
        assert_eq!(c.text(), "second");
        c.handle_key(key(KeyCode::Up, KeyModifiers::NONE), SendShortcut::Enter);
        assert_eq!(c.text(), "first");
        c.handle_key(key(KeyCode::Down, KeyModifiers::NONE), SendShortcut::Enter);
        c.handle_key(key(KeyCode::Down, KeyModifiers::NONE), SendShortcut::Enter);
        assert_eq!(c.text(), "draft");
    }

    #[test]
    fn tab_completes_the_first_slash_match() {
        let mut c = Composer::new(10);
        type_str(&mut c, "/co");
        assert_eq!(c.slash_matches().len(), 2);
        c.handle_key(key(KeyCode::Tab, KeyModifiers::NONE), SendShortcut::Enter);
        assert_eq!(c.text(), "/compact ");
    }

    #[test]
    fn paste_is_verbatim_and_ctrl_g_opens_the_editor() {
        let mut c = Composer::new(10);
        c.paste("line one\nline two");
        assert_eq!(c.text(), "line one\nline two");
        assert_eq!(c.handle_key(key(KeyCode::Char('g'), KeyModifiers::CONTROL), SendShortcut::Enter), ComposerAction::OpenEditor);
        assert_eq!(c.handle_key(key(KeyCode::Esc, KeyModifiers::NONE), SendShortcut::Enter), ComposerAction::Interrupt);
    }

    #[test]
    fn height_grows_with_content_up_to_the_limit() {
        let mut c = Composer::new(3);
        assert_eq!(c.height(), 3);
        c.paste("1\n2\n3\n4\n5");
        assert_eq!(c.height(), 5);
    }
}
```

Add `mod composer;` to `main.rs`.

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p scuttle-tui composer`
Expected: FAIL with unresolved `Composer`.

- [ ] **Step 3: Implement**

Above the tests in `composer.rs`:

```rust
//! The multi-line input box with sent-message history and slash completion.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui_textarea::TextArea;
use scuttle_core::commands::{CommandInfo, completions};
use scuttle_core::density::SendShortcut;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComposerAction {
    None,
    Submit(String),
    OpenEditor,
    Interrupt,
}

pub struct Composer {
    area: TextArea<'static>,
    max_lines: u16,
    history: Vec<String>,
    history_pos: Option<usize>,
    draft: String,
}

impl Composer {
    pub fn new(max_lines: u16) -> Composer {
        let mut area = TextArea::default();
        area.set_placeholder_text("Message the agent. Enter to send, Shift+Enter for a new line, /help for commands.");
        Composer { area, max_lines: max_lines.max(1), history: Vec::new(), history_pos: None, draft: String::new() }
    }

    pub fn widget(&self) -> &TextArea<'static> {
        &self.area
    }

    pub fn text(&self) -> String {
        self.area.lines().join("\n")
    }

    pub fn set_text(&mut self, text: &str) {
        let lines: Vec<String> = if text.is_empty() { vec![String::new()] } else { text.split('\n').map(str::to_owned).collect() };
        let placeholder = self.area.placeholder_text().to_owned();
        self.area = TextArea::new(lines);
        self.area.set_placeholder_text(placeholder);
        self.area.move_cursor(ratatui_textarea::CursorMove::Bottom);
        self.area.move_cursor(ratatui_textarea::CursorMove::End);
    }

    pub fn paste(&mut self, text: &str) {
        self.area.insert_str(text);
    }

    /// Content lines plus the border, never more than `max_lines` of content.
    pub fn height(&self) -> u16 {
        (self.area.lines().len() as u16).clamp(1, self.max_lines) + 2
    }

    pub fn slash_matches(&self) -> Vec<&'static CommandInfo> {
        let text = self.text();
        if !text.starts_with('/') || text.contains(char::is_whitespace) {
            return Vec::new();
        }
        completions(&text)
    }

    fn submit(&mut self) -> ComposerAction {
        let text = self.text();
        if text.trim().is_empty() {
            return ComposerAction::None;
        }
        self.history.push(text.clone());
        self.history_pos = None;
        self.draft.clear();
        self.set_text("");
        ComposerAction::Submit(text)
    }

    fn recall(&mut self, older: bool) {
        if self.history.is_empty() {
            return;
        }
        let next = match (self.history_pos, older) {
            (None, true) => {
                self.draft = self.text();
                Some(self.history.len() - 1)
            }
            (Some(0), true) => Some(0),
            (Some(i), true) => Some(i - 1),
            (Some(i), false) if i + 1 < self.history.len() => Some(i + 1),
            (Some(_), false) => None,
            (None, false) => return,
        };
        self.history_pos = next;
        let text = match next {
            Some(i) => self.history[i].clone(),
            None => std::mem::take(&mut self.draft),
        };
        self.set_text(&text);
    }

    pub fn handle_key(&mut self, key: KeyEvent, shortcut: SendShortcut) -> ComposerAction {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let sup = key.modifiers.contains(KeyModifiers::SUPER);
        match key.code {
            KeyCode::Enter if alt => self.area.insert_newline(),
            KeyCode::Enter => {
                let send = match shortcut {
                    SendShortcut::Enter => !shift && !ctrl && !sup,
                    SendShortcut::ModifierEnter => ctrl || sup,
                };
                if send {
                    return self.submit();
                }
                self.area.insert_newline();
            }
            KeyCode::Char('j') if ctrl => self.area.insert_newline(),
            KeyCode::Char('g') if ctrl => return ComposerAction::OpenEditor,
            KeyCode::Esc => return ComposerAction::Interrupt,
            KeyCode::Tab => {
                if let Some(first) = self.slash_matches().first() {
                    self.set_text(&format!("{} ", first.name));
                }
            }
            KeyCode::Up if self.area.cursor().0 == 0 => self.recall(true),
            KeyCode::Down if self.area.cursor().0 + 1 >= self.area.lines().len() && self.history_pos.is_some() => self.recall(false),
            _ => {
                self.area.input(key);
            }
        }
        ComposerAction::None
    }
}
```

`height()` is the content rows, clamped to `max_lines`, plus 2 rows of chrome, so five lines with `max_lines` 3 gives 5. If `ratatui-textarea` 0.9 names differ (`set_placeholder_text`, `placeholder_text`, `insert_str`, `insert_newline`, `move_cursor`, `CursorMove`, `cursor`, `input` accepting a crossterm `KeyEvent`), adapt and report.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p scuttle-tui composer`
Expected: all 8 tests pass.

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat(scuttle-tui): add the composer with history and slash completion

Assisted-by: AI"
```

---

### Task 12: Footer and pickers

**Files:**
- Create: `crates/scuttle-tui/src/footer.rs`, `crates/scuttle-tui/src/picker.rs`
- Modify: `crates/scuttle-tui/src/main.rs` (module declarations)

**Interfaces:**
- Consumes: `scuttle_core::app::{App, Connection, Notice, Picker}`, `scuttle_core::usage::{context_usage, format_tokens}`, `theme::Theme`.
- Produces:
  - `footer::footer_line(app: &App, theme: &Theme, width: u16) -> Line<'static>`.
  - `picker::{PickerState, PickerChoice}`. `PickerState::open(kind: Picker, app: &App) -> PickerState`; `PickerState::handle_key(&mut self, key: KeyEvent) -> Option<PickerChoice>`; `PickerState::render(&self, frame: &mut Frame, area: Rect, theme: &Theme)`. `PickerChoice` is `Model(Uuid) | Workspace(Option<Uuid>) | Cancel`.

- [ ] **Step 1: Write the failing tests**

`crates/scuttle-tui/src/footer.rs`, tests only for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use scuttle_core::config::BusyBehavior;
    use serde_json::json;

    fn text(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn shows_model_context_and_status() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let id = uuid::Uuid::new_v4();
        app.update(scuttle_core::app::Msg::ModelsLoaded(vec![serde_json::from_value(json!({"id": id, "display_name": "Big", "is_default": true, "enabled": true, "reasoning_efforts": []})).unwrap()]));
        let chat = serde_json::from_value(json!({"id": uuid::Uuid::new_v4(), "children": [], "files": [], "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}})).unwrap();
        app.update(scuttle_core::app::Msg::ChatLoaded { chat: Box::new(chat), messages: serde_json::from_value(json!([{"id": 1, "role": "assistant", "content": [], "usage": {"input_tokens": 12000, "context_limit": 200000}}])).unwrap() });
        let line = footer_line(&app, &Theme::terminal(true), 80);
        let t = text(&line);
        assert!(t.contains("Big"), "{t}");
        assert!(t.contains("12.0k/200.0k"), "{t}");
        assert!(t.contains("connecting"), "{t}");
    }

    #[test]
    fn latest_notice_replaces_the_status_and_reconnecting_is_visible() {
        let mut app = App::new(BusyBehavior::Queue, true);
        app.connection = Connection::Reconnecting { attempt: 2 };
        assert!(text(&footer_line(&app, &Theme::terminal(true), 80)).contains("reconnecting"));
        app.notices.push(Notice::Error("Could not send message: HTTP 409".into()));
        assert!(text(&footer_line(&app, &Theme::terminal(true), 80)).contains("HTTP 409"));
    }

    #[test]
    fn fits_narrow_widths() {
        let mut app = App::new(BusyBehavior::Queue, true);
        app.notices.push(Notice::Info("a very long notice that will not fit in a narrow terminal at all".into()));
        let line = footer_line(&app, &Theme::terminal(true), 20);
        assert!(unicode_width::UnicodeWidthStr::width(text(&line).as_str()) <= 20);
    }
}
```

`crates/scuttle-tui/src/picker.rs`, tests only for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use scuttle_core::app::{Msg, WorkspaceRef};
    use scuttle_core::config::BusyBehavior;
    use serde_json::json;

    fn press(p: &mut PickerState, code: KeyCode) -> Option<PickerChoice> {
        p.handle_key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    #[test]
    fn model_picker_selects_with_arrows_and_enter() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let (a, b) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        app.update(Msg::ModelsLoaded(serde_json::from_value(json!([
            {"id": a, "display_name": "A", "enabled": true, "reasoning_efforts": []},
            {"id": b, "display_name": "B", "enabled": true, "reasoning_efforts": []}
        ])).unwrap()));
        let mut p = PickerState::open(Picker::Model, &app);
        press(&mut p, KeyCode::Down);
        assert_eq!(press(&mut p, KeyCode::Enter), Some(PickerChoice::Model(b)));
    }

    #[test]
    fn workspace_picker_offers_none_first_and_escape_cancels() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let ws = uuid::Uuid::new_v4();
        app.update(Msg::WorkspacesLoaded(vec![WorkspaceRef { id: ws, name: "dev".into() }]));
        let mut p = PickerState::open(Picker::Workspace, &app);
        assert_eq!(press(&mut p, KeyCode::Enter), Some(PickerChoice::Workspace(None)));
        let mut p = PickerState::open(Picker::Workspace, &app);
        press(&mut p, KeyCode::Down);
        assert_eq!(press(&mut p, KeyCode::Enter), Some(PickerChoice::Workspace(Some(ws))));
        let mut p = PickerState::open(Picker::Workspace, &app);
        assert_eq!(press(&mut p, KeyCode::Esc), Some(PickerChoice::Cancel));
    }
}
```

Add `mod footer; mod picker;` to `main.rs`.

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p scuttle-tui footer picker`
Expected: FAIL with unresolved names.

- [ ] **Step 3: Implement**

Above the tests in `footer.rs`:

```rust
//! The one-line status footer: model, context, and status, or the latest notice.

use ratatui::text::{Line, Span};
use scuttle_core::app::{App, Connection, Notice};
use scuttle_core::usage::{context_usage, format_tokens};
use unicode_width::UnicodeWidthChar;

use crate::theme::Theme;

fn fit(text: String, width: usize) -> String {
    let mut used = 0;
    let mut out = String::new();
    for c in text.chars() {
        let w = c.width().unwrap_or(0);
        if used + w > width {
            break;
        }
        used += w;
        out.push(c);
    }
    out
}

pub fn footer_line(app: &App, theme: &Theme, width: u16) -> Line<'static> {
    let width = width as usize;
    if let Some(notice) = app.notices.last() {
        let (text, style) = match notice {
            Notice::Info(t) => (t.clone(), theme.dim),
            Notice::Error(t) => (t.clone(), theme.error),
        };
        return Line::from(Span::styled(fit(text, width), style));
    }
    let mut parts = Vec::new();
    if let Some(name) = app.model_name() {
        parts.push(name);
    }
    if let Some(u) = context_usage(app.transcript.messages()) {
        let pct = if u.limit > 0 { u.used * 100 / u.limit } else { 0 };
        parts.push(format!("{}/{} ({pct}%)", format_tokens(u.used), format_tokens(u.limit)));
    }
    let status = match app.connection {
        Connection::Reconnecting { attempt } => format!("reconnecting (attempt {attempt})"),
        Connection::Connecting => "connecting".into(),
        Connection::Idle => "new chat".into(),
        Connection::Live => app.transcript.status.as_ref().map(|s| s.as_str().replace('_', " ")).unwrap_or_else(|| "ready".into()),
    };
    parts.push(status);
    Line::from(Span::styled(fit(parts.join(" · "), width), theme.dim))
}
```

Above the tests in `picker.rs`:

```rust
//! Pickers for the model and the workspace, drawn above the composer.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState};
use scuttle_core::app::{App, Picker};
use uuid::Uuid;

use crate::theme::Theme;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickerChoice {
    Model(Uuid),
    Workspace(Option<Uuid>),
    Cancel,
}

pub struct PickerState {
    kind: Picker,
    items: Vec<(String, Option<Uuid>)>,
    selected: usize,
}

impl PickerState {
    pub fn open(kind: Picker, app: &App) -> PickerState {
        let items = match kind {
            Picker::Model => app
                .models
                .iter()
                .filter_map(|m| Some((m.display_name.clone().or_else(|| m.model.clone())?, Some(m.id?))))
                .collect(),
            Picker::Workspace => std::iter::once(("none (no workspace)".to_string(), None))
                .chain(app.workspaces.iter().map(|w| (w.name.clone(), Some(w.id))))
                .collect(),
        };
        PickerState { kind, items, selected: 0 }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Option<PickerChoice> {
        match key.code {
            KeyCode::Esc => Some(PickerChoice::Cancel),
            KeyCode::Up => {
                self.selected = self.selected.saturating_sub(1);
                None
            }
            KeyCode::Down => {
                if self.selected + 1 < self.items.len() {
                    self.selected += 1;
                }
                None
            }
            KeyCode::Enter => {
                let (_, id) = self.items.get(self.selected)?;
                Some(match self.kind {
                    Picker::Model => PickerChoice::Model((*id)?),
                    Picker::Workspace => PickerChoice::Workspace(*id),
                })
            }
            _ => None,
        }
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let title = match self.kind {
            Picker::Model => " Model ",
            Picker::Workspace => " Workspace ",
        };
        let items: Vec<ListItem> = self.items.iter().map(|(name, _)| ListItem::new(name.clone())).collect();
        let list = List::new(items).block(Block::default().borders(Borders::ALL).title(title)).highlight_style(theme.accent).highlight_symbol("› ");
        let mut state = ListState::default().with_selected(Some(self.selected));
        frame.render_widget(Clear, area);
        frame.render_stateful_widget(list, area, &mut state);
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p scuttle-tui footer picker`
Expected: all 5 tests pass.

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat(scuttle-tui): add the status footer and model and workspace pickers

Assisted-by: AI"
```

---

### Task 13: Clipboard

**Files:**
- Create: `crates/scuttle-tui/src/clipboard.rs`
- Modify: `crates/scuttle-tui/src/main.rs` (module declaration)

**Interfaces:**
- Produces: `clipboard::{Clipboard, CopyOutcome, osc52_sequence, OSC52_MAX_BYTES}`. `Clipboard::new() -> Clipboard`; `Clipboard::copy(&mut self, text: &str) -> CopyOutcome`; `CopyOutcome` is `Copied | Failed(String)`; `osc52_sequence(text: &str, in_tmux: bool) -> Option<String>` (returns `None` above `OSC52_MAX_BYTES`, which is `100_000`).

- [ ] **Step 1: Write the failing tests**

`crates/scuttle-tui/src/clipboard.rs`, tests only for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_osc52_and_tmux_passthrough() {
        assert_eq!(osc52_sequence("hi", false).as_deref(), Some("\x1b]52;c;aGk=\x07"));
        assert_eq!(osc52_sequence("hi", true).as_deref(), Some("\x1bPtmux;\x1b\x1b]52;c;aGk=\x07\x1b\\"));
    }

    #[test]
    fn refuses_payloads_over_the_limit() {
        let big = "x".repeat(OSC52_MAX_BYTES + 1);
        assert!(osc52_sequence(&big, false).is_none());
        assert!(osc52_sequence(&"x".repeat(OSC52_MAX_BYTES), false).is_some());
    }
}
```

Add `mod clipboard;` to `main.rs`.

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p scuttle-tui clipboard`
Expected: FAIL with unresolved `osc52_sequence`.

- [ ] **Step 3: Implement**

Above the tests in `clipboard.rs`:

```rust
//! Copying text: the native clipboard locally, OSC 52 over SSH, and tmux passthrough.

use std::io::Write;

use base64::Engine;

pub const OSC52_MAX_BYTES: usize = 100_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CopyOutcome {
    Copied,
    Failed(String),
}

/// The OSC 52 escape sequence for `text`, wrapped for tmux when needed.
pub fn osc52_sequence(text: &str, in_tmux: bool) -> Option<String> {
    if text.len() > OSC52_MAX_BYTES {
        return None;
    }
    let b64 = base64::engine::general_purpose::STANDARD.encode(text);
    let seq = format!("\x1b]52;c;{b64}\x07");
    Some(if in_tmux { format!("\x1bPtmux;{}\x1b\\", seq.replace('\x1b', "\x1b\x1b")) } else { seq })
}

pub struct Clipboard {
    native: Option<arboard::Clipboard>,
    in_tmux: bool,
    over_ssh: bool,
    warned_tmux: bool,
}

impl Clipboard {
    pub fn new() -> Clipboard {
        let over_ssh = std::env::var_os("SSH_CONNECTION").is_some() || std::env::var_os("SSH_TTY").is_some();
        Clipboard {
            // X11 and some Wayland compositors drop the clipboard when the owner exits, so keep this alive.
            native: if over_ssh { None } else { arboard::Clipboard::new().ok() },
            in_tmux: std::env::var_os("TMUX").is_some(),
            over_ssh,
            warned_tmux: false,
        }
    }

    fn tmux_clipboard_off(&self) -> bool {
        std::process::Command::new("tmux")
            .args(["show", "-gv", "set-clipboard"])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim() != "on")
            .unwrap_or(false)
    }

    fn osc52(&mut self, text: &str) -> CopyOutcome {
        let Some(seq) = osc52_sequence(text, self.in_tmux) else {
            return CopyOutcome::Failed(format!("too large to copy over the terminal ({} bytes, limit {OSC52_MAX_BYTES})", text.len()));
        };
        let mut out = std::io::stdout();
        if out.write_all(seq.as_bytes()).and_then(|_| out.flush()).is_err() {
            return CopyOutcome::Failed("could not write to the terminal".into());
        }
        if self.in_tmux && !self.warned_tmux && self.tmux_clipboard_off() {
            self.warned_tmux = true;
            return CopyOutcome::Failed("copied, but tmux set-clipboard is not on, so it may not reach your clipboard".into());
        }
        CopyOutcome::Copied
    }

    pub fn copy(&mut self, text: &str) -> CopyOutcome {
        if !self.over_ssh {
            if let Some(native) = self.native.as_mut() {
                if native.set_text(text.to_owned()).is_ok() {
                    return CopyOutcome::Copied;
                }
            }
        }
        self.osc52(text)
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p scuttle-tui clipboard`
Expected: both tests pass.

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat(scuttle-tui): add clipboard copy with osc 52 and tmux support

Assisted-by: AI"
```

---

### Task 14: Wire the terminal, runtime, and app

**Files:**
- Create: `crates/scuttle-tui/src/terminal.rs`, `crates/scuttle-tui/src/runtime.rs`, `crates/scuttle-tui/src/app.rs`
- Modify: `crates/scuttle-tui/src/main.rs`

**Interfaces:**
- Consumes: everything above; `coder_sdk::{Client, discover_session, GENERATED_FROM, Error}`; the generated calls `get_organizations_by_user("me")`, `get_chat_by_id`, `list_chat_messages(&chat, None, None, Some(200))`, `create_chat`, `send_chat_message`, `interrupt_chat`, `compact_chat`, `clear_chat_context`, `update_chat`, `get_user_preference_settings("me")`, `list_ai_models_and_provider_descriptors_in_an_organization(&org.to_string())`, `list_workspaces(Some(100), None, Some("owner:me"))`.
- Produces: the runnable `scuttle [chat-id]` binary. `terminal::{enter, leave, set_mouse}`; `runtime::Runtime::new(client: Client, tx: UnboundedSender<Msg>) -> Runtime`, `Runtime::run(&mut self, effect: Effect)` for API effects; `app::Tui::new(...)`, `Tui::handle(&mut self, ev: crossterm::event::Event) -> Vec<Effect>`, `Tui::draw(&mut self, f: &mut Frame)`, `Tui::apply_ui_effect(&mut self, e: &Effect) -> bool` (returns false for effects the runtime handles).

- [ ] **Step 1: Write the failing tests**

`crates/scuttle-tui/src/app.rs`, tests only for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use serde_json::json;

    fn tui() -> Tui {
        Tui::new(scuttle_core::config::LocalConfig::default(), None, Theme::terminal(true), Welcome { url: "https://x".into(), user: "nick".into(), art: vec![], show: true })
    }

    fn key(code: KeyCode, mods: KeyModifiers) -> Event {
        Event::Key(KeyEvent::new(code, mods))
    }

    #[test]
    fn typing_and_enter_submits_through_core() {
        let mut t = tui();
        t.core.update(Msg::Started { org_id: uuid::Uuid::new_v4(), open_chat: None });
        for c in "hi".chars() {
            t.handle(key(KeyCode::Char(c), KeyModifiers::NONE));
        }
        let effects = t.handle(key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(effects.as_slice(), [Effect::CreateChat { text, .. }] if text == "hi"));
    }

    #[test]
    fn ctrl_c_twice_quits() {
        let mut t = tui();
        assert!(t.handle(key(KeyCode::Char('c'), KeyModifiers::CONTROL)).is_empty());
        assert_eq!(t.handle(key(KeyCode::Char('c'), KeyModifiers::CONTROL)), vec![Effect::Quit]);
    }

    #[test]
    fn click_on_a_code_block_copies_it() {
        let mut t = tui();
        let chat = serde_json::from_value(json!({"id": uuid::Uuid::new_v4(), "children": [], "files": [], "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}})).unwrap();
        t.core.update(Msg::ChatLoaded {
            chat: Box::new(chat),
            messages: serde_json::from_value(json!([{"id": 1, "role": "assistant", "content": [{"type": "text", "text": "```\necho hi\n```"}]}])).unwrap(),
        });
        let mut term = Terminal::new(TestBackend::new(60, 20)).unwrap();
        term.draw(|f| t.draw(f)).unwrap();
        let row = t.row_of(|target| matches!(target, HitTarget::CopyCode(_))).expect("code block on screen");
        t.handle(Event::Mouse(MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: 2, row, modifiers: KeyModifiers::NONE }));
        assert_eq!(t.last_copied.as_deref(), Some("echo hi\n"));
    }

    #[test]
    fn slash_model_opens_the_picker_and_escape_closes_it() {
        let mut t = tui();
        let effects = t.core.update(Msg::Command(scuttle_core::commands::Command::Model(None)));
        for e in &effects {
            t.apply_ui_effect(e);
        }
        assert!(t.picker.is_some());
        t.handle(key(KeyCode::Esc, KeyModifiers::NONE));
        assert!(t.picker.is_none());
    }

    #[test]
    fn editor_failure_keeps_composer_text() {
        let mut t = tui();
        t.composer.set_text("keep me");
        let result = t.edit_with(|_path| Err(std::io::Error::other("editor crashed")));
        assert!(result.is_err());
        assert_eq!(t.composer.text(), "keep me");
    }

    #[test]
    fn draw_fits_small_terminals() {
        let mut t = tui();
        for (w, h) in [(40u16, 10u16), (20, 6), (120, 40)] {
            let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
            term.draw(|f| t.draw(f)).unwrap();
        }
    }
}
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p scuttle-tui app::tests`
Expected: FAIL with unresolved `Tui`.

- [ ] **Step 3: Implement terminal setup and restore**

`crates/scuttle-tui/src/terminal.rs`:

```rust
//! Entering and leaving the full-screen terminal, restored on exit and on panic.

use std::io::{Write, stdout};

use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, KeyboardEnhancementFlags,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use ratatui::DefaultTerminal;

pub fn enter(mouse: bool) -> std::io::Result<DefaultTerminal> {
    let terminal = ratatui::init();
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = leave();
        hook(info);
    }));
    let mut out = stdout();
    execute!(out, EnableBracketedPaste)?;
    let _ = execute!(out, PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES));
    set_mouse(mouse)?;
    Ok(terminal)
}

pub fn set_mouse(enabled: bool) -> std::io::Result<()> {
    let mut out = stdout();
    if enabled {
        execute!(out, EnableMouseCapture)
    } else {
        execute!(out, DisableMouseCapture)
    }
}

/// Restores every mode `enter` changed. Safe to call more than once.
pub fn leave() -> std::io::Result<()> {
    let mut out = stdout();
    let _ = execute!(out, PopKeyboardEnhancementFlags);
    let _ = execute!(out, DisableMouseCapture, DisableBracketedPaste);
    ratatui::restore();
    out.flush()
}
```

- [ ] **Step 4: Implement the runtime**

`crates/scuttle-tui/src/runtime.rs`:

```rust
//! Executes API effects with coder-sdk and reports results back as `Msg`s.

use coder_sdk::{Client, types};
use futures::StreamExt;
use scuttle_core::app::{Effect, Msg, WorkspaceRef};
use scuttle_core::density::DisplayPrefs;
use tokio::sync::mpsc::UnboundedSender;
use tokio::task::JoinHandle;
use uuid::Uuid;

pub struct Runtime {
    client: Client,
    tx: UnboundedSender<Msg>,
    stream: Option<JoinHandle<()>>,
}

fn text_part(text: &str) -> types::CodersdkChatInputPart {
    types::CodersdkChatInputPart {
        type_: Some(types::CodersdkChatInputPartType("text".into())),
        text: Some(text.to_owned()),
        ..Default::default()
    }
}

async fn err<E: serde::Serialize + std::fmt::Debug>(e: progenitor_client::Error<E>) -> String {
    coder_sdk::Error::from_progenitor(e).await.to_string()
}

impl Runtime {
    pub fn new(client: Client, tx: UnboundedSender<Msg>) -> Runtime {
        Runtime { client, tx, stream: None }
    }

    /// Resolves the user's first organization, the one new chats are created in.
    pub async fn organization(&self) -> Result<Uuid, String> {
        let orgs = match self.client.api().get_organizations_by_user("me").await {
            Ok(r) => r.into_inner(),
            Err(e) => return Err(err(e).await),
        };
        orgs.into_iter().find_map(|o| o.id).ok_or_else(|| "you are not a member of any organization".into())
    }

    fn open_stream(&mut self, chat: Uuid, after_id: Option<i64>, delay: std::time::Duration) {
        if let Some(old) = self.stream.take() {
            old.abort();
        }
        let client = self.client.clone();
        let tx = self.tx.clone();
        self.stream = Some(tokio::spawn(async move {
            if !delay.is_zero() {
                let jitter = std::time::Duration::from_millis(u64::from(uuid::Uuid::new_v4().as_bytes()[0]) * 2);
                tokio::time::sleep(delay + jitter).await;
            }
            let mut stream = match client.stream_chat(chat, after_id).await {
                Ok(s) => s,
                Err(e) => {
                    let _ = tx.send(Msg::StreamEnded { error: Some(e.to_string()) });
                    return;
                }
            };
            while let Some(item) = stream.next().await {
                match item {
                    Ok(ev) => {
                        let _ = tx.send(Msg::Stream(ev));
                    }
                    Err(coder_sdk::Error::Decode(_)) => continue,
                    Err(e) => {
                        let _ = tx.send(Msg::StreamEnded { error: Some(e.to_string()) });
                        return;
                    }
                }
            }
            let _ = tx.send(Msg::StreamEnded { error: None });
        }));
    }

    /// Runs one API effect in the background. Effects the UI owns are ignored here.
    pub fn run(&mut self, effect: Effect) {
        let client = self.client.clone();
        let tx = self.tx.clone();
        let spawn = |fut: std::pin::Pin<Box<dyn std::future::Future<Output = Msg> + Send>>| {
            let tx = tx.clone();
            tokio::spawn(async move {
                let _ = tx.send(fut.await);
            });
        };
        match effect {
            Effect::OpenStream { chat, after_id } => self.open_stream(chat, after_id, std::time::Duration::ZERO),
            Effect::ReconnectAfter { chat, after_id, delay } => self.open_stream(chat, after_id, delay),
            Effect::LoadChat(id) => spawn(Box::pin(async move {
                let chat = match client.api().get_chat_by_id(&id).await {
                    Ok(c) => c.into_inner(),
                    Err(e) => return Msg::ApiFailed { action: "load the chat", message: err(e).await },
                };
                let messages = match client.api().list_chat_messages(&id, None, None, Some(200)).await {
                    Ok(m) => m.into_inner().messages,
                    Err(e) => return Msg::ApiFailed { action: "load messages", message: err(e).await },
                };
                Msg::ChatLoaded { chat: Box::new(chat), messages }
            })),
            Effect::CreateChat { org, text, model, workspace } => spawn(Box::pin(async move {
                let body = types::CodersdkCreateChatRequest {
                    organization_id: Some(org),
                    content: vec![text_part(&text)],
                    model_config_id: model,
                    workspace_id: workspace,
                    ..Default::default()
                };
                match client.api().create_chat(&body).await {
                    Ok(c) => Msg::ChatCreated(Box::new(c.into_inner())),
                    Err(e) => Msg::ApiFailed { action: "create the chat", message: err(e).await },
                }
            })),
            Effect::SendMessage { chat, text, model, busy } => spawn(Box::pin(async move {
                let body = types::CodersdkCreateChatMessageRequest {
                    content: vec![text_part(&text)],
                    model_config_id: model,
                    busy_behavior: Some(types::CodersdkChatBusyBehavior(busy.as_str().into())),
                    ..Default::default()
                };
                match client.api().send_chat_message(&chat, &body).await {
                    Ok(_) => Msg::Refresh,
                    Err(e) => Msg::ApiFailed { action: "send the message", message: err(e).await },
                }
            })),
            Effect::Interrupt(chat) => spawn(Box::pin(async move {
                match client.api().interrupt_chat(&chat).await {
                    Ok(_) => Msg::Refresh,
                    Err(e) => Msg::ApiFailed { action: "interrupt", message: err(e).await },
                }
            })),
            Effect::Compact(chat) => spawn(Box::pin(async move {
                match client.api().compact_chat(&chat).await {
                    Ok(_) => Msg::Refresh,
                    Err(e) => Msg::ApiFailed { action: "compact the chat", message: err(e).await },
                }
            })),
            Effect::Clear(chat) => spawn(Box::pin(async move {
                match client.api().clear_chat_context(&chat).await {
                    Ok(_) => Msg::Refresh,
                    Err(e) => Msg::ApiFailed { action: "clear the context", message: err(e).await },
                }
            })),
            Effect::SetWorkspace { chat, workspace } => spawn(Box::pin(async move {
                let body = types::CodersdkUpdateChatRequest { workspace_id: Some(workspace.unwrap_or(Uuid::nil())), ..Default::default() };
                match client.api().update_chat(&chat, &body).await {
                    Ok(_) => Msg::Refresh,
                    Err(e) => Msg::ApiFailed { action: "change the workspace", message: err(e).await },
                }
            })),
            Effect::FetchPrefs => spawn(Box::pin(async move {
                match client.api().get_user_preference_settings("me").await {
                    Ok(p) => Msg::PrefsLoaded(DisplayPrefs::from(&p.into_inner())),
                    Err(e) => Msg::ApiFailed { action: "load display preferences", message: err(e).await },
                }
            })),
            Effect::FetchModels(org) => spawn(Box::pin(async move {
                match client.api().list_ai_models_and_provider_descriptors_in_an_organization(&org.to_string()).await {
                    Ok(r) => Msg::ModelsLoaded(r.into_inner().models),
                    Err(e) => Msg::ApiFailed { action: "load models", message: err(e).await },
                }
            })),
            Effect::FetchWorkspaces => spawn(Box::pin(async move {
                match client.api().list_workspaces(Some(100), None, Some("owner:me")).await {
                    Ok(r) => Msg::WorkspacesLoaded(
                        r.into_inner().workspaces.into_iter().filter_map(|w| Some(WorkspaceRef { id: w.id?, name: w.name? })).collect(),
                    ),
                    Err(e) => Msg::ApiFailed { action: "load workspaces", message: err(e).await },
                }
            })),
            _ => {}
        }
    }
}
```

Add `progenitor-client = "0.15"` and `serde = { workspace = true }` to `crates/scuttle-tui/Cargo.toml` for the `err` helper. If the generated request structs do not implement `Default`, build them with every field set explicitly and report it.

- [ ] **Step 5: Implement the app**

Above the tests in `crates/scuttle-tui/src/app.rs`:

```rust
//! The TUI: owns UI state, turns terminal events into core messages, and draws.

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use scuttle_core::app::{App, CopyTarget, Effect, Msg, Notice};
use scuttle_core::commands::COMMANDS;
use scuttle_core::config::{self, LocalConfig};

use crate::clipboard::{Clipboard, CopyOutcome};
use crate::composer::{Composer, ComposerAction};
use crate::footer::footer_line;
use crate::picker::{PickerChoice, PickerState};
use crate::theme::Theme;
use crate::transcript_view::{self, BlockId, HitTarget, View, Welcome};

pub struct Tui {
    pub core: App,
    pub composer: Composer,
    pub picker: Option<PickerState>,
    pub last_copied: Option<String>,
    config: LocalConfig,
    config_path: Option<PathBuf>,
    theme: Theme,
    welcome: Welcome,
    toggles: HashSet<BlockId>,
    view: View,
    area: Rect,
    scroll_from_bottom: usize,
    last_ctrl_c: Option<Instant>,
    show_help: bool,
    clipboard: Option<Clipboard>,
}

impl Tui {
    pub fn new(config: LocalConfig, config_path: Option<PathBuf>, theme: Theme, welcome: Welcome) -> Tui {
        Tui {
            core: App::new(config.busy_behavior, config.mouse),
            composer: Composer::new(config.composer_max_lines),
            picker: None,
            last_copied: None,
            config,
            config_path,
            theme,
            welcome,
            toggles: HashSet::new(),
            view: View::default(),
            area: Rect::default(),
            scroll_from_bottom: 0,
            last_ctrl_c: None,
            show_help: false,
            clipboard: None,
        }
    }

    fn notice(&mut self, n: Notice) {
        self.core.notices.push(n);
    }

    fn copy(&mut self, text: String) {
        self.last_copied = Some(text.clone());
        if cfg!(test) {
            return;
        }
        let outcome = self.clipboard.get_or_insert_with(Clipboard::new).copy(&text);
        match outcome {
            CopyOutcome::Copied => self.notice(Notice::Info("Copied".into())),
            CopyOutcome::Failed(why) => self.notice(Notice::Error(format!("Copy failed: {why}"))),
        }
    }

    /// Handles effects the UI owns. Returns false for effects the runtime should run.
    pub fn apply_ui_effect(&mut self, effect: &Effect) -> bool {
        match effect {
            Effect::ShowPicker(kind) => self.picker = Some(PickerState::open(*kind, &self.core)),
            Effect::ShowHelp => self.show_help = true,
            Effect::Copy(CopyTarget::LastMessage) => {
                let text = self.core.transcript.messages().rev().find(|m| m.role.as_ref().map(|r| r.as_str()) == Some("assistant")).map(|m| {
                    m.content.iter().filter(|p| p.type_.as_ref().map(|t| t.as_str()) == Some("text")).filter_map(|p| p.text.clone()).collect::<Vec<_>>().join("\n\n")
                });
                match text {
                    Some(t) => self.copy(t),
                    None => self.notice(Notice::Error("Nothing to copy yet.".into())),
                }
            }
            Effect::Copy(CopyTarget::CodeBlock(n)) => match n.checked_sub(1).and_then(|i| self.view.last_code_blocks.get(i)).cloned() {
                Some(code) => self.copy(code),
                None => self.notice(Notice::Error(format!("The last message has no code block {n}."))),
            },
            Effect::SetMouse(enabled) => {
                if !cfg!(test) {
                    let _ = crate::terminal::set_mouse(*enabled);
                }
                if let Some(path) = self.config_path.as_ref() {
                    if let Err(e) = config::set_mouse(path, *enabled) {
                        self.notice(Notice::Error(e.to_string()));
                    }
                }
                let note = if *enabled {
                    "Mouse capture on. Hold Shift (Option in iTerm2 or Terminal.app) to select text."
                } else {
                    "Mouse capture off."
                };
                self.notice(Notice::Info(note.into()));
            }
            _ => return false,
        }
        true
    }

    /// Replaces the composer text with what the editor saved, or leaves it untouched on failure.
    pub fn edit_with(&mut self, run: impl FnOnce(&std::path::Path) -> std::io::Result<()>) -> std::io::Result<()> {
        let path = std::env::temp_dir().join(format!("scuttle-{}.md", uuid::Uuid::new_v4()));
        std::fs::write(&path, self.composer.text())?;
        let result = run(&path).and_then(|_| std::fs::read_to_string(&path));
        let _ = std::fs::remove_file(&path);
        let text = result?;
        self.composer.set_text(text.trim_end_matches('\n'));
        Ok(())
    }

    /// The first screen row showing a click target that matches `pred`, for tests.
    pub fn row_of(&self, pred: impl Fn(&HitTarget) -> bool) -> Option<u16> {
        let top = self.top_line();
        let hit = self.view.hits.iter().find(|h| pred(&h.target))?;
        let line = hit.lines.start.max(top);
        (line < top + self.area.height as usize).then(|| self.area.y + (line - top) as u16)
    }

    fn top_line(&self) -> usize {
        let total = self.view.lines.len();
        let height = self.area.height as usize;
        total.saturating_sub(height).saturating_sub(self.scroll_from_bottom)
    }

    fn click(&mut self, column: u16, row: u16) {
        let inside = row >= self.area.y && row < self.area.y + self.area.height && column >= self.area.x && column < self.area.x + self.area.width;
        if !inside {
            return;
        }
        let line = self.top_line() + (row - self.area.y) as usize;
        let target = self.view.hits.iter().rev().find(|h| h.lines.contains(&line)).map(|h| h.target.clone());
        match target {
            Some(HitTarget::CopyCode(code)) => self.copy(code),
            Some(HitTarget::Toggle(id)) => {
                if !self.toggles.remove(&id) {
                    self.toggles.insert(id);
                }
            }
            None => {}
        }
    }

    pub fn handle(&mut self, event: Event) -> Vec<Effect> {
        match event {
            Event::Key(key) if key.kind != KeyEventKind::Release => self.key(key),
            Event::Paste(text) => {
                self.composer.paste(&text);
                vec![]
            }
            Event::Mouse(m) => {
                match m.kind {
                    MouseEventKind::Down(MouseButton::Left) => self.click(m.column, m.row),
                    MouseEventKind::ScrollUp => self.scroll_from_bottom += 3,
                    MouseEventKind::ScrollDown => self.scroll_from_bottom = self.scroll_from_bottom.saturating_sub(3),
                    _ => {}
                }
                vec![]
            }
            _ => vec![],
        }
    }

    fn key(&mut self, key: KeyEvent) -> Vec<Effect> {
        if self.show_help {
            self.show_help = false;
            return vec![];
        }
        if let Some(picker) = self.picker.as_mut() {
            let choice = picker.handle_key(key);
            return match choice {
                Some(PickerChoice::Cancel) => {
                    self.picker = None;
                    vec![]
                }
                Some(PickerChoice::Model(id)) => {
                    self.picker = None;
                    self.core.update(Msg::ModelChosen(id))
                }
                Some(PickerChoice::Workspace(ws)) => {
                    self.picker = None;
                    self.core.update(Msg::WorkspaceChosen(ws))
                }
                None => vec![],
            };
        }
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            let now = Instant::now();
            if self.last_ctrl_c.is_some_and(|t| now.duration_since(t) < Duration::from_secs(2)) {
                return vec![Effect::Quit];
            }
            self.last_ctrl_c = Some(now);
            self.notice(Notice::Info("Press Ctrl+C again to quit.".into()));
            return vec![];
        }
        match key.code {
            KeyCode::PageUp => {
                self.scroll_from_bottom += self.area.height.max(1) as usize;
                return vec![];
            }
            KeyCode::PageDown => {
                self.scroll_from_bottom = self.scroll_from_bottom.saturating_sub(self.area.height.max(1) as usize);
                return vec![];
            }
            KeyCode::End => {
                self.scroll_from_bottom = 0;
                return vec![];
            }
            _ => {}
        }
        match self.composer.handle_key(key, self.core.prefs.send_shortcut) {
            ComposerAction::Submit(text) => {
                self.scroll_from_bottom = 0;
                self.core.update(Msg::Submit(text))
            }
            ComposerAction::Interrupt => self.core.update(Msg::Interrupt),
            ComposerAction::OpenEditor => self.open_editor(),
            ComposerAction::None => vec![],
        }
    }

    fn open_editor(&mut self) -> Vec<Effect> {
        let editor = std::env::var("EDITOR").or_else(|_| std::env::var("VISUAL")).unwrap_or_else(|_| "vi".into());
        let mouse = self.core.mouse;
        let result = self.edit_with(|path| {
            crate::terminal::leave()?;
            let status = std::process::Command::new("sh").arg("-c").arg(format!("{editor} \"$1\"")).arg("sh").arg(path).status();
            let restored = crate::terminal::enter(mouse).map(|_| ());
            match status {
                Ok(s) if s.success() => restored,
                Ok(s) => Err(std::io::Error::other(format!("editor exited with {s}"))),
                Err(e) => Err(e),
            }
        });
        if let Err(e) = result {
            self.notice(Notice::Error(format!("Editor failed: {e}")));
        }
        vec![]
    }

    pub fn draw(&mut self, f: &mut Frame) {
        let composer_height = self.composer.height().min(f.area().height.saturating_sub(2).max(3));
        let [transcript, composer, footer] = Layout::vertical([Constraint::Min(1), Constraint::Length(composer_height), Constraint::Length(1)]).areas(f.area());
        self.area = transcript;
        self.view = transcript_view::build(&self.core, &self.config.density, &self.toggles, &self.welcome, &self.theme, transcript.width);
        let top = self.top_line();
        let visible: Vec<Line> = self.view.lines.iter().skip(top).take(transcript.height as usize).cloned().collect();
        f.render_widget(Paragraph::new(visible), transcript);
        f.render_widget(Block::default().borders(Borders::TOP), composer);
        let inner = Rect { y: composer.y + 1, height: composer.height.saturating_sub(1), ..composer };
        f.render_widget(self.composer.widget(), inner);
        f.render_widget(Paragraph::new(footer_line(&self.core, &self.theme, footer.width)), footer);
        let matches = self.composer.slash_matches();
        if !matches.is_empty() {
            let h = (matches.len() as u16 + 2).min(transcript.height);
            let area = Rect { y: transcript.y + transcript.height - h, height: h, ..transcript };
            let lines: Vec<Line> = matches.iter().map(|c| Line::from(vec![Span::styled(c.usage, self.theme.accent), Span::raw("  "), Span::styled(c.description, self.theme.dim)])).collect();
            f.render_widget(Clear, area);
            f.render_widget(Paragraph::new(lines).block(Block::default().borders(Borders::ALL)), area);
        }
        if let Some(picker) = self.picker.as_ref() {
            let h = 10.min(transcript.height);
            picker.render(f, Rect { y: transcript.y + transcript.height - h, height: h, ..transcript }, &self.theme);
        }
        if self.show_help {
            let mut lines: Vec<Line> = COMMANDS.iter().map(|c| Line::from(vec![Span::styled(c.usage, self.theme.accent), Span::raw("  "), Span::raw(c.description)])).collect();
            lines.push(Line::default());
            lines.push(Line::from("Esc interrupts · Ctrl+G opens $EDITOR · Ctrl+C twice quits · PageUp/PageDown/End scroll · click tool calls to expand"));
            let h = (lines.len() as u16 + 2).min(transcript.height);
            let area = Rect { y: transcript.y, height: h, ..transcript };
            f.render_widget(Clear, area);
            f.render_widget(Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(" Help ")), area);
        }
    }
}
```

- [ ] **Step 6: Implement startup**

Replace `crates/scuttle-tui/src/main.rs`:

```rust
mod app;
mod clipboard;
mod composer;
mod footer;
mod highlight;
mod markdown;
mod picker;
mod runtime;
mod terminal;
mod theme;
mod transcript_view;
mod wrap;

use std::process::ExitCode;

use futures::StreamExt;
use scuttle_core::app::{Effect, Msg};
use scuttle_core::config;

fn detect_dark() -> bool {
    if std::env::var_os("SCUTTLE_NO_TERMINAL_QUERY").is_some() {
        return true;
    }
    let mut options = terminal_colorsaurus::QueryOptions::default();
    options.timeout = std::time::Duration::from_millis(150);
    !matches!(terminal_colorsaurus::theme_mode(options), Ok(terminal_colorsaurus::ThemeMode::Light))
}

#[tokio::main]
async fn main() -> ExitCode {
    let open_chat = match std::env::args().nth(1).map(|a| a.parse::<uuid::Uuid>()) {
        None => None,
        Some(Ok(id)) => Some(id),
        Some(Err(_)) => {
            eprintln!("usage: scuttle [chat-id]");
            return ExitCode::from(2);
        }
    };
    let session = match coder_sdk::discover_session() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let client = match coder_sdk::Client::new(&session) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let config_path = config::config_path(&|k| std::env::var(k).ok().filter(|v| !v.is_empty()));
    let local = match config_path.as_deref().map(config::load).transpose() {
        Ok(c) => c.unwrap_or_default(),
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let art = local.welcome.art_file.as_ref().and_then(|p| std::fs::read_to_string(p).ok()).map(|t| t.lines().map(str::to_owned).collect()).unwrap_or_default();
    let welcome = transcript_view::Welcome { url: session.url.to_string(), user: String::new(), art, show: local.welcome.show };
    highlight::warm();
    let dark = detect_dark();

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let mut runtime = runtime::Runtime::new(client.clone(), tx);
    let mut tui = app::Tui::new(local.clone(), config_path, theme::Theme::terminal(dark), welcome);

    if let Ok(version) = client.server_version().await {
        if let Some(w) = scuttle_core::skew::skew_warning(&version, coder_sdk::GENERATED_FROM) {
            tui.core.notices.push(scuttle_core::app::Notice::Info(w));
        }
    }
    let first = match runtime.organization().await {
        Ok(org) => tui.core.update(Msg::Started { org_id: org, open_chat }),
        Err(e) => {
            tui.core.notices.push(scuttle_core::app::Notice::Error(format!("Could not load your organization: {e}")));
            vec![]
        }
    };

    let mut term = match terminal::enter(local.mouse) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("could not set up the terminal: {e}");
            return ExitCode::FAILURE;
        }
    };
    let mut events = crossterm::event::EventStream::new();
    let mut pending = first;
    let code = loop {
        for effect in std::mem::take(&mut pending) {
            if effect == Effect::Quit {
                let _ = terminal::leave();
                return ExitCode::SUCCESS;
            }
            if !tui.apply_ui_effect(&effect) {
                runtime.run(effect);
            }
        }
        if term.draw(|f| tui.draw(f)).is_err() {
            break ExitCode::FAILURE;
        }
        tokio::select! {
            Some(Ok(ev)) = events.next() => pending = tui.handle(ev),
            Some(msg) = rx.recv() => pending = tui.core.update(msg),
            else => break ExitCode::SUCCESS,
        }
    };
    let _ = terminal::leave();
    code
}
```

If `terminal_colorsaurus` 1.0 exposes the timeout or theme mode differently, adapt and report.

- [ ] **Step 7: Run all tests and lint**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check`
Expected: all tests pass, including the six `app::tests`; clippy and fmt are clean.

- [ ] **Step 8: Try it for real**

Run: `cargo run -p scuttle-tui` in a real terminal (the author's session, from a deployment that tracks main, is discovered automatically).
Expected: the welcome screen appears, typing a message and pressing Enter creates a chat and streams a reply, Esc interrupts, clicking a tool call expands it, `/copy` copies, `/model` opens the picker, and Ctrl+C twice exits with the terminal restored. Record what happened in the report, including anything that looked wrong.

- [ ] **Step 9: Commit**

```bash
git add -A && git commit -m "feat(scuttle-tui): wire the terminal, runtime, and app into the scuttle binary

Assisted-by: AI"
```

---

### Task 15: End-to-end tests in a pseudo terminal

**Files:**
- Create: `crates/scuttle-tui/tests/pty.rs`

**Interfaces:**
- Consumes: the `scuttle` binary (`env!("CARGO_BIN_EXE_scuttle")`).
- Produces: PTY tests `missing_session_says_run_coder_login`, `welcome_screen_then_quit`, and `exit_restores_terminal_modes`.

- [ ] **Step 1: Write the tests**

`crates/scuttle-tui/tests/pty.rs`:

```rust
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use wiremock::matchers::{method, path, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

struct Session {
    output: Arc<Mutex<Vec<u8>>>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
}

fn spawn(envs: &[(&str, String)]) -> Session {
    let pair = native_pty_system().openpty(PtySize { rows: 24, cols: 80, pixel_width: 0, pixel_height: 0 }).unwrap();
    let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_scuttle"));
    let home = std::env::temp_dir().join(format!("scuttle-pty-{}", std::process::id()));
    std::fs::create_dir_all(&home).unwrap();
    cmd.env("HOME", &home);
    cmd.env("XDG_CONFIG_HOME", home.join("config"));
    cmd.env("CODER_CONFIG_DIR", home.join("coder"));
    cmd.env("SCUTTLE_NO_TERMINAL_QUERY", "1");
    cmd.env("TERM", "xterm-256color");
    cmd.env_remove("CODER_URL");
    cmd.env_remove("CODER_SESSION_TOKEN");
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let child = pair.slave.spawn_command(cmd).unwrap();
    let mut reader = pair.master.try_clone_reader().unwrap();
    let writer = pair.master.take_writer().unwrap();
    let output = Arc::new(Mutex::new(Vec::new()));
    let sink = output.clone();
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 {
                break;
            }
            sink.lock().unwrap().extend_from_slice(&buf[..n]);
        }
    });
    Session { output, writer, child }
}

impl Session {
    fn screen(&self) -> String {
        let mut parser = vt100::Parser::new(24, 80, 0);
        parser.process(&self.output.lock().unwrap());
        parser.screen().contents()
    }

    fn raw(&self) -> Vec<u8> {
        self.output.lock().unwrap().clone()
    }

    fn wait_for(&self, needle: &str) {
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            if self.screen().contains(needle) || String::from_utf8_lossy(&self.raw()).contains(needle) {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("timed out waiting for {needle:?}; screen:\n{}", self.screen());
    }

    fn exit_code(&mut self) -> u32 {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status.exit_code();
            }
            assert!(Instant::now() < deadline, "scuttle did not exit");
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

async fn fake_coder() -> MockServer {
    let server = MockServer::start().await;
    let org = uuid::Uuid::new_v4();
    Mock::given(method("GET")).and(path("/api/v2/buildinfo")).respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"version": "v2.37.3"}))).mount(&server).await;
    Mock::given(method("GET"))
        .and(path("/api/v2/users/me/organizations"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([{
            "id": org, "name": "coder", "display_name": "Coder", "description": "", "icon": "",
            "is_default": true, "created_at": "2026-01-01T00:00:00Z", "updated_at": "2026-01-01T00:00:00Z"
        }])))
        .mount(&server)
        .await;
    Mock::given(method("GET")).and(path("/api/v2/users/me/preferences")).respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({}))).mount(&server).await;
    Mock::given(method("GET")).and(path_regex(r"^/api/v2/organizations/.+/chats/models$")).respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"models": [], "providers": [], "unsupported_providers": []}))).mount(&server).await;
    Mock::given(method("GET")).and(path("/api/v2/workspaces")).respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"workspaces": [], "count": 0}))).mount(&server).await;
    server
}

#[test]
fn missing_session_says_run_coder_login() {
    let mut s = spawn(&[]);
    s.wait_for("coder login");
    assert_ne!(s.exit_code(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn welcome_screen_then_quit() {
    let server = fake_coder().await;
    let mut s = spawn(&[("CODER_URL", server.uri()), ("CODER_SESSION_TOKEN", "test-token-not-real".into())]);
    s.wait_for("scuttle");
    s.wait_for("/help");
    s.writer.write_all(b"\x03").unwrap();
    s.wait_for("Ctrl+C again");
    s.writer.write_all(b"\x03").unwrap();
    assert_eq!(s.exit_code(), 0);
    assert!(!s.screen().contains("test-token-not-real"));
}

#[tokio::test(flavor = "multi_thread")]
async fn exit_restores_terminal_modes() {
    let server = fake_coder().await;
    let mut s = spawn(&[("CODER_URL", server.uri()), ("CODER_SESSION_TOKEN", "test-token-not-real".into())]);
    s.wait_for("scuttle");
    s.writer.write_all(b"/quit\r").unwrap();
    assert_eq!(s.exit_code(), 0);
    let raw = String::from_utf8_lossy(&s.raw()).to_string();
    let tail = &raw[raw.rfind("\x1b[?1049h").unwrap_or(0)..];
    assert!(tail.contains("\x1b[?1049l"), "alternate screen left");
    assert!(tail.contains("\x1b[?1000l") || tail.contains("\x1b[?1006l"), "mouse capture disabled");
    assert!(tail.contains("\x1b[?2004l"), "bracketed paste disabled");
}
```

Add `serde_json` and `uuid` to `[dev-dependencies]` in `crates/scuttle-tui/Cargo.toml` if they are not already available to integration tests.

- [ ] **Step 2: Run the tests**

Run: `cargo test -p scuttle-tui --test pty -- --test-threads=1`
Expected: all three pass. If the organization JSON fails to decode because the generated `CodersdkOrganization` requires other fields, add them with neutral values and report which.

- [ ] **Step 3: Run the whole suite and lint**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check`
Expected: everything passes.

- [ ] **Step 4: Commit**

```bash
git add -A && git commit -m "test(scuttle-tui): add end-to-end tests in a pseudo terminal

Assisted-by: AI"
```

---

## Handoff notes for the author

- Both branches stay local: `m1-sdk` in the SDK repo (stacked on `m0-sdk`) and `m1` in the scuttle repo. Merging `m0-sdk` first keeps `m1-sdk`'s history simple.
- Task 14 Step 8 is the first real daily-use check; its report is the most important artifact of M1.
- The Codernaut stays a user-supplied art file until the brand question is answered.
