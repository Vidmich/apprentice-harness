//! The file index: every non-ignored file under the root with size,
//! mtime and language, sorted by path. Built with the parallel walker on
//! first use, rebuilt on `workspace.refresh` or when older than
//! [`INDEX_TTL`] at the next use. `list_dir`/`glob` (M01-03) and the
//! context selector (M03) read it.

use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime};

use ignore::WalkState;

use super::rules::IgnoreRules;

/// Files past this are not indexed (the index says so).
pub const MAX_INDEX_FILES: usize = 200_000;
/// An index older than this is rebuilt at the next use.
pub const INDEX_TTL: Duration = Duration::from_secs(30);

/// One indexed file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEntry {
    /// Root-relative, `/` separators.
    pub path: String,
    pub size: u64,
    /// Seconds since the Unix epoch, when the filesystem reports one.
    pub mtime: Option<u64>,
    /// Language by extension or well-known file name.
    pub language: Option<&'static str>,
}

/// A snapshot of the tree. Immutable; the workspace swaps in a new one.
#[derive(Debug)]
pub struct FileIndex {
    entries: Vec<FileEntry>,
    directories: u64,
    truncated: bool,
    built_at: Instant,
    build_time: Duration,
}

impl FileIndex {
    /// Walks `root` with `rules` and collects up to [`MAX_INDEX_FILES`]
    /// files.
    pub fn build(root: &Path, rules: &IgnoreRules) -> Self {
        let started = Instant::now();
        let files = Mutex::new(Vec::new());
        let directories = AtomicU64::new(0);
        let truncated = AtomicBool::new(false);
        rules.walk_builder().build_parallel().run(|| {
            Box::new(|entry| {
                let entry = match entry {
                    Ok(e) => e,
                    Err(err) => {
                        tracing::debug!(error = %err, "index walk");
                        return WalkState::Continue;
                    }
                };
                if entry.depth() == 0 {
                    return WalkState::Continue;
                }
                let Some(ft) = entry.file_type() else {
                    return WalkState::Continue;
                };
                if ft.is_dir() {
                    directories.fetch_add(1, Ordering::Relaxed);
                    return WalkState::Continue;
                }
                if !ft.is_file() {
                    return WalkState::Continue;
                }
                let rel = entry.path().strip_prefix(root).unwrap_or(entry.path());
                let path = rel.to_string_lossy().replace('\\', "/");
                let meta = entry.metadata().ok();
                let size = meta.as_ref().map_or(0, std::fs::Metadata::len);
                let mtime = meta
                    .as_ref()
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs());
                let language = language_of(rel);
                let mut files = files
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if files.len() >= MAX_INDEX_FILES {
                    truncated.store(true, Ordering::Relaxed);
                    return WalkState::Quit;
                }
                files.push(FileEntry {
                    path,
                    size,
                    mtime,
                    language,
                });
                WalkState::Continue
            })
        });
        let mut entries = files
            .into_inner()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        entries.sort_unstable_by(|a, b| a.path.cmp(&b.path));
        let truncated = truncated.into_inner();
        if truncated {
            tracing::warn!(
                root = %root.display(),
                limit = MAX_INDEX_FILES,
                "workspace has more files than the index holds; add ignore rules"
            );
        }
        Self {
            entries,
            directories: directories.into_inner(),
            truncated,
            built_at: started,
            build_time: started.elapsed(),
        }
    }

    /// Sorted by path.
    pub fn entries(&self) -> &[FileEntry] {
        &self.entries
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Directories seen (not counting the root).
    pub fn directories(&self) -> u64 {
        self.directories
    }

    /// The walk stopped at [`MAX_INDEX_FILES`].
    pub fn is_truncated(&self) -> bool {
        self.truncated
    }

    pub fn get(&self, rel_path: &str) -> Option<&FileEntry> {
        self.entries
            .binary_search_by(|e| e.path.as_str().cmp(rel_path))
            .ok()
            .map(|i| &self.entries[i])
    }

    /// Entries whose path is `dir` or under it (`""` for everything).
    pub fn under<'a>(&'a self, dir: &'a str) -> impl Iterator<Item = &'a FileEntry> + 'a {
        let dir = dir.trim_matches('/');
        let prefix = if dir.is_empty() {
            String::new()
        } else {
            format!("{dir}/")
        };
        let start = self
            .entries
            .partition_point(|e| e.path.as_str() < prefix.as_str());
        self.entries[start..]
            .iter()
            .take_while(move |e| e.path.starts_with(&prefix))
    }

    pub fn age(&self) -> Duration {
        self.built_at.elapsed()
    }

    pub fn is_stale(&self) -> bool {
        self.age() > INDEX_TTL
    }

    pub fn build_time(&self) -> Duration {
        self.build_time
    }
}

/// Language by extension or well-known file name; a short, stable
/// vocabulary used in prompts and by the context selector.
pub fn language_of(path: &Path) -> Option<&'static str> {
    let name = path.file_name()?.to_str()?;
    match name {
        "Makefile" | "GNUmakefile" | "makefile" => return Some("make"),
        "Dockerfile" | "Containerfile" => return Some("dockerfile"),
        "justfile" | "Justfile" => return Some("just"),
        "CMakeLists.txt" => return Some("cmake"),
        "Cargo.lock" | "Cargo.toml" => return Some("toml"),
        _ => {}
    }
    let ext = path.extension()?.to_str()?;
    Some(match ext.to_ascii_lowercase().as_str() {
        "rs" => "rust",
        "py" | "pyi" => "python",
        "ts" | "mts" | "cts" => "typescript",
        "tsx" => "tsx",
        "js" | "mjs" | "cjs" => "javascript",
        "jsx" => "jsx",
        "go" => "go",
        "java" => "java",
        "kt" | "kts" => "kotlin",
        "scala" => "scala",
        "c" | "h" => "c",
        "cc" | "cpp" | "cxx" | "hh" | "hpp" | "hxx" => "cpp",
        "cs" => "csharp",
        "rb" => "ruby",
        "php" => "php",
        "swift" => "swift",
        "m" | "mm" => "objective-c",
        "zig" => "zig",
        "lua" => "lua",
        "sh" | "bash" | "zsh" => "shell",
        "ps1" | "psm1" => "powershell",
        "bat" | "cmd" => "batch",
        "sql" => "sql",
        "html" | "htm" => "html",
        "css" => "css",
        "scss" | "sass" => "scss",
        "vue" => "vue",
        "svelte" => "svelte",
        "json" | "jsonc" => "json",
        "yaml" | "yml" => "yaml",
        "toml" => "toml",
        "xml" => "xml",
        "md" | "markdown" => "markdown",
        "txt" => "text",
        "proto" => "protobuf",
        "graphql" | "gql" => "graphql",
        "tf" => "terraform",
        "nix" => "nix",
        "ex" | "exs" => "elixir",
        "erl" => "erlang",
        "hs" => "haskell",
        "ml" | "mli" => "ocaml",
        "clj" | "cljs" => "clojure",
        "dart" => "dart",
        "r" => "r",
        "jl" => "julia",
        "cmake" => "cmake",
        "gradle" => "gradle",
        "ipynb" => "notebook",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn languages_by_extension_and_name() {
        let l = |s: &str| language_of(Path::new(s));
        assert_eq!(l("src/main.rs"), Some("rust"));
        assert_eq!(l("a/B.PY"), Some("python"));
        assert_eq!(l("Makefile"), Some("make"));
        assert_eq!(l("x/Dockerfile"), Some("dockerfile"));
        assert_eq!(l("Cargo.toml"), Some("toml"));
        assert_eq!(l("LICENSE"), None);
        assert_eq!(l("a.unknownext"), None);
    }

    #[test]
    fn index_lists_files_sorted_with_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("src/deep")).unwrap();
        std::fs::create_dir_all(root.join("target")).unwrap();
        std::fs::write(root.join("src/main.rs"), "fn main() {}").unwrap();
        std::fs::write(root.join("src/deep/x.py"), "x = 1").unwrap();
        std::fs::write(root.join("README.md"), "# hi").unwrap();
        std::fs::write(root.join("target/out.o"), "").unwrap();
        let index = FileIndex::build(root, &IgnoreRules::load(root));
        let paths: Vec<&str> = index.entries().iter().map(|e| e.path.as_str()).collect();
        assert_eq!(paths, ["README.md", "src/deep/x.py", "src/main.rs"]);
        assert_eq!(index.len(), 3);
        assert_eq!(index.directories(), 2);
        assert!(!index.is_truncated());
        assert!(!index.is_stale());
        let main = index.get("src/main.rs").unwrap();
        assert_eq!(main.size, 12);
        assert_eq!(main.language, Some("rust"));
        assert!(main.mtime.is_some());
        assert!(index.get("target/out.o").is_none());
        let under: Vec<&str> = index.under("src").map(|e| e.path.as_str()).collect();
        assert_eq!(under, ["src/deep/x.py", "src/main.rs"]);
        let under: Vec<&str> = index.under("src/deep/").map(|e| e.path.as_str()).collect();
        assert_eq!(under, ["src/deep/x.py"]);
        assert_eq!(index.under("").count(), 3);
        assert_eq!(index.under("sr").count(), 0);
    }
}
