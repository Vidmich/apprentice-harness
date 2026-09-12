//! Atomic file replacement: the new content goes to a temporary file in
//! the target's directory, is flushed to disk, and takes the target's
//! place with one rename, so a crash or an error at any point leaves
//! either the old file or the new one, never a partial write.

use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static SEQ: AtomicU64 = AtomicU64::new(0);

/// Writes `bytes` to `path` atomically, creating parent directories.
///
/// # Errors
/// Any I/O failure; the target is untouched and the temporary file is
/// removed.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    write_atomic_with(path, bytes, || Ok(()))
}

/// [`write_atomic`] with a hook run between the flush and the rename
/// (tests inject a failure there).
pub(crate) fn write_atomic_with(
    path: &Path,
    bytes: &[u8],
    before_rename: impl FnOnce() -> io::Result<()>,
) -> io::Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no parent"))?;
    std::fs::create_dir_all(parent)?;
    let tmp = temp_name(path)?;
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        #[cfg(unix)]
        if let Ok(meta) = std::fs::metadata(path) {
            let _ = std::fs::set_permissions(&tmp, meta.permissions());
        }
        before_rename()?;
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
        return result;
    }
    #[cfg(unix)]
    if let Ok(dir) = std::fs::File::open(parent) {
        let _ = dir.sync_all();
    }
    Ok(())
}

/// `.<name>.<pid>.<seq>.harness-tmp` next to `path`.
fn temp_name(path: &Path) -> io::Result<PathBuf> {
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no file name"))?;
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let mut tmp = std::ffi::OsString::from(".");
    tmp.push(name);
    tmp.push(format!(".{}.{seq}.harness-tmp", std::process::id()));
    Ok(path.with_file_name(tmp))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_content_and_creates_parents() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a").join("b").join("f.txt");
        write_atomic(&path, b"one").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"one");
        write_atomic(&path, b"two").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"two");
        let leftovers: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(leftovers, ["f.txt"]);
    }

    #[test]
    fn a_failure_before_the_rename_leaves_the_original_and_no_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.txt");
        std::fs::write(&path, b"original").unwrap();
        let err = write_atomic_with(&path, b"replacement", || {
            Err(io::Error::other("disk on fire"))
        })
        .unwrap_err();
        assert_eq!(err.to_string(), "disk on fire");
        assert_eq!(std::fs::read(&path).unwrap(), b"original");
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, ["f.txt"]);
    }
}
