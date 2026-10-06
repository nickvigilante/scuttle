//! The local, TUI-only config file. It never holds secrets.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Deserializer};
use uuid::Uuid;

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

/// The loading animation: one style, or a random one each turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SpinnerSetting {
    #[default]
    Random,
    Braille,
    Line,
    Arc,
    Bounce,
    Bar,
}

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

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct WelcomeConfig {
    pub show: bool,
    pub art_file: Option<PathBuf>,
}

impl Default for WelcomeConfig {
    fn default() -> Self {
        WelcomeConfig {
            show: true,
            art_file: None,
        }
    }
}

/// The `[chats]` table: how `/chats` draws its rows.
#[derive(Debug, Clone, PartialEq, Deserialize, Default)]
#[serde(default)]
pub struct ChatsConfig {
    /// The marker before a pinned chat's title. Any text works, and an empty string shows
    /// none. Left out, the icon set's pin shows: 📌 in text, or nf-oct-pin with Nerd Font icons.
    pub pin_icon: Option<String>,
}

/// The `[files]` table: where chat files are saved.
#[derive(Debug, Clone, PartialEq, Deserialize, Default)]
#[serde(default)]
pub struct FilesConfig {
    /// The directory Enter in `/files` and a click on an attached file save to; `~/` is the
    /// home directory. Left out, `save_dir` picks one.
    pub save_dir: Option<PathBuf>,
}

/// Where files are saved: `files.save_dir` with `~` expanded, else `~/Downloads` when it is a
/// directory, else the home directory, else the current directory.
pub fn save_dir(
    files: &FilesConfig,
    home: Option<&Path>,
    is_dir: impl Fn(&Path) -> bool,
) -> PathBuf {
    if let Some(dir) = files.save_dir.as_deref() {
        return crate::files::expand_home(&dir.to_string_lossy(), home);
    }
    match home {
        Some(home) if is_dir(&home.join("Downloads")) => home.join("Downloads"),
        Some(home) => home.to_owned(),
        None => PathBuf::from("."),
    }
}

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
    Mcp,
}

impl StatusField {
    /// Every field, in the order the `/statusline` editor lists the hidden ones.
    pub const ALL: [StatusField; 12] = [
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
        StatusField::Mcp,
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
            StatusField::Mcp => "mcp",
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
                "The open chat's organization, when you have several (/organization)"
            }
            StatusField::PlanMode => "Plan mode, while it is on (/plan-mode)",
            StatusField::Spend => "Your AI spend this period, of your budget",
            StatusField::Status => "The chat's status and the connection",
            StatusField::Cost => "This chat's cost, for its whole tree",
            StatusField::Quota => "Workspace credits used, of your quota",
            StatusField::Queue => "Messages waiting in the queue",
            StatusField::Mcp => "MCP servers the next message uses (/mcp)",
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
            (Some(v), true) => Some(
                THRESHOLD_STEPS
                    .iter()
                    .copied()
                    .find(|s| *s > v)
                    .unwrap_or(v),
            ),
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

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct LocalConfig {
    pub mouse: bool,
    pub busy_behavior: BusyBehavior,
    pub composer_max_lines: u16,
    /// The loading animation, `"random"` unless the file names a style.
    pub spinner: SpinnerSetting,
    /// Which icons to draw. `None` leaves it to the `NERD_FONT` environment variable, then text.
    pub icons: Option<IconSet>,
    pub welcome: WelcomeConfig,
    pub chats: ChatsConfig,
    /// Where chat files are saved.
    pub files: FilesConfig,
    /// The footer's fields and their warnings, which `/statusline` edits.
    pub statusline: StatuslineConfig,
    pub density: BTreeMap<String, Density>,
    /// The organization new chats go to, saved by `/organization`. An ID, not a secret.
    /// A value that is not a valid ID is ignored, so startup falls back to the default.
    #[serde(deserialize_with = "lenient_uuid")]
    pub organization: Option<Uuid>,
    /// The reasoning effort chosen per model for a blank chat's new-chat form, saved by
    /// `/effort` and the slider, keyed by model config ID. Not a secret. An entry whose key
    /// is not a UUID or whose value is not a string is dropped, like the lenient
    /// `organization` key.
    #[serde(deserialize_with = "lenient_efforts")]
    pub efforts: BTreeMap<Uuid, String>,
}

impl Default for LocalConfig {
    fn default() -> Self {
        LocalConfig {
            mouse: true,
            busy_behavior: BusyBehavior::Queue,
            composer_max_lines: 10,
            spinner: SpinnerSetting::Random,
            icons: None,
            welcome: WelcomeConfig::default(),
            chats: ChatsConfig::default(),
            files: FilesConfig::default(),
            statusline: StatuslineConfig::default(),
            density: BTreeMap::new(),
            organization: None,
            efforts: BTreeMap::new(),
        }
    }
}

/// Reads an optional string and keeps it only when it parses as a UUID.
fn lenient_uuid<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Uuid>, D::Error> {
    let text = Option::<String>::deserialize(d)?;
    Ok(text.and_then(|t| Uuid::parse_str(t.trim()).ok()))
}

/// Reads the `[efforts]` table, dropping any entry whose key is not a UUID or whose value
/// is not a string. An `efforts` value that is not a table at all (a string, a number, an
/// array) is dropped wholesale instead of failing the whole config load.
fn lenient_efforts<'de, D: Deserializer<'de>>(d: D) -> Result<BTreeMap<Uuid, String>, D::Error> {
    let raw = toml::Value::deserialize(d)?;
    let Some(table) = raw.as_table() else {
        return Ok(BTreeMap::new());
    };
    Ok(table
        .iter()
        .filter_map(|(k, v)| {
            let id = Uuid::parse_str(k.trim()).ok()?;
            let effort = v.as_str()?.to_owned();
            Some((id, effort))
        })
        .collect())
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    #[error("config key {0:?} looks like a secret; scuttle's config never holds secrets")]
    Secret(String),
    #[error("could not parse the config file: {0}")]
    Parse(String),
    #[error("could not read or write the config file: {0}")]
    Io(String),
    /// The owner took write permission off the file, so a save leaves it as it is.
    #[error(
        "config.toml is read-only, so the change was not saved; make it writable to save settings"
    )]
    ReadOnly,
}

fn looks_secret(key: &str) -> bool {
    let k = key.to_ascii_lowercase();
    ["token", "secret", "password", "api_key", "apikey"].contains(&k.as_str())
        || ["_token", "_secret", "_password", "_key"]
            .iter()
            .any(|s| k.ends_with(s))
}

fn check_secrets(table: &toml::Table, path: &str) -> Result<(), ConfigError> {
    check_secrets_impl(table, path, false)
}

fn check_secrets_impl(
    table: &toml::Table,
    path: &str,
    skip_secret_check: bool,
) -> Result<(), ConfigError> {
    for (key, value) in table {
        let full = if path.is_empty() {
            key.clone()
        } else {
            format!("{path}.{key}")
        };

        // Skip secret check if we're directly under the density table
        if !skip_secret_check && looks_secret(key) {
            return Err(ConfigError::Secret(full));
        }

        match value {
            toml::Value::Table(inner) => {
                // Check if we're at the root level and this is the density table
                let next_skip = path.is_empty() && key == "density";
                check_secrets_impl(inner, &full, next_skip)?;
            }
            toml::Value::Array(arr) => {
                check_array_secrets(arr, &full)?;
            }
            _ => {}
        }
    }
    Ok(())
}

fn check_array_secrets(arr: &[toml::Value], path: &str) -> Result<(), ConfigError> {
    for (idx, value) in arr.iter().enumerate() {
        match value {
            toml::Value::Table(table) => {
                let array_path = format!("{path}[{idx}]");
                check_secrets_impl(table, &array_path, false)?;
            }
            toml::Value::Array(inner_arr) => {
                let array_path = format!("{path}[{idx}]");
                check_array_secrets(inner_arr, &array_path)?;
            }
            _ => {}
        }
    }
    Ok(())
}

/// `$XDG_CONFIG_HOME/scuttle/config.toml`, else `$HOME/.config/scuttle/config.toml`. A
/// relative or empty `XDG_CONFIG_HOME` is ignored, as the XDG Base Directory spec requires
/// and as `state::state_path` does.
pub fn config_path(env: &dyn Fn(&str) -> Option<String>) -> Option<PathBuf> {
    let base = env("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| env("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("scuttle").join("config.toml"))
}

/// What `/settings` writes when the file does not exist yet: every key at its default,
/// commented out, so the file changes nothing until a line is uncommented.
pub const TEMPLATE: &str = r##"# scuttle settings. Remove the "# " in front of a key to change it.
# /settings opens this file, and scuttle applies it when the editor exits.
# scuttle also saves organization and [efforts] here when you choose them.

# Capture the mouse to select and copy text, click links, and scroll with the wheel.
# mouse = true

# What sending a message does while the agent works: "queue" or "interrupt".
# busy_behavior = "queue"

# The most rows the composer grows to.
# composer_max_lines = 10

# The loading animation: "random" picks one each turn, or name one of
# "braille", "line", "arc", "bounce", or "bar".
# spinner = "random"

# Icons: "nerd" draws Nerd Font glyphs, which need a Nerd Font as the terminal's font, and
# "text" keeps plain text and emoji. Left out, NERD_FONT=1 in the environment picks "nerd",
# and otherwise scuttle uses "text". Get a font at https://www.nerdfonts.com/.
# icons = "text"

# [statusline]
# The footer's fields, in order; /statusline edits this list. A field with nothing to show
# stays hidden, such as spend on a deployment without AI Gateway.
# Other fields: "cost" (this chat's cost), "quota" (workspace credits), "queue", and "mcp"
# (how many MCP servers the next message uses).
# fields = ["model", "effort", "context", "workspace", "organization", "plan-mode", "spend", "status"]

# [statusline.thresholds]
# Highlight context, spend, or quota once it reaches this percent of its limit, even when
# the footer leaves the field out. Leave a key out for no warning.
# context = 80
# spend = 80
# quota = 90

# [chats]
# The marker before a pinned chat in /chats. Any text works, and "" shows none. Left out,
# it follows icons: 📌 in text, or the Octicons pin with "nerd" icons.
# pin_icon = "📌"

# [files]
# Where Enter in /files and a click on an attached file save it; "~/" is your home
# directory. Left out, files go to ~/Downloads, or to your home directory without one.
# save_dir = "~/Downloads"

# [welcome]
# Whether a blank chat shows the welcome screen.
# show = true
# A text file whose lines replace the Coder wordmark.
# art_file = "/path/to/art.txt"

# [density]
# How much of a tool's output shows, by tool name: "expanded", "summary", or "hidden".
# read_file = "summary"
"##;

/// Writes `TEMPLATE` to `path`, readable only by its owner, creating its directory, unless
/// the file already exists. The file appears whole or not at all.
pub fn create_if_missing(path: &Path) -> Result<(), ConfigError> {
    if path.exists() {
        return Ok(());
    }
    write_config(path, TEMPLATE, Existing::Keep)
}

/// What `write_config` does with a file already at its path.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Existing {
    /// Replace it, as a saved setting does, unless its owner made it read-only.
    Replace,
    /// Leave it, as the template does for a file made since the caller checked.
    Keep,
}

/// Writes `text` to `path`, creating its directory, readable only by its owner: a temp file
/// in the same directory with mode 0600, moved into place, so the file appears whole or not
/// at all. Every config write goes through here.
pub(crate) fn write_config(path: &Path, text: &str, existing: Existing) -> Result<(), ConfigError> {
    write_config_with(path, text, existing, |from, to| {
        std::fs::hard_link(from, to)
    })
}

/// `write_config`, with the hard link `Existing::Keep` moves the temp file with.
fn write_config_with(
    path: &Path,
    text: &str,
    existing: Existing,
    link: impl FnOnce(&Path, &Path) -> std::io::Result<()>,
) -> Result<(), ConfigError> {
    let io = |e: std::io::Error| ConfigError::Io(e.to_string());
    // A rename over a symlink would replace the link, so a linked file is written where
    // the link points, and created there when the link dangles.
    let target = link_target(path).map_err(io)?;
    if matches!(existing, Existing::Replace) && owner_cannot_write(&target) {
        return Err(ConfigError::ReadOnly);
    }
    write_through(&target, text, existing, link).map_err(io)
}

/// Where `path` leads after following every symlink, each relative link read against its
/// own directory. Unlike `canonicalize`, the last target need not exist.
fn link_target(path: &Path) -> std::io::Result<PathBuf> {
    let mut target = path.to_owned();
    // The same bound the kernel puts on a chain of links.
    for _ in 0..40 {
        match std::fs::symlink_metadata(&target) {
            Ok(meta) if meta.file_type().is_symlink() => {
                let link = std::fs::read_link(&target)?;
                target = target.parent().unwrap_or(Path::new("")).join(link);
            }
            _ => return Ok(target),
        }
    }
    Err(std::io::Error::other("too many levels of symbolic links"))
}

/// Whether the file at `path` exists and its owner took away their own write permission.
fn owner_cannot_write(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o200 == 0
    }
    #[cfg(not(unix))]
    {
        meta.permissions().readonly()
    }
}

/// `write_config_with` past the checks: the temp file, then the move to `target`.
fn write_through(
    target: &Path,
    text: &str,
    existing: Existing,
    link: impl FnOnce(&Path, &Path) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let dir = match target.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        _ => Path::new("."),
    };
    std::fs::create_dir_all(dir)?;
    let name = target
        .file_name()
        .map_or_else(|| "config.toml".into(), |n| n.to_string_lossy());
    let temp = dir.join(format!(".{name}.{}.tmp", Uuid::new_v4()));
    write_new(&temp, text)?;
    let moved = match existing {
        Existing::Replace => std::fs::rename(&temp, target),
        // A hard link, unlike a rename, never replaces a file made since the check. Where the
        // file system has none (FAT, some network mounts), an exclusive create still never
        // replaces one, though a failed write there can leave a short file.
        Existing::Keep => match link(&temp, target) {
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
            Err(_) => match write_new(target, text) {
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
                other => other,
            },
            Ok(()) => Ok(()),
        },
    };
    let _ = std::fs::remove_file(&temp);
    moved
}

/// Writes `text` to a new file at `path` with mode 0600 on unix, and removes the file again
/// when the write fails.
fn write_new(path: &Path, text: &str) -> std::io::Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    let written = file
        .write_all(text.as_bytes())
        .and_then(|()| file.sync_all());
    if written.is_err() {
        let _ = std::fs::remove_file(path);
    }
    written
}

/// A parse error of `text`, naming the line it is on when toml says where.
fn parse_error(text: &str, e: &toml::de::Error) -> ConfigError {
    let message = e.message().trim();
    match e.span() {
        Some(span) => {
            let before = text.get(..span.start).unwrap_or(text);
            let line = before.matches('\n').count() + 1;
            ConfigError::Parse(format!("line {line}: {message}"))
        }
        None => ConfigError::Parse(message.to_owned()),
    }
}

/// The keys `/settings` cannot apply until scuttle restarts that differ between `old` and
/// `new`, by name.
pub fn restart_keys(old: &LocalConfig, new: &LocalConfig) -> Vec<&'static str> {
    let mut keys = Vec::new();
    if old.welcome.show != new.welcome.show {
        keys.push("welcome.show");
    }
    if old.welcome.art_file != new.welcome.art_file {
        keys.push("welcome.art_file");
    }
    if old.organization != new.organization {
        keys.push("organization");
    }
    if old.efforts != new.efforts {
        keys.push("efforts");
    }
    keys
}

pub fn load_from_str(text: &str) -> Result<LocalConfig, ConfigError> {
    let table: toml::Table = text.parse().map_err(|e| parse_error(text, &e))?;
    check_secrets(&table, "")?;
    let mut config: LocalConfig = toml::from_str(text).map_err(|e| parse_error(text, &e))?;
    config.statusline.normalize()?;
    Ok(config)
}

pub fn load(path: &Path) -> Result<LocalConfig, ConfigError> {
    match std::fs::read_to_string(path) {
        Ok(text) => load_from_str(&text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(LocalConfig::default()),
        Err(e) => Err(ConfigError::Io(e.to_string())),
    }
}

/// Reads the file (or starts an empty document when it does not exist yet), lets `edit`
/// change it, and writes it back through `write_config`. Shared by
/// `set_mouse`, `set_organization`, and `set_effort` so every local-config write preserves
/// comments and formatting.
fn edit_document(
    path: &Path,
    edit: impl FnOnce(&mut toml_edit::DocumentMut),
) -> Result<(), ConfigError> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(ConfigError::Io(e.to_string())),
    };
    let mut doc: toml_edit::DocumentMut = text
        .parse()
        .map_err(|e: toml_edit::TomlError| ConfigError::Parse(e.to_string()))?;
    edit(&mut doc);
    write_config(path, &doc.to_string(), Existing::Replace)
}

/// Sets one top-level key in the file, creating it if needed and preserving comments and
/// formatting.
fn set_value(path: &Path, key: &str, value: toml_edit::Item) -> Result<(), ConfigError> {
    edit_document(path, |doc| doc[key] = value)
}

/// Sets `mouse` in the file.
pub fn set_mouse(path: &Path, enabled: bool) -> Result<(), ConfigError> {
    set_value(path, "mouse", toml_edit::value(enabled))
}

/// Saves the organization new chats go to.
pub fn set_organization(path: &Path, id: Uuid) -> Result<(), ConfigError> {
    set_value(path, "organization", toml_edit::value(id.to_string()))
}

/// Saves the reasoning effort chosen for `model_id` for a blank chat's new-chat form,
/// keeping every other model's saved effort.
pub fn set_effort(path: &Path, model_id: Uuid, effort: &str) -> Result<(), ConfigError> {
    edit_document(path, |doc| {
        if !doc.contains_key("efforts") || !doc["efforts"].is_table_like() {
            doc["efforts"] = toml_edit::Item::Table(toml_edit::Table::new());
        }
        doc["efforts"][&model_id.to_string()] = toml_edit::value(effort);
    })
}

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
        if thresholds
            .as_table()
            .is_some_and(toml_edit::Table::is_empty)
            && let Some(table) = section.as_table_mut()
        {
            table.remove("thresholds");
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_spinner_is_random_by_default_and_names_a_style() {
        assert_eq!(LocalConfig::default().spinner, SpinnerSetting::Random);
        assert_eq!(
            load_from_str("spinner = \"arc\"").unwrap().spinner,
            SpinnerSetting::Arc
        );
        assert_eq!(
            load_from_str("spinner = \"random\"").unwrap().spinner,
            SpinnerSetting::Random
        );
        assert!(
            load_from_str("spinner = \"wobble\"").is_err(),
            "an unknown style is an error, as an unknown busy_behavior is"
        );
    }

    #[test]
    fn reads_the_saved_organization() {
        let id = uuid::Uuid::new_v4();
        let cfg = load_from_str(&format!("organization = \"{id}\"\n")).unwrap();
        assert_eq!(cfg.organization, Some(id));
        assert_eq!(LocalConfig::default().organization, None);
    }

    #[test]
    fn a_malformed_saved_organization_is_ignored() {
        let cfg = load_from_str(
            "organization = \"nope\"\nmouse = false\nbusy_behavior = \"interrupt\"\ncomposer_max_lines = 6\n",
        )
        .unwrap();
        assert_eq!(cfg.organization, None);
        assert!(!cfg.mouse);
        assert_eq!(cfg.busy_behavior, BusyBehavior::Interrupt);
        assert_eq!(cfg.composer_max_lines, 6);
        assert_eq!(
            load_from_str("organization = \"nope\"")
                .unwrap()
                .organization,
            None
        );
    }

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
        for text in [
            "token = \"x\"",
            "api_key = \"x\"",
            "[welcome]\nsession_token = \"x\"",
            "coder_password = \"x\"",
        ] {
            match load_from_str(text) {
                Err(ConfigError::Secret(key)) => assert!(!key.is_empty()),
                other => panic!("{text:?} gave {other:?}"),
            }
        }
    }

    #[test]
    fn a_relative_xdg_config_home_is_ignored() {
        for value in ["config", ""] {
            let env = |k: &str| match k {
                "XDG_CONFIG_HOME" => Some(value.to_string()),
                "HOME" => Some("/h".to_string()),
                _ => None,
            };
            assert_eq!(
                config_path(&env),
                Some(PathBuf::from("/h/.config/scuttle/config.toml")),
                "XDG_CONFIG_HOME={value:?}"
            );
        }
    }

    #[test]
    fn files_save_to_the_set_directory_else_downloads_else_home() {
        let home = Path::new("/h");
        let downloads = |p: &Path| p == Path::new("/h/Downloads");
        let nothing = |_: &Path| false;
        let unset = FilesConfig::default();
        assert_eq!(
            save_dir(&unset, Some(home), downloads),
            PathBuf::from("/h/Downloads")
        );
        assert_eq!(save_dir(&unset, Some(home), nothing), PathBuf::from("/h"));
        assert_eq!(save_dir(&unset, None, nothing), PathBuf::from("."));
        let set = load_from_str("[files]\nsave_dir = \"~/dl\"\n")
            .unwrap()
            .files;
        assert_eq!(save_dir(&set, Some(home), nothing), PathBuf::from("/h/dl"));
        let absolute = load_from_str("[files]\nsave_dir = \"/srv/x\"\n")
            .unwrap()
            .files;
        assert_eq!(
            save_dir(&absolute, Some(home), nothing),
            PathBuf::from("/srv/x")
        );
    }

    #[test]
    fn config_path_prefers_xdg_then_home() {
        let xdg = |k: &str| match k {
            "XDG_CONFIG_HOME" => Some("/x".to_string()),
            "HOME" => Some("/h".to_string()),
            _ => None,
        };
        assert_eq!(
            config_path(&xdg),
            Some(PathBuf::from("/x/scuttle/config.toml"))
        );
        let home = |k: &str| (k == "HOME").then(|| "/h".to_string());
        assert_eq!(
            config_path(&home),
            Some(PathBuf::from("/h/.config/scuttle/config.toml"))
        );
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
    fn set_organization_saves_the_id_and_keeps_comments() {
        let dir = std::env::temp_dir().join(format!("scuttle-org-{}", uuid::Uuid::new_v4()));
        let path = dir.join("scuttle/config.toml");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "# keep me\nmouse = false\n").unwrap();
        let id = uuid::Uuid::new_v4();
        set_organization(&path, id).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# keep me"), "{text}");
        assert!(text.contains(&format!("organization = \"{id}\"")), "{text}");
        let cfg = load(&path).unwrap();
        assert_eq!(cfg.organization, Some(id));
        assert!(!cfg.mouse);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn set_organization_leaves_every_other_line_of_a_commented_config_alone() {
        let dir = std::env::temp_dir().join(format!("scuttle-org-{}", uuid::Uuid::new_v4()));
        let path = dir.join("config.toml");
        std::fs::create_dir_all(&dir).unwrap();
        let original = "\
# scuttle config
mouse = false # no capture
busy_behavior = \"interrupt\"

# How the welcome screen looks.
[welcome]
show = false
art_file = \"/tmp/art.txt\" # my art

[density]
# Tool output I never read.
read_file = \"hidden\"
";
        std::fs::write(&path, original).unwrap();
        let (first, second) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        set_organization(&path, first).unwrap();
        set_organization(&path, second).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        for line in original.lines() {
            assert!(text.contains(line), "lost {line:?} in:\n{text}");
        }
        assert_eq!(
            text.matches("organization =").count(),
            1,
            "a second save replaces the first:\n{text}"
        );
        assert!(!text.contains(&first.to_string()), "{text}");
        let cfg = load(&path).unwrap();
        assert_eq!(cfg.organization, Some(second));
        assert!(!cfg.mouse);
        assert_eq!(cfg.busy_behavior, BusyBehavior::Interrupt);
        assert!(!cfg.welcome.show);
        assert_eq!(cfg.welcome.art_file, Some(PathBuf::from("/tmp/art.txt")));
        assert_eq!(cfg.density.get("read_file"), Some(&Density::Hidden));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn set_effort_saves_it_and_keeps_comments_and_other_models_efforts() {
        let dir = std::env::temp_dir().join(format!("scuttle-effort-{}", uuid::Uuid::new_v4()));
        let path = dir.join("config.toml");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&path, "# keep me\nmouse = false\n").unwrap();
        let (first, second) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        set_effort(&path, first, "high").unwrap();
        set_effort(&path, second, "low").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# keep me"), "{text}");
        let cfg = load(&path).unwrap();
        assert!(!cfg.mouse);
        assert_eq!(cfg.efforts.get(&first), Some(&"high".to_string()));
        assert_eq!(cfg.efforts.get(&second), Some(&"low".to_string()));

        // Saving the first model's effort again replaces only its own entry.
        set_effort(&path, first, "medium").unwrap();
        let cfg = load(&path).unwrap();
        assert_eq!(cfg.efforts.get(&first), Some(&"medium".to_string()));
        assert_eq!(cfg.efforts.get(&second), Some(&"low".to_string()));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_malformed_efforts_entry_is_ignored() {
        let id = uuid::Uuid::new_v4();
        let cfg = load_from_str(&format!(
            "mouse = false\n[efforts]\nnope = \"high\"\n{id} = \"low\"\nother = 6\n"
        ))
        .unwrap();
        assert_eq!(cfg.efforts.len(), 1);
        assert_eq!(cfg.efforts.get(&id), Some(&"low".to_string()));
        assert!(!cfg.mouse);
        assert_eq!(LocalConfig::default().efforts, BTreeMap::new());
    }

    #[test]
    fn a_non_table_efforts_value_is_ignored() {
        let cfg = load_from_str("mouse = false\nefforts = \"oops\"\n").unwrap();
        assert_eq!(cfg.efforts, BTreeMap::new());
        assert!(!cfg.mouse);
    }

    #[test]
    fn an_efforts_array_is_ignored() {
        let cfg = load_from_str("mouse = false\nefforts = [1, 2, 3]\n").unwrap();
        assert_eq!(cfg.efforts, BTreeMap::new());
        assert!(!cfg.mouse);
    }

    #[test]
    fn an_efforts_number_is_ignored() {
        let cfg = load_from_str("mouse = false\nefforts = 6\n").unwrap();
        assert_eq!(cfg.efforts, BTreeMap::new());
        assert!(!cfg.mouse);
    }

    #[test]
    fn missing_file_loads_defaults() {
        let path =
            std::env::temp_dir().join(format!("scuttle-missing-{}.toml", uuid::Uuid::new_v4()));
        assert_eq!(load(&path).unwrap(), LocalConfig::default());
    }

    #[test]
    fn rejects_secrets_inside_arrays_of_tables() {
        // Test array of tables: [[hack]] with api_key inside
        match load_from_str("[[hack]]\napi_key = \"x\"\n") {
            Err(ConfigError::Secret(key)) => assert!(key.contains("api_key")),
            other => panic!("Expected Secret error, got {other:?}"),
        }

        // Test inline array: foo = [{ api_key = "x" }]
        match load_from_str("foo = [{ api_key = \"x\" }]\n") {
            Err(ConfigError::Secret(key)) => assert!(key.contains("api_key")),
            other => panic!("Expected Secret error, got {other:?}"),
        }
    }

    #[test]
    fn density_tool_names_are_not_treated_as_secrets() {
        let cfg =
            load_from_str("[density]\napi_key = \"hidden\"\nfetch_token = \"summary\"\n").unwrap();
        assert_eq!(cfg.density.get("api_key"), Some(&Density::Hidden));
        assert_eq!(cfg.density.get("fetch_token"), Some(&Density::Summary));
    }

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

    #[test]
    fn the_template_holds_only_defaults_and_every_key_in_it_parses() {
        assert_eq!(load_from_str(TEMPLATE).unwrap(), LocalConfig::default());
        let uncommented: String = TEMPLATE
            .lines()
            .filter_map(|l| l.strip_prefix("# "))
            .filter(|l| l.starts_with('[') || l.contains(" = "))
            .map(|l| format!("{l}\n"))
            .collect();
        let cfg = load_from_str(&uncommented).unwrap_or_else(|e| panic!("{e}:\n{uncommented}"));
        assert_eq!(
            LocalConfig {
                welcome: WelcomeConfig::default(),
                density: BTreeMap::new(),
                icons: None,
                chats: ChatsConfig::default(),
                files: FilesConfig::default(),
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
        assert_eq!(cfg.icons, Some(IconSet::Text), "the example icons parse");
        assert_eq!(
            cfg.chats.pin_icon.as_deref(),
            Some("📌"),
            "the example pin parses"
        );
        assert_eq!(
            cfg.files.save_dir,
            Some(PathBuf::from("~/Downloads")),
            "the example save directory parses"
        );
        assert_eq!(
            cfg.statusline.thresholds,
            Thresholds {
                context: Some(80),
                spend: Some(80),
                quota: Some(90)
            },
            "the example thresholds parse"
        );
    }

    #[test]
    fn a_parse_error_names_its_line() {
        for (text, line) in [
            ("mouse = true\n\nbusy_behavior = 3\n", 3),
            ("mouse = true\nnot toml\n", 2),
        ] {
            match load_from_str(text) {
                Err(ConfigError::Parse(message)) => {
                    assert!(message.starts_with(&format!("line {line}: ")), "{message}");
                }
                other => panic!("{text:?} gave {other:?}"),
            }
        }
    }

    #[test]
    fn the_template_is_written_only_when_the_file_is_missing() {
        let dir = std::env::temp_dir().join(format!("scuttle-template-{}", uuid::Uuid::new_v4()));
        let path = dir.join("scuttle/config.toml");
        create_if_missing(&path).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), TEMPLATE);
        std::fs::write(&path, "mouse = false\n").unwrap();
        create_if_missing(&path).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "mouse = false\n");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn the_template_is_readable_only_by_its_owner_and_leaves_no_temp_file() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("scuttle-mode-{}", uuid::Uuid::new_v4()));
        let path = dir.join("scuttle/config.toml");
        create_if_missing(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "{mode:o}");
        let names: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, ["config.toml"], "the temp file is gone");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn saving_a_setting_keeps_the_template_and_a_users_edits_to_it() {
        let dir = std::env::temp_dir().join(format!("scuttle-saved-{}", uuid::Uuid::new_v4()));
        let path = dir.join("scuttle/config.toml");
        create_if_missing(&path).unwrap();
        // The user uncomments a table and a key, as `/settings` invites.
        let edited = TEMPLATE
            .replace("# [chats]", "[chats]")
            .replace("# pin_icon = \"📌\"", "pin_icon = \"*\" # mine");
        std::fs::write(&path, &edited).unwrap();
        let (org, model) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        set_mouse(&path, false).unwrap();
        set_organization(&path, org).unwrap();
        set_effort(&path, model, "high").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        for line in edited.lines() {
            assert!(text.contains(line), "lost {line:?} in:\n{text}");
        }
        let cfg = load(&path).unwrap();
        assert!(!cfg.mouse);
        assert_eq!(cfg.chats.pin_icon.as_deref(), Some("*"));
        assert_eq!(cfg.organization, Some(org));
        assert_eq!(cfg.efforts.get(&model), Some(&"high".to_string()));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The mode bits of `path` and the names in its directory.
    #[cfg(unix)]
    fn mode_and_names(path: &Path) -> (u32, Vec<std::ffi::OsString>) {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        let names = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        (mode, names)
    }

    #[cfg(unix)]
    #[test]
    fn mouse_on_a_missing_file_creates_it_readable_only_by_its_owner() {
        let dir = std::env::temp_dir().join(format!("scuttle-0600-{}", uuid::Uuid::new_v4()));
        let path = dir.join("scuttle/config.toml");
        set_mouse(&path, false).unwrap();
        assert!(!load(&path).unwrap().mouse);
        let (mode, names) = mode_and_names(&path);
        assert_eq!(mode, 0o600, "{mode:o}");
        assert_eq!(names, ["config.toml"], "the temp file is gone");
        // Every save leaves the file 0600, and organization and effort saves go the same way.
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        set_organization(&path, uuid::Uuid::new_v4()).unwrap();
        set_effort(&path, uuid::Uuid::new_v4(), "low").unwrap();
        assert_eq!(mode_and_names(&path), (0o600, vec!["config.toml".into()]));
        assert!(!load(&path).unwrap().mouse, "earlier saves are kept");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_save_through_a_symlink_keeps_the_link() {
        let dir = std::env::temp_dir().join(format!("scuttle-link-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("dotfiles")).unwrap();
        std::fs::create_dir_all(dir.join("scuttle")).unwrap();
        let real = dir.join("dotfiles/config.toml");
        std::fs::write(&real, "# mine\nmouse = true\n").unwrap();
        let path = dir.join("scuttle/config.toml");
        std::os::unix::fs::symlink(&real, &path).unwrap();
        set_mouse(&path, false).unwrap();
        assert!(
            std::fs::symlink_metadata(&path)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            std::fs::read_to_string(&real).unwrap(),
            "# mine\nmouse = false\n"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_save_through_a_dangling_symlink_creates_its_target() {
        let dir = std::env::temp_dir().join(format!("scuttle-dangle-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("scuttle")).unwrap();
        let path = dir.join("scuttle/config.toml");
        // A relative link resolves against the link's own directory.
        std::os::unix::fs::symlink("../dotfiles/config.toml", &path).unwrap();
        set_mouse(&path, false).unwrap();
        assert!(
            std::fs::symlink_metadata(&path)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        let real = dir.join("dotfiles/config.toml");
        assert!(!load(&real).unwrap().mouse);
        assert_eq!(mode_and_names(&real), (0o600, vec!["config.toml".into()]));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_save_refuses_a_file_its_owner_made_read_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("scuttle-ro-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(&path, "mouse = true\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444)).unwrap();
        let err = set_mouse(&path, false).unwrap_err();
        assert!(matches!(err, ConfigError::ReadOnly), "{err:?}");
        assert!(
            err.to_string().contains("config.toml is read-only"),
            "{err}"
        );
        assert!(set_organization(&path, uuid::Uuid::new_v4()).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "mouse = true\n");
        assert_eq!(mode_and_names(&path), (0o444, vec!["config.toml".into()]));
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_template_falls_back_to_an_exclusive_create_without_hard_links() {
        let dir = std::env::temp_dir().join(format!("scuttle-nolink-{}", uuid::Uuid::new_v4()));
        let path = dir.join("scuttle/config.toml");
        let no_links = |_: &Path, _: &Path| -> std::io::Result<()> {
            Err(std::io::ErrorKind::Unsupported.into())
        };
        write_config_with(&path, TEMPLATE, Existing::Keep, no_links).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), TEMPLATE);
        #[cfg(unix)]
        assert_eq!(mode_and_names(&path), (0o600, vec!["config.toml".into()]));
        std::fs::write(&path, "mouse = false\n").unwrap();
        write_config_with(&path, TEMPLATE, Existing::Keep, no_links).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "mouse = false\n",
            "the fallback never replaces a file"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn restart_keys_name_the_changed_settings_that_wait_for_a_restart() {
        let old = LocalConfig::default();
        let new = LocalConfig {
            mouse: false,
            spinner: SpinnerSetting::Bar,
            welcome: WelcomeConfig {
                show: false,
                ..WelcomeConfig::default()
            },
            organization: Some(Uuid::new_v4()),
            ..LocalConfig::default()
        };
        assert_eq!(restart_keys(&old, &new), ["welcome.show", "organization"]);
        assert!(restart_keys(&old, &old).is_empty());
    }

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
        assert!(
            empty.statusline.fields.is_empty(),
            "an empty list is allowed"
        );
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
        for (bad, why) in [
            ("-1", "negative"),
            ("50.5", "a float"),
            ("-0.5", "a negative float"),
        ] {
            match load_from_str(&format!("[statusline.thresholds]\nspend = {bad}\n")) {
                Err(ConfigError::Parse(m)) => assert!(m.starts_with("line 2"), "{why}: {m}"),
                other => panic!("{why}: {other:?}"),
            }
        }
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
        assert_eq!(
            t.spend,
            Some(85),
            "a hand-written value moves to the next level up"
        );
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
        assert_eq!(
            list.move_by(last, false),
            last,
            "the last row cannot move down"
        );
    }

    #[test]
    fn saving_the_status_line_keeps_the_rest_of_the_file_and_drops_warnings_turned_off() {
        let dir = std::env::temp_dir().join(format!("scuttle-statusline-{}", uuid::Uuid::new_v4()));
        let path = dir.join("config.toml");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            &path,
            "# mine\nmouse = false\n\n[statusline]\nfields = [\"model\"]\n",
        )
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
        assert!(
            !text.contains("thresholds"),
            "an empty table is removed:\n{text}"
        );
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

    #[test]
    fn the_mcp_field_is_optional_and_takes_no_threshold() {
        assert_eq!(StatusField::Mcp.name(), "mcp");
        assert_eq!(StatusField::ALL.last(), Some(&StatusField::Mcp));
        assert!(!StatusField::DEFAULT.contains(&StatusField::Mcp));
        assert!(!StatusField::Mcp.takes_threshold());
        let cfg = load_from_str("[statusline]\nfields = [\"mcp\", \"status\"]\n").unwrap();
        assert_eq!(
            cfg.statusline.fields,
            vec![StatusField::Mcp, StatusField::Status]
        );
    }
}
