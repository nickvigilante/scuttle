//! Executes API effects with coder-sdk and reports results back as `Msg`s.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use coder_sdk::{Client, types};
use futures::StreamExt;
use scuttle_core::app::{
    ChatChange, Effect, Msg, OrgRef, UserRef, WorkspaceDeletion, WorkspaceRef,
};
use scuttle_core::compaction::{Change, Save};
use scuttle_core::density::DisplayPrefs;
use scuttle_core::usage::{Limit, Refusal};
use secrecy::{ExposeSecret, SecretString};
use tokio::sync::mpsc::UnboundedSender;
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::links;

/// How long a stream must stay open before its reconnect backoff restarts.
pub const STREAM_HEALTHY_AFTER: Duration = Duration::from_secs(10);

/// How long a spend or quota request may take. It is under the minute between refreshes, so
/// a slow endpoint fails and is asked again rather than being superseded by every refresh.
pub const LIMITS_TIMEOUT: Duration = Duration::from_secs(30);

/// Why a spend or quota request that ran past `LIMITS_TIMEOUT` failed.
pub const LIMITS_TIMED_OUT: &str = "the request timed out";

/// Which stream slot a chat stream task fills.
#[derive(Debug, Clone, Copy)]
enum Slot {
    Main,
    Preview,
}

/// How a slot's messages are tagged for the core: the main stream's in `Msg::ForStream` and
/// the preview's in `Msg::ForPreview`, so neither can reach the other's transcript.
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

pub struct Runtime {
    client: Client,
    redact: Redactor,
    tx: UnboundedSender<Msg>,
    stream: Option<JoinHandle<()>>,
    /// The chat list watch; one runs for the life of the process, reopened after each end.
    watch: Option<JoinHandle<()>>,
    /// Bumped each time a stream opens; a stream task only speaks while it is current.
    stream_generation: Arc<AtomicU64>,
    /// The subagent preview's stream, beside the open chat's.
    preview: Option<JoinHandle<()>>,
    /// The preview slot's own `stream_generation`.
    preview_generation: Arc<AtomicU64>,
    /// The `/git` panel's local-changes socket, open only while the panel is.
    git: Option<JoinHandle<()>>,
    /// The chat and core generation of the open `/git` socket, kept while a handoff closes it.
    git_target: Option<(Uuid, u64)>,
    /// Uploads still running, by their chip's local number, so removing the chip stops one.
    uploads: HashMap<u64, JoinHandle<()>>,
    /// How long a stream stays open before it reports `Msg::StreamHealthy`; tests shorten it.
    pub(crate) healthy_after: Duration,
    /// How long a spend or quota request may take before it fails; tests shorten it.
    pub(crate) limits_timeout: Duration,
}

/// A stream task's sender, silenced once a newer stream replaces it or the stream closes.
/// Stream messages are never sent unwrapped: every one goes through `wrap`, which tags it
/// for its slot with the chat and the core's generation.
struct StreamSender {
    tx: UnboundedSender<Msg>,
    generation: Arc<AtomicU64>,
    mine: u64,
    /// The chat this stream belongs to. Every message is tagged with it, because one already
    /// in the channel when the stream is replaced still arrives.
    chat: Uuid,
    /// The core's generation for this stream, which the core checks before applying anything.
    tag: u64,
    /// Tags each message for the slot this stream fills; see [`tag_for`].
    wrap: fn(Uuid, u64, Msg) -> Msg,
}

impl StreamSender {
    /// Sends `msg` for this stream's chat if the stream is still current. Returns whether it was.
    fn send(&self, msg: Msg) -> bool {
        if self.generation.load(Ordering::SeqCst) != self.mine {
            return false;
        }
        let _ = self.tx.send((self.wrap)(self.chat, self.tag, msg));
        true
    }
}

/// What [`pump`] hands its caller for each turn of a socket.
enum Pumped<T> {
    Item(T),
    /// The socket stayed open for the healthy interval.
    Healthy,
    /// The socket ended, with why unless it closed normally. Nothing follows it.
    Ended(Option<String>),
}

/// Polls `stream` until it ends, handing each item to `on` and skipping items that fail to
/// decode. With `healthy_after`, `Pumped::Healthy` comes once after the stream has stayed
/// open that long. `on` returns whether to keep going, so a replaced stream stops at once.
async fn pump<T, S>(
    mut stream: S,
    redact: &Redactor,
    healthy_after: Option<Duration>,
    mut on: impl FnMut(Pumped<T>) -> bool,
) where
    S: futures::Stream<Item = coder_sdk::Result<T>> + Unpin,
{
    let healthy = tokio::time::sleep(healthy_after.unwrap_or_default());
    tokio::pin!(healthy);
    let mut reported = healthy_after.is_none();
    loop {
        let item = tokio::select! {
            item = stream.next() => item,
            () = &mut healthy, if !reported => {
                reported = true;
                if !on(Pumped::Healthy) {
                    return;
                }
                continue;
            }
        };
        let event = match item {
            Some(Ok(item)) => Pumped::Item(item),
            Some(Err(coder_sdk::Error::Decode(_))) => continue,
            Some(Err(e)) => {
                on(Pumped::Ended(Some(redact.error(e).to_string())));
                return;
            }
            None => {
                on(Pumped::Ended(None));
                return;
            }
        };
        if !on(event) {
            return;
        }
    }
}

/// Why a file of `size` bytes is refused; in exact bytes when MiB would round to the limit.
fn too_big(name: &str, size: u64) -> String {
    use scuttle_core::attachments::{MAX_FILE_BYTES, size_label};
    let label = size_label(size);
    if label == size_label(MAX_FILE_BYTES) {
        format!("{name} is {size} bytes; the limit is {MAX_FILE_BYTES} bytes.")
    } else {
        format!("{name} is {label}; the limit is 10 MiB.")
    }
}

/// Reads at most the upload limit from `reader`, refusing a file that is longer, so one that
/// grew after its size was checked is still refused.
fn read_capped(reader: impl std::io::Read, name: &str) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let max = scuttle_core::attachments::MAX_FILE_BYTES;
    let mut bytes = Vec::new();
    reader
        .take(max + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("could not read {name}: {e}"))?;
    if bytes.len() as u64 > max {
        return Err(format!(
            "{name} grew past the 10 MiB limit while it was read."
        ));
    }
    Ok(bytes)
}

/// Reads the file at `path` for upload. The size check comes before any read, so a huge
/// file never leaves the disk. Blocking, so it runs off the async workers.
fn read_upload(path: &str, name: &str) -> Result<Vec<u8>, String> {
    // Checked before opening: opening a named pipe blocks until a writer appears.
    let meta = std::fs::metadata(path).map_err(|e| format!("could not read {path}: {e}"))?;
    if !meta.is_file() {
        return Err(format!("{name} is not a file."));
    }
    if meta.len() > scuttle_core::attachments::MAX_FILE_BYTES {
        return Err(too_big(name, meta.len()));
    }
    let file = std::fs::File::open(path).map_err(|e| format!("could not read {path}: {e}"))?;
    read_capped(file, name)
}

/// The `Content-Disposition` header for an upload of `name`.
fn content_disposition(name: &str) -> String {
    // A header value must be ASCII, and inside the quoted name the server reads `"` as its
    // end and `\\` as an escape.
    let safe: String = name
        .chars()
        .map(|c| {
            if c.is_ascii() && !c.is_ascii_control() && c != '"' && c != '\\' {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("attachment; filename=\"{safe}\"")
}

/// The content of a message: its text, then one part per uploaded file. A message that is only
/// files, such as a lone large paste, has no text part.
pub(crate) fn parts(text: &str, files: &[Uuid]) -> Vec<types::CodersdkChatInputPart> {
    let text = (!text.is_empty()).then(|| types::CodersdkChatInputPart {
        type_: Some(types::CodersdkChatInputPartType("text".into())),
        text: Some(text.to_owned()),
        ..Default::default()
    });
    text.into_iter()
        .chain(files.iter().map(|id| types::CodersdkChatInputPart {
            type_: Some(types::CodersdkChatInputPartType("file".into())),
            file_id: Some(*id),
            ..Default::default()
        }))
        .collect()
}

/// The wire value of a plan mode switch: `"plan"` turns it on and `""` clears it.
fn plan_mode_value(on: bool) -> types::CodersdkChatPlanMode {
    types::CodersdkChatPlanMode(if on { "plan" } else { "" }.into())
}

/// The web UI page for `chat`, on the deployment's origin without any userinfo, path, or query.
pub fn chat_web_url(base: &url::Url, chat: Uuid) -> url::Url {
    let mut url = base.clone();
    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.set_path(&format!("/agents/{chat}"));
    url.set_query(None);
    url.set_fragment(None);
    url
}

/// The web UI page for a workspace, on the deployment's origin without any userinfo or query.
pub fn workspace_web_url(base: &url::Url, owner: &str, workspace: &str) -> url::Url {
    let mut url = base.clone();
    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.set_path(&format!("/@{owner}/{workspace}"));
    url.set_query(None);
    url.set_fragment(None);
    url
}

/// Whether scuttle runs over SSH, where a browser would open on the wrong machine.
pub fn over_ssh(is_set: impl Fn(&str) -> bool) -> bool {
    is_set("SSH_CONNECTION") || is_set("SSH_TTY")
}

/// Why downloading `name` failed, from an already redacted error. The server answers 404
/// both for a file it no longer has and for one the user cannot access.
fn download_failed(name: &str, e: coder_sdk::Error) -> String {
    match e {
        coder_sdk::Error::Api {
            status: 404 | 410, ..
        } => format!("{name} is no longer available, or you can't access it."),
        e => format!("Could not download {name}: {e}"),
    }
}

/// Downloads chat file `file` and writes it to `to` under `name`, a chunk at a time,
/// through a partial file that only a finished download moves into place. A taken name is
/// asked about before any byte is fetched, and the move checks again.
async fn save_chat_file(
    client: Client,
    redact: Redactor,
    file: Uuid,
    name: String,
    to: scuttle_core::files::SaveTo,
    conflict: scuttle_core::files::OnConflict,
) -> Msg {
    use scuttle_core::attachments::MAX_FILE_BYTES;
    use scuttle_core::files::OnConflict;
    let failed = |message: String| Msg::FileFailed { file, message };
    let target = to.target(&name, |p| p.is_dir());
    if conflict == OnConflict::Ask && std::fs::symlink_metadata(&target).is_ok() {
        return Msg::FileConflict {
            file,
            name,
            path: target,
        };
    }
    // A rename cannot replace a folder, so say so before fetching a byte.
    if conflict == OnConflict::Replace
        && std::fs::symlink_metadata(&target).is_ok_and(|m| m.is_dir())
    {
        return failed(format!(
            "{} is a folder, so {name} was not saved there.",
            target.display()
        ));
    }
    let mut download = match client.download_chat_file(file).await {
        Ok(download) => download,
        Err(e) => return failed(download_failed(&name, redact.error(e))),
    };
    if let Some(size) = download.size.filter(|s| *s > MAX_FILE_BYTES) {
        return failed(too_big(&name, size));
    }
    let mut partial = match crate::save::Partial::beside(&target) {
        Ok(partial) => partial,
        Err(e) => {
            return failed(format!(
                "Could not save {name} in {}: {e}",
                crate::save::dir_of(&target).display()
            ));
        }
    };
    // The writes and the sync before the move run on the blocking pool, so a slow mount never
    // holds an async worker. A partial file dropped there, on an error, removes itself.
    loop {
        match download.chunk().await {
            Ok(Some(chunk)) => {
                if partial.written() + chunk.len() as u64 > MAX_FILE_BYTES {
                    return failed(format!(
                        "{name} grew past the 10 MiB limit while it downloaded, so it was not saved."
                    ));
                }
                let wrote =
                    tokio::task::spawn_blocking(move || partial.write(&chunk).map(|()| partial))
                        .await
                        .map_err(std::io::Error::other)
                        .and_then(|r| r);
                partial = match wrote {
                    Ok(partial) => partial,
                    Err(e) => return failed(format!("Could not save {name}: {e}")),
                };
            }
            Ok(None) => break,
            Err(e) => return failed(format!("Could not download {name}: {}", redact.error(e))),
        }
    }
    let at = target.clone();
    let placed = tokio::task::spawn_blocking(move || match conflict {
        OnConflict::Ask => partial.keep_new(&at).map(|()| at),
        OnConflict::Replace => partial.replace(&at).map(|()| at),
        OnConflict::KeepBoth => partial.keep_both(&at),
    })
    .await
    .map_err(std::io::Error::other)
    .and_then(|r| r);
    match placed {
        Ok(path) => Msg::FileSaved { file, path },
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Msg::FileConflict {
            file,
            name,
            path: target,
        },
        Err(e) => failed(format!(
            "Could not save {name} to {}: {e}",
            target.display()
        )),
    }
}

/// Downloads text file `file` into memory, at most 10 MiB, for the pager.
async fn read_chat_text(client: Client, redact: Redactor, file: Uuid, name: String) -> Msg {
    use scuttle_core::attachments::MAX_FILE_BYTES;
    let failed = |message: String| Msg::FileFailed { file, message };
    let mut download = match client.download_chat_file(file).await {
        Ok(download) => download,
        Err(e) => return failed(download_failed(&name, redact.error(e))),
    };
    if let Some(size) = download.size.filter(|s| *s > MAX_FILE_BYTES) {
        return failed(too_big(&name, size));
    }
    let mut bytes = Vec::new();
    loop {
        match download.chunk().await {
            Ok(Some(chunk)) => {
                if (bytes.len() + chunk.len()) as u64 > MAX_FILE_BYTES {
                    return failed(format!(
                        "{name} grew past the 10 MiB limit while it downloaded."
                    ));
                }
                bytes.extend_from_slice(&chunk);
            }
            Ok(None) => break,
            Err(e) => return failed(format!("Could not download {name}: {}", redact.error(e))),
        }
    }
    match String::from_utf8(bytes) {
        Ok(text) => Msg::FileText { file, text },
        Err(_) => failed(format!(
            "{name} is not UTF-8 text, so it cannot be shown. Press Enter to save it."
        )),
    }
}

/// Up to half a second of random delay, so reconnecting clients do not arrive together.
fn jitter() -> Duration {
    Duration::from_millis(u64::from(Uuid::new_v4().as_bytes()[0]) * 2)
}

/// Opens `url` with the system browser. Every standard stream is closed, because the opener's
/// output would land on top of the full-screen UI.
///
/// `cfg!(test)` only holds for unit tests in this crate; `tests/pty.rs` spawns the real
/// `scuttle` binary, so it sets `SCUTTLE_NO_BROWSER` to keep those tests from launching one too.
async fn open_in_browser(url: &str) -> Result<(), String> {
    if cfg!(test) || std::env::var_os("SCUTTLE_NO_BROWSER").is_some() {
        return Err("not opened".into());
    }
    if over_ssh(|k| std::env::var_os(k).is_some()) {
        return Err("over SSH, so open it on your own machine".into());
    }
    let program = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let status = tokio::process::Command::from(child_command(program))
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await
        .map_err(|e| format!("could not run {program}: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{program} exited with {status}"))
    }
}

/// Variables a child process never inherits, so the session never reaches the pager, the
/// editor, `git`, or the browser opener even when the user exported it.
const SESSION_VARS: [&str; 2] = ["CODER_SESSION_TOKEN", "CODER_URL"];

/// A child process for `program`, without the session variables.
pub fn child_command(program: &str) -> std::process::Command {
    let mut process = std::process::Command::new(program);
    for var in SESSION_VARS {
        process.env_remove(var);
    }
    process
}

/// The pager for diffs, in git's order: `$GIT_PAGER`, then `git config core.pager`, then
/// `$PAGER`, then `less -R`. The first one set wins. Git reads `cat` or an empty value as
/// "no pager", but output printed outside the full-screen UI vanishes when it resumes, so
/// those fall back to `less -R`.
pub fn pager_command(
    env: impl Fn(&str) -> Option<String>,
    git_pager: impl FnOnce() -> Option<String>,
) -> String {
    env("GIT_PAGER")
        .or_else(git_pager)
        .or_else(|| env("PAGER"))
        .filter(|p| !p.trim().is_empty() && p.trim() != "cat")
        .unwrap_or_else(|| "less -R".into())
}

/// The variables the pager runs with, as git sets them when they are unset, except that
/// `LESS` leaves out git's `F` and `X`: `less` would quit at once on a short diff, and the
/// full-screen UI would cover it before it could be read.
pub fn pager_env(is_set: impl Fn(&str) -> bool) -> Vec<(&'static str, &'static str)> {
    [("LESS", "R"), ("LV", "-c")]
        .into_iter()
        .filter(|(k, _)| !is_set(k))
        .collect()
}

/// `git config core.pager` in the current directory, if git is installed and it is set, even
/// to an empty value.
pub fn git_config_pager() -> Option<String> {
    let out = child_command("git")
        .args(["config", "core.pager"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    let pager = String::from_utf8(out.stdout).ok()?.trim().to_owned();
    out.status.success().then_some(pager)
}

/// Runs `command` through `sh` with `envs` added to its environment and `text` on its
/// standard input, and waits for it. A pager the user quits early closes its input, so a
/// failed write is not an error.
pub fn run_pager(command: &str, envs: &[(&str, &str)], text: &str) -> std::io::Result<()> {
    use std::io::Write;
    let mut child = pager_process(command, envs).spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(text.as_bytes());
    }
    let status = child.wait()?;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "the pager exited with {status}"
        )))
    }
}

/// The pager process: `command` through `sh`, with `envs` added and a piped standard input.
fn pager_process(command: &str, envs: &[(&str, &str)]) -> std::process::Command {
    let mut process = child_command("sh");
    process
        .arg("-c")
        .arg(command)
        .envs(envs.iter().copied())
        .stdin(std::process::Stdio::piped());
    process
}

/// Shows `text` in the user's pager, resolved by `pager_command`, and waits for it to exit.
/// The caller hands the terminal over first and takes it back afterward.
pub fn page(text: &str) -> std::io::Result<()> {
    let command = pager_command(|k| std::env::var(k).ok(), git_config_pager);
    let envs = pager_env(|k| std::env::var_os(k).is_some());
    run_pager(&command, &envs, text)
}

/// Opens a link from the transcript. Only web links open, and in normalized form.
async fn open_link(url: &str) -> Result<(), String> {
    match links::web_link(url) {
        Some(parsed) => open_in_browser(parsed.as_str()).await,
        None => Err("only http and https links open in a browser".into()),
    }
}

/// What stands in for the session token in error text.
const REDACTED: &str = "[redacted]";

/// Keeps the session token out of every error text the runtime hands the core. The token
/// travels only in a request header, but a server or proxy can echo it back in a response
/// body, and the generated client's errors carry the body's text as it came.
#[derive(Clone)]
struct Redactor(Arc<SecretString>);

impl Redactor {
    /// `e` with every occurrence of the session token in its text replaced by [`REDACTED`].
    fn error(&self, e: coder_sdk::Error) -> coder_sdk::Error {
        use coder_sdk::Error as E;
        let token = self.0.expose_secret();
        if token.is_empty() {
            return e;
        }
        let scrub = |text: String| text.replace(token, REDACTED);
        match e {
            E::Api {
                status,
                message,
                detail,
                validations,
            } => E::Api {
                status,
                message: scrub(message),
                detail: detail.map(scrub),
                validations: validations
                    .into_iter()
                    .map(|v| coder_sdk::Validation {
                        detail: scrub(v.detail),
                        ..v
                    })
                    .collect(),
            },
            E::Transport(text) => E::Transport(scrub(text)),
            E::Decode(text) => E::Decode(scrub(text)),
            E::NotLoggedIn(text) => E::NotLoggedIn(scrub(text)),
            E::StreamClosed { code, reason } => E::StreamClosed {
                code,
                reason: scrub(reason),
            },
            other @ (E::Unauthorized | E::InvalidToken) => other,
        }
    }

    /// A generated-client error as a redacted [`coder_sdk::Error`].
    async fn progenitor<E: serde::Serialize + std::fmt::Debug>(
        &self,
        e: progenitor_client::Error<E>,
    ) -> coder_sdk::Error {
        self.error(coder_sdk::Error::from_progenitor(e).await)
    }

    /// The user-facing text of a generated-client error.
    async fn err<E: serde::Serialize + std::fmt::Debug>(
        &self,
        e: progenitor_client::Error<E>,
    ) -> String {
        self.progenitor(e).await.to_string()
    }

    /// Like [`Redactor::err`], but an API error also carries its detail and each
    /// validation's detail, as sentences after the message. A refused chat search names the
    /// reason only in a validation.
    async fn err_detailed<E: serde::Serialize + std::fmt::Debug>(
        &self,
        e: progenitor_client::Error<E>,
    ) -> String {
        match self.progenitor(e).await {
            coder_sdk::Error::Api {
                message,
                detail,
                validations,
                ..
            } => std::iter::once(message)
                .chain(detail)
                .chain(validations.into_iter().map(|v| v.detail))
                .filter(|part| !part.trim().is_empty())
                .map(|part| {
                    let part = part.trim().to_owned();
                    if part.ends_with(['.', '!', '?']) {
                        part
                    } else {
                        format!("{part}.")
                    }
                })
                .collect::<Vec<_>>()
                .join(" "),
            other => other.to_string(),
        }
    }
}

/// Whether the server refused a send for its model: deleted, disabled, in another
/// organization, or behind a provider that is off or lacks credentials. The 400 carries no
/// code, detail, or validation that names the field, and other refusals share the status, so
/// the message is matched: both of the server's wordings start this way
/// (`validateUserChatModelConfigAvailability` and `chatd.ErrInvalidModelConfigID` in
/// `coderd/exp_chats.go`). The server checks the content and the MCP selection first, so a
/// 400 in this wording is about the model.
fn model_refused(e: &coder_sdk::Error) -> bool {
    match e {
        coder_sdk::Error::Api {
            status: 400,
            message,
            ..
        } => {
            let message = message.trim_start().to_ascii_lowercase();
            message.starts_with("invalid model_config_id")
                || message.starts_with("invalid model config id")
        }
        _ => false,
    }
}

/// What the router answers for a route it does not have (`httpapi.RouteNotFound` in
/// `coderd/httpapi/httpapi.go`).
const ROUTE_NOT_FOUND: &str = "Route not found.";

/// What a failed spend or quota request means for the limit: a `404` from a deployment
/// without the route, a `403` from one without the license, or a rejected session token.
/// Any other `404`, such as the quota route's answer for an organization the user cannot
/// read, fails only this request, since another organization may answer.
fn refusal(e: coder_sdk::Error) -> Refusal {
    match e {
        coder_sdk::Error::Api {
            status: 404,
            message,
            ..
        } if message == ROUTE_NOT_FOUND => Refusal::Absent,
        coder_sdk::Error::Api {
            status: 403,
            message,
            ..
        } => Refusal::Unlicensed(message),
        coder_sdk::Error::Unauthorized => Refusal::Unauthorized,
        other => Refusal::Failed(other.to_string()),
    }
}

type Job = Pin<Box<dyn Future<Output = Msg> + Send>>;

/// The user's organizations, in the server's order, which is not stable; pick one with
/// `scuttle_core::app::pick_organization`. With two or more, each is marked with whether
/// the user may create chats there, which costs those users one extra round trip at
/// startup, since the pick needs the answer before the first frame.
pub async fn fetch_organizations(client: &Client) -> Result<Vec<OrgRef>, coder_sdk::Error> {
    let mut orgs: Vec<OrgRef> = match client.api().get_organizations_by_user("me").await {
        Ok(r) => r
            .into_inner()
            .into_iter()
            .map(|o| OrgRef {
                id: o.id,
                name: o.name.unwrap_or_default(),
                display_name: o.display_name.unwrap_or_default(),
                is_default: o.is_default,
                can_create_chats: true,
            })
            .collect(),
        Err(e) => return Err(coder_sdk::Error::from_progenitor(e).await),
    };
    // With one organization there is nothing to choose between, so skip the request.
    if orgs.len() > 1 {
        let denied = chat_denied(client, &orgs).await;
        for org in &mut orgs {
            org.can_create_chats = !denied.contains(&org.id);
        }
    }
    Ok(orgs)
}

/// The organizations where the server says the user may not create chats, from one
/// `POST /authcheck` with a check per organization: the check the web UI's
/// `permittedOrganizations` runs (`site/src/api/queries/organizations.ts:308-326`, called
/// from `AgentCreateForm.tsx:236-243`). Empty when the check fails, so it hides nothing.
async fn chat_denied(client: &Client, orgs: &[OrgRef]) -> HashSet<Uuid> {
    let checks = orgs
        .iter()
        .map(|o| {
            (
                o.id.to_string(),
                types::CodersdkAuthorizationCheck {
                    action: Some(types::CodersdkRbacAction("create".into())),
                    object: Some(types::CodersdkAuthorizationObject {
                        organization_id: Some(o.id.to_string()),
                        owner_id: Some("me".into()),
                        resource_type: Some(types::CodersdkRbacResource("chat".into())),
                        ..Default::default()
                    }),
                },
            )
        })
        .collect();
    let body = types::CodersdkAuthorizationRequest { checks };
    match client.api().check_authorization(&body).await {
        Ok(r) => r
            .into_inner()
            .0
            .into_iter()
            .filter(|(_, allowed)| !allowed)
            .filter_map(|(id, _)| id.parse().ok())
            .collect(),
        Err(_) => HashSet::new(),
    }
}

impl Runtime {
    /// A runtime that calls the deployment through `client`, signed in with `token`, which
    /// it keeps only to redact out of error text.
    pub fn new(client: Client, token: SecretString, tx: UnboundedSender<Msg>) -> Runtime {
        Runtime {
            client,
            redact: Redactor(Arc::new(token)),
            tx,
            stream: None,
            watch: None,
            stream_generation: Arc::new(AtomicU64::new(0)),
            preview: None,
            preview_generation: Arc::new(AtomicU64::new(0)),
            git: None,
            git_target: None,
            uploads: HashMap::new(),
            healthy_after: STREAM_HEALTHY_AFTER,
            limits_timeout: LIMITS_TIMEOUT,
        }
    }

    /// The user's organizations; see [`fetch_organizations`].
    pub async fn organizations(&self) -> Result<Vec<OrgRef>, coder_sdk::Error> {
        fetch_organizations(&self.client)
            .await
            .map_err(|e| self.redact.error(e))
    }

    /// The server's version, for the skew warning at startup.
    pub async fn server_version(&self) -> Result<String, coder_sdk::Error> {
        self.client
            .server_version()
            .await
            .map_err(|e| self.redact.error(e))
    }

    /// Opens a chat stream in `slot` after `delay`, replacing the one there.
    fn open_stream(
        &mut self,
        slot: Slot,
        chat: Uuid,
        after_id: Option<i64>,
        delay: Duration,
        tag: u64,
    ) {
        let client = self.client.clone();
        let redact = self.redact.clone();
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
            let stream = match client.stream_chat(chat, after_id).await {
                Ok(s) => s,
                Err(e) => {
                    out.send(Msg::StreamEnded {
                        error: Some(redact.error(e).to_string()),
                    });
                    return;
                }
            };
            pump(stream, &redact, Some(healthy_after), |event| {
                out.send(match event {
                    Pumped::Item(ev) => Msg::Stream(ev),
                    Pumped::Healthy => Msg::StreamHealthy,
                    Pumped::Ended(error) => Msg::StreamEnded { error },
                })
            })
            .await;
        }));
    }

    /// Stops the stream in `slot`; the bump silences a task that is mid-send, and the core
    /// drops what is already queued.
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

    /// Opens the chat list watch after `delay`, replacing any open one. Every message is
    /// tagged with the core's `generation`, because one already in the channel when the watch
    /// is replaced still arrives, and the core drops it.
    fn open_watch(&mut self, delay: Duration, generation: u64) {
        if let Some(old) = self.watch.take() {
            old.abort();
        }
        let client = self.client.clone();
        let redact = self.redact.clone();
        let tx = self.tx.clone();
        let send = move |msg: Msg| {
            let _ = tx.send(Msg::ForWatch {
                generation,
                msg: Box::new(msg),
            });
        };
        let healthy_after = self.healthy_after;
        self.watch = Some(tokio::spawn(async move {
            if !delay.is_zero() {
                tokio::time::sleep(delay + jitter()).await;
            }
            let stream = match client.watch_chats().await {
                Ok(s) => s,
                Err(e) => {
                    send(Msg::WatchEnded {
                        error: Some(redact.error(e).to_string()),
                    });
                    return;
                }
            };
            send(Msg::WatchConnected);
            pump(stream, &redact, Some(healthy_after), |event| {
                send(match event {
                    Pumped::Item(ev) => Msg::Watch(ev),
                    Pumped::Healthy => Msg::WatchHealthy,
                    Pumped::Ended(error) => Msg::WatchEnded { error },
                });
                true
            })
            .await;
        }));
    }

    /// Opens the local-changes socket of `chat`, replacing any open one. Every message is
    /// tagged with the chat and the core's `generation`, because one already in the channel
    /// when the panel closes still arrives, and the core drops it. The socket serves only the
    /// open panel, so it is never reopened here.
    fn open_git_watch(&mut self, chat: Uuid, generation: u64) {
        self.close_git_watch();
        self.git_target = Some((chat, generation));
        let client = self.client.clone();
        let redact = self.redact.clone();
        let tx = self.tx.clone();
        let send = move |msg: Msg| {
            let _ = tx.send(Msg::ForGit {
                chat,
                generation,
                msg: Box::new(msg),
            });
        };
        self.git = Some(tokio::spawn(async move {
            let stream = match client.watch_chat_git(chat).await {
                Ok(s) => s,
                Err(e) => {
                    // A refused upgrade carries the server's reason, such as a missing workspace.
                    let message = match redact.error(e) {
                        coder_sdk::Error::Api { message, .. } => message,
                        other => other.to_string(),
                    };
                    send(Msg::GitWatchEnded(message));
                    return;
                }
            };
            pump(stream, &redact, None, |event| {
                send(match event {
                    Pumped::Item(message) => Msg::GitChanges(Box::new(message)),
                    Pumped::Healthy => return true,
                    Pumped::Ended(error) => {
                        Msg::GitWatchEnded(error.unwrap_or_else(|| "the connection closed".into()))
                    }
                });
                true
            })
            .await;
        }));
    }

    /// Closes the `/git` socket for a terminal handoff, keeping what `resume_git_watch` needs
    /// to reopen it, so full diffs do not queue up while the main loop is blocked.
    pub fn pause_git_watch(&mut self) {
        if let Some(old) = self.git.take() {
            old.abort();
        }
    }

    /// Reopens the `/git` socket a handoff closed, with the same tag, since the panel it
    /// serves is still open. The server sends the current changes on connect, but nothing for
    /// a repository removed meanwhile, so `Msg::GitReopened` goes first to clear the rows.
    pub fn resume_git_watch(&mut self) {
        if self.git.is_none()
            && let Some((chat, generation)) = self.git_target
        {
            let _ = self.tx.send(Msg::ForGit {
                chat,
                generation,
                msg: Box::new(Msg::GitReopened),
            });
            self.open_git_watch(chat, generation);
        }
    }

    fn close_git_watch(&mut self) {
        self.git_target = None;
        if let Some(old) = self.git.take() {
            old.abort();
        }
    }

    fn spawn(&self, job: Job) {
        self.spawn_tracked(job);
    }

    /// `spawn`, returning the task so it can be aborted.
    fn spawn_tracked(&self, job: Job) -> JoinHandle<()> {
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let _ = tx.send(job.await);
        })
    }

    /// Runs one API effect in the background. Effects the UI owns are ignored here.
    pub fn run(&mut self, effect: Effect) {
        let client = self.client.clone();
        let redact = self.redact.clone();
        match effect {
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
            Effect::OpenWatch { delay, generation } => self.open_watch(delay, generation),
            Effect::OpenGitWatch { chat, generation } => self.open_git_watch(chat, generation),
            Effect::CloseGitWatch => self.close_git_watch(),
            Effect::SaveFile {
                file,
                name,
                to,
                conflict,
            } => self.spawn(Box::pin(save_chat_file(
                client, redact, file, name, to, conflict,
            ))),
            Effect::ReadFile { file, name } => {
                self.spawn(Box::pin(read_chat_text(client, redact, file, name)))
            }
            Effect::FetchDiff { chat, generation } => self.spawn(Box::pin(async move {
                let msg = match client.api().get_chat_diff_contents(&chat).await {
                    Ok(d) => Msg::DiffLoaded {
                        diff: Box::new(d.into_inner()),
                        generation,
                    },
                    Err(e) => Msg::DiffFailed {
                        message: redact.err(e).await,
                        generation,
                    },
                };
                Msg::ForChat {
                    chat,
                    msg: Box::new(msg),
                }
            })),
            Effect::RefreshChat { chat, generation } => self.spawn(Box::pin(async move {
                let msg = match client.api().get_chat_by_id(&chat).await {
                    Ok(c) => Msg::ChatRefreshed(Box::new(c.into_inner())),
                    Err(e) => Msg::ApiFailed {
                        action: scuttle_core::app::REFRESH_CHAT,
                        message: redact.err(e).await,
                    },
                };
                Msg::ForChat {
                    chat,
                    msg: Box::new(Msg::ForRefresh {
                        generation,
                        msg: Box::new(msg),
                    }),
                }
            })),
            Effect::LoadOlder {
                chat,
                before_id,
                generation,
            } => self.spawn(Box::pin(async move {
                let msg = match client
                    .api()
                    .list_chat_messages(
                        &chat,
                        None,
                        Some(before_id),
                        Some(scuttle_core::app::HISTORY_PAGE),
                    )
                    .await
                {
                    Ok(r) => {
                        let r = r.into_inner();
                        Msg::OlderLoaded {
                            messages: r.messages,
                            has_more: r.has_more.unwrap_or(false),
                            generation,
                        }
                    }
                    Err(e) => Msg::OlderFailed {
                        message: redact.err(e).await,
                        generation,
                    },
                };
                Msg::ForChat {
                    chat,
                    msg: Box::new(msg),
                }
            })),
            Effect::LoadChat(id) => self.spawn(Box::pin(async move {
                let chat = match client.api().get_chat_by_id(&id).await {
                    Ok(c) => c.into_inner(),
                    Err(e) => {
                        return Msg::ChatLoadFailed {
                            chat_id: id,
                            message: redact.err(e).await,
                        };
                    }
                };
                let page = match client
                    .api()
                    .list_chat_messages(&id, None, None, Some(scuttle_core::app::HISTORY_PAGE))
                    .await
                {
                    Ok(m) => m.into_inner(),
                    Err(e) => {
                        return Msg::ChatLoadFailed {
                            chat_id: id,
                            message: format!(
                                "could not load its messages: {}",
                                redact.err(e).await
                            ),
                        };
                    }
                };
                Msg::ChatLoaded {
                    chat: Box::new(chat),
                    messages: page.messages,
                    has_more: page.has_more,
                }
            })),
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
                        message: redact.err_detailed(e).await,
                    },
                }
            })),
            Effect::CreateChat {
                org,
                text,
                model,
                workspace,
                turn,
                seq,
            } => self.spawn(Box::pin(async move {
                let body = types::CodersdkCreateChatRequest {
                    organization_id: Some(org),
                    content: parts(&text, &turn.files),
                    model_config_id: model,
                    workspace_id: workspace,
                    reasoning_effort: turn.effort,
                    plan_mode: turn.plan_mode.map(plan_mode_value),
                    // The request type omits an empty list from the body, which the server
                    // reads as "only the required servers".
                    mcp_server_ids: turn.mcp_servers.unwrap_or_default(),
                    ..Default::default()
                };
                match client.api().create_chat(&body).await {
                    Ok(c) => Msg::ChatCreated(Box::new(c.into_inner())),
                    Err(e) => Msg::CreateFailed {
                        message: redact.err(e).await,
                        seq,
                    },
                }
            })),
            Effect::SendMessage {
                chat,
                text,
                model,
                busy,
                turn,
                seq,
            } => self.spawn(Box::pin(async move {
                let plan_mode = turn.plan_mode;
                let plan_generation = turn.plan_generation;
                let body = types::CodersdkCreateChatMessageRequest {
                    content: parts(&text, &turn.files),
                    model_config_id: model,
                    busy_behavior: Some(types::CodersdkChatBusyBehavior(busy.as_str().into())),
                    reasoning_effort: turn.effort,
                    plan_mode: plan_mode.map(plan_mode_value),
                    ..Default::default()
                };
                // The generated request drops an empty selection, which the server reads as
                // "turn every server off", so a selection goes through the call that keeps it.
                let sent = match turn.mcp_servers.as_deref() {
                    Some(ids) => {
                        client
                            .send_chat_message_with_mcp_servers(chat, &body, ids)
                            .await
                    }
                    None => match client.api().send_chat_message(&chat, &body).await {
                        Ok(_) => Ok(()),
                        Err(e) => Err(redact.progenitor(e).await),
                    },
                };
                let msg = match sent.map_err(|e| redact.error(e)) {
                    // A carried plan mode change settles like a PATCH.
                    Ok(()) => Msg::Sent {
                        seq,
                        then: Box::new(match plan_mode {
                            Some(on) => Msg::PlanModeApplied { on },
                            None => Msg::Refresh,
                        }),
                    },
                    // The core holds the text and these files while the user picks a model.
                    Err(e) if model_refused(&e) => Msg::ModelUnavailable {
                        text,
                        files: turn.files,
                        message: e.to_string(),
                        plan_mode,
                        seq,
                    },
                    Err(e) => Msg::SendFailed {
                        text,
                        // The server answers 400 for an MCP server id it will not accept,
                        // such as a server disabled since /mcp loaded.
                        mcp_rejected: turn.mcp_servers.is_some()
                            && matches!(e, coder_sdk::Error::Api { status: 400, .. }),
                        message: e.to_string(),
                        plan_mode,
                        seq,
                    },
                };
                // A carried change settles the core's plan-mode queue, so it names its request.
                match plan_mode {
                    Some(_) => Msg::ForPlan {
                        chat,
                        generation: plan_generation,
                        msg: Box::new(msg),
                    },
                    None => Msg::ForChat {
                        chat,
                        msg: Box::new(msg),
                    },
                }
            })),
            Effect::CancelUpload(local) => {
                if let Some(upload) = self.uploads.remove(&local) {
                    upload.abort();
                }
            }
            Effect::UploadFile { local, path, org } => {
                self.uploads.retain(|_, upload| !upload.is_finished());
                let upload = self.spawn_tracked(Box::pin(async move {
                    let name = std::path::Path::new(&path)
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or(&path)
                        .to_owned();
                    let failed = |message: String| Msg::UploadFailed { local, message };
                    let read = {
                        let (path, name) = (path.clone(), name.clone());
                        tokio::task::spawn_blocking(move || read_upload(&path, &name)).await
                    };
                    let bytes = match read {
                        Ok(Ok(bytes)) => bytes,
                        Ok(Err(message)) => return failed(message),
                        Err(e) => return failed(format!("could not read {path}: {e}")),
                    };
                    let size = bytes.len() as u64;
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
            Effect::Interrupt(chat) => self.spawn(Box::pin(async move {
                let msg = match client.api().interrupt_chat(&chat).await {
                    Ok(_) => Msg::Refresh,
                    Err(e) => Msg::ApiFailed {
                        action: "interrupt",
                        message: redact.err(e).await,
                    },
                };
                Msg::ForChat {
                    chat,
                    msg: Box::new(msg),
                }
            })),
            Effect::Compact(chat) => self.spawn(Box::pin(async move {
                let msg = match client.api().compact_chat(&chat).await {
                    Ok(_) => Msg::Refresh,
                    Err(e) => Msg::ApiFailed {
                        action: "compact the chat",
                        message: redact.err(e).await,
                    },
                };
                Msg::ForChat {
                    chat,
                    msg: Box::new(msg),
                }
            })),
            Effect::Clear(chat) => self.spawn(Box::pin(async move {
                let msg = match client.api().clear_chat_context(&chat).await {
                    Ok(_) => Msg::Refresh,
                    Err(e) => Msg::ApiFailed {
                        action: "clear the context",
                        message: redact.err(e).await,
                    },
                };
                Msg::ForChat {
                    chat,
                    msg: Box::new(msg),
                }
            })),
            Effect::SetWorkspace { chat, workspace } => self.spawn(Box::pin(async move {
                // The nil UUID detaches the workspace; `None` would mean "no change".
                let body = types::CodersdkUpdateChatRequest {
                    workspace_id: Some(workspace.unwrap_or(Uuid::nil())),
                    ..Default::default()
                };
                let msg = match client.api().update_chat(&chat, &body).await {
                    Ok(_) => Msg::Refresh,
                    Err(e) => Msg::ApiFailed {
                        action: "change the workspace",
                        message: redact.err(e).await,
                    },
                };
                Msg::ForChat {
                    chat,
                    msg: Box::new(msg),
                }
            })),
            Effect::SetPlanMode {
                chat,
                on,
                generation,
            } => self.spawn(Box::pin(async move {
                let body = types::CodersdkUpdateChatRequest {
                    plan_mode: Some(plan_mode_value(on)),
                    ..Default::default()
                };
                let msg = match client.api().update_chat(&chat, &body).await {
                    Ok(_) => Msg::PlanModeApplied { on },
                    Err(e) => Msg::PlanModeFailed {
                        on,
                        message: redact.err(e).await,
                    },
                };
                Msg::ForPlan {
                    chat,
                    generation,
                    msg: Box::new(msg),
                }
            })),
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
                        message: redact.err(e).await,
                    },
                }
            })),
            Effect::ArchiveAndDeleteWorkspace { chat, workspace } => {
                self.spawn(Box::pin(async move {
                    let archive = types::CodersdkUpdateChatRequest {
                        archived: Some(true),
                        ..Default::default()
                    };
                    // The archive goes first: the server refuses it while the family runs, and
                    // then nothing is deleted.
                    if let Err(e) = client.api().update_chat(&chat, &archive).await {
                        return Msg::ChatUpdateFailed {
                            chat,
                            change: ChatChange::Archived(true),
                            message: redact.err(e).await,
                        };
                    }
                    let delete = types::CodersdkCreateWorkspaceBuildRequest {
                        transition: types::CodersdkWorkspaceTransition("delete".into()),
                        dry_run: None,
                        log_level: None,
                        on_success: None,
                        orphan: None,
                        reason: None,
                        rich_parameter_values: Vec::new(),
                        state: None,
                        template_version_id: None,
                        template_version_preset_id: None,
                    };
                    let outcome = match client
                        .api()
                        .create_workspace_build(&workspace, &delete)
                        .await
                    {
                        Ok(build) => WorkspaceDeletion::Started {
                            no_provisioner: build
                                .into_inner()
                                .matched_provisioners
                                .and_then(|m| m.count)
                                == Some(0),
                        },
                        Err(e) => match redact.progenitor(e).await {
                            // Already deleted, or hidden from this user, which the web UI
                            // treats the same way (`isWorkspaceNotFound`).
                            coder_sdk::Error::Api {
                                status: 404 | 410, ..
                            } => WorkspaceDeletion::AlreadyGone,
                            other => WorkspaceDeletion::Failed(other.to_string()),
                        },
                    };
                    Msg::ArchivedWithWorkspace { chat, outcome }
                }))
            }
            Effect::ProposeTitle { chat, generation } => self.spawn(Box::pin(async move {
                let msg = match client.api().propose_chat_title(&chat).await {
                    Ok(r) => Msg::TitleProposed {
                        title: r.into_inner().title.unwrap_or_default(),
                        generation,
                    },
                    Err(e) => Msg::TitleProposeFailed {
                        message: redact.err(e).await,
                        generation,
                    },
                };
                Msg::ForChat {
                    chat,
                    msg: Box::new(msg),
                }
            })),
            Effect::OpenWeb(chat) => {
                let url = chat_web_url(client.base_url(), chat).to_string();
                self.spawn(Box::pin(async move {
                    let outcome = open_in_browser(&url).await;
                    Msg::WebOpened { url, outcome }
                }));
            }
            Effect::OpenLink(url) => self.spawn(Box::pin(async move {
                // Report a web link in the normalized form the opener gets, so the notice names
                // the real destination, such as the punycode form of a lookalike host. Any other
                // text never reaches the opener and is copied as written.
                let url = links::web_link(&url).map_or(url, |u| u.to_string());
                let outcome = open_link(&url).await;
                Msg::LinkOpened { url, outcome }
            })),
            Effect::OpenWorkspaceWeb { owner, workspace } => {
                let url = workspace_web_url(client.base_url(), &owner, &workspace).to_string();
                // Answered as a link, so the fallback copy never calls it the chat URL.
                self.spawn(Box::pin(async move {
                    let outcome = open_in_browser(&url).await;
                    Msg::LinkOpened { url, outcome }
                }));
            }
            Effect::FetchWorkspaceDetails { chat, workspace } => self.spawn(Box::pin(async move {
                let msg = match client
                    .api()
                    .get_workspace_metadata_by_id(&workspace, None, None)
                    .await
                {
                    Ok(w) => Msg::WorkspaceDetailsLoaded(Box::new(w.into_inner())),
                    Err(e) => Msg::WorkspaceDetailsFailed {
                        workspace,
                        message: redact.err(e).await,
                    },
                };
                Msg::ForChat {
                    chat,
                    msg: Box::new(msg),
                }
            })),
            Effect::FetchSshSuffix => self.spawn(Box::pin(async move {
                match client.api().ssh_config().await {
                    Ok(c) => Msg::SshSuffixLoaded(
                        c.into_inner().hostname_suffix.filter(|s| !s.is_empty()),
                    ),
                    Err(_) => Msg::SshSuffixFailed,
                }
            })),
            Effect::FetchPrefs => self.spawn(Box::pin(async move {
                match client.api().get_user_preference_settings("me").await {
                    Ok(p) => Msg::PrefsLoaded(DisplayPrefs::from(&p.into_inner())),
                    Err(e) => Msg::ApiFailed {
                        action: "load display preferences",
                        message: redact.err(e).await,
                    },
                }
            })),
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
                        message: redact.err(e).await,
                    },
                }
            })),
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
                    Err(e) => Msg::SkillsFailed(redact.err(e).await),
                }
            })),
            Effect::FetchOrganizations => self.spawn(Box::pin(async move {
                match fetch_organizations(&client).await {
                    Ok(organizations) => Msg::OrganizationsLoaded(organizations),
                    Err(e) => Msg::OrganizationsFailed {
                        message: redact.error(e).to_string(),
                        open_chat: None,
                    },
                }
            })),
            Effect::FetchModels(org) => self.spawn(Box::pin(async move {
                let msg = match client
                    .api()
                    .list_ai_models_and_provider_descriptors_in_an_organization(&org.to_string())
                    .await
                {
                    Ok(r) => Msg::CatalogLoaded(Box::new(r.into_inner())),
                    Err(e) => Msg::ModelsFailed {
                        message: redact.err(e).await,
                    },
                };
                Msg::ForOrg {
                    org,
                    msg: Box::new(msg),
                }
            })),
            Effect::FetchWorkspaces(org) => self.spawn(Box::pin(async move {
                let query = format!("owner:me organization:{org}");
                let msg = match client
                    .api()
                    .list_workspaces(Some(100), None, Some(query.as_str()))
                    .await
                {
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
                        message: redact.err(e).await,
                    },
                };
                Msg::ForOrg {
                    org,
                    msg: Box::new(msg),
                }
            })),
            Effect::FetchThresholds { generation } => self.spawn(Box::pin(async move {
                match client.api().get_user_chat_compaction_thresholds().await {
                    Ok(r) => Msg::ThresholdsLoaded {
                        thresholds: r
                            .into_inner()
                            .thresholds
                            .into_iter()
                            .filter_map(|t| Some((t.model_config_id?, t.threshold_percent?)))
                            .collect(),
                        generation,
                    },
                    Err(e) => Msg::ThresholdsFailed {
                        message: redact.err(e).await,
                        generation,
                    },
                }
            })),
            Effect::SaveThreshold(Save {
                model,
                change,
                generation,
            }) => self.spawn(Box::pin(async move {
                let saved = match change {
                    Change::Set(percent) => {
                        let body = types::CodersdkUpdateUserChatCompactionThresholdRequest {
                            threshold_percent: Some(percent),
                        };
                        client
                            .api()
                            .update_user_chat_compaction_threshold(&model, &body)
                            .await
                            .map(|r| Some(r.into_inner().threshold_percent.unwrap_or(percent)))
                    }
                    // A 204 with no body, also when no override was stored.
                    Change::Reset => client
                        .api()
                        .delete_user_chat_compaction_threshold(&model)
                        .await
                        .map(|_| None),
                };
                match saved {
                    Ok(percent) => Msg::ThresholdSaved {
                        model,
                        percent,
                        generation,
                    },
                    // A refused percent or a disabled model is named only in the detail.
                    Err(e) => Msg::ThresholdFailed {
                        model,
                        message: redact.err_detailed(e).await,
                        generation,
                    },
                }
            })),
            Effect::DeleteQueued { chat, id } => self.spawn(Box::pin(async move {
                let msg = match client.api().delete_chat_queued_message(&chat, id).await {
                    Ok(_) => Msg::Refresh,
                    Err(e) => Msg::ApiFailed {
                        action: "remove the queued message",
                        message: redact.err(e).await,
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
                        action: scuttle_core::app::PROMOTE_QUEUED,
                        message: redact.err(e).await,
                    },
                };
                Msg::ForChat {
                    chat,
                    msg: Box::new(msg),
                }
            })),
            Effect::FetchCost { chat, generation } => self.spawn(Box::pin(async move {
                let msg = match client.api().get_chat_cost(&chat).await {
                    Ok(r) => Msg::CostLoaded {
                        cost: r.into_inner(),
                        generation,
                    },
                    Err(e) => match redact.progenitor(e).await {
                        coder_sdk::Error::Api {
                            status: 403 | 404, ..
                        } => Msg::CostHidden { generation },
                        coder_sdk::Error::Unauthorized => Msg::CostUnauthorized { generation },
                        other => Msg::CostFailed {
                            message: other.to_string(),
                            generation,
                        },
                    },
                };
                Msg::ForChat {
                    chat,
                    msg: Box::new(msg),
                }
            })),
            // Neither limit belongs to a chat, so the replies go back unwrapped, tagged only
            // with the core's generation.
            Effect::FetchSpend { generation } => {
                let timeout = self.limits_timeout;
                self.spawn(Box::pin(async move {
                    let failed = |refusal| Msg::LimitFailed {
                        limit: Limit::Spend,
                        refusal,
                        generation,
                    };
                    match tokio::time::timeout(timeout, client.api().get_user_ai_spend("me")).await
                    {
                        Ok(Ok(r)) => Msg::SpendLoaded {
                            spend: Box::new(r.into_inner()),
                            generation,
                        },
                        Ok(Err(e)) => failed(refusal(redact.progenitor(e).await)),
                        Err(_) => failed(Refusal::Failed(LIMITS_TIMED_OUT.into())),
                    }
                }))
            }
            Effect::FetchQuota { org, generation } => {
                let timeout = self.limits_timeout;
                self.spawn(Box::pin(async move {
                    let failed = |refusal| Msg::LimitFailed {
                        limit: Limit::Quota,
                        refusal,
                        generation,
                    };
                    let request = client.api().get_workspace_quota_by_user(&org, "me");
                    match tokio::time::timeout(timeout, request).await {
                        Ok(Ok(r)) => Msg::QuotaLoaded {
                            org,
                            quota: r.into_inner(),
                            generation,
                        },
                        Ok(Err(e)) => failed(refusal(redact.progenitor(e).await)),
                        Err(_) => failed(Refusal::Failed(LIMITS_TIMED_OUT.into())),
                    }
                }))
            }
            Effect::FetchMcpServers {
                chat,
                org,
                generation,
            } => self.spawn(Box::pin(async move {
                let msg = match client.api().list_mcp_server_configs(&org.to_string()).await {
                    Ok(r) => Msg::McpServersLoaded {
                        servers: r.into_inner(),
                        generation,
                    },
                    Err(e) => Msg::McpServersFailed {
                        message: redact.err(e).await,
                        generation,
                    },
                };
                Msg::ForChat {
                    chat,
                    msg: Box::new(msg),
                }
            })),
            Effect::FetchOrgMcpServers(org) => self.spawn(Box::pin(async move {
                let msg = match client.api().list_mcp_server_configs(&org.to_string()).await {
                    Ok(r) => Msg::OrgMcpLoaded(r.into_inner()),
                    Err(e) => Msg::OrgMcpFailed(redact.err(e).await),
                };
                Msg::ForOrg {
                    org,
                    msg: Box::new(msg),
                }
            })),
            Effect::FetchMcpHealth { chat, generation } => self.spawn(Box::pin(async move {
                // The debug runs are read only while debug logging is on, since they are the
                // only record of connect outcomes; an unreadable setting counts as off.
                let enabled = match client.api().get_user_chat_debug_logging_setting().await {
                    Ok(r) => r.into_inner().debug_logging_enabled == Some(true),
                    Err(_) => false,
                };
                let msg = if !enabled {
                    Msg::McpHealthLoaded {
                        outcomes: None,
                        generation,
                    }
                } else {
                    match client.latest_mcp_connect(chat).await {
                        Ok(outcomes) => Msg::McpHealthLoaded {
                            outcomes,
                            generation,
                        },
                        // The server hides the runs it will not show this user behind a 404,
                        // so health is unknown rather than failed.
                        Err(coder_sdk::Error::Api {
                            status: 403 | 404, ..
                        }) => Msg::McpHealthLoaded {
                            outcomes: None,
                            generation,
                        },
                        Err(e) => Msg::McpHealthFailed {
                            message: redact.error(e).to_string(),
                            generation,
                        },
                    }
                };
                Msg::ForChat {
                    chat,
                    msg: Box::new(msg),
                }
            })),
            Effect::ShowPicker(_)
            | Effect::ShowHelp
            | Effect::ShowChats(_)
            | Effect::ShowSubagents
            | Effect::ShowQueue
            | Effect::ShowInfo
            | Effect::ShowWorkspace
            | Effect::ShowGit
            | Effect::ShowMcp
            | Effect::ShowStatusline
            | Effect::ShowUsage
            | Effect::Page(_)
            | Effect::CopyText { .. }
            | Effect::Copy(_)
            | Effect::CopyWebUrl(_)
            | Effect::CopyLink(_)
            | Effect::SetMouse(_)
            | Effect::EditSettings
            | Effect::SaveOrganization(_)
            | Effect::SaveEffort { .. }
            | Effect::RestoreComposer(_)
            | Effect::ClearView
            | Effect::ShowFiles
            | Effect::ScrollToMessage(_)
            | Effect::ScrollToLatest => {}
            Effect::Quit => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::SinkExt;
    use scuttle_core::app::TurnOptions;
    use scuttle_core::files::{OnConflict, SaveTo};
    use secrecy::SecretString;
    use std::path::{Path, PathBuf};
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
        (Runtime::new(client(url), SecretString::from(TOKEN), tx), rx)
    }

    async fn next(rx: &mut UnboundedReceiver<Msg>) -> Msg {
        tokio::time::timeout(WAIT, rx.recv())
            .await
            .expect("a message within the timeout")
            .expect("the channel is open")
    }

    /// The message inside a `Msg::ForChat`, `Msg::ForStream`, `Msg::ForPlan`, or
    /// `Msg::ForGit`, or the message itself.
    fn untag(msg: Msg) -> Msg {
        match msg {
            Msg::ForChat { msg, .. }
            | Msg::ForStream { msg, .. }
            | Msg::ForPlan { msg, .. }
            | Msg::ForGit { msg, .. } => untag(*msg),
            Msg::Sent { then, .. } => untag(*then),
            other => other,
        }
    }

    fn api_error(status: u16, message: &str) -> ResponseTemplate {
        ResponseTemplate::new(status).set_body_json(serde_json::json!({ "message": message }))
    }

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
        rt.run(Effect::FetchMcpHealth {
            chat,
            generation: 1,
        });
        assert!(matches!(
            untag(next(&mut rx).await),
            Msg::McpHealthLoaded {
                outcomes: None,
                generation: 1
            }
        ));
    }

    #[tokio::test]
    async fn hidden_or_unrecorded_mcp_health_is_unknown_and_not_an_error() {
        let server = MockServer::start().await;
        let (hidden, quiet, broken) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        Mock::given(method("GET"))
            .and(path("/api/v2/chats/config/user-debug-logging"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "debug_logging_enabled": true, "user_toggle_allowed": true, "forced_by_deployment": false
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/api/experimental/chats/{hidden}/debug/runs")))
            .respond_with(api_error(404, "Resource not found"))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/api/experimental/chats/{quiet}/debug/runs")))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/api/experimental/chats/{broken}/debug/runs")))
            .respond_with(api_error(500, "Boom."))
            .mount(&server)
            .await;
        let (mut rt, mut rx) = runtime(&server.uri());
        // A 404 means the runs are not visible to this user or the chat is gone.
        rt.run(Effect::FetchMcpHealth {
            chat: hidden,
            generation: 2,
        });
        let reply = next(&mut rx).await;
        assert!(
            matches!(&reply, Msg::ForChat { chat, .. } if *chat == hidden),
            "{reply:?}"
        );
        assert!(matches!(
            untag(reply),
            Msg::McpHealthLoaded {
                outcomes: None,
                generation: 2
            }
        ));
        rt.run(Effect::FetchMcpHealth {
            chat: quiet,
            generation: 3,
        });
        assert!(matches!(
            untag(next(&mut rx).await),
            Msg::McpHealthLoaded {
                outcomes: None,
                generation: 3
            }
        ));
        rt.run(Effect::FetchMcpHealth {
            chat: broken,
            generation: 4,
        });
        assert!(matches!(
            untag(next(&mut rx).await),
            Msg::McpHealthFailed { message, generation: 4 } if message == "Boom."
        ));
    }

    #[tokio::test]
    async fn mcp_servers_are_listed_for_the_organization_with_the_requests_generation() {
        let server = MockServer::start().await;
        let (chat, org) = (Uuid::new_v4(), Uuid::new_v4());
        Mock::given(method("GET"))
            .and(path(format!("/api/v2/organizations/{org}/mcp-servers")))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"id": Uuid::new_v4(), "slug": "github", "tool_allow_list": [], "tool_deny_list": []}
            ])))
            .mount(&server)
            .await;
        let (mut rt, mut rx) = runtime(&server.uri());
        rt.run(Effect::FetchMcpServers {
            chat,
            org,
            generation: 7,
        });
        let reply = next(&mut rx).await;
        assert!(
            matches!(&reply, Msg::ForChat { chat: c, .. } if *c == chat),
            "{reply:?}"
        );
        assert!(matches!(
            untag(reply),
            Msg::McpServersLoaded { servers, generation: 7 }
                if servers.len() == 1 && servers[0].slug.as_deref() == Some("github")
        ));
    }

    #[tokio::test]
    async fn the_organizations_mcp_servers_come_back_tagged_with_the_organization() {
        let server = MockServer::start().await;
        let org = Uuid::new_v4();
        Mock::given(method("GET"))
            .and(path(format!("/api/v2/organizations/{org}/mcp-servers")))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"id": Uuid::new_v4(), "slug": "github", "tool_allow_list": [], "tool_deny_list": []}
            ])))
            .mount(&server)
            .await;
        let (mut rt, mut rx) = runtime(&server.uri());
        rt.run(Effect::FetchOrgMcpServers(org));
        match next(&mut rx).await {
            Msg::ForOrg { org: tagged, msg } => {
                assert_eq!(tagged, org);
                assert!(
                    matches!(*msg, Msg::OrgMcpLoaded(ref servers) if servers.len() == 1),
                    "{msg:?}"
                );
            }
            other => panic!("expected ForOrg, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn create_chat_carries_the_mcp_selection() {
        let server = MockServer::start().await;
        let (id, org, github) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        Mock::given(method("POST"))
            .and(path("/api/v2/chats"))
            .and(wiremock::matchers::body_partial_json(serde_json::json!({
                "organization_id": org,
                "mcp_server_ids": [github],
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "id": id, "children": [], "files": [], "mcp_server_ids": [github],
                "inline_mcp_servers": [], "labels": {}
            })))
            .expect(1)
            .mount(&server)
            .await;
        let (mut rt, mut rx) = runtime(&server.uri());
        rt.run(Effect::CreateChat {
            org,
            text: "hello".into(),
            model: None,
            workspace: None,
            turn: TurnOptions {
                mcp_servers: Some(vec![github]),
                ..TurnOptions::default()
            },
            seq: 0,
        });
        match next(&mut rx).await {
            Msg::ChatCreated(chat) => assert_eq!(chat.id, Some(id)),
            other => panic!("expected ChatCreated, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_diff_reply_carries_its_requests_generation() {
        let server = MockServer::start().await;
        let (found, broken) = (Uuid::new_v4(), Uuid::new_v4());
        Mock::given(method("GET"))
            .and(path(format!("/api/v2/chats/{found}/diff")))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"diff": "+x\n"})),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/api/v2/chats/{broken}/diff")))
            .respond_with(api_error(500, "Boom."))
            .mount(&server)
            .await;
        let (mut rt, mut rx) = runtime(&server.uri());
        rt.run(Effect::FetchDiff {
            chat: found,
            generation: 4,
        });
        let reply = next(&mut rx).await;
        assert!(
            matches!(&reply, Msg::ForChat { chat, .. } if *chat == found),
            "{reply:?}"
        );
        assert!(matches!(
            untag(reply),
            Msg::DiffLoaded { diff, generation: 4 } if diff.diff.as_deref() == Some("+x\n")
        ));
        rt.run(Effect::FetchDiff {
            chat: broken,
            generation: 5,
        });
        assert!(matches!(
            untag(next(&mut rx).await),
            Msg::DiffFailed { generation: 5, .. }
        ));
    }

    #[test]
    fn the_pager_follows_gits_order() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| {
                pairs
                    .iter()
                    .find(|(key, _)| *key == k)
                    .map(|(_, v)| v.to_string())
            }
        };
        assert_eq!(
            pager_command(env(&[("GIT_PAGER", "delta"), ("PAGER", "more")]), || Some(
                "bat".into()
            )),
            "delta"
        );
        assert_eq!(
            pager_command(env(&[("PAGER", "more")]), || Some("bat".into())),
            "bat"
        );
        assert_eq!(pager_command(env(&[("PAGER", "more")]), || None), "more");
        assert_eq!(pager_command(env(&[]), || None), "less -R");
    }

    #[test]
    fn the_pager_gets_the_text_on_its_standard_input() {
        let out = std::env::temp_dir().join(format!("scuttle-pager-{}", Uuid::new_v4()));
        run_pager(&format!("cat > '{}'", out.display()), &[], "+new line\n").unwrap();
        assert_eq!(std::fs::read_to_string(&out).unwrap(), "+new line\n");
        std::fs::remove_file(out).unwrap();
    }

    #[test]
    fn a_pager_of_cat_or_nothing_falls_back_to_less() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| {
                pairs
                    .iter()
                    .find(|(key, _)| *key == k)
                    .map(|(_, v)| v.to_string())
            }
        };
        assert_eq!(
            pager_command(env(&[("GIT_PAGER", "cat")]), || None),
            "less -R"
        );
        assert_eq!(
            pager_command(env(&[("GIT_PAGER", " "), ("PAGER", "more")]), || None),
            "less -R"
        );
        assert_eq!(pager_command(env(&[]), || Some(String::new())), "less -R");
        assert_eq!(pager_command(env(&[("PAGER", "cat")]), || None), "less -R");
    }

    #[test]
    fn the_pager_keeps_a_short_diff_on_screen_unless_the_user_chose_otherwise() {
        assert_eq!(pager_env(|_| false), [("LESS", "R"), ("LV", "-c")]);
        assert_eq!(pager_env(|k| k == "LESS"), [("LV", "-c")]);
        assert!(pager_env(|_| true).is_empty());
    }

    #[test]
    fn the_pager_runs_with_the_environment_it_is_given() {
        let out = std::env::temp_dir().join(format!("scuttle-pager-env-{}", Uuid::new_v4()));
        run_pager(
            &format!("printf %s \"$LESS\" > '{}'", out.display()),
            &[("LESS", "R")],
            "",
        )
        .unwrap();
        assert_eq!(std::fs::read_to_string(&out).unwrap(), "R");
        std::fs::remove_file(out).unwrap();
    }

    #[test]
    fn every_child_runs_without_the_session_variables() {
        let removed = |process: &std::process::Command| {
            process
                .get_envs()
                .filter(|(_, value)| value.is_none())
                .map(|(key, _)| key.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
        };
        for process in [
            child_command("git"),
            pager_process("less -R", &[("LESS", "R")]),
        ] {
            let removed = removed(&process);
            assert!(
                removed.contains(&"CODER_SESSION_TOKEN".to_owned())
                    && removed.contains(&"CODER_URL".to_owned()),
                "{removed:?}"
            );
        }
    }

    #[test]
    fn a_pager_that_fails_is_an_error() {
        let failed = run_pager("exit 3", &[], "+x\n").unwrap_err();
        assert!(failed.to_string().contains('3'), "{failed}");
    }

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
        rt.run(Effect::FetchCost {
            chat: seen,
            generation: 7,
        });
        let reply = next(&mut rx).await;
        assert!(
            matches!(&reply, Msg::ForChat { chat, .. } if *chat == seen),
            "{reply:?}"
        );
        assert!(matches!(
            untag(reply),
            Msg::CostLoaded { cost, generation: 7 } if cost.request_count == Some(1)
        ));
        rt.run(Effect::FetchCost {
            chat: hidden,
            generation: 8,
        });
        assert!(matches!(
            untag(next(&mut rx).await),
            Msg::CostHidden { generation: 8 }
        ));
    }

    #[tokio::test]
    async fn a_rejected_token_on_the_cost_says_so() {
        let server = MockServer::start().await;
        let chat = Uuid::new_v4();
        Mock::given(method("GET"))
            .and(path(format!("/api/v2/chats/{chat}/cost")))
            .respond_with(api_error(401, "You must be logged in."))
            .mount(&server)
            .await;
        let (mut rt, mut rx) = runtime(&server.uri());
        rt.run(Effect::FetchCost {
            chat,
            generation: 3,
        });
        let reply = untag(next(&mut rx).await);
        assert!(
            matches!(reply, Msg::CostUnauthorized { generation: 3 }),
            "{reply:?}"
        );
    }

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
    async fn a_limit_slower_than_its_timeout_fails_so_the_next_refresh_asks_again() {
        let server = MockServer::start().await;
        let org = Uuid::new_v4();
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"credits_consumed": 3, "budget": 10}))
                    .set_delay(Duration::from_secs(5)),
            )
            .mount(&server)
            .await;
        let (mut rt, mut rx) = runtime(&server.uri());
        rt.limits_timeout = Duration::from_millis(50);
        rt.run(Effect::FetchSpend { generation: 1 });
        match next(&mut rx).await {
            Msg::LimitFailed {
                limit: Limit::Spend,
                refusal: Refusal::Failed(message),
                generation: 1,
            } => assert_eq!(message, LIMITS_TIMED_OUT),
            other => panic!("spend: {other:?}"),
        }
        rt.run(Effect::FetchQuota { org, generation: 2 });
        match next(&mut rx).await {
            Msg::LimitFailed {
                limit: Limit::Quota,
                refusal: Refusal::Failed(message),
                generation: 2,
            } => assert_eq!(message, LIMITS_TIMED_OUT),
            other => panic!("quota: {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_missing_unlicensed_or_rejected_limit_says_which() {
        let org = Uuid::new_v4();
        // The router's unknown-route `404` is "Route not found." (`httpapi.RouteNotFound`); the
        // organization middleware's `404` for one the user cannot read is another message.
        const NO_ACCESS: &str = "Resource not found or you do not have access to this resource";
        for (status, body, want) in [
            (404, "Route not found.", Refusal::Absent),
            (404, NO_ACCESS, Refusal::Failed(NO_ACCESS.into())),
            (404, "", Refusal::Failed("HTTP 404".into())),
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
                } => assert_eq!(refusal, want, "spend, HTTP {status} {body:?}"),
                other => panic!("spend, HTTP {status} {body:?}: {other:?}"),
            }
            rt.run(Effect::FetchQuota { org, generation: 2 });
            match next(&mut rx).await {
                Msg::LimitFailed {
                    limit: Limit::Quota,
                    refusal,
                    generation: 2,
                } => assert_eq!(refusal, want, "quota, HTTP {status} {body:?}"),
                other => panic!("quota, HTTP {status} {body:?}: {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn an_error_that_echoes_the_session_token_never_shows_it() {
        let echo = format!("the token {TOKEN} was refused");
        for status in [400, 404, 403] {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .respond_with(
                    ResponseTemplate::new(status).set_body_json(serde_json::json!({
                        "message": echo,
                        "detail": echo,
                        "validations": [{"field": "q", "detail": echo}]
                    })),
                )
                .mount(&server)
                .await;
            let (mut rt, mut rx) = runtime(&server.uri());
            rt.run(Effect::FetchSpend { generation: 1 });
            let spend = match next(&mut rx).await {
                Msg::LimitFailed {
                    refusal: Refusal::Failed(message) | Refusal::Unlicensed(message),
                    ..
                } => message,
                other => panic!("spend, HTTP {status}: {other:?}"),
            };
            rt.run(Effect::FetchChats {
                query: scuttle_core::chat_list::ListQuery::Archived,
                offset: 0,
            });
            let chats = match next(&mut rx).await {
                Msg::ChatsFailed { message, .. } => message,
                other => panic!("chats, HTTP {status}: {other:?}"),
            };
            for message in [spend, chats] {
                assert!(!message.contains(TOKEN), "HTTP {status}: {message}");
                assert!(message.contains("[redacted]"), "HTTP {status}: {message}");
            }
        }
    }

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
                assert_eq!(
                    message,
                    "huge.png is 10485761 bytes; the limit is 10485760 bytes."
                );
            }
            other => panic!("expected UploadFailed, got {other:?}"),
        }
    }

    #[test]
    fn a_fifo_is_refused_without_waiting_for_a_writer() {
        let fifo = std::env::temp_dir().join(format!("scuttle-fifo-{}", Uuid::new_v4()));
        let made = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap();
        assert!(made.success());
        let (tx, rx) = std::sync::mpsc::channel();
        let path = fifo.to_string_lossy().into_owned();
        std::thread::spawn(move || {
            let _ = tx.send(read_upload(&path, "pipe"));
        });
        let read = rx.recv_timeout(Duration::from_secs(5));
        // A writer releases a read still blocked in `open`, so the thread can end.
        if read.is_err() {
            let _ = std::fs::OpenOptions::new().write(true).open(&fifo);
        }
        std::fs::remove_file(&fifo).unwrap();
        assert_eq!(
            read.expect("read_upload returned without a writer"),
            Err("pipe is not a file.".into())
        );
    }

    #[tokio::test]
    async fn cancelling_an_upload_stops_it_before_it_reports() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v2/chats/files"))
            .respond_with(
                ResponseTemplate::new(201)
                    .set_body_json(serde_json::json!({"id": Uuid::new_v4()}))
                    .set_delay(Duration::from_millis(300)),
            )
            .mount(&server)
            .await;
        let (mut rt, mut rx) = runtime(&server.uri());
        rt.run(Effect::UploadFile {
            local: 4,
            path: temp_file("slow.md", 5),
            org: Uuid::new_v4(),
        });
        rt.run(Effect::CancelUpload(4));
        assert!(
            tokio::time::timeout(Duration::from_millis(1500), rx.recv())
                .await
                .is_err(),
            "a cancelled upload reports nothing"
        );
    }

    #[test]
    fn a_size_over_the_limit_reads_in_mib_unless_it_rounds_to_the_limit() {
        assert_eq!(
            too_big("big.png", 12 * 1024 * 1024),
            "big.png is 12.0 MiB; the limit is 10 MiB."
        );
        assert_eq!(
            too_big("huge.png", scuttle_core::attachments::MAX_FILE_BYTES + 1),
            "huge.png is 10485761 bytes; the limit is 10485760 bytes."
        );
    }

    #[test]
    fn a_file_that_grew_past_the_limit_is_still_refused() {
        let max = scuttle_core::attachments::MAX_FILE_BYTES as usize;
        let at_limit = read_capped(std::io::Cursor::new(vec![0u8; max]), "a.txt");
        assert_eq!(at_limit.map(|b| b.len()), Ok(max));
        assert_eq!(
            read_capped(std::io::Cursor::new(vec![0u8; max + 1]), "a.txt"),
            Err("a.txt grew past the 10 MiB limit while it was read.".into())
        );
    }

    #[test]
    fn the_file_name_header_escapes_nothing_the_server_would_unquote() {
        assert_eq!(
            content_disposition("a\\b\"c\u{e9}.txt"),
            "attachment; filename=\"a_b_c_.txt\""
        );
    }

    #[tokio::test]
    async fn a_file_uploads_with_its_name_and_messages_carry_file_parts() {
        use wiremock::matchers::{header, query_param};
        let server = MockServer::start().await;
        let (org, file) = (Uuid::new_v4(), Uuid::new_v4());
        Mock::given(method("POST"))
            .and(path("/api/v2/chats/files"))
            .and(query_param("organization", org.to_string()))
            .and(header(
                "content-disposition",
                "attachment; filename=\"notes.md\"",
            ))
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
        assert_eq!(
            serde_json::to_value(super::parts("", &[file])).unwrap(),
            serde_json::json!([{"type": "file", "file_id": file}]),
            "a message of only a file has no empty text part"
        );
    }

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

    #[tokio::test]
    async fn open_web_reports_the_chat_url() {
        let server = MockServer::start().await;
        let (mut rt, mut rx) = runtime(&server.uri());
        let chat = Uuid::new_v4();
        rt.run(Effect::OpenWeb(chat));
        match next(&mut rx).await {
            Msg::WebOpened { url, outcome } => {
                assert_eq!(url, format!("{}/agents/{chat}", server.uri()));
                assert!(outcome.is_err(), "tests never launch a browser");
            }
            other => panic!("expected WebOpened, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn open_link_reports_the_link() {
        let server = MockServer::start().await;
        let (mut rt, mut rx) = runtime(&server.uri());
        rt.run(Effect::OpenLink("https://coder.com/docs".into()));
        match next(&mut rx).await {
            Msg::LinkOpened { url, outcome } => {
                assert_eq!(url, "https://coder.com/docs");
                assert_eq!(
                    outcome,
                    Err("not opened".into()),
                    "tests never launch a browser"
                );
            }
            other => panic!("expected LinkOpened, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn an_opened_link_names_its_real_destination() {
        let server = MockServer::start().await;
        let (mut rt, mut rx) = runtime(&server.uri());
        // The first letter is Cyrillic, so the browser goes to the punycode host.
        rt.run(Effect::OpenLink("https://\u{430}pple.com".into()));
        let url = match next(&mut rx).await {
            Msg::LinkOpened { url, .. } => url,
            other => panic!("expected LinkOpened, got {other:?}"),
        };
        let mut app = scuttle_core::app::App::new(scuttle_core::config::BusyBehavior::Queue, true);
        app.update(Msg::LinkOpened {
            url,
            outcome: Ok(()),
        });
        assert_eq!(
            app.notices.last(),
            Some(&scuttle_core::app::Notice::Info(
                "Opened https://xn--pple-43d.com/".into()
            ))
        );
    }

    #[tokio::test]
    async fn only_web_links_reach_the_browser() {
        for url in [
            "file:///etc/passwd",
            "javascript:alert(1)",
            "-a Calculator",
            "docs/setup.md",
        ] {
            assert_eq!(
                open_link(url).await,
                Err("only http and https links open in a browser".into()),
                "{url}"
            );
        }
        assert_eq!(
            open_link("https://coder.com").await,
            Err("not opened".into()),
            "a web link gets as far as the opener"
        );
    }

    #[test]
    fn the_web_url_keeps_only_the_origin() {
        let base: url::Url = "https://user:pw@coder.example.com:8443/coder/?q=1#f"
            .parse()
            .unwrap();
        assert_eq!(
            chat_web_url(&base, Uuid::nil()).as_str(),
            "https://coder.example.com:8443/agents/00000000-0000-0000-0000-000000000000"
        );
    }

    #[test]
    fn ssh_sessions_are_detected() {
        assert!(over_ssh(|k| k == "SSH_CONNECTION"));
        assert!(over_ssh(|k| k == "SSH_TTY"));
        assert!(!over_ssh(|_| false));
    }

    #[tokio::test]
    async fn a_failed_stream_open_ends_the_stream() {
        let server = MockServer::start().await;
        let (mut rt, mut rx) = runtime(&server.uri());
        rt.run(Effect::OpenStream {
            chat: Uuid::new_v4(),
            after_id: None,
            generation: 1,
        });
        match untag(next(&mut rx).await) {
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
            generation: 1,
        });
        assert!(matches!(
            untag(next(&mut rx).await),
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
            turn: TurnOptions::default(),
            seq: 9,
        });
        match next(&mut rx).await {
            Msg::CreateFailed { message, seq } => {
                assert_eq!(seq, 9, "the failure names its request");
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
            turn: TurnOptions::default(),
            seq: 0,
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
            Msg::ForOrg { org: tagged, msg } => {
                assert_eq!(tagged, org);
                assert!(
                    matches!(*msg, Msg::ModelsFailed { ref message } if message.contains("not allowed")),
                    "{msg:?}"
                );
            }
            other => panic!("expected a tagged ModelsFailed, got {other:?}"),
        }
    }

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
                assert!(
                    matches!(*msg, Msg::CatalogLoaded(ref c) if c.providers.len() == 1),
                    "{msg:?}"
                )
            }
            other => panic!("expected a tagged catalog, got {other:?}"),
        }
    }

    fn org_json(id: Uuid, name: &str, is_default: bool) -> serde_json::Value {
        serde_json::json!({
            "id": id, "name": name.to_lowercase(), "display_name": name, "description": "",
            "icon": "", "is_default": is_default, "default_org_member_roles": [],
            "created_at": "2026-01-01T00:00:00Z", "updated_at": "2026-01-01T00:00:00Z"
        })
    }

    #[tokio::test]
    async fn the_default_organization_wins_over_list_order() {
        let server = MockServer::start().await;
        let (product, coder) = (Uuid::new_v4(), Uuid::new_v4());
        Mock::given(method("GET"))
            .and(path("/api/v2/users/me/organizations"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                org_json(product, "Product", false),
                org_json(coder, "Coder", true)
            ])))
            .mount(&server)
            .await;
        let (rt, _rx) = runtime(&server.uri());
        let orgs = rt.organizations().await.unwrap();
        let labels: Vec<&str> = orgs.iter().map(|o| o.label()).collect();
        assert_eq!(labels, ["Product", "Coder"]);
        assert_eq!(
            scuttle_core::app::pick_organization(None, &orgs),
            Some(coder)
        );
    }

    #[tokio::test]
    async fn organizations_where_chats_are_not_allowed_are_marked() {
        let server = MockServer::start().await;
        let (product, coder) = (Uuid::new_v4(), Uuid::new_v4());
        Mock::given(method("GET"))
            .and(path("/api/v2/users/me/organizations"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                org_json(product, "Product", false),
                org_json(coder, "Coder", true)
            ])))
            .mount(&server)
            .await;
        let mut checks = serde_json::Map::new();
        checks.insert(
            product.to_string(),
            serde_json::json!({"action": "create", "object": {"resource_type": "chat", "owner_id": "me", "organization_id": product.to_string()}}),
        );
        let mut answers = serde_json::Map::new();
        answers.insert(product.to_string(), false.into());
        answers.insert(coder.to_string(), true.into());
        Mock::given(method("POST"))
            .and(path("/api/v2/authcheck"))
            .and(wiremock::matchers::body_partial_json(
                serde_json::json!({ "checks": checks }),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(answers))
            .mount(&server)
            .await;
        let (rt, _rx) = runtime(&server.uri());
        let orgs = rt.organizations().await.unwrap();
        let allowed: Vec<(&str, bool)> = orgs
            .iter()
            .map(|o| (o.label(), o.can_create_chats))
            .collect();
        assert_eq!(allowed, [("Product", false), ("Coder", true)]);
        assert_eq!(
            scuttle_core::app::pick_organization(Some(product), &orgs),
            Some(coder)
        );
        assert_no_token_in_authcheck_bodies(&server).await;
    }

    #[tokio::test]
    async fn a_failed_permission_check_hides_no_organization() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/users/me/organizations"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                org_json(Uuid::new_v4(), "Product", false),
                org_json(Uuid::new_v4(), "Coder", true)
            ])))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v2/authcheck"))
            .respond_with(api_error(500, "authorizer is down"))
            .mount(&server)
            .await;
        let (rt, _rx) = runtime(&server.uri());
        let orgs = rt.organizations().await.unwrap();
        assert!(orgs.iter().all(|o| o.can_create_chats), "{orgs:?}");
        assert_no_token_in_authcheck_bodies(&server).await;
    }

    #[tokio::test]
    async fn one_organization_skips_the_permission_check() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/users/me/organizations"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!([org_json(
                    Uuid::new_v4(),
                    "Coder",
                    true
                )])),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v2/authcheck"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(0)
            .mount(&server)
            .await;
        let (rt, _rx) = runtime(&server.uri());
        let orgs = rt.organizations().await.unwrap();
        assert!(orgs[0].can_create_chats);
    }

    /// Asserts that at least one `POST /authcheck` arrived and that none of their bodies carry
    /// the session token, which belongs only in the auth header.
    async fn assert_no_token_in_authcheck_bodies(server: &MockServer) {
        let checks: Vec<_> = server
            .received_requests()
            .await
            .unwrap()
            .into_iter()
            .filter(|r| r.url.path() == "/api/v2/authcheck")
            .collect();
        assert!(!checks.is_empty(), "the permission check was sent");
        for r in checks {
            assert!(
                !String::from_utf8_lossy(&r.body).contains(TOKEN),
                "the session token is never in the request body"
            );
        }
    }

    #[tokio::test]
    async fn workspaces_are_listed_for_one_organization() {
        let server = MockServer::start().await;
        let org = Uuid::new_v4();
        Mock::given(method("GET"))
            .and(path("/api/v2/workspaces"))
            .and(wiremock::matchers::query_param(
                "q",
                format!("owner:me organization:{org}"),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "workspaces": [{"id": Uuid::new_v4(), "name": "dev", "template_name": "docker",
                    "template_display_name": "Docker", "last_used_at": "2026-09-30T10:00:00Z",
                    "latest_build": {"status": "running", "resources": []}, "shared_with": []}],
                "count": 1
            })))
            .mount(&server)
            .await;
        let (mut rt, mut rx) = runtime(&server.uri());
        rt.run(Effect::FetchWorkspaces(org));
        match next(&mut rx).await {
            Msg::ForOrg { org: tagged, msg } => {
                assert_eq!(tagged, org);
                assert!(
                    matches!(*msg, Msg::WorkspacesLoaded(ref w) if w.len() == 1 && w[0].name == "dev" && w[0].template == "Docker" && w[0].status == "running" && w[0].last_used.is_some()),
                    "{msg:?}"
                );
            }
            other => panic!("expected a tagged workspace list, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn other_api_errors_send_api_failed() {
        let server = MockServer::start().await;
        let (mut rt, mut rx) = runtime(&server.uri());
        rt.run(Effect::Compact(Uuid::new_v4()));
        assert!(matches!(
            untag(next(&mut rx).await),
            Msg::ApiFailed {
                action: "compact the chat",
                ..
            }
        ));
    }

    #[tokio::test]
    async fn load_chat_errors_send_chat_load_failed() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(api_error(404, "chat not found"))
            .mount(&server)
            .await;
        let (mut rt, mut rx) = runtime(&server.uri());
        let id = Uuid::new_v4();
        rt.run(Effect::LoadChat(id));
        match next(&mut rx).await {
            Msg::ChatLoadFailed { chat_id, message } => {
                assert_eq!(chat_id, id);
                assert!(message.contains("chat not found"), "{message}");
                assert!(!message.contains(TOKEN), "{message}");
            }
            other => panic!("expected ChatLoadFailed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_loaded_chat_carries_the_servers_has_more() {
        let server = MockServer::start().await;
        let id = Uuid::new_v4();
        Mock::given(method("GET"))
            .and(path(format!("/api/v2/chats/{id}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": id, "children": [], "files": [], "mcp_server_ids": [],
                "inline_mcp_servers": [], "labels": {}
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/api/v2/chats/{id}/messages")))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "messages": [{"id": 9, "role": "user", "content": []}],
                "queued_messages": [], "has_more": true
            })))
            .mount(&server)
            .await;
        let (mut rt, mut rx) = runtime(&server.uri());
        rt.run(Effect::LoadChat(id));
        match next(&mut rx).await {
            Msg::ChatLoaded {
                messages, has_more, ..
            } => {
                assert_eq!(messages.len(), 1);
                assert_eq!(has_more, Some(true));
            }
            other => panic!("expected ChatLoaded, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn load_messages_errors_send_chat_load_failed() {
        let server = MockServer::start().await;
        let id = Uuid::new_v4();
        Mock::given(method("GET"))
            .and(path(format!("/api/v2/chats/{id}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": id, "children": [], "files": [], "mcp_server_ids": [],
                "inline_mcp_servers": [], "labels": {}
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/api/v2/chats/{id}/messages")))
            .respond_with(api_error(500, "messages unavailable"))
            .mount(&server)
            .await;
        let (mut rt, mut rx) = runtime(&server.uri());
        rt.run(Effect::LoadChat(id));
        match next(&mut rx).await {
            Msg::ChatLoadFailed { chat_id, message } => {
                assert_eq!(chat_id, id);
                assert!(message.contains("messages unavailable"), "{message}");
            }
            other => panic!("expected ChatLoadFailed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_message_with_an_mcp_selection_sends_it_even_when_empty() {
        use wiremock::matchers::body_partial_json;
        let server = MockServer::start().await;
        let chat = Uuid::new_v4();
        Mock::given(method("POST"))
            .and(path(format!("/api/v2/chats/{chat}/messages")))
            .and(body_partial_json(serde_json::json!({"mcp_server_ids": []})))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"queued": false})),
            )
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
            seq: 3,
        });
        let reply = next(&mut rx).await;
        assert!(
            matches!(&reply, Msg::ForChat { msg, .. } if matches!(**msg, Msg::Sent { seq: 3, .. })),
            "the success settles its send: {reply:?}"
        );
        assert!(matches!(untag(reply), Msg::Refresh));
    }

    #[tokio::test]
    async fn a_failed_send_with_an_mcp_selection_says_whether_the_server_refused_it() {
        let cases = [
            (400, Some(vec![Uuid::new_v4()]), true),
            (500, Some(vec![Uuid::new_v4()]), false),
            (400, None, false),
        ];
        for (status, mcp_servers, refused) in cases {
            let server = MockServer::start().await;
            let chat = Uuid::new_v4();
            Mock::given(method("POST"))
                .and(path(format!("/api/v2/chats/{chat}/messages")))
                .respond_with(api_error(status, "unknown MCP server"))
                .mount(&server)
                .await;
            let (mut rt, mut rx) = runtime(&server.uri());
            let carried = mcp_servers.is_some();
            rt.run(Effect::SendMessage {
                chat,
                text: "keep this".into(),
                model: None,
                busy: scuttle_core::config::BusyBehavior::Queue,
                turn: TurnOptions {
                    mcp_servers,
                    ..Default::default()
                },
                seq: 4,
            });
            match untag(next(&mut rx).await) {
                Msg::SendFailed {
                    text,
                    message,
                    seq,
                    mcp_rejected,
                    ..
                } => {
                    assert_eq!(seq, 4);
                    assert_eq!(text, "keep this");
                    assert_eq!(mcp_rejected, refused, "{status}, carried: {carried}");
                    assert!(message.contains("unknown MCP server"), "{message}");
                    assert!(!message.contains(TOKEN), "{message}");
                }
                other => panic!("expected SendFailed, got {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn send_message_errors_send_send_failed_with_the_text() {
        let server = MockServer::start().await;
        let chat = Uuid::new_v4();
        Mock::given(method("POST"))
            .and(path(format!("/api/v2/chats/{chat}/messages")))
            .respond_with(api_error(409, "the chat cannot accept messages"))
            .mount(&server)
            .await;
        let (mut rt, mut rx) = runtime(&server.uri());
        rt.run(Effect::SendMessage {
            chat,
            text: "keep this".into(),
            model: None,
            busy: scuttle_core::config::BusyBehavior::Queue,
            turn: TurnOptions::default(),
            seq: 7,
        });
        match untag(next(&mut rx).await) {
            Msg::SendFailed {
                text,
                message,
                plan_mode,
                seq,
                mcp_rejected,
            } => {
                assert_eq!(seq, 7, "the failure names its send");
                assert!(!mcp_rejected);
                assert_eq!(plan_mode, None);
                assert_eq!(text, "keep this");
                assert!(message.contains("cannot accept"), "{message}");
                assert!(!message.contains(TOKEN), "{message}");
            }
            other => panic!("expected SendFailed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_sent_message_reply_carries_its_number() {
        let server = MockServer::start().await;
        let chat = Uuid::new_v4();
        Mock::given(method("POST"))
            .and(path(format!("/api/v2/chats/{chat}/messages")))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .mount(&server)
            .await;
        let (mut rt, mut rx) = runtime(&server.uri());
        for (seq, plan_mode) in [(4, None), (5, Some(true))] {
            rt.run(Effect::SendMessage {
                chat,
                text: "hi".into(),
                model: None,
                busy: scuttle_core::config::BusyBehavior::Queue,
                turn: TurnOptions {
                    plan_mode,
                    ..Default::default()
                },
                seq,
            });
            let reply = next(&mut rx).await;
            let (Msg::ForChat { msg, .. } | Msg::ForPlan { msg, .. }) = reply else {
                panic!("expected a chat-tagged reply, got {reply:?}");
            };
            match *msg {
                Msg::Sent { seq: got, then } => {
                    assert_eq!(got, seq);
                    match plan_mode {
                        None => assert!(matches!(*then, Msg::Refresh)),
                        Some(on) => {
                            assert!(matches!(*then, Msg::PlanModeApplied { on: got } if got == on))
                        }
                    }
                }
                other => panic!("expected Sent, got {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn messages_and_new_chats_carry_the_chosen_effort() {
        let server = MockServer::start().await;
        let chat = Uuid::new_v4();
        Mock::given(method("POST"))
            .and(path(format!("/api/v2/chats/{chat}/messages")))
            .and(wiremock::matchers::body_partial_json(
                serde_json::json!({"reasoning_effort": "high"}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .mount(&server)
            .await;
        let created = Uuid::new_v4();
        Mock::given(method("POST"))
            .and(path("/api/v2/chats"))
            .and(wiremock::matchers::body_partial_json(
                serde_json::json!({"reasoning_effort": "low"}),
            ))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "id": created, "children": [], "files": [], "mcp_server_ids": [],
                "inline_mcp_servers": [], "labels": {}
            })))
            .mount(&server)
            .await;
        let (mut rt, mut rx) = runtime(&server.uri());
        rt.run(Effect::SendMessage {
            chat,
            text: "hi".into(),
            model: None,
            busy: scuttle_core::config::BusyBehavior::Queue,
            turn: TurnOptions {
                effort: Some("high".into()),
                ..Default::default()
            },
            seq: 0,
        });
        assert!(matches!(untag(next(&mut rx).await), Msg::Refresh));
        rt.run(Effect::CreateChat {
            org: Uuid::new_v4(),
            text: "hi".into(),
            model: None,
            workspace: None,
            turn: TurnOptions {
                effort: Some("low".into()),
                ..Default::default()
            },
            seq: 0,
        });
        assert!(matches!(next(&mut rx).await, Msg::ChatCreated(_)));
    }

    #[tokio::test]
    async fn set_plan_mode_patches_the_chat() {
        let server = MockServer::start().await;
        let chat = Uuid::new_v4();
        for value in ["plan", ""] {
            Mock::given(method("PATCH"))
                .and(path(format!("/api/v2/chats/{chat}")))
                .and(wiremock::matchers::body_partial_json(
                    serde_json::json!({"plan_mode": value}),
                ))
                .respond_with(ResponseTemplate::new(204))
                .mount(&server)
                .await;
        }
        let (mut rt, mut rx) = runtime(&server.uri());
        rt.run(Effect::SetPlanMode {
            chat,
            on: true,
            generation: 1,
        });
        assert!(matches!(
            untag(next(&mut rx).await),
            Msg::PlanModeApplied { on: true }
        ));
        rt.run(Effect::SetPlanMode {
            chat,
            on: false,
            generation: 2,
        });
        assert!(matches!(
            untag(next(&mut rx).await),
            Msg::PlanModeApplied { on: false }
        ));
    }

    #[tokio::test]
    async fn a_failed_plan_mode_update_says_which_way() {
        let server = MockServer::start().await;
        let (mut rt, mut rx) = runtime(&server.uri());
        rt.run(Effect::SetPlanMode {
            chat: Uuid::new_v4(),
            on: true,
            generation: 1,
        });
        match untag(next(&mut rx).await) {
            Msg::PlanModeFailed { on, message } => {
                assert!(on);
                assert!(!message.contains(TOKEN), "{message}");
            }
            other => panic!("expected PlanModeFailed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn plan_mode_rides_on_new_chats_and_messages() {
        let server = MockServer::start().await;
        let chat = Uuid::new_v4();
        Mock::given(method("POST"))
            .and(path("/api/v2/chats"))
            .and(wiremock::matchers::body_partial_json(
                serde_json::json!({"plan_mode": "plan"}),
            ))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "id": chat, "children": [], "files": [], "mcp_server_ids": [],
                "inline_mcp_servers": [], "labels": {}
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(format!("/api/v2/chats/{chat}/messages")))
            .and(wiremock::matchers::body_partial_json(
                serde_json::json!({"plan_mode": ""}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .mount(&server)
            .await;
        let (mut rt, mut rx) = runtime(&server.uri());
        rt.run(Effect::CreateChat {
            org: Uuid::new_v4(),
            text: "plan this".into(),
            model: None,
            workspace: None,
            turn: TurnOptions {
                plan_mode: Some(true),
                ..Default::default()
            },
            seq: 0,
        });
        assert!(matches!(next(&mut rx).await, Msg::ChatCreated(_)));
        rt.run(Effect::SendMessage {
            chat,
            text: "now build it".into(),
            model: None,
            busy: scuttle_core::config::BusyBehavior::Queue,
            turn: TurnOptions {
                plan_mode: Some(false),
                ..Default::default()
            },
            seq: 0,
        });
        // The message carried a plan mode change, so the core settles it like a PATCH.
        assert!(matches!(
            untag(next(&mut rx).await),
            Msg::PlanModeApplied { on: false }
        ));
    }

    #[tokio::test]
    async fn a_failed_send_echoes_the_plan_mode_it_carried() {
        let server = MockServer::start().await;
        let chat = Uuid::new_v4();
        let (mut rt, mut rx) = runtime(&server.uri());
        rt.run(Effect::SendMessage {
            chat,
            text: "plan it".into(),
            model: None,
            busy: scuttle_core::config::BusyBehavior::Queue,
            turn: TurnOptions {
                plan_mode: Some(true),
                plan_generation: 4,
                ..Default::default()
            },
            seq: 0,
        });
        let reply = next(&mut rx).await;
        assert!(
            matches!(reply, Msg::ForPlan { chat: tagged, generation: 4, .. } if tagged == chat),
            "the reply names the plan-mode request it settles: {reply:?}"
        );
        match untag(reply) {
            Msg::SendFailed {
                text, plan_mode, ..
            } => {
                assert_eq!(text, "plan it");
                assert_eq!(plan_mode, Some(true));
            }
            other => panic!("expected SendFailed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn ui_effects_are_ignored() {
        let server = MockServer::start().await;
        let (mut rt, mut rx) = runtime(&server.uri());
        rt.run(Effect::ShowHelp);
        rt.run(Effect::ClearView);
        rt.run(Effect::Quit);
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(rx.try_recv().is_err());
    }

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
            .respond_with(api_error(
                409,
                "The chat has no queued messages to promote.",
            ))
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

    #[test]
    fn a_stale_stream_sender_delivers_nothing() {
        let (tx, mut rx) = unbounded_channel();
        let generation = Arc::new(AtomicU64::new(1));
        let old = StreamSender {
            tx,
            generation: generation.clone(),
            mine: 1,
            chat: Uuid::new_v4(),
            tag: 1,
            wrap: tag_for(Slot::Main),
        };
        assert!(old.send(Msg::StreamEnded { error: None }));
        generation.store(2, Ordering::SeqCst);
        assert!(!old.send(Msg::StreamEnded { error: None }));
        assert!(matches!(
            rx.try_recv(),
            Ok(Msg::ForStream { generation: 1, .. })
        ));
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
            Msg::ForStream { msg, .. } => match msg.as_ref() {
                Msg::Stream(ev) => ev.raw["conn"].as_u64(),
                _ => None,
            },
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
            generation: 1,
        });
        assert_eq!(conn_of(&next(&mut rx).await), Some(1));
        rt.run(Effect::ReconnectAfter {
            chat,
            after_id: None,
            delay: Duration::ZERO,
            generation: 1,
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

    #[test]
    fn a_preview_sender_tags_every_message_for_the_preview() {
        let (tx, mut rx) = unbounded_channel();
        let chat = Uuid::new_v4();
        let out = StreamSender {
            tx,
            generation: Arc::new(AtomicU64::new(1)),
            mine: 1,
            chat,
            tag: 3,
            wrap: tag_for(Slot::Preview),
        };
        assert!(out.send(Msg::StreamHealthy));
        assert!(out.send(Msg::StreamEnded { error: None }));
        for _ in 0..2 {
            match rx.try_recv() {
                Ok(Msg::ForPreview {
                    chat: tagged,
                    generation: 3,
                    ..
                }) => assert_eq!(tagged, chat),
                other => panic!("expected a preview-tagged message, got {other:?}"),
            }
        }
    }

    /// Serves chat streams that send one status event and then stay quiet, and reports on the
    /// returned channel once a connection's client has gone away.
    async fn serve_streams_reporting_closes() -> (String, UnboundedReceiver<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (closed_tx, closed_rx) = unbounded_channel();
        tokio::spawn(async move {
            loop {
                let Ok((tcp, _)) = listener.accept().await else {
                    return;
                };
                let closed = closed_tx.clone();
                tokio::spawn(async move {
                    let Ok(mut ws) = tokio_tungstenite::accept_async(tcp).await else {
                        return;
                    };
                    let frame =
                        serde_json::json!([{"type": "status", "status": {"status": "running"}}])
                            .to_string();
                    if ws.send(Message::text(frame)).await.is_ok() {
                        while let Some(Ok(_)) = ws.next().await {}
                    }
                    let _ = closed.send(());
                });
            }
        });
        (format!("http://{addr}"), closed_rx)
    }

    #[tokio::test]
    async fn closing_the_preview_ends_its_task_and_connection() {
        let (url, mut closed) = serve_streams_reporting_closes().await;
        let (mut rt, mut rx) = runtime(&url);
        // A quiet stream that never reports healthy ends only if its task is stopped.
        rt.healthy_after = Duration::from_secs(3600);
        rt.run(Effect::OpenPreview {
            chat: Uuid::new_v4(),
            after_id: None,
            delay: Duration::ZERO,
            generation: 1,
        });
        assert!(matches!(next(&mut rx).await, Msg::ForPreview { .. }));
        assert!(closed.try_recv().is_err(), "the preview is still open");
        rt.run(Effect::ClosePreview);
        assert!(rt.preview.is_none());
        tokio::time::timeout(WAIT, closed.recv())
            .await
            .expect("the preview's connection closes within the timeout")
            .expect("the server is still running");
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
        // The server never stops sending, so bound the wait instead of looping forever.
        tokio::time::timeout(WAIT, async {
            loop {
                match next(&mut rx).await {
                    Msg::ForStream {
                        generation: 3, msg, ..
                    } if matches!(*msg, Msg::StreamHealthy) => break,
                    Msg::ForStream { .. } => continue,
                    other => panic!("unexpected {other:?}"),
                }
            }
        })
        .await
        .expect("the stream never reported healthy");
    }

    #[tokio::test]
    async fn closing_the_stream_stops_it() {
        let url = serve_tagged_streams().await;
        let (mut rt, mut rx) = runtime(&url);
        rt.run(Effect::OpenStream {
            chat: Uuid::new_v4(),
            after_id: None,
            generation: 1,
        });
        next(&mut rx).await;
        rt.run(Effect::CloseStream);
        // Whatever was queued before the close may still arrive; after that, silence. The
        // server sends every 10 ms, so a stream that kept going would fill all 50 turns.
        for _ in 0..50 {
            if tokio::time::timeout(Duration::from_millis(200), rx.recv())
                .await
                .is_err()
            {
                return;
            }
        }
        panic!("the stream kept sending after CloseStream");
    }

    #[tokio::test]
    async fn plan_mode_results_are_tagged_with_their_chat() {
        let server = MockServer::start().await;
        let (mut rt, mut rx) = runtime(&server.uri());
        let chat = Uuid::new_v4();
        rt.run(Effect::SetPlanMode {
            chat,
            on: true,
            generation: 7,
        });
        match next(&mut rx).await {
            Msg::ForPlan {
                chat: tagged,
                generation,
                msg,
            } => {
                assert_eq!((tagged, generation), (chat, 7));
                assert!(
                    matches!(*msg, Msg::PlanModeFailed { on: true, .. }),
                    "{msg:?}"
                );
            }
            other => panic!("expected a tagged plan mode result, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn chat_request_failures_are_tagged_with_their_chat() {
        let server = MockServer::start().await;
        let (mut rt, mut rx) = runtime(&server.uri());
        let chat = Uuid::new_v4();
        rt.run(Effect::Interrupt(chat));
        match next(&mut rx).await {
            Msg::ForChat { chat: tagged, msg } => {
                assert_eq!(tagged, chat);
                assert!(
                    matches!(
                        *msg,
                        Msg::ApiFailed {
                            action: "interrupt",
                            ..
                        }
                    ),
                    "{msg:?}"
                );
            }
            other => panic!("expected a tagged interrupt failure, got {other:?}"),
        }
    }

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
                assert_eq!(
                    (skills[0].name.as_str(), skills[0].description.as_str()),
                    ("deploy", "Ship it")
                )
            }
            other => panic!("expected SkillsLoaded, got {other:?}"),
        }
    }

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
            Msg::OrganizationsFailed {
                open_chat: None,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn a_refused_chat_search_fails_with_the_server_message_and_details() {
        use scuttle_core::chat_list::ListQuery;
        use wiremock::matchers::query_param;
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/chats"))
            .and(query_param("q", "title:ci search:\"flaky\""))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "message": "Invalid chat search query.",
                "detail": "Check the query syntax",
                "validations": [
                    {"field": "search", "detail": "\"search\" cannot be combined with \"title\""}
                ]
            })))
            .mount(&server)
            .await;
        let (mut rt, mut rx) = runtime(&server.uri());
        let query = ListQuery::Search("title:ci flaky".into());
        rt.run(Effect::FetchChats {
            query: query.clone(),
            offset: 0,
        });
        match next(&mut rx).await {
            Msg::ChatsFailed {
                query: failed,
                message,
            } => {
                assert_eq!(failed, query);
                assert_eq!(
                    message,
                    "Invalid chat search query. Check the query syntax. \
                     \"search\" cannot be combined with \"title\"."
                );
            }
            other => panic!("expected ChatsFailed, got {other:?}"),
        }
    }

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

    /// The message inside a `Msg::ForWatch`, after checking it carries `generation`.
    async fn watched(rx: &mut UnboundedReceiver<Msg>, generation: u64) -> Msg {
        match next(rx).await {
            Msg::ForWatch {
                generation: tag,
                msg,
            } => {
                assert_eq!(tag, generation, "tagged with its connection");
                *msg
            }
            other => panic!("expected a tagged watch message, got {other:?}"),
        }
    }

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
            generation: 7,
        });
        assert!(matches!(watched(&mut rx, 7).await, Msg::WatchConnected));
        match watched(&mut rx, 7).await {
            Msg::Watch(ev) => {
                assert_eq!(ev.kind, "title_change");
                let title = ev.event.and_then(|e| e.chat).and_then(|c| c.title);
                assert_eq!(title.as_deref(), Some("Renamed"));
            }
            other => panic!("expected a watch event, got {other:?}"),
        }
        assert!(matches!(watched(&mut rx, 7).await, Msg::WatchEnded { .. }));
    }

    #[tokio::test]
    async fn a_watch_that_stays_open_reports_healthy() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
            while ws.next().await.is_some() {}
        });
        let (mut rt, mut rx) = runtime(&format!("http://{addr}"));
        rt.healthy_after = Duration::from_millis(50);
        rt.run(Effect::OpenWatch {
            delay: Duration::ZERO,
            generation: 1,
        });
        assert!(matches!(watched(&mut rx, 1).await, Msg::WatchConnected));
        assert!(matches!(watched(&mut rx, 1).await, Msg::WatchHealthy));
    }

    #[tokio::test]
    async fn a_refused_watch_ends_with_the_reason() {
        let server = MockServer::start().await;
        let (mut rt, mut rx) = runtime(&server.uri());
        rt.run(Effect::OpenWatch {
            delay: Duration::ZERO,
            generation: 1,
        });
        match watched(&mut rx, 1).await {
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
        rt.run(Effect::RefreshChat {
            chat,
            generation: 4,
        });
        match next(&mut rx).await {
            Msg::ForChat { chat: tagged, msg } => {
                assert_eq!(tagged, chat);
                let Msg::ForRefresh { generation, msg } = *msg else {
                    panic!("expected a refresh tagged with its generation, got {msg:?}");
                };
                assert_eq!(generation, 4);
                assert!(
                    matches!(*msg, Msg::ChatRefreshed(ref c) if c.title.as_deref() == Some("fresh"))
                );
            }
            other => panic!("expected a tagged refresh, got {other:?}"),
        }
    }

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
            .respond_with(api_error(
                400,
                "Chat archive state can only be changed on the root chat.",
            ))
            .mount(&server)
            .await;
        let (mut rt, mut rx) = runtime(&server.uri());
        rt.run(Effect::UpdateChat {
            chat,
            change: ChatChange::PinOrder(1),
        });
        assert!(matches!(
            next(&mut rx).await,
            Msg::ChatUpdated {
                change: ChatChange::PinOrder(1),
                ..
            }
        ));
        rt.run(Effect::UpdateChat {
            chat,
            change: ChatChange::Archived(true),
        });
        match next(&mut rx).await {
            Msg::ChatUpdateFailed { message, .. } => {
                assert!(message.contains("root chat"), "{message}")
            }
            other => panic!("expected ChatUpdateFailed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn archive_and_delete_archives_first_then_deletes_the_workspace() {
        use scuttle_core::app::WorkspaceDeletion;
        use wiremock::matchers::body_json;
        let server = MockServer::start().await;
        let (chat, ws) = (Uuid::new_v4(), Uuid::new_v4());
        Mock::given(method("PATCH"))
            .and(path(format!("/api/v2/chats/{chat}")))
            .and(body_json(serde_json::json!({"archived": true})))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(format!("/api/v2/workspaces/{ws}/builds")))
            .and(body_json(serde_json::json!({"transition": "delete"})))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "id": Uuid::new_v4(), "matched_provisioners": {"count": 0, "available": 0}
            })))
            .expect(1)
            .mount(&server)
            .await;
        let (mut rt, mut rx) = runtime(&server.uri());
        rt.run(Effect::ArchiveAndDeleteWorkspace {
            chat,
            workspace: ws,
        });
        match next(&mut rx).await {
            Msg::ArchivedWithWorkspace { chat: c, outcome } => {
                assert_eq!(c, chat);
                assert_eq!(
                    outcome,
                    WorkspaceDeletion::Started {
                        no_provisioner: true
                    }
                );
            }
            other => panic!("expected ArchivedWithWorkspace, got {other:?}"),
        }
        let methods: Vec<String> = server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .map(|r| r.method.to_string())
            .collect();
        assert_eq!(methods, ["PATCH", "POST"], "the archive goes first");
    }

    #[tokio::test]
    async fn a_refused_archive_never_deletes_the_workspace() {
        use scuttle_core::app::ChatChange;
        let server = MockServer::start().await;
        let (chat, ws) = (Uuid::new_v4(), Uuid::new_v4());
        Mock::given(method("PATCH"))
            .and(path(format!("/api/v2/chats/{chat}")))
            .respond_with(api_error(409, "Chat has a running subagent."))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(format!("/api/v2/workspaces/{ws}/builds")))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({})))
            .expect(0)
            .mount(&server)
            .await;
        let (mut rt, mut rx) = runtime(&server.uri());
        rt.run(Effect::ArchiveAndDeleteWorkspace {
            chat,
            workspace: ws,
        });
        match next(&mut rx).await {
            Msg::ChatUpdateFailed {
                chat: c,
                change: ChatChange::Archived(true),
                message,
            } => {
                assert_eq!(c, chat);
                assert!(message.contains("running subagent"), "{message}");
            }
            other => panic!("expected ChatUpdateFailed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_failed_workspace_delete_leaves_the_chat_archived_and_says_so() {
        use scuttle_core::app::WorkspaceDeletion;
        for (status, gone) in [(404, true), (410, true), (403, false), (500, false)] {
            let server = MockServer::start().await;
            let (chat, ws) = (Uuid::new_v4(), Uuid::new_v4());
            Mock::given(method("PATCH"))
                .and(path(format!("/api/v2/chats/{chat}")))
                .respond_with(ResponseTemplate::new(204))
                .mount(&server)
                .await;
            Mock::given(method("POST"))
                .and(path(format!("/api/v2/workspaces/{ws}/builds")))
                .respond_with(api_error(status, &format!("no luck {TOKEN}")))
                .mount(&server)
                .await;
            let (mut rt, mut rx) = runtime(&server.uri());
            rt.run(Effect::ArchiveAndDeleteWorkspace {
                chat,
                workspace: ws,
            });
            match next(&mut rx).await {
                Msg::ArchivedWithWorkspace { outcome, .. } if gone => {
                    assert_eq!(outcome, WorkspaceDeletion::AlreadyGone, "{status}");
                }
                Msg::ArchivedWithWorkspace {
                    outcome: WorkspaceDeletion::Failed(message),
                    ..
                } => {
                    assert!(message.contains("no luck"), "{status}: {message}");
                    assert!(!message.contains(TOKEN), "{status}: {message}");
                }
                other => panic!("{status}: expected ArchivedWithWorkspace, got {other:?}"),
            }
        }
    }

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
        rt.run(Effect::ProposeTitle {
            chat,
            generation: 7,
        });
        match next(&mut rx).await {
            Msg::ForChat { chat: tagged, msg } => {
                assert_eq!(tagged, chat);
                assert!(matches!(
                    *msg,
                    Msg::TitleProposed {
                        ref title,
                        generation: 7
                    } if title == "Watch fix"
                ));
            }
            other => panic!("expected a tagged title, got {other:?}"),
        }
    }

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

    #[tokio::test]
    async fn a_failed_ssh_suffix_request_is_reported_as_a_failure() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/deployment/ssh"))
            .respond_with(ResponseTemplate::new(502))
            .mount(&server)
            .await;
        let (mut rt, mut rx) = runtime(&server.uri());
        rt.run(Effect::FetchSshSuffix);
        assert!(matches!(next(&mut rx).await, Msg::SshSuffixFailed));
    }

    #[tokio::test]
    async fn opening_the_workspace_web_page_falls_back_to_copying_a_link() {
        let server = MockServer::start().await;
        let (mut rt, mut rx) = runtime(&server.uri());
        rt.run(Effect::OpenWorkspaceWeb {
            owner: "nick".into(),
            workspace: "dev".into(),
        });
        let (url, outcome) = match next(&mut rx).await {
            Msg::LinkOpened { url, outcome } => (url, outcome),
            other => panic!("expected LinkOpened, got {other:?}"),
        };
        assert_eq!(url, format!("{}/@nick/dev", server.uri()));
        assert!(outcome.is_err(), "tests never launch a browser");
        // The fallback copy is a link, so its notice never calls it the chat URL.
        let mut app = scuttle_core::app::App::new(scuttle_core::config::BusyBehavior::Queue, true);
        assert_eq!(
            app.update(Msg::LinkOpened {
                url: url.clone(),
                outcome
            }),
            vec![Effect::CopyLink(url)]
        );
    }

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
        rt.run(Effect::OpenGitWatch {
            chat,
            generation: 7,
        });
        match next(&mut rx).await {
            Msg::ForGit {
                chat: tagged,
                generation,
                msg,
            } => {
                assert_eq!(tagged, chat);
                assert_eq!(generation, 7);
                assert!(
                    matches!(*msg, Msg::GitChanges(ref m) if m.repositories.len() == 1),
                    "{msg:?}"
                );
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
        rt.run(Effect::OpenGitWatch {
            chat,
            generation: 8,
        });
        assert!(matches!(
            untag(next(&mut rx).await),
            Msg::GitWatchEnded(ref m) if m == "Chat has no workspace to watch."
        ));
    }

    #[tokio::test]
    async fn a_handoff_closes_the_git_watch_and_reopens_it_after() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (closed_tx, mut closed) = unbounded_channel();
        tokio::spawn(async move {
            loop {
                let Ok((tcp, _)) = listener.accept().await else {
                    return;
                };
                let closed = closed_tx.clone();
                tokio::spawn(async move {
                    let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
                    let frame = serde_json::json!({"type": "changes", "repositories": []});
                    if ws.send(Message::text(frame.to_string())).await.is_ok() {
                        while let Some(Ok(_)) = ws.next().await {}
                    }
                    let _ = closed.send(());
                });
            }
        });
        let (mut rt, mut rx) = runtime(&format!("http://{addr}"));
        let chat = Uuid::new_v4();
        rt.run(Effect::OpenGitWatch {
            chat,
            generation: 3,
        });
        assert!(matches!(
            next(&mut rx).await,
            Msg::ForGit { generation: 3, .. }
        ));
        rt.pause_git_watch();
        tokio::time::timeout(WAIT, closed.recv())
            .await
            .expect("the socket closes for the handoff")
            .expect("the server is still running");
        rt.resume_git_watch();
        match next(&mut rx).await {
            Msg::ForGit {
                chat: tagged,
                generation: 3,
                msg,
            } if tagged == chat => {
                assert!(matches!(*msg, Msg::GitReopened), "{msg:?}");
            }
            other => panic!("expected the reopen before its frames, got {other:?}"),
        }
        match next(&mut rx).await {
            Msg::ForGit {
                chat: tagged,
                generation: 3,
                msg,
            } if tagged == chat => {
                assert!(matches!(*msg, Msg::GitChanges(_)), "{msg:?}");
            }
            other => panic!("expected the reopened socket's changes, got {other:?}"),
        }
        rt.run(Effect::CloseGitWatch);
        rt.pause_git_watch();
        rt.resume_git_watch();
        assert!(rt.git.is_none(), "a closed panel's socket stays closed");
    }

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
        rt.run(Effect::LoadOlder {
            chat,
            before_id: 301,
            generation: 7,
        });
        let reply = next(&mut rx).await;
        assert!(
            matches!(&reply, Msg::ForChat { chat: c, .. } if *c == chat),
            "{reply:?}"
        );
        assert!(matches!(
            untag(reply),
            Msg::OlderLoaded { ref messages, has_more: true, generation: 7 } if messages.len() == 1
        ));
    }

    #[tokio::test]
    async fn a_failed_older_page_is_tagged_with_its_generation() {
        let server = MockServer::start().await;
        let chat = Uuid::new_v4();
        Mock::given(method("GET"))
            .and(path(format!("/api/v2/chats/{chat}/messages")))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;
        let (mut rt, mut rx) = runtime(&server.uri());
        rt.run(Effect::LoadOlder {
            chat,
            before_id: 301,
            generation: 3,
        });
        assert!(matches!(
            untag(next(&mut rx).await),
            Msg::OlderFailed { generation: 3, .. }
        ));
    }

    #[tokio::test]
    async fn a_send_refused_for_its_model_reports_it_with_the_files_it_carried() {
        let wordings = [
            "Invalid model_config_id: model config not found or disabled.",
            "Invalid model_config_id: provider is not enabled for this model.",
            "Invalid model_config_id: provider credentials unavailable for this model.",
            "Invalid model_config_id.",
            "Invalid model config ID.",
        ];
        for wording in wordings {
            for mcp_servers in [None, Some(vec![Uuid::new_v4()])] {
                let server = MockServer::start().await;
                let chat = Uuid::new_v4();
                Mock::given(method("POST"))
                    .and(path(format!("/api/v2/chats/{chat}/messages")))
                    .respond_with(api_error(400, wording))
                    .mount(&server)
                    .await;
                let (mut rt, mut rx) = runtime(&server.uri());
                let file = Uuid::new_v4();
                let carried = mcp_servers.is_some();
                rt.run(Effect::SendMessage {
                    chat,
                    text: "keep this".into(),
                    model: Some(Uuid::new_v4()),
                    busy: scuttle_core::config::BusyBehavior::Queue,
                    turn: TurnOptions {
                        files: vec![file],
                        mcp_servers,
                        ..Default::default()
                    },
                    seq: 9,
                });
                match untag(next(&mut rx).await) {
                    Msg::ModelUnavailable {
                        text,
                        files,
                        message,
                        plan_mode,
                        seq,
                    } => {
                        assert_eq!(seq, 9);
                        assert_eq!(text, "keep this");
                        assert_eq!(files, vec![file], "the files come back for the resend");
                        assert_eq!(plan_mode, None);
                        assert_eq!(message, wording);
                        assert!(!message.contains(TOKEN), "{message}");
                    }
                    other => panic!(
                        "{wording:?}, MCP carried: {carried}: expected ModelUnavailable, got {other:?}"
                    ),
                }
            }
        }
    }

    #[tokio::test]
    async fn only_a_400_that_names_the_model_is_a_model_refusal() {
        let cases = [
            (
                500,
                "Invalid model_config_id: model config not found or disabled.",
            ),
            (400, "Invalid busy_behavior value."),
        ];
        for (status, wording) in cases {
            let server = MockServer::start().await;
            let chat = Uuid::new_v4();
            Mock::given(method("POST"))
                .and(path(format!("/api/v2/chats/{chat}/messages")))
                .respond_with(api_error(status, wording))
                .mount(&server)
                .await;
            let (mut rt, mut rx) = runtime(&server.uri());
            rt.run(Effect::SendMessage {
                chat,
                text: "keep this".into(),
                model: Some(Uuid::new_v4()),
                busy: scuttle_core::config::BusyBehavior::Queue,
                turn: TurnOptions::default(),
                seq: 10,
            });
            match untag(next(&mut rx).await) {
                Msg::SendFailed { message, seq, .. } => {
                    assert_eq!(seq, 10);
                    assert_eq!(message, wording);
                }
                other => panic!("{status} {wording:?}: expected SendFailed, got {other:?}"),
            }
        }
    }

    /// A fresh directory for saved files, under the system temp directory.
    fn save_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("scuttle-dl-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    async fn serve_file(server: &MockServer, file: Uuid, body: &[u8], expect: u64) {
        Mock::given(method("GET"))
            .and(path(format!("/api/v2/chats/files/{file}")))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_bytes(body.to_vec())
                    .insert_header("content-type", "text/plain"),
            )
            .expect(expect)
            .mount(server)
            .await;
    }

    fn save(file: Uuid, name: &str, to: SaveTo, conflict: OnConflict) -> Effect {
        Effect::SaveFile {
            file,
            name: name.into(),
            to,
            conflict,
        }
    }

    #[tokio::test]
    async fn a_hostile_name_saves_inside_the_directory() {
        let server = MockServer::start().await;
        let file = Uuid::new_v4();
        serve_file(&server, file, b"ssh-ed25519 AAAA", 1).await;
        let dir = save_dir();
        let (mut rt, mut rx) = runtime(&server.uri());
        let name = scuttle_core::files::safe_name("../../.ssh/authorized_keys", "text/plain");
        rt.run(save(file, &name, SaveTo::Dir(dir.clone()), OnConflict::Ask));
        match next(&mut rx).await {
            Msg::FileSaved { path, .. } => assert_eq!(path, dir.join("authorized_keys.txt")),
            other => panic!("expected FileSaved, got {other:?}"),
        }
        assert_eq!(names(&dir), ["authorized_keys.txt"]);
        assert_eq!(
            std::fs::read(dir.join("authorized_keys.txt")).unwrap(),
            b"ssh-ed25519 AAAA"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn a_taken_name_is_never_replaced_without_asking() {
        let server = MockServer::start().await;
        let file = Uuid::new_v4();
        serve_file(&server, file, b"new", 2).await;
        let dir = save_dir();
        std::fs::write(dir.join("a.txt"), b"mine").unwrap();
        std::fs::write(dir.join("a (1).txt"), b"also mine").unwrap();
        let (mut rt, mut rx) = runtime(&server.uri());
        rt.run(save(
            file,
            "a.txt",
            SaveTo::Dir(dir.clone()),
            OnConflict::Ask,
        ));
        match next(&mut rx).await {
            Msg::FileConflict { path, .. } => assert_eq!(path, dir.join("a.txt")),
            other => panic!("expected FileConflict, got {other:?}"),
        }
        assert_eq!(std::fs::read(dir.join("a.txt")).unwrap(), b"mine");
        rt.run(save(
            file,
            "a.txt",
            SaveTo::File(dir.join("a.txt")),
            OnConflict::KeepBoth,
        ));
        match next(&mut rx).await {
            Msg::FileSaved { path, .. } => assert_eq!(path, dir.join("a (2).txt")),
            other => panic!("expected FileSaved, got {other:?}"),
        }
        rt.run(save(
            file,
            "a.txt",
            SaveTo::File(dir.join("a.txt")),
            OnConflict::Replace,
        ));
        assert!(matches!(next(&mut rx).await, Msg::FileSaved { .. }));
        assert_eq!(std::fs::read(dir.join("a.txt")).unwrap(), b"new");
        assert_eq!(std::fs::read(dir.join("a (1).txt")).unwrap(), b"also mine");
        assert_eq!(names(&dir), ["a (1).txt", "a (2).txt", "a.txt"]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn replace_onto_a_folder_fails_before_downloading() {
        let server = MockServer::start().await;
        let file = Uuid::new_v4();
        serve_file(&server, file, b"new", 0).await;
        let dir = save_dir();
        std::fs::create_dir(dir.join("a.txt")).unwrap();
        let (mut rt, mut rx) = runtime(&server.uri());
        rt.run(save(
            file,
            "a.txt",
            SaveTo::File(dir.join("a.txt")),
            OnConflict::Replace,
        ));
        match next(&mut rx).await {
            Msg::FileFailed { message, .. } => assert_eq!(
                message,
                format!(
                    "{} is a folder, so a.txt was not saved there.",
                    dir.join("a.txt").display()
                )
            ),
            other => panic!("expected FileFailed, got {other:?}"),
        }
        assert!(dir.join("a.txt").is_dir());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn a_file_over_the_limit_is_refused_before_it_is_written() {
        let server = MockServer::start().await;
        let file = Uuid::new_v4();
        let big = vec![b'x'; (scuttle_core::attachments::MAX_FILE_BYTES + 1) as usize];
        serve_file(&server, file, &big, 1).await;
        let dir = save_dir();
        let (mut rt, mut rx) = runtime(&server.uri());
        rt.run(save(
            file,
            "big.txt",
            SaveTo::Dir(dir.clone()),
            OnConflict::Ask,
        ));
        match next(&mut rx).await {
            Msg::FileFailed { message, .. } => assert_eq!(
                message,
                "big.txt is 10485761 bytes; the limit is 10485760 bytes."
            ),
            other => panic!("expected FileFailed, got {other:?}"),
        }
        assert!(names(&dir).is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A server whose one response streams 11 MiB with no Content-Length.
    async fn serve_endless() -> std::net::SocketAddr {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut tcp, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 4096];
            let _ = tcp.read(&mut request).await;
            let head =
                "HTTP/1.1 200 OK\r\ncontent-type: text/plain\r\ntransfer-encoding: chunked\r\n\r\n";
            let _ = tcp.write_all(head.as_bytes()).await;
            let chunk = vec![b'x'; 1024 * 1024];
            for _ in 0..11 {
                let _ = tcp
                    .write_all(format!("{:x}\r\n", chunk.len()).as_bytes())
                    .await;
                let _ = tcp.write_all(&chunk).await;
                let _ = tcp.write_all(b"\r\n").await;
            }
            let _ = tcp.write_all(b"0\r\n\r\n").await;
        });
        addr
    }

    #[tokio::test]
    async fn a_body_past_the_limit_without_a_length_leaves_nothing() {
        let addr = serve_endless().await;
        let dir = save_dir();
        let (mut rt, mut rx) = runtime(&format!("http://{addr}"));
        rt.run(save(
            Uuid::new_v4(),
            "endless.txt",
            SaveTo::Dir(dir.clone()),
            OnConflict::Ask,
        ));
        match next(&mut rx).await {
            Msg::FileFailed { message, .. } => assert_eq!(
                message,
                "endless.txt grew past the 10 MiB limit while it downloaded, so it was not saved."
            ),
            other => panic!("expected FileFailed, got {other:?}"),
        }
        assert!(names(&dir).is_empty(), "no partial file: {:?}", names(&dir));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn viewing_a_body_past_the_limit_without_a_length_is_refused() {
        let addr = serve_endless().await;
        let (mut rt, mut rx) = runtime(&format!("http://{addr}"));
        rt.run(Effect::ReadFile {
            file: Uuid::new_v4(),
            name: "endless.txt".into(),
        });
        match next(&mut rx).await {
            Msg::FileFailed { message, .. } => assert_eq!(
                message,
                "endless.txt grew past the 10 MiB limit while it downloaded."
            ),
            other => panic!("expected FileFailed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_refused_download_names_the_file_and_never_the_token() {
        let server = MockServer::start().await;
        let (refused, gone) = (Uuid::new_v4(), Uuid::new_v4());
        Mock::given(path(format!("/api/v2/chats/files/{refused}")))
            .respond_with(api_error(403, &format!("bad token {TOKEN}")))
            .mount(&server)
            .await;
        Mock::given(path(format!("/api/v2/chats/files/{gone}")))
            .respond_with(api_error(404, "Resource not found."))
            .mount(&server)
            .await;
        let dir = save_dir();
        let (mut rt, mut rx) = runtime(&server.uri());
        rt.run(save(
            refused,
            "a.txt",
            SaveTo::Dir(dir.clone()),
            OnConflict::Ask,
        ));
        match next(&mut rx).await {
            Msg::FileFailed { message, .. } => {
                assert!(
                    message.starts_with("Could not download a.txt: "),
                    "{message}"
                );
                assert!(
                    message.contains("[redacted]") && !message.contains(TOKEN),
                    "{message}"
                );
            }
            other => panic!("expected FileFailed, got {other:?}"),
        }
        rt.run(save(
            gone,
            "b.txt",
            SaveTo::Dir(dir.clone()),
            OnConflict::Ask,
        ));
        match next(&mut rx).await {
            Msg::FileFailed { message, .. } => {
                assert_eq!(
                    message,
                    "b.txt is no longer available, or you can't access it."
                );
            }
            other => panic!("expected FileFailed, got {other:?}"),
        }
        assert!(names(&dir).is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn reading_text_returns_it_and_refuses_bytes_that_are_not_utf8() {
        let server = MockServer::start().await;
        let (text, bytes) = (Uuid::new_v4(), Uuid::new_v4());
        serve_file(&server, text, "caf\u{e9}\n".as_bytes(), 1).await;
        serve_file(&server, bytes, &[0xff, 0xfe, 0x00], 1).await;
        let (mut rt, mut rx) = runtime(&server.uri());
        rt.run(Effect::ReadFile {
            file: text,
            name: "a.txt".into(),
        });
        match next(&mut rx).await {
            Msg::FileText { text: got, .. } => assert_eq!(got, "caf\u{e9}\n"),
            other => panic!("expected FileText, got {other:?}"),
        }
        rt.run(Effect::ReadFile {
            file: bytes,
            name: "b.txt".into(),
        });
        match next(&mut rx).await {
            Msg::FileFailed { message, .. } => assert_eq!(
                message,
                "b.txt is not UTF-8 text, so it cannot be shown. Press Enter to save it."
            ),
            other => panic!("expected FileFailed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_threshold_save_puts_once_and_a_reset_deletes() {
        use wiremock::matchers::body_json;
        let server = MockServer::start().await;
        let model = Uuid::new_v4();
        let route = format!("/api/v2/chats/config/user-compaction-thresholds/{model}");
        Mock::given(method("PUT"))
            .and(path(route.clone()))
            .and(body_json(serde_json::json!({ "threshold_percent": 55 })))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                serde_json::json!({ "model_config_id": model, "threshold_percent": 55 }),
            ))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path(route))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
        let (mut rt, mut rx) = runtime(&server.uri());
        rt.run(Effect::SaveThreshold(Save {
            model,
            change: Change::Set(55),
            generation: 3,
        }));
        assert!(matches!(
            next(&mut rx).await,
            Msg::ThresholdSaved { model: m, percent: Some(55), generation: 3 } if m == model
        ));
        rt.run(Effect::SaveThreshold(Save {
            model,
            change: Change::Reset,
            generation: 4,
        }));
        assert!(matches!(
            next(&mut rx).await,
            Msg::ThresholdSaved { model: m, percent: None, generation: 4 } if m == model
        ));
    }

    #[tokio::test]
    async fn a_refused_threshold_save_names_the_reason_and_never_the_token() {
        let server = MockServer::start().await;
        let model = Uuid::new_v4();
        Mock::given(method("PUT"))
            .and(path(format!(
                "/api/v2/chats/config/user-compaction-thresholds/{model}"
            )))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "message": "threshold_percent is out of range.",
                "detail": format!("got 105 with {TOKEN}"),
            })))
            .mount(&server)
            .await;
        let (mut rt, mut rx) = runtime(&server.uri());
        rt.run(Effect::SaveThreshold(Save {
            model,
            change: Change::Set(105),
            generation: 7,
        }));
        match next(&mut rx).await {
            Msg::ThresholdFailed {
                model: m,
                message,
                generation: 7,
            } => {
                assert_eq!(m, model);
                assert!(
                    message.contains("threshold_percent is out of range."),
                    "{message}"
                );
                assert!(message.contains("[redacted]"), "{message}");
                assert!(!message.contains(TOKEN), "{message}");
            }
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn the_threshold_overrides_load_as_model_and_percent_pairs() {
        let server = MockServer::start().await;
        let model = Uuid::new_v4();
        Mock::given(method("GET"))
            .and(path("/api/v2/chats/config/user-compaction-thresholds"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "thresholds": [
                    { "model_config_id": model, "threshold_percent": 40 },
                    { "threshold_percent": 10 }
                ]
            })))
            .mount(&server)
            .await;
        let (mut rt, mut rx) = runtime(&server.uri());
        rt.run(Effect::FetchThresholds { generation: 2 });
        match next(&mut rx).await {
            Msg::ThresholdsLoaded {
                thresholds,
                generation: 2,
            } => assert_eq!(
                thresholds,
                vec![(model, 40)],
                "an entry without a model is skipped"
            ),
            other => panic!("{other:?}"),
        }
    }
}
