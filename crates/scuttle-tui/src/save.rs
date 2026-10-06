//! Writing a downloaded chat file into place without replacing anything by accident.
//!
//! The bytes go to a hidden partial file beside the target, so a failed save, or one the
//! runtime drops as it shuts down, leaves nothing behind; only a hard kill can leave the
//! partial file. Only a finished file takes the name: through a hard link, which fails when
//! the name is taken (by a file, a directory, or a symlink, dangling or not), or through a
//! rename once the user chose to replace what is there. A save is never cancelled once it
//! starts, so closing `/files` or switching chats lets it finish.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// The directory `target` is saved in.
pub(crate) fn dir_of(target: &Path) -> &Path {
    match target.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        _ => Path::new("."),
    }
}

/// A new file at `path`, readable by everyone and writable by its owner, before the umask,
/// as a downloaded file usually is. It fails when anything is at `path`.
fn new_file(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o644);
    }
    options.open(path)
}

/// A copy of `from` at a new `to`, where the file system has no hard links. The exclusive
/// create never replaces a file; a failed copy removes what it wrote.
fn copy_new(from: &Path, to: &Path) -> io::Result<()> {
    let mut out = new_file(to)?;
    let copied = File::open(from)
        .and_then(|mut source| io::copy(&mut source, &mut out))
        .and_then(|_| out.sync_all());
    if copied.is_err() {
        let _ = std::fs::remove_file(to);
    }
    copied
}

/// The raw errors besides the portable kinds that say a file system cannot make a hard link
/// here: EOPNOTSUPP (95) on Linux, ENOTSUP (45) and EOPNOTSUPP (102) on macOS, and
/// ERROR_INVALID_FUNCTION (1) and ERROR_NOT_SUPPORTED (50) on Windows.
#[cfg(target_os = "linux")]
const NO_LINKS: &[i32] = &[95];
#[cfg(target_os = "macos")]
const NO_LINKS: &[i32] = &[45, 102];
#[cfg(windows)]
const NO_LINKS: &[i32] = &[1, 50];
#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
const NO_LINKS: &[i32] = &[];

/// Whether a failed hard link means the file system or mount cannot link here, as FAT and
/// some network mounts cannot, so a copy is worth trying. Any other error, such as EACCES or
/// ENOSPC, would fail the copy too, so it is reported as it is.
fn links_unsupported(e: &io::Error) -> bool {
    use io::ErrorKind;
    match e.kind() {
        ErrorKind::CrossesDevices | ErrorKind::Unsupported | ErrorKind::TooManyLinks => true,
        // EPERM shares its kind with EACCES; it is 1 on every Unix.
        ErrorKind::PermissionDenied => cfg!(unix) && e.raw_os_error() == Some(1),
        _ => e.raw_os_error().is_some_and(|n| NO_LINKS.contains(&n)),
    }
}

/// A download being written, removed when dropped unless a rename moved it into place.
pub(crate) struct Partial {
    path: PathBuf,
    file: File,
    written: u64,
    moved: bool,
}

impl Partial {
    /// A hidden partial file in `target`'s directory. Its name is short whatever `target` is
    /// called, so a 255-byte target name still fits.
    pub(crate) fn beside(target: &Path) -> io::Result<Partial> {
        let path = dir_of(target).join(format!(".scuttle-{}.part", uuid::Uuid::new_v4()));
        let file = new_file(&path)?;
        Ok(Partial {
            path,
            file,
            written: 0,
            moved: false,
        })
    }

    pub(crate) fn write(&mut self, chunk: &[u8]) -> io::Result<()> {
        self.file.write_all(chunk)?;
        self.written += chunk.len() as u64;
        Ok(())
    }

    /// The bytes written so far.
    pub(crate) fn written(&self) -> u64 {
        self.written
    }

    /// Gives the finished file the name `target`, failing with `AlreadyExists` when
    /// anything is there. The partial file goes when this is dropped.
    pub(crate) fn keep_new(&mut self, target: &Path) -> io::Result<()> {
        self.file.sync_all()?;
        match std::fs::hard_link(&self.path, target) {
            Ok(()) => Ok(()),
            Err(e) if links_unsupported(&e) => copy_new(&self.path, target),
            Err(e) => Err(e),
        }
    }

    /// Moves the finished file to `target`, replacing a file or a symlink there; a symlink
    /// itself is replaced, never the file it points to.
    pub(crate) fn replace(&mut self, target: &Path) -> io::Result<()> {
        self.file.sync_all()?;
        std::fs::rename(&self.path, target)?;
        self.moved = true;
        Ok(())
    }

    /// `keep_new` at `target`, else at the first free ` (n)` name beside it.
    pub(crate) fn keep_both(&mut self, target: &Path) -> io::Result<PathBuf> {
        match self.keep_new(target) {
            Ok(()) => return Ok(target.to_owned()),
            Err(e) if e.kind() != io::ErrorKind::AlreadyExists => return Err(e),
            Err(_) => {}
        }
        let name = target
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "attachment".into());
        for n in 1..=999 {
            let candidate = target.with_file_name(scuttle_core::files::numbered(&name, n));
            match self.keep_new(&candidate) {
                Ok(()) => return Ok(candidate),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }
        }
        Err(io::Error::other(format!(
            "every name from {name} (1) to (999) is taken"
        )))
    }
}

impl Drop for Partial {
    fn drop(&mut self) {
        if !self.moved {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::FileTypeExt;

    /// A fresh directory under the system temp directory, removed when the guard drops.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> TempDir {
            let dir = std::env::temp_dir().join(format!("scuttle-save-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).unwrap();
            TempDir(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn entries(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn a_new_name_is_kept_and_the_partial_file_goes() {
        let dir = TempDir::new();
        let target = dir.0.join("a.txt");
        let mut partial = Partial::beside(&target).unwrap();
        partial.write(b"hello").unwrap();
        assert_eq!(partial.written(), 5);
        partial.keep_new(&target).unwrap();
        drop(partial);
        assert_eq!(entries(&dir.0), ["a.txt"]);
        assert_eq!(std::fs::read(&target).unwrap(), b"hello");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&target).unwrap().permissions().mode();
            assert_eq!(mode & 0o111, 0, "never executable: {mode:o}");
            assert_eq!(
                mode & 0o600,
                0o600,
                "the owner reads and writes it: {mode:o}"
            );
            assert_eq!(mode & 0o022, 0, "only the owner writes it: {mode:o}");
        }
    }

    #[test]
    fn a_dropped_partial_leaves_nothing() {
        let dir = TempDir::new();
        let mut partial = Partial::beside(&dir.0.join("a.txt")).unwrap();
        partial.write(b"half").unwrap();
        drop(partial);
        assert!(entries(&dir.0).is_empty());
    }

    #[test]
    #[cfg(unix)]
    fn a_taken_name_or_a_dangling_link_is_never_replaced() {
        let dir = TempDir::new();
        let target = dir.0.join("a.txt");
        std::fs::write(&target, b"mine").unwrap();
        let mut partial = Partial::beside(&target).unwrap();
        partial.write(b"theirs").unwrap();
        let err = partial.keep_new(&target).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(&target).unwrap(), b"mine");
        let link = dir.0.join("link.txt");
        std::os::unix::fs::symlink(dir.0.join("nowhere"), &link).unwrap();
        assert_eq!(
            partial.keep_new(&link).unwrap_err().kind(),
            std::io::ErrorKind::AlreadyExists
        );
        assert!(!dir.0.join("nowhere").exists(), "the link was not followed");
    }

    #[test]
    #[cfg(unix)]
    fn replace_replaces_a_symlink_and_never_its_target() {
        let dir = TempDir::new();
        let elsewhere = TempDir::new();
        let outside = elsewhere.0.join("keep.txt");
        std::fs::write(&outside, b"keep").unwrap();
        let target = dir.0.join("a.txt");
        std::os::unix::fs::symlink(&outside, &target).unwrap();
        let mut partial = Partial::beside(&target).unwrap();
        partial.write(b"new").unwrap();
        partial.replace(&target).unwrap();
        drop(partial);
        assert!(
            !std::fs::symlink_metadata(&target)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(std::fs::read(&target).unwrap(), b"new");
        assert_eq!(std::fs::read(&outside).unwrap(), b"keep");
        assert_eq!(entries(&dir.0), ["a.txt"]);
    }

    #[test]
    #[cfg(unix)]
    fn keep_new_refuses_a_folder_a_fifo_and_a_link_to_a_folder() {
        let dir = TempDir::new();
        let folder = dir.0.join("folder");
        std::fs::create_dir(&folder).unwrap();
        let fifo = dir.0.join("fifo");
        let made = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap();
        assert!(made.success());
        let link = dir.0.join("link");
        std::os::unix::fs::symlink(&folder, &link).unwrap();
        let mut partial = Partial::beside(&dir.0.join("a.txt")).unwrap();
        partial.write(b"theirs").unwrap();
        for taken in [&folder, &fifo, &link] {
            assert_eq!(
                partial.keep_new(taken).unwrap_err().kind(),
                std::io::ErrorKind::AlreadyExists,
                "{}",
                taken.display()
            );
        }
        assert!(folder.is_dir());
        assert!(
            entries(&folder).is_empty(),
            "nothing was written through the link"
        );
        assert!(
            std::fs::symlink_metadata(&fifo)
                .unwrap()
                .file_type()
                .is_fifo()
        );
    }

    #[test]
    fn replace_onto_a_folder_fails_and_leaves_it() {
        let dir = TempDir::new();
        let folder = dir.0.join("a.txt");
        std::fs::create_dir(&folder).unwrap();
        std::fs::write(folder.join("inside"), b"keep").unwrap();
        let mut partial = Partial::beside(&folder).unwrap();
        partial.write(b"new").unwrap();
        assert!(partial.replace(&folder).is_err());
        drop(partial);
        assert_eq!(std::fs::read(folder.join("inside")).unwrap(), b"keep");
        assert_eq!(entries(&dir.0), ["a.txt"], "the partial file went too");
    }

    #[test]
    #[cfg(unix)]
    fn copy_new_writes_a_new_name_and_never_a_taken_one_or_through_a_link() {
        let dir = TempDir::new();
        let from = dir.0.join("from");
        std::fs::write(&from, b"bytes").unwrap();
        let fresh = dir.0.join("fresh");
        copy_new(&from, &fresh).unwrap();
        assert_eq!(std::fs::read(&fresh).unwrap(), b"bytes");
        let taken = dir.0.join("taken");
        std::fs::write(&taken, b"mine").unwrap();
        assert_eq!(
            copy_new(&from, &taken).unwrap_err().kind(),
            std::io::ErrorKind::AlreadyExists
        );
        assert_eq!(std::fs::read(&taken).unwrap(), b"mine");
        let link = dir.0.join("link");
        std::os::unix::fs::symlink(dir.0.join("nowhere"), &link).unwrap();
        assert_eq!(
            copy_new(&from, &link).unwrap_err().kind(),
            std::io::ErrorKind::AlreadyExists
        );
        assert!(!dir.0.join("nowhere").exists(), "the link was not followed");
    }

    #[test]
    #[cfg(unix)]
    fn only_a_file_system_without_links_falls_back_to_a_copy() {
        let raw = std::io::Error::from_raw_os_error;
        // EPERM and EXDEV: the file system or the mount refuses a hard link.
        assert!(links_unsupported(&raw(1)));
        assert!(links_unsupported(&raw(18)));
        assert!(links_unsupported(&std::io::Error::from(
            std::io::ErrorKind::Unsupported
        )));
        // EACCES and ENOSPC would fail a copy too, so they are reported as they are.
        assert!(!links_unsupported(&raw(13)));
        assert!(!links_unsupported(&raw(28)));
    }

    #[test]
    fn keep_both_takes_the_next_free_number() {
        let dir = TempDir::new();
        let target = dir.0.join("a.txt");
        std::fs::write(&target, b"1").unwrap();
        std::fs::write(dir.0.join("a (1).txt"), b"2").unwrap();
        let mut partial = Partial::beside(&target).unwrap();
        partial.write(b"3").unwrap();
        assert_eq!(partial.keep_both(&target).unwrap(), dir.0.join("a (2).txt"));
        drop(partial);
        assert_eq!(entries(&dir.0), ["a (1).txt", "a (2).txt", "a.txt"]);
        assert_eq!(std::fs::read(&target).unwrap(), b"1");
    }
}
