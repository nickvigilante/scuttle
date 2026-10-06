//! Files attached to the next message, and the checks made before any bytes are sent.

use uuid::Uuid;

/// The server's upload limit, `codersdk.MaxChatFileSizeBytes`.
pub const MAX_FILE_BYTES: u64 = 10 * 1024 * 1024;

/// Extensions of formats the server never accepts. Anything else uploads, and the server
/// classifies the bytes, which accepts source code as plain text.
const REJECTED: &[&str] = &[
    "zip", "tar", "gz", "tgz", "bz2", "xz", "7z", "rar", "exe", "dll", "so", "dylib", "bin", "iso",
    "dmg", "mp3", "mp4", "mov", "avi", "mkv", "wav", "doc", "docx", "xls", "xlsx", "ppt", "pptx",
    "heic", "tiff", "bmp", "ico",
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
    /// A file an `@path` mention named, at this path. Nothing is read or uploaded until the
    /// message is sent, so a chip removed before then never reaches the server.
    Held(String),
    Uploading,
    Ready(Uuid),
    Failed(String),
    /// A large paste waiting for its message's send, which uploads `Chip::pasted`.
    Pasted,
}

/// One attachment above the composer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chip {
    /// A local number that matches the upload's reply to its chip.
    pub local: u64,
    pub name: String,
    pub size: Option<u64>,
    pub state: ChipState,
    /// The text of a chip made from a large paste, kept through its upload so a failed send
    /// can put the paste back instead of losing it.
    pub pasted: Option<String>,
}

impl Chip {
    /// The chip's text. The terminal cannot show an image, so every file shows its name and size.
    pub fn label(&self) -> String {
        match &self.state {
            ChipState::Held(_) => format!("{} uploads when sent", self.name),
            ChipState::Pasted => format!(
                "{} {} uploads when sent",
                self.name,
                size_label(self.size.unwrap_or(0))
            ),
            ChipState::Uploading => format!("{} uploading", self.name),
            ChipState::Ready(_) => match self.size {
                Some(size) => format!("{} {}", self.name, size_label(size)),
                None => self.name.clone(),
            },
            ChipState::Failed(why) => format!("{}: {why}", self.name),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_the_server_never_takes_are_rejected_by_extension() {
        assert!(check_type("report.pdf").is_ok());
        assert!(
            check_type("main.rs").is_ok(),
            "source code uploads as plain text"
        );
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
            pasted: None,
        };
        assert_eq!(
            chip(ChipState::Ready(uuid::Uuid::nil())).label(),
            "shot.png 1.2 MiB"
        );
        assert_eq!(chip(ChipState::Uploading).label(), "shot.png uploading");
        assert_eq!(
            chip(ChipState::Held("/tmp/shot.png".into())).label(),
            "shot.png uploads when sent"
        );
        assert_eq!(
            chip(ChipState::Failed("too big".into())).label(),
            "shot.png: too big"
        );
        let paste = Chip {
            local: 2,
            name: "paste-1.txt".into(),
            size: Some(2048),
            state: ChipState::Pasted,
            pasted: Some("x".repeat(2048)),
        };
        assert_eq!(paste.label(), "paste-1.txt 2 KiB uploads when sent");
    }
}
