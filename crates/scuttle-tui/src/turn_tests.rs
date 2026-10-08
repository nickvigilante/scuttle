//! Whole-turn tests: the `Tui`, the core, and the runtime together against a fake Coder that
//! serves HTTP through wiremock and the chat stream over a real WebSocket.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use futures::SinkExt;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use scuttle_core::app::{Effect, Msg, Notice};
use secrecy::SecretString;
use serde_json::json;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Notify;
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};
use tokio_tungstenite::tungstenite::Message;
use uuid::Uuid;
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::app::Tui;
use crate::runtime::Runtime;

const WAIT: Duration = Duration::from_secs(10);

fn tui() -> Tui {
    Tui::new(
        scuttle_core::config::LocalConfig::default(),
        None,
        crate::theme::Theme::terminal(true),
        crate::transcript_view::Welcome {
            url: "https://x".into(),
            user: String::new(),
            art: vec![],
            art_accent: true,
            show: true,
            tip: true,
        },
        0,
    )
}

fn runtime(url: &str) -> (Runtime, UnboundedReceiver<Msg>) {
    let token = SecretString::from("test-token-not-real-e1");
    let client = coder_sdk::Client::new(&coder_sdk::Session {
        url: url.parse().unwrap(),
        token: token.clone(),
    })
    .unwrap();
    let (tx, rx) = unbounded_channel();
    (Runtime::new(client, token, tx), rx)
}

fn chat_json(id: Uuid) -> serde_json::Value {
    json!({
        "id": id, "children": [], "files": [], "mcp_server_ids": [],
        "inline_mcp_servers": [], "labels": {}
    })
}

fn type_text(t: &mut Tui, text: &str) {
    for c in text.chars() {
        t.handle(Event::Key(KeyEvent::new(
            KeyCode::Char(c),
            KeyModifiers::NONE,
        )));
    }
}

fn enter(t: &mut Tui) -> Vec<Effect> {
    t.handle(Event::Key(KeyEvent::new(
        KeyCode::Enter,
        KeyModifiers::NONE,
    )))
}

/// Runs effects the way the main loop does: the UI's own first, the rest on the runtime.
fn dispatch(t: &mut Tui, rt: &mut Runtime, effects: Vec<Effect>) {
    for effect in effects {
        if !t.apply_ui_effect(&effect) {
            rt.run(effect);
        }
    }
}

/// Feeds runtime messages to the UI until `done` holds, failing after `WAIT`.
async fn pump_until(
    t: &mut Tui,
    rt: &mut Runtime,
    rx: &mut UnboundedReceiver<Msg>,
    what: &str,
    done: impl Fn(&Tui) -> bool,
) {
    let deadline = Instant::now() + WAIT;
    while !done(t) {
        let left = deadline.saturating_duration_since(Instant::now());
        let msg = match tokio::time::timeout(left, rx.recv()).await {
            Ok(Some(msg)) => msg,
            _ => panic!(
                "timed out waiting for {what}; notices: {:?}",
                t.core.notices
            ),
        };
        let effects = t.update(msg);
        dispatch(t, rt, effects);
    }
}

fn screen(t: &mut Tui) -> String {
    let (w, h) = (80, 24);
    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
    term.draw(|f| t.draw(f)).unwrap();
    let buf = term.backend().buffer().clone();
    (0..h)
        .map(|y| (0..w).map(|x| buf[(x, y)].symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Serves one address for both halves of the fake Coder: a chat stream upgrade is answered by
/// a WebSocket that sends `before`, waits for `release`, then sends `after` and stays open;
/// every other connection is forwarded to the wiremock server at `http`.
async fn serve(
    http: SocketAddr,
    before: Vec<serde_json::Value>,
    release: Arc<Notify>,
    after: Vec<serde_json::Value>,
) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let frames = Arc::new((before, after));
    tokio::spawn(async move {
        while let Ok((mut tcp, _)) = listener.accept().await {
            let frames = frames.clone();
            let release = release.clone();
            tokio::spawn(async move {
                if !request_line(&tcp).await.contains("/stream") {
                    let mut upstream = TcpStream::connect(http).await.unwrap();
                    let _ = tokio::io::copy_bidirectional(&mut tcp, &mut upstream).await;
                    return;
                }
                let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
                let (before, after) = &*frames;
                for frame in before {
                    ws.send(Message::text(json!([frame]).to_string()))
                        .await
                        .unwrap();
                }
                release.notified().await;
                for frame in after {
                    ws.send(Message::text(json!([frame]).to_string()))
                        .await
                        .unwrap();
                }
                use futures::StreamExt;
                while ws.next().await.is_some() {}
            });
        }
    });
    format!("http://{addr}")
}

/// The HTTP request line of a connection, read without consuming it.
async fn request_line(tcp: &TcpStream) -> String {
    let mut buf = [0u8; 2048];
    loop {
        let n = tcp.peek(&mut buf).await.unwrap();
        if let Some(end) = buf[..n].windows(2).position(|w| w == b"\r\n") {
            return String::from_utf8_lossy(&buf[..end]).into_owned();
        }
        if n == buf.len() || n == 0 {
            return String::from_utf8_lossy(&buf[..n]).into_owned();
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

fn part(seq: i64, text: &str) -> serde_json::Value {
    json!({"type": "message_part", "message_part": {
        "history_version": 1, "generation_attempt": 1, "seq": seq, "role": "assistant",
        "part": {"type": "text", "text": text}
    }})
}

fn message(id: i64, role: &str, text: &str) -> serde_json::Value {
    json!({"type": "message", "message": {
        "id": id, "role": role, "content": [{"type": "text", "text": text}]
    }})
}

fn live_text(t: &Tui) -> String {
    t.core
        .transcript
        .live
        .blocks
        .iter()
        .filter_map(|b| match b {
            scuttle_core::live::LiveBlock::Text(s) => Some(s.as_str()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_submitted_message_streams_a_live_turn_that_lands_as_a_durable_message() {
    let http = MockServer::start().await;
    let chat = Uuid::new_v4();
    Mock::given(method("POST"))
        .and(path("/api/v2/chats"))
        .and(body_partial_json(
            json!({"content": [{"type": "text", "text": "hello agent"}]}),
        ))
        .respond_with(ResponseTemplate::new(201).set_body_json(chat_json(chat)))
        .expect(1)
        .mount(&http)
        .await;
    let release = Arc::new(Notify::new());
    let url = serve(
        *http.address(),
        vec![
            json!({"type": "status", "status": {"status": "running"}}),
            message(1, "user", "hello agent"),
            part(1, "Streaming"),
            part(2, " reply"),
        ],
        release.clone(),
        vec![
            message(2, "assistant", "Durable answer from the agent."),
            json!({"type": "status", "status": {"status": "waiting"}}),
        ],
    )
    .await;
    let (mut rt, mut rx) = runtime(&url);
    let mut t = tui();
    // The settings fetches `Started` asks for are not part of this turn.
    let _ = t.update(Msg::Started {
        org_id: Uuid::new_v4(),
        open_chat: None,
    });

    type_text(&mut t, "hello agent");
    let effects = enter(&mut t);
    assert!(
        matches!(effects.as_slice(), [Effect::CreateChat { text, .. }] if text == "hello agent"),
        "{effects:?}"
    );
    dispatch(&mut t, &mut rt, effects);

    pump_until(&mut t, &mut rt, &mut rx, "the live turn", |t| {
        live_text(t) == "Streaming reply"
    })
    .await;
    assert_eq!(t.core.chat_id, Some(chat));
    assert!(screen(&mut t).contains("Streaming reply"));

    release.notify_one();
    pump_until(&mut t, &mut rt, &mut rx, "the durable message", |t| {
        t.core.transcript.messages().count() == 2
    })
    .await;
    assert!(t.core.transcript.live.is_empty());
    let shown = screen(&mut t);
    assert!(shown.contains("Durable answer from the agent."), "{shown}");
    assert!(!shown.contains("Streaming reply"), "{shown}");
    assert!(!shown.contains("test-token-not-real-e1"));
}

#[tokio::test]
async fn a_failed_send_puts_the_text_back_in_the_composer() {
    let http = MockServer::start().await;
    let chat = Uuid::new_v4();
    Mock::given(method("POST"))
        .and(path(format!("/api/v2/chats/{chat}/messages")))
        .respond_with(
            ResponseTemplate::new(409)
                .set_body_json(json!({"message": "the chat cannot accept messages"})),
        )
        .mount(&http)
        .await;
    let (mut rt, mut rx) = runtime(&http.uri());
    let mut t = tui();
    let _ = t.update(Msg::Started {
        org_id: Uuid::new_v4(),
        open_chat: None,
    });
    // The stream this asks for is not part of this test.
    let _ = t.update(Msg::ChatLoaded {
        has_more: None,
        chat: Box::new(serde_json::from_value(chat_json(chat)).unwrap()),
        messages: vec![],
    });

    type_text(&mut t, "keep me");
    let effects = enter(&mut t);
    assert!(matches!(effects.as_slice(), [Effect::SendMessage { .. }]));
    assert_eq!(t.composer.text(), "");
    type_text(&mut t, "typed since");
    dispatch(&mut t, &mut rt, effects);

    pump_until(&mut t, &mut rt, &mut rx, "the restored text", |t| {
        t.composer.text().contains("keep me")
    })
    .await;
    assert_eq!(t.composer.text(), "keep me\n\ntyped since");
    assert!(matches!(
        t.core.notices.last(),
        Some(Notice::Error(m)) if m.contains("cannot accept")
    ));
}
