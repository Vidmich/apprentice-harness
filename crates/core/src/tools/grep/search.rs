//! The search itself: a matcher and a searcher configured for the
//! mentor, run over the workspace walker in parallel, results
//! collected per file and sorted by path afterwards so the output does
//! not depend on thread timing.

use std::io;
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use grep_matcher::{LineTerminator, Matcher};
use grep_regex::{RegexMatcher, RegexMatcherBuilder};
use grep_searcher::{
    BinaryDetection, Searcher, SearcherBuilder, Sink, SinkContext, SinkFinish, SinkMatch,
};
use ignore::WalkState;
use tokio_util::sync::CancellationToken;

use super::{GREP_LINE_CHARS, GREP_MAX_RESULTS, Mode};
use crate::tools::file::{PathGlob, Target};
use crate::workspace::Workspace;

/// Lines kept for display across all files stop here; matches are
/// still counted beyond it, so totals stay exact.
const STORE_CEILING: usize = 10 * GREP_MAX_RESULTS;

/// The mentor's request, normalised.
#[derive(Debug)]
pub(super) struct Options {
    pub mode: Mode,
    pub case_insensitive: bool,
    pub multiline: bool,
    /// Context lines each side (`content` mode).
    pub context: usize,
    /// Matching lines (`content`) or files (`files`, `count`) to show.
    pub limit: usize,
    pub glob: Option<PathGlob>,
}

#[derive(Debug)]
pub(super) enum SearchError {
    /// The regex engine refused the pattern (its message).
    Regex(String),
    /// The path cannot be searched (message for the mentor).
    Path(String),
    Cancelled,
}

#[derive(Debug, Default)]
pub(super) struct SearchResult {
    /// Files with at least one match, sorted by path.
    pub files: Vec<FileHits>,
    /// Files the searcher opened.
    pub searched: usize,
    /// [`STORE_CEILING`] was hit: some matching files have no lines
    /// kept, so the shown lines may not be the first ones by path.
    pub overflowed: bool,
}

#[derive(Debug)]
pub(super) struct FileHits {
    /// Workspace-relative display form.
    pub path: String,
    /// Matches in the file (matching lines; in multiline mode, match
    /// blocks). Exact even when not every line was kept.
    pub matches: usize,
    /// `content` mode: matching and context lines in file order, at
    /// most `limit` matching ones.
    pub lines: Vec<Line>,
    /// Lines were dropped because of [`STORE_CEILING`].
    pub partial: bool,
}

#[derive(Debug)]
pub(super) enum Line {
    /// One match: a single line, or the lines of a multi-line match.
    Match {
        /// Line number of the first line.
        n: u64,
        /// Per line: the 1-based byte column of the first match starting
        /// on it (1 for the later lines of a multi-line match), and its
        /// text.
        lines: Vec<(usize, String)>,
    },
    Context {
        n: u64,
        text: String,
    },
    /// Between non-adjacent groups of lines.
    Break,
}

/// Searches `t` (a directory walked with the ignore rules, or one file)
/// for `pattern`.
pub(super) fn run(
    ws: &Workspace,
    t: &Target,
    pattern: &str,
    o: &Options,
    cancel: &CancellationToken,
) -> Result<SearchResult, SearchError> {
    let matcher = build_matcher(pattern, o)?;
    let searcher = build_searcher(o);
    let meta = match std::fs::metadata(&t.abs) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Err(SearchError::Path(format!("`{}` does not exist", t.shown)));
        }
        Err(e) => {
            return Err(SearchError::Path(format!(
                "cannot search `{}`: {e}",
                t.shown
            )));
        }
    };
    let shared = Shared::default();
    if meta.is_file() {
        let mut searcher = searcher;
        search_one(&mut searcher, &matcher, &t.abs, t.shown.clone(), o, &shared);
    } else {
        let dir = t.abs.as_path();
        ws.ignore_rules()
            .walk_builder_at(dir, None)
            .build_parallel()
            .run(|| {
                let mut searcher = searcher.clone();
                let matcher = matcher.clone();
                let shared = &shared;
                Box::new(move |entry| {
                    if cancel.is_cancelled() {
                        return WalkState::Quit;
                    }
                    let entry = match entry {
                        Ok(e) => e,
                        Err(err) => {
                            tracing::debug!(error = %err, "grep walk");
                            return WalkState::Continue;
                        }
                    };
                    if !entry.file_type().is_some_and(|k| k.is_file()) {
                        return WalkState::Continue;
                    }
                    if let Some(glob) = &o.glob
                        && !glob.is_match(&relative(dir, entry.path()))
                    {
                        return WalkState::Continue;
                    }
                    let shown = ws.display(entry.path());
                    search_one(&mut searcher, &matcher, entry.path(), shown, o, shared);
                    WalkState::Continue
                })
            });
    }
    if cancel.is_cancelled() {
        return Err(SearchError::Cancelled);
    }
    let mut files = shared
        .files
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(SearchResult {
        files,
        searched: shared.searched.into_inner(),
        overflowed: shared.overflowed.into_inner(),
    })
}

/// ripgrep's matcher settings: `^`/`$` are line anchors, `$` accepts
/// `\r\n`, Unicode on. Line mode forbids a pattern that could match a
/// line terminator; multiline mode lets `.` cross lines.
fn build_matcher(pattern: &str, o: &Options) -> Result<RegexMatcher, SearchError> {
    let mut b = RegexMatcherBuilder::new();
    b.case_insensitive(o.case_insensitive)
        .multi_line(true)
        .unicode(true)
        .octal(false)
        .crlf(true);
    if o.multiline {
        b.dot_matches_new_line(true).line_terminator(None);
    } else {
        b.ban_byte(Some(0));
    }
    b.build(pattern)
        .map_err(|e| SearchError::Regex(e.to_string()))
}

fn build_searcher(o: &Options) -> Searcher {
    let mut b = SearcherBuilder::new();
    b.line_number(true)
        .line_terminator(LineTerminator::crlf())
        .multi_line(o.multiline)
        .binary_detection(BinaryDetection::quit(0));
    if o.mode == Mode::Content {
        b.before_context(o.context).after_context(o.context);
    }
    b.build()
}

/// `path` under `dir`, `/`-separated, for the glob filter.
fn relative(dir: &Path, path: &Path) -> String {
    path.strip_prefix(dir)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

#[derive(Debug, Default)]
struct Shared {
    files: Mutex<Vec<FileHits>>,
    searched: AtomicUsize,
    /// Lines kept so far, against [`STORE_CEILING`].
    stored: AtomicUsize,
    overflowed: AtomicBool,
}

fn search_one(
    searcher: &mut Searcher,
    matcher: &RegexMatcher,
    path: &Path,
    shown: String,
    o: &Options,
    shared: &Shared,
) {
    shared.searched.fetch_add(1, Ordering::Relaxed);
    let mut sink = Collector {
        matcher,
        o,
        shared,
        hits: FileHits {
            path: shown,
            matches: 0,
            lines: Vec::new(),
            partial: false,
        },
        kept: 0,
        frozen: false,
        binary: false,
    };
    if let Err(err) = searcher.search_path(matcher, path, &mut sink) {
        tracing::debug!(path = %path.display(), error = %err, "grep read");
        return;
    }
    if sink.binary || sink.hits.matches == 0 {
        return;
    }
    let hits = sink.hits;
    shared
        .files
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push(hits);
}

/// The sink for one file.
struct Collector<'a> {
    matcher: &'a RegexMatcher,
    o: &'a Options,
    shared: &'a Shared,
    hits: FileHits,
    /// Matching lines kept (per-file cap `o.limit`).
    kept: usize,
    /// No more lines are kept for this file.
    frozen: bool,
    binary: bool,
}

impl Collector<'_> {
    /// Claims room for one more kept line, or freezes the file.
    fn reserve(&mut self) -> bool {
        if self.frozen {
            return false;
        }
        if self.shared.stored.fetch_add(1, Ordering::Relaxed) >= STORE_CEILING {
            self.shared.overflowed.store(true, Ordering::Relaxed);
            self.hits.partial = true;
            self.frozen = true;
            return false;
        }
        true
    }
}

impl Sink for Collector<'_> {
    type Error = io::Error;

    fn matched(&mut self, _searcher: &Searcher, mat: &SinkMatch<'_>) -> Result<bool, io::Error> {
        self.hits.matches += 1;
        match self.o.mode {
            Mode::Files => return Ok(false),
            Mode::Count => return Ok(true),
            Mode::Content => {}
        }
        if self.kept >= self.o.limit {
            // Enough for any output; keep counting only.
            self.frozen = true;
        }
        if !self.reserve() {
            return Ok(true);
        }
        self.kept += 1;
        let bytes = mat.bytes();
        let mut lines = Vec::new();
        let mut offset = 0usize;
        for line in mat.lines() {
            let col = self
                .matcher
                .find_at(bytes, offset)
                .ok()
                .flatten()
                .filter(|m| m.start() < offset + line.len())
                .map_or(1, |m| m.start() - offset + 1);
            lines.push((col, clean(line)));
            offset += line.len();
        }
        self.hits.lines.push(Line::Match {
            n: mat.line_number().unwrap_or(0),
            lines,
        });
        Ok(true)
    }

    fn context(&mut self, _searcher: &Searcher, ctx: &SinkContext<'_>) -> Result<bool, io::Error> {
        if self.reserve() {
            self.hits.lines.push(Line::Context {
                n: ctx.line_number().unwrap_or(0),
                text: clean(ctx.bytes()),
            });
        }
        Ok(true)
    }

    fn context_break(&mut self, _searcher: &Searcher) -> Result<bool, io::Error> {
        if !self.frozen
            && matches!(
                self.hits.lines.last(),
                Some(Line::Match { .. } | Line::Context { .. })
            )
        {
            self.hits.lines.push(Line::Break);
        }
        Ok(true)
    }

    fn binary_data(&mut self, _searcher: &Searcher, _offset: u64) -> Result<bool, io::Error> {
        self.binary = true;
        Ok(false)
    }

    fn finish(&mut self, _searcher: &Searcher, finish: &SinkFinish) -> Result<(), io::Error> {
        if finish.binary_byte_offset().is_some() {
            self.binary = true;
        }
        Ok(())
    }
}

/// A line for the output: no terminator, valid UTF-8 (lossy), at most
/// [`GREP_LINE_CHARS`] characters plus `…`.
fn clean(line: &[u8]) -> String {
    let mut end = line.len();
    if line[..end].ends_with(b"\n") {
        end -= 1;
    }
    if line[..end].ends_with(b"\r") {
        end -= 1;
    }
    let text = String::from_utf8_lossy(&line[..end]);
    match text.char_indices().nth(GREP_LINE_CHARS) {
        Some((cut, _)) => {
            let mut s = text[..cut].to_owned();
            s.push('\u{2026}');
            s
        }
        None => text.into_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_strips_terminators_and_cuts_long_lines() {
        assert_eq!(clean(b"abc\r\n"), "abc");
        assert_eq!(clean(b"abc\n"), "abc");
        assert_eq!(clean(b"abc"), "abc");
        assert_eq!(clean(b"caf\xE9\n"), "caf\u{FFFD}");
        let long = "x".repeat(GREP_LINE_CHARS + 5);
        let cut = clean(long.as_bytes());
        assert_eq!(cut.chars().count(), GREP_LINE_CHARS + 1);
        assert!(cut.ends_with('\u{2026}'));
        let exact = "y".repeat(GREP_LINE_CHARS);
        assert_eq!(clean(exact.as_bytes()), exact);
    }

    #[test]
    fn line_mode_rejects_patterns_that_cross_lines() {
        let o = Options {
            mode: Mode::Content,
            case_insensitive: false,
            multiline: false,
            context: 0,
            limit: 10,
            glob: None,
        };
        assert!(matches!(
            build_matcher("a\\nb", &o),
            Err(SearchError::Regex(_))
        ));
        assert!(matches!(
            build_matcher("a(", &o),
            Err(SearchError::Regex(_))
        ));
        assert!(build_matcher("a$", &o).is_ok());
        let multi = Options {
            multiline: true,
            ..o
        };
        assert!(build_matcher("a\\nb", &multi).is_ok());
    }
}
