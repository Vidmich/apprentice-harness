//! Ignore rules for discovery (listing, searching, the index). They are
//! not access control: a tool may still read an ignored file the mentor
//! names explicitly; the sandbox and the permission engine decide that.
//!
//! Combined in order: the built-in defaults, the `.gitignore` chain
//! (`.gitignore` files from the root down plus `.git/info/exclude`, via
//! the `ignore` crate), then `.harness/ignore` in the same syntax. A
//! `!pattern` in `.harness/ignore` can un-ignore a built-in default but
//! not something a `.gitignore` excludes.

use std::path::{Path, PathBuf};

use ignore::gitignore::{Gitignore, GitignoreBuilder};
use ignore::{DirEntry, WalkBuilder};

use super::{HARNESS_DIR, IGNORE_FILE};

/// Always ignored, in gitignore syntax. `.harness/` is hidden except
/// for `HARNESS.md`, which the mentor may read.
pub const BUILTIN_IGNORES: &[&str] = &[
    ".git/",
    ".hg/",
    ".svn/",
    "node_modules/",
    "target/",
    "dist/",
    "__pycache__/",
    ".venv/",
    ".harness/*",
    "!.harness/HARNESS.md",
    // binaries by extension
    "*.png",
    "*.jpg",
    "*.jpeg",
    "*.gif",
    "*.bmp",
    "*.ico",
    "*.webp",
    "*.pdf",
    "*.zip",
    "*.gz",
    "*.tgz",
    "*.bz2",
    "*.xz",
    "*.zst",
    "*.7z",
    "*.rar",
    "*.tar",
    "*.jar",
    "*.exe",
    "*.dll",
    "*.so",
    "*.dylib",
    "*.o",
    "*.obj",
    "*.a",
    "*.lib",
    "*.pdb",
    "*.class",
    "*.pyc",
    "*.pyo",
    "*.wasm",
    "*.bin",
    "*.gguf",
    "*.safetensors",
    "*.pt",
    "*.pth",
    "*.onnx",
    "*.ckpt",
    "*.npy",
    "*.npz",
    "*.parquet",
    "*.sqlite",
    "*.sqlite3",
    "*.db",
    "*.woff",
    "*.woff2",
    "*.ttf",
    "*.otf",
    "*.eot",
    "*.mp3",
    "*.mp4",
    "*.wav",
    "*.ogg",
    "*.flac",
    "*.avi",
    "*.mov",
    "*.mkv",
    "*.webm",
    "*.iso",
    "*.dmg",
    "*.msi",
    "*.deb",
    "*.rpm",
];

/// The built-in defaults plus `.harness/ignore`, matched relative to
/// the root. The `.gitignore` chain is the walker's own.
#[derive(Debug, Clone)]
pub struct IgnoreRules {
    root: PathBuf,
    rules: Gitignore,
    /// `.harness/ignore` was present and read.
    has_custom: bool,
}

impl IgnoreRules {
    /// Reads `.harness/ignore` under `root` (a missing file is fine; a
    /// bad line is logged and skipped).
    pub fn load(root: &Path) -> Self {
        let mut builder = GitignoreBuilder::new(root);
        for line in BUILTIN_IGNORES {
            builder
                .add_line(None, line)
                .expect("built-in ignore patterns are valid");
        }
        let custom = root.join(HARNESS_DIR).join(IGNORE_FILE);
        let has_custom = custom.is_file();
        if has_custom && let Some(err) = builder.add(&custom) {
            tracing::warn!(file = %custom.display(), error = %err, "ignore pattern skipped");
        }
        let rules = builder.build().unwrap_or_else(|err| {
            tracing::warn!(root = %root.display(), error = %err, "ignore rules unusable; using none");
            Gitignore::empty()
        });
        Self {
            root: root.to_path_buf(),
            rules,
            has_custom,
        }
    }

    pub fn has_custom_file(&self) -> bool {
        self.has_custom
    }

    /// Whether `path` (absolute under the root, or root-relative) or one
    /// of its parents is ignored by the defaults or `.harness/ignore`.
    pub fn is_ignored(&self, path: &Path, is_dir: bool) -> bool {
        let rel = path.strip_prefix(&self.root).unwrap_or(path);
        if rel.as_os_str().is_empty() {
            return false;
        }
        self.rules
            .matched_path_or_any_parents(rel, is_dir)
            .is_ignore()
    }

    /// A walker over the root applying every rule (see the module docs):
    /// no symlink following, dotfiles included, deterministic order.
    pub fn walk_builder(&self) -> WalkBuilder {
        self.walk_builder_from(&self.root)
    }

    /// The same walker started at `dir` (under the root) and limited to
    /// `max_depth` levels below it. The rules still match relative to
    /// the root, and the `.gitignore` chain above `dir` applies.
    pub fn walk_builder_at(&self, dir: &Path, max_depth: usize) -> WalkBuilder {
        let mut b = self.walk_builder_from(dir);
        b.max_depth(Some(max_depth));
        b
    }

    fn walk_builder_from(&self, start: &Path) -> WalkBuilder {
        // A start directory that the rules themselves hide (`target/`)
        // was named on purpose: list it without them.
        let explicit = start != self.root && self.is_ignored(start, true);
        let rules = self.clone();
        let mut b = WalkBuilder::new(start);
        b.hidden(false)
            .follow_links(false)
            .require_git(false)
            .git_global(false)
            .git_ignore(true)
            .git_exclude(true)
            .ignore(false)
            .parents(true)
            .sort_by_file_path(Ord::cmp)
            .filter_entry(move |e: &DirEntry| {
                if explicit || e.depth() == 0 {
                    return true;
                }
                let is_dir = e.file_type().is_some_and(|t| t.is_dir());
                !rules.is_ignored(e.path(), is_dir)
            });
        b
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_hide_harness_dir_except_instructions() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let rules = IgnoreRules::load(root);
        assert!(!rules.has_custom_file());
        let ignored = |p: &str, d: bool| rules.is_ignored(&root.join(p), d);
        assert!(ignored(".git", true));
        assert!(ignored("a/node_modules", true));
        assert!(ignored("a/node_modules/x/index.js", false));
        assert!(ignored("target", true));
        assert!(ignored("img/logo.png", false));
        assert!(ignored(".harness/config.toml", false));
        assert!(ignored(".harness/blobs/x", false));
        assert!(!ignored(".harness", true));
        assert!(!ignored(".harness/HARNESS.md", false));
        assert!(!ignored("src/main.rs", false));
        assert!(!ignored(".github/workflows/ci.yml", false));
        // Relative paths work too.
        assert!(rules.is_ignored(Path::new("dist/app.js"), false));
    }

    #[test]
    fn custom_file_adds_and_unignores() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join(".harness")).unwrap();
        std::fs::write(
            root.join(".harness/ignore"),
            "generated/\n*.log\n!important.log\n!*.png\n",
        )
        .unwrap();
        let rules = IgnoreRules::load(root);
        assert!(rules.has_custom_file());
        assert!(rules.is_ignored(Path::new("generated/x.rs"), false));
        assert!(rules.is_ignored(Path::new("a/b.log"), false));
        assert!(!rules.is_ignored(Path::new("a/important.log"), false));
        assert!(!rules.is_ignored(Path::new("img/logo.png"), false));
        assert!(rules.is_ignored(Path::new(".harness/ignore"), false));
    }
}
