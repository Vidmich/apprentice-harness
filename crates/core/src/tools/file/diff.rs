//! Unified diffs of what a write or an edit changed, and the fuzzy
//! "closest line" hint for a failed edit.

use std::time::Duration;

use similar::{ChangeTag, TextDiff};

/// Lines of context around each hunk.
const CONTEXT: usize = 3;
/// Lines considered for the closest-line hint (the head of the file).
const HINT_MAX_LINES: usize = 20_000;
/// Similarity below which no hint is given.
const HINT_CUTOFF: f32 = 0.5;

/// A unified diff and its line counts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diff {
    /// `--- a/<path>` / `+++ b/<path>` headers, hunks with 3 lines of
    /// context; empty when nothing changed.
    pub text: String,
    pub added: usize,
    pub removed: usize,
}

impl Diff {
    pub fn is_empty(&self) -> bool {
        self.added == 0 && self.removed == 0
    }
}

/// The unified diff from `old` to `new` for the file shown as `path`.
pub fn unified_diff(path: &str, old: &str, new: &str) -> Diff {
    if old == new {
        return Diff {
            text: String::new(),
            added: 0,
            removed: 0,
        };
    }
    let diff = TextDiff::configure()
        .timeout(Duration::from_secs(2))
        .diff_lines(old, new);
    let (mut added, mut removed) = (0, 0);
    for change in diff.iter_all_changes() {
        match change.tag() {
            ChangeTag::Insert => added += 1,
            ChangeTag::Delete => removed += 1,
            ChangeTag::Equal => {}
        }
    }
    let text = diff
        .unified_diff()
        .context_radius(CONTEXT)
        .header(&format!("a/{path}"), &format!("b/{path}"))
        .to_string();
    Diff {
        text,
        added,
        removed,
    }
}

/// The line of `text` most similar to `needle` (1-based number and the
/// line): the first line containing the needle (whitespace trimmed),
/// else the fuzzily closest one when it is at least half similar.
pub(crate) fn closest_line<'a>(needle: &str, text: &'a str) -> Option<(usize, &'a str)> {
    let needle = needle.trim();
    if needle.is_empty() {
        return None;
    }
    let lines: Vec<&str> = text
        .lines()
        .take(HINT_MAX_LINES)
        .map(|l| l.trim_end_matches('\r'))
        .collect();
    if let Some(i) = lines.iter().position(|l| l.contains(needle)) {
        return Some((i + 1, lines[i]));
    }
    let best = similar::get_close_matches(needle, &lines, 1, HINT_CUTOFF)
        .into_iter()
        .next()?;
    let index = lines.iter().position(|l| *l == best)?;
    Some((index + 1, best))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_and_headers() {
        let d = unified_diff("src/x.rs", "a\nb\nc\n", "a\nB\nc\nd\n");
        assert_eq!((d.added, d.removed), (2, 1));
        assert!(
            d.text
                .starts_with("--- a/src/x.rs\n+++ b/src/x.rs\n@@ -1,3 +1,4 @@\n")
        );
        assert!(d.text.contains("-b\n+B\n"));
        let same = unified_diff("p", "x\n", "x\n");
        assert!(same.is_empty());
        assert_eq!(same.text, "");
    }

    #[test]
    fn closest_line_hint() {
        let text = "fn main() {\n    let count = 1;\n    println!(\"{count}\");\n}\n";
        assert_eq!(
            closest_line("let count = 2;", text),
            Some((2, "    let count = 1;"))
        );
        assert_eq!(closest_line("zzzzzzzzzzzzzzzzzz", text), None);
        assert_eq!(closest_line("   \n", text), None);
    }
}
