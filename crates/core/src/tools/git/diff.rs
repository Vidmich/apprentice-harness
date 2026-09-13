//! Cutting a unified diff to a byte budget without breaking a hunk:
//! whole files while they fit, then the hunks of the next file while
//! they fit, a `[diff truncated for <path>]` marker, and one
//! `[diff omitted for <path>]` line per file that did not make it.

use std::fmt::Write as _;
use std::ops::Range;

/// Omitted files are listed one per line up to here.
const OMITTED_LIST_MAX: usize = 50;

/// A cut diff and its numbers (counted over the whole input).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cut {
    pub text: String,
    pub files: usize,
    /// `+` lines.
    pub added: usize,
    /// `-` lines.
    pub removed: usize,
    pub truncated: bool,
}

struct File {
    path: String,
    /// The whole section, `diff --git` line included.
    span: Range<usize>,
    /// Up to the first hunk.
    header: Range<usize>,
    hunks: Vec<Range<usize>>,
}

/// Cuts `diff` to about `max_bytes`.
pub fn cut(diff: &str, max_bytes: usize) -> Cut {
    let files = split(diff);
    let (mut added, mut removed) = (0, 0);
    for line in diff.lines() {
        if line.starts_with('+') && !line.starts_with("+++") {
            added += 1;
        } else if line.starts_with('-') && !line.starts_with("---") {
            removed += 1;
        }
    }
    if diff.len() <= max_bytes {
        return Cut {
            text: diff.to_owned(),
            files: files.len(),
            added,
            removed,
            truncated: false,
        };
    }
    let mut text = String::with_capacity(max_bytes + 256);
    for (i, file) in files.iter().enumerate() {
        if text.len() + file.span.len() <= max_bytes {
            text.push_str(&diff[file.span.clone()]);
            continue;
        }
        text.push_str(&diff[file.header.clone()]);
        for hunk in &file.hunks {
            if text.len() + hunk.len() > max_bytes {
                break;
            }
            text.push_str(&diff[hunk.clone()]);
        }
        ensure_newline(&mut text);
        let _ = writeln!(text, "[diff truncated for {}]", file.path);
        let rest = &files[i + 1..];
        for file in rest.iter().take(OMITTED_LIST_MAX) {
            let _ = writeln!(text, "[diff omitted for {}]", file.path);
        }
        if rest.len() > OMITTED_LIST_MAX {
            let _ = writeln!(
                text,
                "[... {} more files omitted]",
                rest.len() - OMITTED_LIST_MAX
            );
        }
        break;
    }
    Cut {
        text,
        files: files.len(),
        added,
        removed,
        truncated: true,
    }
}

fn ensure_newline(text: &mut String) {
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
}

/// Splits at `diff --git` lines; hunks at `@@` lines. Anything before
/// the first file line belongs to no file and is dropped.
fn split(diff: &str) -> Vec<File> {
    let mut files: Vec<File> = Vec::new();
    let mut pos = 0;
    for line in diff.split_inclusive('\n') {
        let start = pos;
        pos += line.len();
        if let Some(rest) = line.strip_prefix("diff --git ") {
            files.push(File {
                path: path_of(rest.trim_end()),
                span: start..pos,
                header: start..pos,
                hunks: Vec::new(),
            });
            continue;
        }
        let Some(file) = files.last_mut() else {
            continue;
        };
        file.span.end = pos;
        if line.starts_with("@@") {
            file.hunks.push(start..pos);
        } else if let Some(hunk) = file.hunks.last_mut() {
            hunk.end = pos;
        } else {
            file.header.end = pos;
        }
    }
    files
}

/// The `b/` side of `a/<old> b/<new>`. Git quotes a path with a
/// space or an unusual byte, so an unquoted one ends at the first
/// ` b/`; the quotes are dropped, escapes inside are left as they are.
fn path_of(rest: &str) -> String {
    let b = if let Some(i) = rest.find(" \"b/") {
        &rest[i + 4..]
    } else if let Some(i) = rest.find(" b/") {
        &rest[i + 3..]
    } else {
        rest
    };
    b.trim_end_matches('"').to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hunks(text: &str) -> usize {
        text.lines().filter(|l| l.starts_with("@@")).count()
    }

    fn file(name: &str, hunks: usize, lines: usize) -> String {
        let mut s = format!(
            "diff --git a/{name} b/{name}\nindex 000..111 100644\n--- a/{name}\n+++ b/{name}\n"
        );
        for h in 0..hunks {
            let _ = writeln!(s, "@@ -{0},{1} +{0},{1} @@", h * 10 + 1, lines);
            for l in 0..lines {
                let _ = writeln!(s, "-old {h} {l}");
                let _ = writeln!(s, "+new {h} {l}");
            }
        }
        s
    }

    #[test]
    fn a_small_diff_is_kept_whole_and_counted() {
        let diff = file("a.rs", 2, 3) + &file("b.rs", 1, 1);
        let cut = cut(&diff, 10_000);
        assert_eq!(cut.text, diff);
        assert_eq!((cut.files, cut.added, cut.removed), (2, 7, 7));
        assert!(!cut.truncated);
    }

    #[test]
    fn the_cut_lands_between_hunks_and_lists_the_rest() {
        let a = file("a.rs", 3, 4);
        let b = file("b.rs", 1, 1);
        let c = file("c.rs", 1, 1);
        let diff = format!("{a}{b}{c}");
        // Room for `a.rs`'s header and its first two hunks only.
        let header_len = a.find("@@").unwrap();
        let hunk_len = (a.len() - header_len) / 3;
        let cut = cut(&diff, header_len + hunk_len * 2 + 10);
        assert!(cut.truncated);
        assert_eq!(hunks(&cut.text), 2, "{}", cut.text);
        assert!(cut.text.ends_with(
            "+new 1 3\n[diff truncated for a.rs]\n[diff omitted for b.rs]\n[diff omitted for c.rs]\n"
        ), "{}", cut.text);
        assert_eq!((cut.files, cut.added, cut.removed), (3, 14, 14));
    }

    #[test]
    fn a_tiny_budget_still_shows_the_first_header() {
        let diff = file("a.rs", 1, 2);
        let cut = cut(&diff, 8);
        assert!(cut.text.starts_with("diff --git a/a.rs b/a.rs\n"));
        assert!(!cut.text.contains("@@"));
        assert!(cut.text.ends_with("[diff truncated for a.rs]\n"));
    }

    #[test]
    fn paths_come_from_the_b_side() {
        assert_eq!(path_of("a/x.rs b/y.rs"), "y.rs");
        assert_eq!(path_of("\"a/sp ace.rs\" \"b/sp ace.rs\""), "sp ace.rs");
        assert_eq!(path_of("a/old.rs b/new.rs"), "new.rs");
    }
}
