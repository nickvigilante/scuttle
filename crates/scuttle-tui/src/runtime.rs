//! Executes API effects with coder-sdk and reports results back as `Msg`s.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

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
    /// Bumped each time a stream opens; a stream task only speaks while it is current.
    stream_generation: Arc<AtomicU64>,
}

/// A stream task's sender, silenced once a newer stream replaces it.
struct StreamSender {
    tx: UnboundedSender<Msg>,
    generation: Arc<AtomicU64>,
    mine: u64,
}

impl StreamSender {
    /// Sends `msg` if this stream is still the current one. Returns whether it was current.
    fn send(&self, msg: Msg) -> bool {
        if self.generation.load(Ordering::SeqCst) != self.mine {
            return false;
        }
        let _ = self.tx.send(msg);
        true
    }
}

fn text_part(text: &str) -> types::CodersdkChatInputPart {
    types::CodersdkChatInputPart {
        type_: Some(types::CodersdkChatInputPartType("text".into())),
        text: Some(text.to_owned()),
        ..Default::default()
    }
}

/// The user-facing text of a generated-client error. Never includes the session token,
/// which only travels in a request header.
async fn err<E: serde::Serialize + std::fmt::Debug>(e: progenitor_client::Error<E>) -> String {
    coder_sdk::Error::from_progenitor(e).await.to_string()
}

type Job = Pin<Box<dyn Future<Output = Msg> + Send>>;

impl Runtime {
    pub fn new(client: Client, tx: UnboundedSender<Msg>) -> Runtime {
        Runtime {
            client,
            tx,
            stream: None,
            stream_generation: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Resolves the user's first organization, the one new chats are created in.
    pub async fn organization(&self) -> Result<Uuid, String> {
        let orgs = match self.client.api().get_organizations_by_user("me").await {
            Ok(r) => r.into_inner(),
            Err(e) => return Err(err(e).await),
        };
        orgs.first()
            .map(|o| o.id)
            .ok_or_else(|| "you are not a member of any organization".into())
    }

    fn open_stream(&mut self, chat: Uuid, after_id: Option<i64>, delay: Duration) {
        // Bump first so the old task goes quiet even if it is mid-flight on another thread.
        let mine = self.stream_generation.fetch_add(1, Ordering::SeqCst) + 1;
        if let Some(old) = self.stream.take() {
            old.abort();
        }
        let client = self.client.clone();
        let out = StreamSender {
            tx: self.tx.clone(),
            generation: self.stream_generation.clone(),
            mine,
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
            while let Some(item) = stream.next().await {
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

    fn spawn(&self, job: Job) {
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let _ = tx.send(job.await);
        });
    }

    /// Runs one API effect in the background. Effects the UI owns are ignored here.
    pub fn run(&mut self, effect: Effect) {
        let client = self.client.clone();
        match effect {
            Effect::OpenStream { chat, after_id } => {
                self.open_stream(chat, after_id, Duration::ZERO)
            }
            Effect::ReconnectAfter {
                chat,
                after_id,
                delay,
            } => self.open_stream(chat, after_id, delay),
            Effect::LoadChat(id) => self.spawn(Box::pin(async move {
                let chat = match client.api().get_chat_by_id(&id).await {
                    Ok(c) => c.into_inner(),
                    Err(e) => {
                        return Msg::ApiFailed {
                            action: "load the chat",
                            message: err(e).await,
                        };
                    }
                };
                let messages = match client
                    .api()
                    .list_chat_messages(&id, None, None, Some(200))
                    .await
                {
                    Ok(m) => m.into_inner().messages,
                    Err(e) => {
                        return Msg::ApiFailed {
                            action: "load messages",
                            message: err(e).await,
                        };
                    }
                };
                Msg::ChatLoaded {
                    chat: Box::new(chat),
                    messages,
                }
            })),
            Effect::CreateChat {
                org,
                text,
                model,
                workspace,
            } => self.spawn(Box::pin(async move {
                let body = types::CodersdkCreateChatRequest {
                    organization_id: Some(org),
                    content: vec![text_part(&text)],
                    model_config_id: model,
                    workspace_id: workspace,
                    ..Default::default()
                };
                match client.api().create_chat(&body).await {
                    Ok(c) => Msg::ChatCreated(Box::new(c.into_inner())),
                    Err(e) => Msg::CreateFailed {
                        message: err(e).await,
                    },
                }
            })),
            Effect::SendMessage {
                chat,
                text,
                model,
                busy,
            } => self.spawn(Box::pin(async move {
                let body = types::CodersdkCreateChatMessageRequest {
                    content: vec![text_part(&text)],
                    model_config_id: model,
                    busy_behavior: Some(types::CodersdkChatBusyBehavior(busy.as_str().into())),
                    ..Default::default()
                };
                match client.api().send_chat_message(&chat, &body).await {
                    Ok(_) => Msg::Refresh,
                    Err(e) => Msg::ApiFailed {
                        action: "send the message",
                        message: err(e).await,
                    },
                }
            })),
            Effect::Interrupt(chat) => self.spawn(Box::pin(async move {
                match client.api().interrupt_chat(&chat).await {
                    Ok(_) => Msg::Refresh,
                    Err(e) => Msg::ApiFailed {
                        action: "interrupt",
                        message: err(e).await,
                    },
                }
            })),
            Effect::Compact(chat) => self.spawn(Box::pin(async move {
                match client.api().compact_chat(&chat).await {
                    Ok(_) => Msg::Refresh,
                    Err(e) => Msg::ApiFailed {
                        action: "compact the chat",
                        message: err(e).await,
                    },
                }
            })),
            Effect::Clear(chat) => self.spawn(Box::pin(async move {
                match client.api().clear_chat_context(&chat).await {
                    Ok(_) => Msg::Refresh,
                    Err(e) => Msg::ApiFailed {
                        action: "clear the context",
                        message: err(e).await,
                    },
                }
            })),
            Effect::SetWorkspace { chat, workspace } => self.spawn(Box::pin(async move {
                // The nil UUID detaches the workspace; `None` would mean "no change".
                let body = types::CodersdkUpdateChatRequest {
                    workspace_id: Some(workspace.unwrap_or(Uuid::nil())),
                    ..Default::default()
                };
                match client.api().update_chat(&chat, &body).await {
                    Ok(_) => Msg::Refresh,
                    Err(e) => Msg::ApiFailed {
                        action: "change the workspace",
                        message: err(e).await,
                    },
                }
            })),
            Effect::FetchPrefs => self.spawn(Box::pin(async move {
                match client.api().get_user_preference_settings("me").await {
                    Ok(p) => Msg::PrefsLoaded(DisplayPrefs::from(&p.into_inner())),
                    Err(e) => Msg::ApiFailed {
                        action: "load display preferences",
                        message: err(e).await,
                    },
                }
            })),
            Effect::FetchModels(org) => self.spawn(Box::pin(async move {
                match client
                    .api()
                    .list_ai_models_and_provider_descriptors_in_an_organization(&org.to_string())
                    .await
                {
                    Ok(r) => Msg::ModelsLoaded(r.into_inner().models),
                    Err(e) => Msg::ModelsFailed {
                        message: err(e).await,
                    },
                }
            })),
            Effect::FetchWorkspaces => self.spawn(Box::pin(async move {
                match client
                    .api()
                    .list_workspaces(Some(100), None, Some("owner:me"))
                    .await
                {
                    Ok(r) => Msg::WorkspacesLoaded(
                        r.into_inner()
                            .workspaces
                            .into_iter()
                            .filter_map(|w| {
                                Some(WorkspaceRef {
                                    id: w.id?,
                                    name: w.name?,
                                })
                            })
                            .collect(),
                    ),
                    Err(e) => Msg::ApiFailed {
                        action: "load workspaces",
                        message: err(e).await,
                    },
                }
            })),
            Effect::ShowPicker(_)
            | Effect::ShowHelp
            | Effect::Copy(_)
            | Effect::SetMouse(_)
            | Effect::RestoreComposer(_)
            | Effect::Quit => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::SinkExt;
    use secrecy::SecretString;
    use tokio::net::TcpListener;
    use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};
    use tokio_tungstenite::tungstenite::Message;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const TOKEN: &str = "test-token-not-real-5f3a";
    const WAIT: Duration = Duration::from_secs(10);

    fn client(url: &str) -> Client {
        Client::new(&coder_sdk::Session {
            url: url.parse().unwrap(),
            token: SecretString::from(TOKEN),
        })
        .unwrap()
    }

    fn runtime(url: &str) -> (Runtime, UnboundedReceiver<Msg>) {
        let (tx, rx) = unbounded_channel();
        (Runtime::new(client(url), tx), rx)
    }

    async fn next(rx: &mut UnboundedReceiver<Msg>) -> Msg {
        tokio::time::timeout(WAIT, rx.recv())
            .await
            .expect("a message within the timeout")
            .expect("the channel is open")
    }

    fn api_error(status: u16, message: &str) -> ResponseTemplate {
        ResponseTemplate::new(status).set_body_json(serde_json::json!({ "message": message }))
    }

    #[tokio::test]
    async fn a_failed_stream_open_ends_the_stream() {
        let server = MockServer::start().await;
        let (mut rt, mut rx) = runtime(&server.uri());
        rt.run(Effect::OpenStream {
            chat: Uuid::new_v4(),
            after_id: None,
        });
        match next(&mut rx).await {
            Msg::StreamEnded { error: Some(e) } => assert!(!e.contains(TOKEN), "{e}"),
            other => panic!("expected StreamEnded with an error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_failed_reconnect_ends_the_stream() {
        let server = MockServer::start().await;
        let (mut rt, mut rx) = runtime(&server.uri());
        rt.run(Effect::ReconnectAfter {
            chat: Uuid::new_v4(),
            after_id: Some(7),
            delay: Duration::from_millis(1),
        });
        assert!(matches!(
            next(&mut rx).await,
            Msg::StreamEnded { error: Some(_) }
        ));
    }

    #[tokio::test]
    async fn create_chat_errors_send_create_failed() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v2/chats"))
            .respond_with(api_error(500, "database is on fire"))
            .mount(&server)
            .await;
        let (mut rt, mut rx) = runtime(&server.uri());
        rt.run(Effect::CreateChat {
            org: Uuid::new_v4(),
            text: "hello".into(),
            model: None,
            workspace: None,
        });
        match next(&mut rx).await {
            Msg::CreateFailed { message } => {
                assert!(message.contains("database is on fire"), "{message}");
                assert!(!message.contains(TOKEN), "{message}");
            }
            other => panic!("expected CreateFailed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn create_chat_sends_the_text_and_reports_the_chat() {
        let server = MockServer::start().await;
        let id = Uuid::new_v4();
        let org = Uuid::new_v4();
        Mock::given(method("POST"))
            .and(path("/api/v2/chats"))
            .and(wiremock::matchers::body_partial_json(serde_json::json!({
                "organization_id": org,
                "content": [{"type": "text", "text": "hello"}],
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "id": id, "children": [], "files": [], "mcp_server_ids": [],
                "inline_mcp_servers": [], "labels": {}
            })))
            .mount(&server)
            .await;
        let (mut rt, mut rx) = runtime(&server.uri());
        rt.run(Effect::CreateChat {
            org,
            text: "hello".into(),
            model: None,
            workspace: None,
        });
        match next(&mut rx).await {
            Msg::ChatCreated(chat) => assert_eq!(chat.id, Some(id)),
            other => panic!("expected ChatCreated, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn model_list_errors_send_models_failed() {
        let server = MockServer::start().await;
        let org = Uuid::new_v4();
        Mock::given(method("GET"))
            .and(path(format!("/api/v2/organizations/{org}/chats/models")))
            .respond_with(api_error(403, "not allowed"))
            .mount(&server)
            .await;
        let (mut rt, mut rx) = runtime(&server.uri());
        rt.run(Effect::FetchModels(org));
        match next(&mut rx).await {
            Msg::ModelsFailed { message } => assert!(message.contains("not allowed"), "{message}"),
            other => panic!("expected ModelsFailed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn other_api_errors_send_api_failed() {
        let server = MockServer::start().await;
        let (mut rt, mut rx) = runtime(&server.uri());
        rt.run(Effect::Compact(Uuid::new_v4()));
        assert!(matches!(
            next(&mut rx).await,
            Msg::ApiFailed {
                action: "compact the chat",
                ..
            }
        ));
    }

    #[tokio::test]
    async fn ui_effects_are_ignored() {
        let server = MockServer::start().await;
        let (mut rt, mut rx) = runtime(&server.uri());
        rt.run(Effect::ShowHelp);
        rt.run(Effect::Quit);
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn a_stale_stream_sender_delivers_nothing() {
        let (tx, mut rx) = unbounded_channel();
        let generation = Arc::new(AtomicU64::new(1));
        let old = StreamSender {
            tx,
            generation: generation.clone(),
            mine: 1,
        };
        assert!(old.send(Msg::StreamEnded { error: None }));
        generation.store(2, Ordering::SeqCst);
        assert!(!old.send(Msg::StreamEnded { error: None }));
        assert!(matches!(rx.try_recv(), Ok(Msg::StreamEnded { .. })));
        assert!(rx.try_recv().is_err());
    }

    /// Serves chat streams: connection `n` (from 1) sends a status event tagged `"conn": n`
    /// every 10 ms, forever, until the client goes away.
    async fn serve_tagged_streams() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let mut n = 0u64;
            loop {
                let Ok((tcp, _)) = listener.accept().await else {
                    return;
                };
                n += 1;
                let conn = n;
                tokio::spawn(async move {
                    let Ok(mut ws) = tokio_tungstenite::accept_async(tcp).await else {
                        return;
                    };
                    let frame = serde_json::json!([
                        {"type": "status", "status": {"status": "running"}, "conn": conn}
                    ])
                    .to_string();
                    while ws.send(Message::text(frame.clone())).await.is_ok() {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                });
            }
        });
        format!("http://{addr}")
    }

    fn conn_of(msg: &Msg) -> Option<u64> {
        match msg {
            Msg::Stream(ev) => ev.raw["conn"].as_u64(),
            _ => None,
        }
    }

    #[tokio::test]
    async fn reopening_a_stream_silences_the_old_one() {
        let url = serve_tagged_streams().await;
        let (mut rt, mut rx) = runtime(&url);
        let chat = Uuid::new_v4();
        rt.run(Effect::OpenStream {
            chat,
            after_id: None,
        });
        assert_eq!(conn_of(&next(&mut rx).await), Some(1));
        rt.run(Effect::ReconnectAfter {
            chat,
            after_id: None,
            delay: Duration::ZERO,
        });
        // Anything the old stream queued before the switch may still arrive first; once the
        // new stream speaks, the old one must never speak again, not even to say it ended.
        loop {
            let msg = next(&mut rx).await;
            match conn_of(&msg) {
                Some(1) => continue,
                Some(2) => break,
                _ => panic!("unexpected message before the new stream: {msg:?}"),
            }
        }
        for _ in 0..10 {
            let msg = next(&mut rx).await;
            assert_eq!(conn_of(&msg), Some(2), "{msg:?}");
        }
    }
}
