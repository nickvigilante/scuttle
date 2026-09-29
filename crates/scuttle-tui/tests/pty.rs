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
    home: std::path::PathBuf,
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

/// Spawns `scuttle` in a fresh pty with its own `HOME`, named after the pid and `name` so
/// concurrent test binaries (and repeat runs) never share, or leak into, each other's config.
fn spawn(name: &str, envs: &[(&str, String)]) -> Session {
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
    let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_scuttle"));
    let home = std::env::temp_dir().join(format!("scuttle-pty-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
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
    Session {
        output,
        writer,
        child,
        _master: pair.master,
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
    s.writer.write_all(b"\x03").unwrap();
    s.wait_for("Ctrl+C again");
    s.writer.write_all(b"\x03").unwrap();
    assert_eq!(s.exit_code(), 0);
    assert!(!s.screen().contains("test-token-not-real"));
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
    let tail = &raw[raw.rfind("\x1b[?1049h").unwrap_or(0)..];
    assert!(tail.contains("\x1b[?1049l"), "alternate screen left");
    assert!(
        tail.contains("\x1b[?1000l") || tail.contains("\x1b[?1006l"),
        "mouse capture disabled"
    );
    assert!(tail.contains("\x1b[?2004l"), "bracketed paste disabled");
}
