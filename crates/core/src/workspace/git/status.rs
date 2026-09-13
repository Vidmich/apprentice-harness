//! `git status --porcelain=v2 -z --branch` parsed. The format is
//! stable by contract; `-z` keeps paths verbatim (no quoting) and puts
//! a rename's original path in the record after the entry.

/// One path `git status` reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Relative to the top of the work tree, `/` separators.
    pub path: String,
    /// The path before a rename or copy.
    pub orig_path: Option<String>,
    /// Index (staged) state: `M`, `A`, `D`, `R`, `C`, `T`, or `.`.
    pub index: char,
    /// Work tree (unstaged) state, same letters.
    pub worktree: char,
    /// A merge conflict (`index`/`worktree` are the two sides' states).
    pub unmerged: bool,
    pub untracked: bool,
}

impl Entry {
    pub fn is_staged(&self) -> bool {
        !self.unmerged && !self.untracked && self.index != '.'
    }

    pub fn is_unstaged(&self) -> bool {
        !self.unmerged && !self.untracked && self.worktree != '.'
    }
}

/// The parsed status.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Status {
    /// Commit `HEAD` points at; `None` on an unborn branch.
    pub head: Option<String>,
    /// Branch name; `None` when detached.
    pub branch: Option<String>,
    pub upstream: Option<String>,
    pub ahead: u64,
    pub behind: u64,
    pub entries: Vec<Entry>,
}

impl Status {
    /// Tracked changes exist (staged, unstaged or unmerged); untracked
    /// files do not count.
    pub fn is_dirty(&self) -> bool {
        self.entries.iter().any(|e| !e.untracked)
    }

    pub fn is_clean(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn staged(&self) -> impl Iterator<Item = &Entry> {
        self.entries.iter().filter(|e| e.is_staged())
    }

    pub fn unstaged(&self) -> impl Iterator<Item = &Entry> {
        self.entries.iter().filter(|e| e.is_unstaged())
    }

    pub fn unmerged(&self) -> impl Iterator<Item = &Entry> {
        self.entries.iter().filter(|e| e.unmerged)
    }

    pub fn untracked(&self) -> impl Iterator<Item = &Entry> {
        self.entries.iter().filter(|e| e.untracked)
    }

    /// Keeps the entries under `prefix` (as [`super::Repo::prefix`])
    /// with paths made relative to it; returns how many were dropped.
    pub fn restrict(&mut self, prefix: &str) -> usize {
        if prefix.is_empty() {
            return 0;
        }
        let before = self.entries.len();
        self.entries.retain_mut(|e| {
            let Some(rel) = e.path.strip_prefix(prefix).filter(|p| !p.is_empty()) else {
                return false;
            };
            e.path = rel.to_owned();
            e.orig_path = e
                .orig_path
                .take()
                .map(|o| o.strip_prefix(prefix).unwrap_or(&o).to_owned());
            true
        });
        before - self.entries.len()
    }
}

/// Parses the `-z` output. Unknown record types are skipped.
pub fn parse_status(bytes: &[u8]) -> Status {
    let mut status = Status::default();
    let mut records = bytes
        .split(|b| *b == 0)
        .filter(|r| !r.is_empty())
        .map(|r| String::from_utf8_lossy(r).into_owned());
    while let Some(record) = records.next() {
        if let Some(header) = record.strip_prefix("# ") {
            parse_header(&mut status, header);
            continue;
        }
        let mut fields = record.splitn(2, ' ');
        let (Some(kind), Some(rest)) = (fields.next(), fields.next()) else {
            continue;
        };
        match kind {
            "1" => {
                // XY sub mH mI mW hH hI path
                let mut f = rest.splitn(8, ' ');
                let (Some(xy), Some(path)) = (f.next(), f.nth(6)) else {
                    continue;
                };
                status.entries.push(entry(path, None, xy, false));
            }
            "2" => {
                // XY sub mH mI mW hH hI hW Xscore path (orig path follows)
                let mut f = rest.splitn(9, ' ');
                let (Some(xy), Some(path)) = (f.next(), f.nth(7)) else {
                    continue;
                };
                let orig = records.next();
                status.entries.push(entry(path, orig, xy, false));
            }
            "u" => {
                // XY sub m1 m2 m3 mW h1 h2 h3 path
                let mut f = rest.splitn(10, ' ');
                let (Some(xy), Some(path)) = (f.next(), f.nth(8)) else {
                    continue;
                };
                status.entries.push(entry(path, None, xy, true));
            }
            "?" => status.entries.push(Entry {
                path: rest.to_owned(),
                orig_path: None,
                index: '.',
                worktree: '.',
                unmerged: false,
                untracked: true,
            }),
            _ => {}
        }
    }
    status
}

fn entry(path: &str, orig_path: Option<String>, xy: &str, unmerged: bool) -> Entry {
    let mut xy = xy.chars();
    Entry {
        path: path.to_owned(),
        orig_path,
        index: xy.next().unwrap_or('.'),
        worktree: xy.next().unwrap_or('.'),
        unmerged,
        untracked: false,
    }
}

fn parse_header(status: &mut Status, header: &str) {
    let Some((key, value)) = header.split_once(' ') else {
        return;
    };
    match key {
        "branch.oid" => status.head = (value != "(initial)").then(|| value.to_owned()),
        "branch.head" => status.branch = (value != "(detached)").then(|| value.to_owned()),
        "branch.upstream" => status.upstream = Some(value.to_owned()),
        "branch.ab" => {
            for part in value.split(' ') {
                if let Some(n) = part.strip_prefix('+') {
                    status.ahead = n.parse().unwrap_or(0);
                } else if let Some(n) = part.strip_prefix('-') {
                    status.behind = n.parse().unwrap_or(0);
                }
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn z(records: &[&str]) -> Vec<u8> {
        let mut out = Vec::new();
        for r in records {
            out.extend_from_slice(r.as_bytes());
            out.push(0);
        }
        out
    }

    fn paths<'a>(it: impl Iterator<Item = &'a Entry>) -> Vec<&'a str> {
        it.map(|e| e.path.as_str()).collect()
    }

    #[test]
    fn parses_headers_and_every_entry_type() {
        let bytes = z(&[
            "# branch.oid 0123456789abcdef0123456789abcdef01234567",
            "# branch.head main",
            "# branch.upstream origin/main",
            "# branch.ab +2 -1",
            "1 M. N... 100644 100644 100644 aaaa bbbb src/staged.rs",
            "1 .M N... 100644 100644 100644 aaaa aaaa src/unstaged.rs",
            "1 MM N... 100644 100644 100644 aaaa bbbb src/both.rs",
            "2 R. N... 100644 100644 100644 aaaa aaaa R100 new name.rs",
            "old name.rs",
            "u UU N... 100644 100644 100644 100644 aaaa bbbb cccc conflict.txt",
            "? untracked.txt",
            "! ignored.log",
        ]);
        let status = parse_status(&bytes);
        assert_eq!(
            status.head.as_deref(),
            Some("0123456789abcdef0123456789abcdef01234567")
        );
        assert_eq!(status.branch.as_deref(), Some("main"));
        assert_eq!(status.upstream.as_deref(), Some("origin/main"));
        assert_eq!((status.ahead, status.behind), (2, 1));
        assert_eq!(
            paths(status.staged()),
            ["src/staged.rs", "src/both.rs", "new name.rs"]
        );
        assert_eq!(paths(status.unstaged()), ["src/unstaged.rs", "src/both.rs"]);
        assert_eq!(paths(status.unmerged()), ["conflict.txt"]);
        assert_eq!(paths(status.untracked()), ["untracked.txt"]);
        let renamed = status.entries.iter().find(|e| e.index == 'R').unwrap();
        assert_eq!(renamed.orig_path.as_deref(), Some("old name.rs"));
        assert!(status.is_dirty());
        assert!(!status.is_clean());
    }

    #[test]
    fn unborn_detached_and_clean() {
        let status = parse_status(&z(&["# branch.oid (initial)", "# branch.head main"]));
        assert_eq!(status.head, None);
        assert_eq!(status.branch.as_deref(), Some("main"));
        assert!(status.is_clean());
        assert!(!status.is_dirty());
        let status = parse_status(&z(&["# branch.oid abc", "# branch.head (detached)", "? x"]));
        assert_eq!(status.branch, None);
        assert!(!status.is_dirty(), "untracked files are not dirt");
        assert!(!status.is_clean());
    }

    #[test]
    fn restrict_keeps_the_subtree_relative() {
        let mut status = parse_status(&z(&[
            "1 .M N... 100644 100644 100644 a a sub/dir/x.rs",
            "1 .M N... 100644 100644 100644 a a other/y.rs",
            "2 R. N... 100644 100644 100644 a a R100 sub/dir/new.rs",
            "sub/dir/old.rs",
            "? sub/dir/z",
        ]));
        assert_eq!(status.restrict("sub/dir/"), 1);
        let paths: Vec<&str> = status.entries.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(paths, ["x.rs", "new.rs", "z"]);
        assert_eq!(status.entries[1].orig_path.as_deref(), Some("old.rs"));
        assert_eq!(status.restrict(""), 0);
    }
}
