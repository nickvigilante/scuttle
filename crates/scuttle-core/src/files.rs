//! A chat's files: the rows `/files` lists, and the safe names they are saved under.
//!
//! `chat.files` names every file the chat still stores, with its size and time, but not the
//! message that carried it or who sent it; those come from the loaded messages, which may be
//! only the newest page. A stored name is untrusted: a person or a model chose it, and the
//! server keeps path separators and `..` in it.

use std::path::{Path, PathBuf};

use coder_sdk::types;
use uuid::Uuid;

use crate::live::LiveBlock;
use crate::transcript::Transcript;

/// The longest file name most file systems take, in bytes.
pub const MAX_NAME_BYTES: usize = 255;

/// The most columns a message's first words take in `/files`.
const LABEL_CHARS: usize = 24;

/// Who sent a file into the chat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sender {
    You,
    Agent,
}

/// Where a file's message is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Place {
    /// A loaded message, by id, with the first words of its turn.
    Message { id: i64, label: String },
    /// The turn the agent is streaming now.
    Live,
    /// A message older than the loaded history.
    NotLoaded,
}

/// One row of `/files`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRow {
    pub id: Uuid,
    /// The stored name without control characters, for display. It is not a safe path.
    pub name: String,
    /// The base media type, lowercase, such as `image/png`; empty when unknown.
    pub media_type: String,
    pub size: Option<u64>,
    /// When it was stored, or when its message was sent, in seconds since the Unix epoch.
    pub created: Option<i64>,
    /// `None` while its message is not loaded, since `chat.files` does not say.
    pub from: Option<Sender>,
    pub place: Place,
    /// A message names it, but the chat no longer stores it, so it cannot be downloaded.
    pub expired: bool,
}

/// Where a save writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SaveTo {
    /// The save directory, where the file takes its safe name.
    Dir(PathBuf),
    /// A path the user typed: inside it when it is a directory, else exactly there.
    Typed(PathBuf),
    /// Exactly this path, as a "Keep both" or "Replace" after a taken name.
    File(PathBuf),
}

impl SaveTo {
    /// The file a save of `name`, a `safe_name`, writes.
    pub fn target(&self, name: &str, is_dir: impl Fn(&Path) -> bool) -> PathBuf {
        match self {
            SaveTo::Dir(dir) => dir.join(name),
            SaveTo::Typed(path) if is_dir(path) => path.join(name),
            SaveTo::Typed(path) | SaveTo::File(path) => path.clone(),
        }
    }
}

/// What a save does when its name is taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnConflict {
    /// Asks, with `Msg::FileConflict`, and writes nothing.
    Ask,
    Replace,
    /// Saves under the next free `numbered` name.
    KeepBoth,
}

/// An answer to "the name is taken", given by its letter: `k`, `r`, or `c`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictChoice {
    KeepBoth,
    Replace,
    Cancel,
}

impl ConflictChoice {
    /// The choices in the order the question lists them.
    pub const ALL: [ConflictChoice; 3] = [
        ConflictChoice::KeepBoth,
        ConflictChoice::Replace,
        ConflictChoice::Cancel,
    ];
}

/// A `/files` key, or a click on an attached file's line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileAction {
    /// Saves to the save directory.
    Save(Uuid),
    /// Asks where to save it.
    SaveAs(Uuid),
    /// Shows a text file in the pager.
    View(Uuid),
    /// Scrolls the transcript to the message that carries it.
    Jump(Uuid),
}

impl FileAction {
    pub fn file(self) -> Uuid {
        match self {
            FileAction::Save(id)
            | FileAction::SaveAs(id)
            | FileAction::View(id)
            | FileAction::Jump(id) => id,
        }
    }
}

/// A save that found its name taken, waiting on the user's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaveConflict {
    pub file: Uuid,
    /// The file's safe name, for the messages a retry shows.
    pub name: String,
    /// The taken path.
    pub path: PathBuf,
}

fn kind(p: &types::CodersdkChatMessagePart) -> &str {
    p.type_.as_ref().map(|t| t.as_str()).unwrap_or_default()
}

/// A part's file name as the server sent it: `name`, else the legacy `file_name`, which
/// servers now send empty. Pass it through `display_name` before showing it.
pub fn part_name(p: &types::CodersdkChatMessagePart) -> &str {
    [&p.name, &p.file_name]
        .into_iter()
        .find_map(|n| n.as_deref().filter(|n| !n.trim().is_empty()))
        .unwrap_or_default()
}

/// Whether `c` is a control character or a Unicode format character: a zero-width space or
/// mark, a bidi override or isolate, a tag character, and the like. `invoice\u{202e}gpj.zip`
/// would otherwise read as `invoicepiz.jpg`. The zero width joiner stays, as `is_format`
/// keeps it, so an emoji in a name stays whole.
fn hidden(c: char) -> bool {
    c.is_control() || crate::text::is_format(c)
}

/// `text` without control or invisible format characters. A zero width joiner stays only
/// between two visible characters, so an emoji such as 👩‍💻 stays whole, while a joiner
/// alone, at either end, or beside a space, a dot, or another joiner would leave a name that
/// reads as blank or as something else. `keep` names characters kept as they are, such as
/// a line break.
fn visible(text: &str, keep: &[char]) -> String {
    const ZWJ: char = '\u{200d}';
    let chars: Vec<char> = text
        .chars()
        .filter(|c| keep.contains(c) || !hidden(*c))
        .collect();
    let solid = |c: Option<&char>| {
        c.is_some_and(|c| !c.is_whitespace() && *c != ZWJ && *c != '.' && !hidden(*c))
    };
    chars
        .iter()
        .enumerate()
        .filter(|&(i, c)| {
            *c != ZWJ
                || (solid(i.checked_sub(1).and_then(|j| chars.get(j))) && solid(chars.get(i + 1)))
        })
        .map(|(_, c)| *c)
        .collect()
}

/// `name` without control or invisible format characters, trimmed, for the screen. Show every
/// file name through this, so a bidi override cannot disguise one.
pub fn display_name(name: &str) -> String {
    visible(name, &[]).trim().to_owned()
}

/// The name a row shows: its own, or a stand-in when it has none.
pub fn shown_name(name: &str) -> &str {
    if name.is_empty() { "the file" } else { name }
}

/// `media_type` without parameters, lowercase.
fn base_type(media_type: &str) -> String {
    media_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
}

/// The first words of `text`'s first non-blank line, quoted, cut to `LABEL_CHARS`.
fn label(text: &str) -> Option<String> {
    let text = visible(&text.replace('\t', " "), &['\n']);
    let line = text.lines().map(str::trim).find(|l| !l.is_empty())?;
    let mut words: String = line.chars().take(LABEL_CHARS).collect();
    if line.chars().count() > LABEL_CHARS {
        words.push('\u{2026}');
    }
    Some(format!("\u{201c}{words}\u{201d}"))
}

/// Every file of the chat, newest first: those `chat.files` stores, those the loaded
/// messages carry, and those the live turn streams. A file takes the first message that
/// carries it. One a message carries but the chat no longer stores, and older than the
/// newest stored one, is expired, as the web UI judges it.
pub fn file_rows(chat: Option<&types::CodersdkChat>, transcript: &Transcript) -> Vec<FileRow> {
    let stored = chat.map(|c| c.files.as_slice()).unwrap_or_default();
    let newest_stored = stored
        .iter()
        .filter_map(|f| f.created_at)
        .map(|t| t.timestamp())
        .max();
    let mut rows: Vec<FileRow> = stored
        .iter()
        .filter_map(|meta| {
            Some(FileRow {
                id: meta.id?,
                name: display_name(meta.name.as_deref().unwrap_or_default()),
                media_type: base_type(meta.mime_type.as_deref().unwrap_or_default()),
                size: meta.size_bytes.and_then(|s| u64::try_from(s).ok()),
                created: meta.created_at.map(|t| t.timestamp()),
                from: None,
                place: Place::NotLoaded,
                expired: false,
            })
        })
        .collect();
    let mut turn: Option<String> = None;
    for m in transcript.messages() {
        let from = match m.role.as_ref().map(|r| r.as_str()) {
            Some("user") => Sender::You,
            Some("assistant") => Sender::Agent,
            _ => continue,
        };
        if from == Sender::You
            && let Some(words) = m
                .content
                .iter()
                .filter(|p| kind(p) == "text")
                .find_map(|p| label(p.text.as_deref().unwrap_or_default()))
        {
            turn = Some(words);
        }
        let sent = m.created_at.map(|t| t.timestamp());
        for p in m.content.iter().filter(|p| kind(p) == "file") {
            let Some(id) = p.file_id else { continue };
            let place = Place::Message {
                id: m.id.unwrap_or_default(),
                label: turn.clone().unwrap_or_else(|| "earlier".into()),
            };
            match rows.iter_mut().find(|r| r.id == id) {
                Some(row) if row.from.is_none() => {
                    row.from = Some(from);
                    row.place = place;
                }
                Some(_) => {}
                None => rows.push(FileRow {
                    id,
                    name: display_name(part_name(p)),
                    media_type: base_type(p.media_type.as_deref().unwrap_or_default()),
                    size: None,
                    created: sent,
                    from: Some(from),
                    place,
                    expired: matches!((sent, newest_stored), (Some(s), Some(n)) if s < n),
                }),
            }
        }
    }
    for block in &transcript.live.blocks {
        let LiveBlock::File {
            file_id: Some(id),
            name,
            media_type,
        } = block
        else {
            continue;
        };
        match rows.iter_mut().find(|r| r.id == *id) {
            Some(row) if row.from.is_none() => {
                row.from = Some(Sender::Agent);
                row.place = Place::Live;
            }
            Some(_) => {}
            None => rows.push(FileRow {
                id: *id,
                name: display_name(name.as_deref().unwrap_or_default()),
                media_type: base_type(media_type.as_deref().unwrap_or_default()),
                size: None,
                created: None,
                from: Some(Sender::Agent),
                place: Place::Live,
                expired: false,
            }),
        }
    }
    // A file with no time yet, such as a live one, is the newest. The sort is stable.
    rows.sort_by_key(|r| std::cmp::Reverse(r.created.unwrap_or(i64::MAX)));
    rows
}

/// The id of the first loaded message that carries `file`.
pub fn message_with(transcript: &Transcript, file: Uuid) -> Option<i64> {
    transcript
        .messages()
        .find(|m| {
            m.content
                .iter()
                .any(|p| kind(p) == "file" && p.file_id == Some(file))
        })
        .and_then(|m| m.id)
}

/// The extension a file of `media_type` takes, for the types a chat stores.
pub fn extension(media_type: &str) -> Option<&'static str> {
    Some(match base_type(media_type).as_str() {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/svg+xml" => "svg",
        "text/plain" => "txt",
        "text/markdown" => "md",
        "text/csv" => "csv",
        "application/json" => "json",
        "application/pdf" => "pdf",
        "application/zip" => "zip",
        _ => return None,
    })
}

/// The short type `/files` shows, such as `PNG`, or `FILE` for a type it does not know.
pub fn badge(media_type: &str) -> String {
    extension(media_type).map_or_else(|| "FILE".into(), str::to_ascii_uppercase)
}

/// Whether `v` can show a file of `media_type` in the pager.
pub fn is_text(media_type: &str) -> bool {
    let t = base_type(media_type);
    t.starts_with("text/") || t == "application/json"
}

/// `text` without control characters other than line breaks and tabs, so a file cannot
/// send the terminal an escape sequence through the pager.
pub fn printable(text: &str) -> String {
    text.chars()
        .filter(|c| matches!(c, '\n' | '\t') || !(c.is_control() || crate::text::is_format(*c)))
        .collect()
}

/// `text` with a leading `~` or `~/` resolved against `home`.
pub fn expand_home(text: &str, home: Option<&Path>) -> PathBuf {
    match (text, home) {
        ("~", Some(home)) => home.to_owned(),
        (text, Some(home)) if text.starts_with("~/") => home.join(&text[2..]),
        (text, _) => PathBuf::from(text),
    }
}

/// `path` as notices show it, with `home` as `~`.
pub fn display_path(path: &Path, home: Option<&Path>) -> String {
    match home.and_then(|h| path.strip_prefix(h).ok()) {
        Some(rest) if rest.as_os_str().is_empty() => "~".into(),
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

/// `text` cut to at most `bytes` bytes, on a character boundary.
fn cut(text: &str, bytes: usize) -> &str {
    let mut end = bytes.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// `name`'s stem and extension, when it has a short extension after a non-empty stem.
fn split_ext(name: &str) -> Option<(&str, &str)> {
    name.rsplit_once('.').filter(|(stem, ext)| {
        !stem.is_empty() && !ext.is_empty() && ext.len() <= 10 && !ext.contains(' ')
    })
}

/// `name` cut to `MAX_NAME_BYTES`, keeping its extension, with no space, dot, or joiner left
/// at the end of the stem by the cut. `None` when nothing is left of the stem.
fn fit(name: &str) -> Option<String> {
    let (stem, tail) = match split_ext(name) {
        Some((stem, ext)) => (stem, format!(".{ext}")),
        None => (name, String::new()),
    };
    let stem = cut(stem, MAX_NAME_BYTES.saturating_sub(tail.len()))
        .trim_end_matches(['.', ' ', '\u{200d}']);
    (!stem.is_empty()).then(|| format!("{stem}{tail}"))
}

/// A file name for `name`, a stored name, that is safe to join to a directory: its last
/// path component, without control or invisible format characters, with `<>:"|?*` replaced, without leading
/// dots or trailing dots and spaces, with the extension `media_type` takes when it has none,
/// and at most `MAX_NAME_BYTES`. A name with nothing left is `attachment.<ext>`, as the web
/// UI names it.
pub fn safe_name(name: &str, media_type: &str) -> String {
    let last = name.rsplit(['/', '\\']).next().unwrap_or_default();
    let cleaned: String = visible(last, &[])
        .chars()
        .map(|c| {
            if matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*') {
                '_'
            } else {
                c
            }
        })
        .collect();
    let trimmed = cleaned
        .trim()
        .trim_start_matches('.')
        .trim_end_matches(['.', ' '])
        .trim();
    let ext = extension(media_type);
    let fitted = match ext {
        _ if trimmed.is_empty() => None,
        Some(ext) if split_ext(trimmed).is_none() => fit(&format!("{trimmed}.{ext}")),
        _ => fit(trimmed),
    };
    fitted.unwrap_or_else(|| match ext {
        Some(ext) => format!("attachment.{ext}"),
        None => "attachment".into(),
    })
}

/// `name` with ` (n)` before its extension, for a copy kept beside a taken name.
pub fn numbered(name: &str, n: u32) -> String {
    let (stem, tail) = match split_ext(name) {
        Some((stem, ext)) => (stem, format!(" ({n}).{ext}")),
        None => (name, format!(" ({n})")),
    };
    format!(
        "{}{tail}",
        cut(stem, MAX_NAME_BYTES.saturating_sub(tail.len()))
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::live::LiveBlock;
    use serde_json::json;

    const FILE: &str = "5a1e0c3b-7d2f-4e61-9b8a-0c1d2e3f4a5b";
    const OLD: &str = "6b2f1d4c-8e3a-4f72-8c9b-1d2e3f4a5b6c";
    const GONE: &str = "7c3a2e5d-9f4b-4a83-9dac-2e3f4a5b6c7d";

    fn id(s: &str) -> Uuid {
        s.parse().unwrap()
    }

    fn chat(files: serde_json::Value) -> types::CodersdkChat {
        serde_json::from_value(json!({"id": Uuid::new_v4(), "children": [], "files": files,
            "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}}))
        .unwrap()
    }

    fn transcript(messages: serde_json::Value) -> Transcript {
        let mut t = Transcript::default();
        t.load(serde_json::from_value(messages).unwrap());
        t
    }

    #[test]
    fn a_server_name_never_escapes_the_directory() {
        for (name, kind, want) in [
            (
                "../../.ssh/authorized_keys",
                "text/plain",
                "authorized_keys.txt",
            ),
            ("/etc/passwd", "text/plain", "passwd.txt"),
            ("..\\..\\x.md", "text/markdown", "x.md"),
            ("..", "image/png", "attachment.png"),
            (".", "image/png", "attachment.png"),
            ("", "application/pdf", "attachment.pdf"),
            ("dir/", "application/zip", "attachment.zip"),
            ("", "application/octet-stream", "attachment"),
            (".env", "text/plain", "env.txt"),
            ("a\u{7}b\nc.png", "image/png", "abc.png"),
            ("a\0b.txt", "text/plain", "ab.txt"),
            ("\0", "image/png", "attachment.png"),
            ("\u{202e}\u{200b}", "application/pdf", "attachment.pdf"),
            ("bad:name?.md", "text/markdown", "bad_name_.md"),
            ("  report.pdf. ", "application/pdf", "report.pdf"),
            ("build-logs", "application/zip", "build-logs.zip"),
            ("report.pdf", "application/pdf", "report.pdf"),
            ("README", "application/octet-stream", "README"),
            ("shot", "image/png; charset=binary", "shot.png"),
            (
                "invoice\u{202e}gpj.zip",
                "application/zip",
                "invoicegpj.zip",
            ),
            ("\u{feff}a\u{200b}b\u{2066}.txt", "text/plain", "ab.txt"),
        ] {
            assert_eq!(safe_name(name, kind), want, "{name:?} as {kind}");
        }
        let long = format!("{}.pdf", "a".repeat(300));
        let cut = safe_name(&long, "application/pdf");
        assert!(
            cut.len() <= MAX_NAME_BYTES && cut.ends_with(".pdf"),
            "{cut}"
        );
        let wide = format!("{}.txt", "\u{e9}".repeat(200));
        let cut = safe_name(&wide, "text/plain");
        assert!(
            cut.len() <= MAX_NAME_BYTES && cut.ends_with(".txt"),
            "{cut}"
        );
    }

    #[test]
    fn a_kept_copy_takes_the_next_number_before_its_extension() {
        assert_eq!(numbered("report.pdf", 1), "report (1).pdf");
        assert_eq!(numbered("README", 2), "README (2)");
        let long = numbered(&format!("{}.pdf", "a".repeat(251)), 7);
        assert!(
            long.len() <= MAX_NAME_BYTES && long.ends_with(" (7).pdf"),
            "{long}"
        );
    }

    #[test]
    fn types_have_a_badge_and_say_whether_they_show() {
        assert_eq!(badge("application/zip"), "ZIP");
        assert_eq!(badge("image/svg+xml"), "SVG");
        assert_eq!(badge("application/x-thing"), "FILE");
        assert_eq!(extension("image/jpeg"), Some("jpg"));
        assert!(is_text("text/csv") && is_text("application/json") && is_text("TEXT/PLAIN"));
        assert!(!is_text("image/png") && !is_text("application/zip"));
        assert_eq!(
            printable("a\u{1b}]52;c;eA==\u{7}b\r\n\tc"),
            "a]52;c;eA==b\n\tc"
        );
    }

    #[test]
    fn paths_expand_and_show_the_home_directory_as_a_tilde() {
        let home = Path::new("/h");
        assert_eq!(
            expand_home("~/out/a.txt", Some(home)),
            PathBuf::from("/h/out/a.txt")
        );
        assert_eq!(expand_home("~", Some(home)), PathBuf::from("/h"));
        assert_eq!(expand_home("~/x", None), PathBuf::from("~/x"));
        assert_eq!(expand_home("/tmp/x", Some(home)), PathBuf::from("/tmp/x"));
        assert_eq!(
            display_path(Path::new("/h/Downloads/a"), Some(home)),
            "~/Downloads/a"
        );
        assert_eq!(display_path(Path::new("/h"), Some(home)), "~");
        assert_eq!(display_path(Path::new("/tmp/a"), Some(home)), "/tmp/a");
    }

    #[test]
    fn a_save_target_joins_only_the_safe_name_to_a_directory() {
        let is_dir = |p: &Path| p == Path::new("/dl") || p == Path::new("/typed");
        assert_eq!(
            SaveTo::Dir("/dl".into()).target("a.txt", is_dir),
            PathBuf::from("/dl/a.txt")
        );
        assert_eq!(
            SaveTo::Typed("/typed".into()).target("a.txt", is_dir),
            PathBuf::from("/typed/a.txt"),
            "a typed directory saves inside it"
        );
        assert_eq!(
            SaveTo::Typed("/typed/b.txt".into()).target("a.txt", is_dir),
            PathBuf::from("/typed/b.txt"),
            "a typed file name is used as typed"
        );
        assert_eq!(
            SaveTo::File("/dl".into()).target("a.txt", is_dir),
            PathBuf::from("/dl"),
            "an exact path is never joined"
        );
    }

    #[test]
    fn rows_list_every_file_newest_first_with_who_sent_it_and_where() {
        let chat = chat(json!([
            {"id": OLD, "name": "notes.md", "mime_type": "text/markdown", "size_bytes": 120,
                "created_at": "2026-10-05T11:00:00Z"},
            {"id": FILE, "name": "build-logs.zip", "mime_type": "application/zip",
                "size_bytes": 6144, "created_at": "2026-10-05T12:00:02Z"}
        ]));
        let t = transcript(json!([
            {"id": 3, "role": "user", "created_at": "2026-10-05T12:00:00Z", "content": [
                {"type": "text", "text": "  \nbundle the build logs for me please, thanks"}]},
            {"id": 4, "role": "assistant", "created_at": "2026-10-05T12:00:01Z", "content": [
                {"type": "tool-call", "tool_call_id": "c1", "tool_name": "attach_file",
                    "args": {"name": "build-logs.zip", "path": "/workspace/build-logs.zip"}}]},
            {"id": 6, "role": "assistant", "created_at": "2026-10-05T12:00:02Z", "content": [
                {"type": "file", "file_id": FILE, "media_type": "application/zip",
                    "name": "build-logs.zip", "file_name": ""}]},
            {"id": 7, "role": "user", "created_at": "2026-10-05T12:01:00Z", "content": [
                {"type": "file", "file_id": FILE, "media_type": "application/zip",
                    "name": "build-logs.zip"}]}
        ]));
        let rows = file_rows(Some(&chat), &t);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id, id(FILE), "newest first");
        assert_eq!(rows[0].name, "build-logs.zip");
        assert_eq!(rows[0].media_type, "application/zip");
        assert_eq!(rows[0].size, Some(6144));
        assert_eq!(rows[0].from, Some(Sender::Agent), "its first message wins");
        assert_eq!(
            rows[0].place,
            Place::Message {
                id: 6,
                label: "\u{201c}bundle the build logs fo\u{2026}\u{201d}".into()
            }
        );
        assert!(!rows[0].expired);
        assert_eq!(rows[1].id, id(OLD));
        assert_eq!(
            rows[1].from, None,
            "its message is older than the loaded history"
        );
        assert_eq!(rows[1].place, Place::NotLoaded);
        assert_eq!(message_with(&t, id(FILE)), Some(6));
        assert_eq!(message_with(&t, id(OLD)), None);
    }

    #[test]
    fn a_file_the_chat_dropped_is_expired_and_a_live_one_is_listed() {
        let chat = chat(json!([
            {"id": FILE, "name": "new.png", "mime_type": "image/png", "size_bytes": 10,
                "created_at": "2026-10-05T12:00:00Z"}
        ]));
        let mut t = transcript(json!([
            {"id": 2, "role": "user", "created_at": "2026-10-05T10:00:00Z", "content": [
                {"type": "text", "text": "here"},
                {"type": "file", "file_id": GONE, "media_type": "text/plain", "name": "a\u{7}.txt"}]}
        ]));
        t.live.blocks.push(LiveBlock::File {
            file_id: Some(id(OLD)),
            name: Some("shot.png".into()),
            media_type: Some("image/png".into()),
        });
        let rows = file_rows(Some(&chat), &t);
        let gone = rows.iter().find(|r| r.id == id(GONE)).unwrap();
        assert!(
            gone.expired,
            "older than the newest stored file and not stored"
        );
        assert_eq!(
            gone.name, "a.txt",
            "control characters never reach the screen"
        );
        assert_eq!(gone.from, Some(Sender::You));
        assert_eq!(
            gone.place,
            Place::Message {
                id: 2,
                label: "\u{201c}here\u{201d}".into()
            }
        );
        let live = rows.iter().find(|r| r.id == id(OLD)).unwrap();
        assert_eq!(live.place, Place::Live);
        assert_eq!(live.from, Some(Sender::Agent));
        assert_eq!(rows[0].id, id(OLD), "a file with no time yet is the newest");
        assert!(file_rows(None, &Transcript::default()).is_empty());
    }

    #[test]
    fn invisible_format_characters_never_reach_a_name() {
        assert_eq!(
            safe_name("invoice\u{061c}gpj.zip", "application/zip"),
            "invoicegpj.zip"
        );
        assert_eq!(
            safe_name(
                "a\u{2060}b\u{e0041}c\u{00ad}d\u{180e}e\u{206a}.txt",
                "text/plain"
            ),
            "abcde.txt"
        );
        assert_eq!(
            safe_name("\u{2060}\u{2061}\u{e0041}", "image/png"),
            "attachment.png",
            "a name of only invisible characters is empty, not a hidden file"
        );
        assert_eq!(
            safe_name("\u{1f469}\u{200d}\u{1f4bb}.txt", "text/plain"),
            "\u{1f469}\u{200d}\u{1f4bb}.txt",
            "a joiner keeps an emoji whole"
        );
        assert_eq!(display_name("a\u{061c}\u{2060}b\u{e0041}\u{fff9}c"), "abc");
        assert_eq!(
            display_name("  a.txt \u{200b} "),
            "a.txt",
            "padding is trimmed"
        );
    }

    #[test]
    fn a_cut_name_never_ends_in_a_space_or_dot() {
        let name = format!("{} b", "a".repeat(254));
        assert_eq!(
            safe_name(&name, "application/octet-stream"),
            "a".repeat(254)
        );
    }

    #[test]
    fn a_label_drops_hidden_characters() {
        let chat = chat(json!([]));
        let t = transcript(json!([
            {"id": 1, "role": "user", "created_at": "2026-10-05T12:00:00Z", "content": [
                {"type": "text", "text": "hi\u{202e}\u{7} there"},
                {"type": "file", "file_id": FILE, "media_type": "text/plain", "name": "a.txt"}]}
        ]));
        let rows = file_rows(Some(&chat), &t);
        assert_eq!(
            rows[0].place,
            Place::Message {
                id: 1,
                label: "\u{201c}hi there\u{201d}".into()
            }
        );
    }

    #[test]
    fn a_live_file_never_replaces_a_row_it_matches() {
        let chat = chat(json!([
            {"id": OLD, "name": "stored.png", "mime_type": "image/png", "size_bytes": 9,
                "created_at": "2026-10-05T12:00:00Z"},
            {"id": FILE, "name": "both.zip", "mime_type": "application/zip", "size_bytes": 5,
                "created_at": "2026-10-05T12:00:01Z"}
        ]));
        let mut t = transcript(json!([
            {"id": 8, "role": "user", "created_at": "2026-10-05T12:00:00Z", "content": [
                {"type": "text", "text": "go"}]},
            {"id": 9, "role": "assistant", "created_at": "2026-10-05T12:00:01Z", "content": [
                {"type": "file", "file_id": FILE, "media_type": "application/zip",
                    "name": "both.zip"}]}
        ]));
        for file in [OLD, FILE] {
            t.live.blocks.push(LiveBlock::File {
                file_id: Some(id(file)),
                name: Some("live".into()),
                media_type: Some("application/zip".into()),
            });
        }
        let rows = file_rows(Some(&chat), &t);
        assert_eq!(
            rows.len(),
            2,
            "a final message and a live block share one row"
        );
        let both = rows.iter().find(|r| r.id == id(FILE)).unwrap();
        assert_eq!(
            both.place,
            Place::Message {
                id: 9,
                label: "\u{201c}go\u{201d}".into()
            }
        );
        assert_eq!(both.name, "both.zip");
        let stored = rows.iter().find(|r| r.id == id(OLD)).unwrap();
        assert_eq!(
            stored.place,
            Place::Live,
            "a stored file with no message is live"
        );
        assert_eq!(stored.from, Some(Sender::Agent));
        assert_eq!(
            stored.size,
            Some(9),
            "the stored row keeps its size and name"
        );
        assert_eq!(stored.name, "stored.png");
    }

    #[test]
    fn the_legacy_file_name_names_a_part_with_no_name() {
        let chat = chat(json!([
            {"id": FILE, "name": "n.txt", "mime_type": "text/plain", "size_bytes": 1,
                "created_at": "2026-10-05T12:00:00Z"}
        ]));
        let mut t = transcript(json!([
            {"id": 1, "role": "user", "created_at": "2026-10-05T11:00:00Z", "content": [
                {"type": "file", "file_id": GONE, "media_type": "text/plain",
                    "file_name": "legacy.txt"},
                {"type": "file", "file_id": OLD, "media_type": "text/plain",
                    "name": " ", "file_name": "second.txt"}]}
        ]));
        t.live.blocks.clear();
        let rows = file_rows(Some(&chat), &t);
        let name = |f: &str| rows.iter().find(|r| r.id == id(f)).unwrap().name.clone();
        assert_eq!(name(GONE), "legacy.txt");
        assert_eq!(name(OLD), "second.txt");
    }

    #[test]
    fn a_file_is_expired_only_when_older_than_the_newest_stored_one() {
        let chat = chat(json!([
            {"id": FILE, "name": "new.txt", "mime_type": "text/plain", "size_bytes": 1,
                "created_at": "2026-10-05T12:00:00Z"},
            {"id": OLD, "name": "old.txt", "mime_type": "text/plain", "size_bytes": 1,
                "created_at": "2026-10-05T10:00:00Z"}
        ]));
        let t = transcript(json!([
            {"id": 1, "role": "user", "created_at": "2026-10-05T11:00:00Z", "content": [
                {"type": "file", "file_id": GONE, "media_type": "text/plain", "name": "g.txt"}]}
        ]));
        let rows = file_rows(Some(&chat), &t);
        assert!(rows.iter().find(|r| r.id == id(GONE)).unwrap().expired);
    }

    #[test]
    fn a_joiner_stays_only_between_two_visible_characters() {
        let joined = "\u{1f469}\u{200d}\u{1f4bb}";
        assert_eq!(safe_name("\u{200d}", "image/png"), "attachment.png");
        assert_eq!(safe_name(" \u{200d} ", "image/png"), "attachment.png");
        assert_eq!(safe_name("\u{200d}\u{200d}", "image/png"), "attachment.png");
        assert_eq!(
            safe_name("a.\u{200d}", "image/png"),
            "a.png",
            "the joiner goes, the dot is trimmed, and the media type's extension is added"
        );
        assert_eq!(safe_name("a\u{200d}.png", "image/png"), "a.png");
        assert_eq!(safe_name("a \u{200d}b.txt", "text/plain"), "a b.txt");
        assert_eq!(
            safe_name(&format!("{joined}.png"), "image/png"),
            format!("{joined}.png")
        );
        assert_eq!(display_name(joined), joined);
        assert_eq!(display_name(" \u{200d} "), "");
        assert_eq!(display_name("\u{200d}a\u{200d}\u{200d}b\u{200d}"), "ab");
        assert_eq!(shown_name(&display_name(" \u{200d} ")), "the file");
    }

    #[test]
    fn a_label_turns_a_tab_into_a_space() {
        assert_eq!(label("a\tb").as_deref(), Some("\u{201c}a b\u{201d}"));
    }

    #[test]
    fn a_part_name_beats_its_legacy_file_name() {
        let chat = chat(json!([]));
        let t = transcript(json!([
            {"id": 1, "role": "user", "created_at": "2026-10-05T11:00:00Z", "content": [
                {"type": "file", "file_id": GONE, "media_type": "text/plain",
                    "name": "new.txt", "file_name": "old.txt"}]}
        ]));
        assert_eq!(file_rows(Some(&chat), &t)[0].name, "new.txt");
    }

    #[test]
    fn a_cut_never_leaves_a_dangling_joiner() {
        let joined = "\u{1f469}\u{200d}\u{1f4bb}".repeat(30);
        for prefix in 0..=11 {
            for (ext, kind) in [(".png", "image/png"), (".txt", "text/plain"), ("", "")] {
                let name = format!("{}{joined}{ext}", "a".repeat(prefix));
                let got = safe_name(&name, kind);
                let stem = got.strip_suffix(ext).unwrap_or(&got);
                assert!(got.len() <= MAX_NAME_BYTES, "{got}");
                assert!(!stem.ends_with('\u{200d}'), "{prefix} {ext:?}: {got:?}");
                assert!(!got.ends_with('\u{200d}'), "{prefix} {ext:?}: {got:?}");
            }
        }
    }
}
