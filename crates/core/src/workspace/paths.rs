//! Path resolution and the sandbox: every path a tool touches is resolved
//! against the workspace root, symlinks included, and rejected when it
//! lands outside. Paths shown to the mentor are root-relative with `/`
//! separators on every OS.

use std::path::{Component, Path, PathBuf, Prefix};

/// Why a path could not be resolved inside the workspace.
#[derive(Debug, thiserror::Error)]
pub enum PathError {
    /// The path (after `..`, absolute prefixes and symlinks) is not under
    /// the root. The permission engine may still let the user allow it.
    #[error("`{path}` is outside the workspace")]
    Outside { path: String },
    #[error("invalid path `{path}`: {reason}")]
    Invalid { path: String, reason: String },
    #[error("cannot resolve `{path}`: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
}

impl PathError {
    /// Stable machine-readable kind.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Outside { .. } => "outside_workspace",
            Self::Invalid { .. } => "invalid_path",
            Self::Io { .. } => "io",
        }
    }
}

/// Canonical form of an existing directory: symlinks resolved, and on
/// Windows without the `\\?\` prefix and with an upper-case drive letter.
///
/// # Errors
/// The path does not exist or is not a directory.
pub fn canonical_dir(path: &Path) -> std::io::Result<PathBuf> {
    let canonical = std::fs::canonicalize(path)?;
    if !canonical.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotADirectory,
            "not a directory",
        ));
    }
    Ok(strip_verbatim(&canonical))
}

/// `\\?\C:\x` → `C:\x`, `\\?\UNC\srv\share\x` → `\\srv\share\x`; drive
/// letters upper-cased. A no-op on other platforms.
pub(crate) fn strip_verbatim(path: &Path) -> PathBuf {
    let mut components = path.components();
    let Some(Component::Prefix(prefix)) = components.next() else {
        return path.to_path_buf();
    };
    let head = match prefix.kind() {
        Prefix::VerbatimDisk(d) | Prefix::Disk(d) => {
            format!("{}:\\", d.to_ascii_uppercase() as char)
        }
        Prefix::VerbatimUNC(server, share) | Prefix::UNC(server, share) => format!(
            "\\\\{}\\{}\\",
            server.to_string_lossy(),
            share.to_string_lossy()
        ),
        Prefix::Verbatim(_) | Prefix::DeviceNS(_) => return path.to_path_buf(),
    };
    let mut out = PathBuf::from(head);
    for c in components {
        match c {
            Component::RootDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Removes `.` and folds `..` into the preceding component. A `..` with
/// nothing left to pop is dropped (the containment check rejects the
/// result when that mattered).
fn normalize_lexically(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    let mut depth = 0usize;
    for c in path.components() {
        match c {
            Component::Prefix(_) | Component::RootDir => out.push(c.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if depth > 0 {
                    out.pop();
                    depth -= 1;
                }
            }
            Component::Normal(n) => {
                out.push(n);
                depth += 1;
            }
        }
    }
    out
}

/// Canonicalises the longest existing prefix of `path` (so symlinks in
/// it are followed) and appends the rest untouched, which lets a tool
/// resolve a file it is about to create. `None` when not even the
/// prefix (drive, `/`) exists.
fn canonicalize_existing_prefix(path: &Path) -> std::io::Result<Option<PathBuf>> {
    let mut rest: Vec<&std::ffi::OsStr> = Vec::new();
    let mut cursor = path;
    loop {
        match std::fs::canonicalize(cursor) {
            Ok(canonical) => {
                let mut out = strip_verbatim(&canonical);
                for c in rest.iter().rev() {
                    out.push(c);
                }
                return Ok(Some(out));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            // Access denied on an ancestor is a real error; a component
            // that is a file (`ENOTDIR`) reads as "does not exist" here.
            Err(e) if e.kind() == std::io::ErrorKind::NotADirectory => {}
            Err(e) => return Err(e),
        }
        let Some(name) = cursor.file_name() else {
            return Ok(None);
        };
        rest.push(name);
        let Some(parent) = cursor.parent() else {
            return Ok(None);
        };
        cursor = parent;
    }
}

/// Resolves `user_path` (root-relative, `/` or native separators; an
/// absolute path only when it is inside the root) to an absolute path
/// under `root`, which must be canonical.
pub(crate) fn resolve(root: &Path, user_path: &str) -> Result<PathBuf, PathError> {
    let invalid = |reason: &str| PathError::Invalid {
        path: user_path.to_owned(),
        reason: reason.to_owned(),
    };
    if user_path.contains('\0') {
        return Err(invalid("contains a NUL byte"));
    }
    let given = Path::new(user_path.trim());
    let joined = if given.as_os_str().is_empty() {
        root.to_path_buf()
    } else {
        // `join` replaces the root for an absolute (or, on Windows,
        // rooted or drive-qualified) path, so those are checked as given.
        root.join(given)
    };
    let lexical = normalize_lexically(&joined);
    let resolved = canonicalize_existing_prefix(&lexical)
        .map_err(|source| PathError::Io {
            path: user_path.to_owned(),
            source,
        })?
        .ok_or_else(|| PathError::Outside {
            path: user_path.to_owned(),
        })?;
    if !starts_with(&resolved, root) {
        return Err(PathError::Outside {
            path: user_path.to_owned(),
        });
    }
    Ok(resolved)
}

/// Whether `abs` (canonicalised as far as it exists) is `root` or under
/// it.
pub(crate) fn contains(root: &Path, abs: &Path) -> bool {
    if !abs.is_absolute() {
        return false;
    }
    match canonicalize_existing_prefix(&normalize_lexically(abs)) {
        Ok(Some(resolved)) => starts_with(&resolved, root),
        _ => false,
    }
}

/// `abs` relative to `root` with `/` separators (`.` for the root
/// itself); a path outside the root comes back absolute, also with `/`.
pub(crate) fn display(root: &Path, abs: &Path) -> String {
    let abs = strip_verbatim(abs);
    if !starts_with(&abs, root) {
        return abs.to_string_lossy().replace('\\', "/");
    }
    let n = root.components().count();
    let rel: Vec<String> = abs
        .components()
        .skip(n)
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    if rel.is_empty() {
        return ".".to_owned();
    }
    rel.join("/")
}

/// Component-wise prefix test; case-insensitive on Windows.
fn starts_with(path: &Path, root: &Path) -> bool {
    let mut p = path.components();
    for r in root.components() {
        match p.next() {
            Some(c) if same_component(c, r) => {}
            _ => return false,
        }
    }
    true
}

#[cfg(windows)]
fn same_component(a: Component<'_>, b: Component<'_>) -> bool {
    a.as_os_str().to_string_lossy().to_lowercase() == b.as_os_str().to_string_lossy().to_lowercase()
}

#[cfg(not(windows))]
fn same_component(a: Component<'_>, b: Component<'_>) -> bool {
    a == b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lexical_normalisation_folds_dots() {
        let n = |s: &str| normalize_lexically(Path::new(s));
        assert_eq!(n("a/./b/../c"), PathBuf::from("a").join("c"));
        assert_eq!(n("a/../../b"), PathBuf::from("b"));
        #[cfg(windows)]
        {
            assert_eq!(n(r"C:\r\..\..\x"), PathBuf::from(r"C:\x"));
            assert_eq!(n(r"C:\r\a\..\b"), PathBuf::from(r"C:\r\b"));
        }
        #[cfg(not(windows))]
        {
            assert_eq!(n("/r/../../x"), PathBuf::from("/x"));
        }
    }

    #[cfg(windows)]
    #[test]
    fn verbatim_prefixes_are_stripped() {
        assert_eq!(
            strip_verbatim(Path::new(r"\\?\c:\Users\x")),
            PathBuf::from(r"C:\Users\x")
        );
        assert_eq!(
            strip_verbatim(Path::new(r"\\?\UNC\srv\share\dir")),
            PathBuf::from(r"\\srv\share\dir")
        );
        assert_eq!(strip_verbatim(Path::new(r"d:\x")), PathBuf::from(r"D:\x"));
    }

    #[test]
    fn display_is_posix_and_root_relative() {
        let root = if cfg!(windows) {
            PathBuf::from(r"C:\work\repo")
        } else {
            PathBuf::from("/work/repo")
        };
        assert_eq!(display(&root, &root), ".");
        assert_eq!(
            display(&root, &root.join("src").join("main.rs")),
            "src/main.rs"
        );
        let outside = root.parent().unwrap().join("other");
        let shown = display(&root, &outside);
        assert!(shown.ends_with("/work/other"), "{shown}");
        #[cfg(windows)]
        assert_eq!(
            display(&root, Path::new(r"c:\WORK\repo\A\b.txt")),
            "A/b.txt"
        );
    }
}
