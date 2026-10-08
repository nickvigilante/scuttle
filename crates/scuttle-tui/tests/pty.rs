//! End-to-end tests that drive the real `scuttle` binary in a pseudo terminal against a fake
//! Coder server. Each test gets its own `HOME` (and config/session directories under it) so that
//! config a test writes never leaks into another test, and no test can read the developer's real
//! `~/.config/coderv2` session or contact a real Coder deployment.

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
    /// Kept alive so the pty's master side stays open for the life of the session.
    _master: Box<dyn portable_pty::MasterPty + Send>,
    /// The output-reading thread, joined (with a bound) on drop so it never outlives the test.
    reader: Option<std::thread::JoinHandle<()>>,
    home: std::path::PathBuf,
}

impl Drop for Session {
    fn drop(&mut self) {
        // Kill and reap the child first: closing the slave side makes the reader thread's
        // next read return EOF, so it always exits instead of blocking on the pty forever.
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            // `JoinHandle::join` has no timeout, so bound the wait from the outside: a helper
            // thread does the (now-fast) join and reports back over a channel. If the reader
            // were somehow still stuck, `recv_timeout` gives up without hanging test cleanup;
            // the helper thread finishes on its own once the reader eventually does.
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let _ = reader.join();
                let _ = tx.send(());
            });
            let _ = rx.recv_timeout(Duration::from_secs(5));
        }
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

/// Spawns `scuttle` in a fresh pty with its own `HOME`, named after the pid and `name` so
/// concurrent test binaries (and repeat runs) never share, or leak into, each other's config.
fn spawn(name: &str, envs: &[(&str, String)]) -> Session {
    spawn_with_args(name, &[], envs)
}

/// `spawn` with command-line arguments, such as a chat ID to open.
fn spawn_with_args(name: &str, args: &[String], envs: &[(&str, String)]) -> Session {
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
    let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_scuttle"));
    cmd.args(args);
    let home = std::env::temp_dir().join(format!("scuttle-pty-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    cmd.env("HOME", &home);
    cmd.env("XDG_CONFIG_HOME", home.join("config"));
    cmd.env("XDG_STATE_HOME", home.join("state"));
    cmd.env("CODER_CONFIG_DIR", home.join("coder"));
    cmd.env("SCUTTLE_NO_TERMINAL_QUERY", "1");
    cmd.env("SCUTTLE_NO_BROWSER", "1");
    cmd.env("TERM", "xterm-256color");
    cmd.env_remove("CODER_URL");
    cmd.env_remove("CODER_SESSION_TOKEN");
    // The icons follow NERD_FONT, so the screens these tests wait for are drawn in text.
    cmd.env_remove("NERD_FONT");
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let child = pair.slave.spawn_command(cmd).unwrap();
    let mut reader = pair.master.try_clone_reader().unwrap();
    let writer = pair.master.take_writer().unwrap();
    let output = Arc::new(Mutex::new(Vec::new()));
    let sink = output.clone();
    let reader_handle = std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 {
                break;
            }
            sink.lock().unwrap().extend_from_slice(&buf[..n]);
        }
    });
    Session {
        output,
        writer,
        child,
        _master: pair.master,
        reader: Some(reader_handle),
        home,
    }
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
            if self.screen().contains(needle)
                || String::from_utf8_lossy(&self.raw()).contains(needle)
            {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!(
            "timed out waiting for {needle:?}; screen:\n{}",
            self.screen()
        );
    }

    /// Waits until the raw output holds `needle` somewhere after the last `marker`.
    fn wait_for_after(&self, marker: &str, needle: &str) {
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            let raw = String::from_utf8_lossy(&self.raw()).into_owned();
            if raw
                .rfind(marker)
                .is_some_and(|at| raw[at..].contains(needle))
            {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("timed out waiting for {needle:?} after {marker:?}");
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

/// A fake Coder that serves buildinfo but rejects the session token everywhere else.
async fn fake_coder_rejecting_the_token() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/buildinfo"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({"version": "v2.37.3"})),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(401)
                .set_body_json(serde_json::json!({"message": "You must be logged in."})),
        )
        .mount(&server)
        .await;
    server
}

async fn fake_coder() -> MockServer {
    let server = MockServer::start().await;
    let org = uuid::Uuid::new_v4();
    Mock::given(method("GET"))
        .and(path("/api/v2/users/me"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": uuid::Uuid::new_v4(), "username": "nick", "email": "nick@example.com",
            "created_at": "2026-01-01T00:00:00Z", "organization_ids": [], "roles": []
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v2/buildinfo"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({"version": "v2.37.3"})),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v2/users/me/organizations"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([{
            "id": org, "name": "coder", "display_name": "Coder", "description": "", "icon": "",
            "is_default": true, "created_at": "2026-01-01T00:00:00Z", "updated_at": "2026-01-01T00:00:00Z"
        }])))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v2/users/me/preferences"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path_regex(r"^/api/v2/organizations/.+/chats/models$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            serde_json::json!({"models": [], "providers": [], "unsupported_providers": []}),
        ))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v2/workspaces"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"workspaces": [], "count": 0})),
        )
        .mount(&server)
        .await;
    server
}

#[test]
fn missing_session_says_run_coder_login() {
    let mut s = spawn("missing-session", &[]);
    s.wait_for("coder login");
    assert_ne!(s.exit_code(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn welcome_screen_then_quit() {
    let server = fake_coder().await;
    let mut s = spawn(
        "welcome-then-quit",
        &[
            ("CODER_URL", server.uri()),
            ("CODER_SESSION_TOKEN", "test-token-not-real".into()),
        ],
    );
    s.wait_for("scuttle");
    s.wait_for("/help");
    s.wait_for("Signed in as nick");
    // Snapshot the live, alternate-screen UI before quitting: after exit the terminal has
    // switched back to the primary grid, so checking `screen()` then would be vacuous.
    let live_screen = s.screen();
    assert!(!live_screen.contains("test-token-not-real"));
    s.writer.write_all(b"\x03").unwrap();
    s.wait_for("Ctrl+C again");
    s.writer.write_all(b"\x03").unwrap();
    assert_eq!(s.exit_code(), 0);
    assert!(!s.screen().contains("test-token-not-real"));
    assert!(!String::from_utf8_lossy(&s.raw()).contains("test-token-not-real"));
}

#[tokio::test(flavor = "multi_thread")]
async fn exit_restores_terminal_modes() {
    let server = fake_coder().await;
    let mut s = spawn(
        "exit-restores-modes",
        &[
            ("CODER_URL", server.uri()),
            ("CODER_SESSION_TOKEN", "test-token-not-real".into()),
        ],
    );
    s.wait_for("scuttle");
    s.writer.write_all(b"/quit\r").unwrap();
    assert_eq!(s.exit_code(), 0);
    let raw = String::from_utf8_lossy(&s.raw()).to_string();
    assert!(!raw.contains("test-token-not-real"));
    let enter_at = raw
        .rfind("\x1b[?1049h")
        .expect("scuttle must enter the alternate screen before exiting");
    let tail = &raw[enter_at..];
    assert!(tail.contains("\x1b[?1049l"), "alternate screen left");
    assert!(
        tail.contains("\x1b[?1000l") || tail.contains("\x1b[?1006l"),
        "mouse capture disabled"
    );
    assert!(tail.contains("\x1b[?2004l"), "bracketed paste disabled");
    assert!(raw.contains("\x1b[?1004h"), "focus reporting enabled");
    assert!(tail.contains("\x1b[?1004l"), "focus reporting disabled");
    assert!(
        tail.contains("\x1b[?1007h"),
        "alternate scroll mode restored"
    );
}

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
    // Turning capture back on gives the terminal its alternate scroll mode again.
    s.writer.write_all(b"/mouse\r").unwrap();
    s.wait_for_after("\x1b[?1007l", "\x1b[?1007h");
    s.writer.write_all(b"/quit\r").unwrap();
    assert_eq!(s.exit_code(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn web_without_a_chat_asks_to_start_one() {
    let server = fake_coder().await;
    let mut s = spawn(
        "web-without-a-chat",
        &[
            ("CODER_URL", server.uri()),
            ("CODER_SESSION_TOKEN", "test-token-not-real".into()),
        ],
    );
    s.wait_for("scuttle");
    s.writer.write_all(b"/web\r").unwrap();
    s.wait_for("Start a chat first.");
    s.writer.write_all(b"/quit\r").unwrap();
    assert_eq!(s.exit_code(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_rejected_token_exits_before_the_full_screen_ui() {
    let server = fake_coder_rejecting_the_token().await;
    let mut s = spawn(
        "rejected-token",
        &[
            ("CODER_URL", server.uri()),
            ("CODER_SESSION_TOKEN", "test-token-not-real".into()),
        ],
    );
    s.wait_for("the session token was rejected");
    assert_eq!(s.exit_code(), 1);
    let raw = String::from_utf8_lossy(&s.raw()).to_string();
    assert!(
        raw.contains(&format!("coder login {}", server.uri())),
        "{raw}"
    );
    assert!(
        !raw.contains("\x1b[?1049h"),
        "the alternate screen was entered"
    );
    assert!(!raw.contains("test-token-not-real"));
}

/// Waits until the file at `path` holds something, and returns it.
fn wait_for_file(path: &std::path::Path) -> String {
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if let Ok(text) = std::fs::read_to_string(path)
            && !text.is_empty()
        {
            return text;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("timed out waiting for {}", path.display());
}

#[tokio::test(flavor = "multi_thread")]
async fn the_pager_gets_the_keys_typed_while_it_runs() {
    let server = fake_coder().await;
    let chat = uuid::Uuid::new_v4();
    Mock::given(method("GET"))
        .and(path(format!("/api/v2/chats/{chat}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": chat, "title": "pager chat", "children": [], "files": [],
            "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/api/v2/chats/{chat}/messages")))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/api/v2/chats/{chat}/diff")))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({"diff": "+paged line\n"})),
        )
        .mount(&server)
        .await;
    // The pager keeps its files outside `HOME`, which `spawn` recreates. It reads one key
    // straight from the terminal, as `less` does, and writes down what it got.
    let out = std::env::temp_dir().join(format!("scuttle-pty-pager-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&out);
    std::fs::create_dir_all(&out).unwrap();
    let script = out.join("pager.sh");
    std::fs::write(
        &script,
        "cat > \"$PAGER_OUT/paged\"\n\
         stty -icanon -echo min 1 < /dev/tty\n\
         printf 'PAGER-READY'\n\
         dd bs=1 count=1 < /dev/tty > \"$PAGER_OUT/key\" 2>/dev/null\n",
    )
    .unwrap();
    let mut s = spawn_with_args(
        "pager-keys",
        &[chat.to_string()],
        &[
            ("CODER_URL", server.uri()),
            ("CODER_SESSION_TOKEN", "test-token-not-real".into()),
            ("GIT_PAGER", format!("sh '{}'", script.display())),
            ("PAGER_OUT", out.display().to_string()),
        ],
    );
    // The fake serves no stream, so a reconnect notice means the chat is open.
    s.wait_for("reconnecting");
    s.writer.write_all(b"/diff\r").unwrap();
    s.wait_for("PAGER-READY");
    s.writer.write_all(b"z").unwrap();
    assert_eq!(
        wait_for_file(&out.join("key")),
        "z",
        "the pager got the key"
    );
    assert_eq!(wait_for_file(&out.join("paged")), "+paged line\n");
    // The repaint after the handoff asks where the cursor is, which a real terminal answers.
    s.wait_for_after("PAGER-READY", "\x1b[6n");
    s.writer.write_all(b"\x1b[1;1R").unwrap();
    // A key scuttle had taken would sit in the composer, and turn `/quit` into a message.
    s.writer.write_all(b"/quit\r").unwrap();
    assert_eq!(s.exit_code(), 0);
    let _ = std::fs::remove_dir_all(&out);
}

/// A fake Coder serving one chat whose diff is one line, for the tests that page it.
async fn fake_coder_with_a_diff(chat: uuid::Uuid) -> MockServer {
    let server = fake_coder().await;
    Mock::given(method("GET"))
        .and(path(format!("/api/v2/chats/{chat}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": chat, "title": "pager chat", "children": [], "files": [],
            "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/api/v2/chats/{chat}/messages")))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/api/v2/chats/{chat}/diff")))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({"diff": "+paged line\n"})),
        )
        .mount(&server)
        .await;
    server
}

#[tokio::test(flavor = "multi_thread")]
async fn ctrl_c_in_the_pager_leaves_scuttle_running() {
    let chat = uuid::Uuid::new_v4();
    let server = fake_coder_with_a_diff(chat).await;
    // Like `less`, the pager catches SIGINT itself. The terminal is out of raw mode while it
    // runs, so Ctrl+C signals scuttle too.
    let out = std::env::temp_dir().join(format!("scuttle-pty-sigint-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&out);
    std::fs::create_dir_all(&out).unwrap();
    let script = out.join("pager.sh");
    std::fs::write(
        &script,
        "cat > /dev/null\n\
         trap 'printf caught > \"$PAGER_OUT/int\"; exit 0' INT\n\
         printf 'PAGER-READY'\n\
         while :; do sleep 1; done\n",
    )
    .unwrap();
    let mut s = spawn_with_args(
        "pager-sigint",
        &[chat.to_string()],
        &[
            ("CODER_URL", server.uri()),
            ("CODER_SESSION_TOKEN", "test-token-not-real".into()),
            ("GIT_PAGER", format!("sh '{}'", script.display())),
            ("PAGER_OUT", out.display().to_string()),
        ],
    );
    s.wait_for("reconnecting");
    s.writer.write_all(b"/diff\r").unwrap();
    s.wait_for("PAGER-READY");
    s.writer.write_all(b"\x03").unwrap();
    assert_eq!(
        wait_for_file(&out.join("int")),
        "caught",
        "the pager got the signal"
    );
    // scuttle survived: it takes the terminal back and repaints, then quits normally.
    s.wait_for_after("PAGER-READY", "\x1b[6n");
    s.writer.write_all(b"\x1b[1;1R").unwrap();
    s.writer.write_all(b"/quit\r").unwrap();
    assert_eq!(s.exit_code(), 0);
    let _ = std::fs::remove_dir_all(&out);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_pager_runs_without_the_session_variables() {
    let chat = uuid::Uuid::new_v4();
    let server = fake_coder_with_a_diff(chat).await;
    let out = std::env::temp_dir().join(format!("scuttle-pty-pager-env-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&out);
    std::fs::create_dir_all(&out).unwrap();
    let script = out.join("pager.sh");
    std::fs::write(
        &script,
        "cat > /dev/null\n\
         env > \"$PAGER_OUT/env.tmp\"\n\
         mv \"$PAGER_OUT/env.tmp\" \"$PAGER_OUT/env\"\n",
    )
    .unwrap();
    let mut s = spawn_with_args(
        "pager-env",
        &[chat.to_string()],
        &[
            ("CODER_URL", server.uri()),
            ("CODER_SESSION_TOKEN", "test-token-not-real".into()),
            ("GIT_PAGER", format!("sh '{}'", script.display())),
            ("PAGER_OUT", out.display().to_string()),
        ],
    );
    s.wait_for("reconnecting");
    s.writer.write_all(b"/diff\r").unwrap();
    let env = wait_for_file(&out.join("env"));
    assert!(
        env.contains("PAGER_OUT="),
        "the pager inherits the rest: {env}"
    );
    assert!(!env.contains("CODER_SESSION_TOKEN"), "{env}");
    assert!(!env.contains("CODER_URL"), "{env}");
    assert!(!env.contains("test-token-not-real"), "{env}");
    s.wait_for("\x1b[6n");
    s.writer.write_all(b"\x1b[1;1R").unwrap();
    s.writer.write_all(b"/quit\r").unwrap();
    assert_eq!(s.exit_code(), 0);
    let _ = std::fs::remove_dir_all(&out);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_editor_gets_the_keys_typed_while_it_runs() {
    let server = fake_coder().await;
    // Like the pager test: the editor reads one key straight from the terminal, then saves it.
    let out = std::env::temp_dir().join(format!("scuttle-pty-editor-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&out);
    std::fs::create_dir_all(&out).unwrap();
    let script = out.join("editor.sh");
    std::fs::write(
        &script,
        "stty -icanon -echo min 1 < /dev/tty\n\
         printf 'EDITOR-READY'\n\
         key=$(dd bs=1 count=1 < /dev/tty 2>/dev/null)\n\
         printf 'edited %s' \"$key\" > \"$1\"\n\
         printf %s \"$key\" > \"$EDITOR_OUT/key\"\n",
    )
    .unwrap();
    let mut s = spawn(
        "editor-keys",
        &[
            ("CODER_URL", server.uri()),
            ("CODER_SESSION_TOKEN", "test-token-not-real".into()),
            ("EDITOR", format!("sh '{}'", script.display())),
            ("EDITOR_OUT", out.display().to_string()),
        ],
    );
    s.wait_for("Signed in as nick");
    s.writer.write_all(b"\x07").unwrap();
    s.wait_for("EDITOR-READY");
    s.writer.write_all(b"z").unwrap();
    assert_eq!(
        wait_for_file(&out.join("key")),
        "z",
        "the editor got the key"
    );
    s.wait_for_after("EDITOR-READY", "\x1b[6n");
    s.writer.write_all(b"\x1b[1;1R").unwrap();
    s.wait_for("edited z");
    let _ = std::fs::remove_dir_all(&out);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_bracketed_paste_reaches_the_composer() {
    let server = fake_coder().await;
    let mut s = spawn(
        "bracketed-paste",
        &[
            ("CODER_URL", server.uri()),
            ("CODER_SESSION_TOKEN", "test-token-not-real".into()),
        ],
    );
    s.wait_for("Signed in as nick");
    s.writer
        .write_all(b"\x1b[200~pasted words\x1b[201~")
        .unwrap();
    s.wait_for("pasted words");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_window_title_names_scuttle_and_is_restored_on_exit() {
    let server = fake_coder().await;
    let mut s = spawn(
        "window-title",
        &[
            ("CODER_URL", server.uri()),
            ("CODER_SESSION_TOKEN", "test-token-not-real".into()),
        ],
    );
    s.wait_for("Signed in as nick");
    s.writer.write_all(b"/quit\r").unwrap();
    assert_eq!(s.exit_code(), 0);
    let raw = String::from_utf8_lossy(&s.raw()).to_string();
    let pushed = raw
        .find("\x1b[22;0t")
        .expect("the title is saved at startup");
    let set = raw
        .find("\x1b]2;scuttle\x07")
        .expect("the title names scuttle");
    let popped = raw
        .rfind("\x1b[23;0t")
        .expect("the saved title is restored at exit");
    assert!(pushed < set && set < popped, "save, set, then restore");
}

#[tokio::test(flavor = "multi_thread")]
async fn settings_opens_the_config_in_the_editor_and_applies_it() {
    let server = fake_coder().await;
    let out = std::env::temp_dir().join(format!("scuttle-pty-settings-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&out);
    std::fs::create_dir_all(&out).unwrap();
    let script = out.join("editor.sh");
    std::fs::write(
        &script,
        "printf 'SETTINGS-READY'\n\
         printf '\\nmouse = false\\nbusy_behavior = \"interrupt\"\\n' >> \"$1\"\n",
    )
    .unwrap();
    let mut s = spawn(
        "settings",
        &[
            ("CODER_URL", server.uri()),
            ("CODER_SESSION_TOKEN", "test-token-not-real".into()),
            ("EDITOR", format!("sh '{}'", script.display())),
        ],
    );
    s.wait_for("Signed in as nick");
    s.writer.write_all(b"/settings\r").unwrap();
    s.wait_for("SETTINGS-READY");
    s.wait_for_after("SETTINGS-READY", "\x1b[6n");
    s.writer.write_all(b"\x1b[1;1R").unwrap();
    s.wait_for("Settings applied.");
    // The resume turns capture back on as it was, and the changed `mouse` then turns it off.
    s.wait_for_after("SETTINGS-READY", "\x1b[?1000l");
    let raw = String::from_utf8_lossy(&s.raw()).into_owned();
    let after = &raw[raw.rfind("SETTINGS-READY").unwrap()..];
    assert!(
        after.rfind("\x1b[?1000l") > after.rfind("\x1b[?1000h"),
        "mouse capture ends off: {after:?}"
    );
    let config = std::fs::read_to_string(s.home.join("config/scuttle/config.toml")).unwrap();
    assert!(
        config.contains("# mouse = true"),
        "made from the template: {config}"
    );
    assert!(config.contains("busy_behavior = \"interrupt\""), "{config}");
    assert!(config.contains("\nmouse = false\n"), "{config}");
    let _ = std::fs::remove_dir_all(&out);
}

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
        assert_eq!(
            s.exit_code(),
            code,
            "SIG{signal} exits with 128 plus its number"
        );
        let raw = String::from_utf8_lossy(&s.raw()).to_string();
        let tail = &raw[raw
            .rfind("\x1b[?1049h")
            .expect("scuttle entered the alternate screen")..];
        assert!(
            tail.contains("\x1b[?1049l"),
            "SIG{signal}: alternate screen left"
        );
        assert!(
            tail.contains("\x1b[?1000l") || tail.contains("\x1b[?1006l"),
            "SIG{signal}: mouse capture off"
        );
        assert!(
            tail.contains("\x1b[?2004l"),
            "SIG{signal}: bracketed paste off"
        );
        assert!(
            tail.contains("\x1b[?1007h"),
            "SIG{signal}: alternate scroll mode back"
        );
        assert!(
            tail.contains("\x1b[23;0t"),
            "SIG{signal}: the saved title is back"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn ctrl_c_that_ends_the_pager_leaves_scuttle_running() {
    let chat = uuid::Uuid::new_v4();
    let server = fake_coder_with_a_diff(chat).await;
    // Unlike `less`, this pager dies on SIGINT, so the handoff can end before scuttle's
    // signal driver has seen the SIGINT that reached scuttle too.
    let out = std::env::temp_dir().join(format!("scuttle-pty-sigint-dies-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&out);
    std::fs::create_dir_all(&out).unwrap();
    let script = out.join("pager.sh");
    std::fs::write(
        &script,
        "cat > /dev/null\n\
         printf 'PAGER-READY'\n\
         while :; do sleep 1; done\n",
    )
    .unwrap();
    let mut s = spawn_with_args(
        "pager-sigint-dies",
        &[chat.to_string()],
        &[
            ("CODER_URL", server.uri()),
            ("CODER_SESSION_TOKEN", "test-token-not-real".into()),
            ("GIT_PAGER", format!("sh '{}'", script.display())),
        ],
    );
    s.wait_for("reconnecting");
    s.writer.write_all(b"/diff\r").unwrap();
    s.wait_for("PAGER-READY");
    s.writer.write_all(b"\x03").unwrap();
    // scuttle survived: it takes the terminal back and repaints, then quits normally.
    s.wait_for_after("PAGER-READY", "\x1b[6n");
    s.writer.write_all(b"\x1b[1;1R").unwrap();
    s.writer.write_all(b"/quit\r").unwrap();
    assert_eq!(s.exit_code(), 0);
    let _ = std::fs::remove_dir_all(&out);
}
