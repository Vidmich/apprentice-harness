//! Bounded capture of a command's output: each stream on its own, plus
//! the combined transcript in the order the chunks arrived, and the
//! progress reporter that streams chunks to whoever listens on the
//! call's channel without ever blocking on them.

use std::collections::VecDeque;

use tokio::sync::mpsc::error::TrySendError;

use crate::tools::{ProgressStream, ToolContext, ToolProgress};

/// Bytes kept free for the omission marker inside a full capture.
const MARKER_RESERVE: usize = 96;
/// How far from a cut a line end is looked for, so head and tail
/// start and end on whole lines when lines are of ordinary length.
const LINE_SEARCH: usize = 4096;
/// Chunks the reporter holds back for a lagging consumer before it
/// starts dropping the oldest of them.
const PENDING_MAX: usize = 8;

/// One stream's bytes under a cap: the head (three quarters of the
/// budget) and the tail survive, the middle is counted. Feeding it is
/// O(bytes); nothing is copied twice.
#[derive(Debug)]
pub(crate) struct Capture {
    head_budget: usize,
    tail_budget: usize,
    head: Vec<u8>,
    head_full: bool,
    /// The most recent bytes after the head; trimmed from the front
    /// when it grows past twice the budget.
    tail: Vec<u8>,
    total: usize,
    newlines: usize,
}

impl Capture {
    pub fn new(max_bytes: usize) -> Self {
        let usable = max_bytes.saturating_sub(MARKER_RESERVE).max(16);
        let head_budget = usable * 3 / 4;
        Self {
            head_budget,
            tail_budget: usable - head_budget,
            head: Vec::new(),
            head_full: false,
            tail: Vec::new(),
            total: 0,
            newlines: 0,
        }
    }

    pub fn push(&mut self, bytes: &[u8]) {
        self.total += bytes.len();
        self.newlines += count_newlines(bytes);
        let mut rest = bytes;
        if !self.head_full {
            let room = self.head_budget - self.head.len();
            if bytes.len() <= room {
                self.head.extend_from_slice(bytes);
                return;
            }
            let cut = char_floor(bytes, room);
            self.head.extend_from_slice(&bytes[..cut]);
            self.head_full = true;
            rest = &bytes[cut..];
            // End the head on a line when one ends close enough behind
            // the budget; else on a character. What goes is counted as
            // omitted.
            let from = self.head.len().saturating_sub(LINE_SEARCH);
            match self.head[from..].iter().rposition(|b| *b == b'\n') {
                Some(i) => self.head.truncate(from + i + 1),
                None => trim_incomplete_char(&mut self.head),
            }
        }
        if rest.len() >= self.tail_budget {
            self.tail.clear();
            self.tail
                .extend_from_slice(&rest[rest.len() - self.tail_budget..]);
        } else {
            self.tail.extend_from_slice(rest);
            if self.tail.len() > 2 * self.tail_budget {
                let drop = self.tail.len() - self.tail_budget;
                self.tail.drain(..drop);
            }
        }
    }

    /// Bytes seen.
    pub fn total(&self) -> usize {
        self.total
    }

    /// Lines seen (a last line without a newline counts).
    pub fn lines(&self) -> usize {
        self.newlines + usize::from(self.total > 0 && !self.ends_with_newline())
    }

    pub fn ends_with_newline(&self) -> bool {
        let last = if self.tail.is_empty() {
            self.head.last()
        } else {
            self.tail.last()
        };
        last == Some(&b'\n')
    }

    /// `true` when [`Self::render`] leaves something out.
    pub fn truncated(&self) -> bool {
        self.total > self.head.len() + self.tail.len().min(self.tail_budget)
    }

    /// Head, marker (from `marker(omitted_bytes)`, when anything is
    /// missing) and tail. Cuts land on UTF-8 boundaries when the
    /// input was UTF-8.
    pub fn render(&self, marker: impl FnOnce(usize) -> String) -> Vec<u8> {
        let mut tail: &[u8] = &self.tail;
        if tail.len() > self.tail_budget {
            tail = &tail[tail.len() - self.tail_budget..];
        }
        let omitted = self.total - self.head.len() - tail.len();
        if omitted == 0 {
            let mut out = self.head.clone();
            out.extend_from_slice(tail);
            return out;
        }
        // Start the tail on a line when one begins soon enough; else on
        // a character.
        let skip = tail[..tail.len().min(LINE_SEARCH)]
            .iter()
            .position(|b| *b == b'\n')
            .map_or_else(
                || {
                    tail.iter()
                        .position(|b| !is_continuation(*b))
                        .unwrap_or(tail.len())
                },
                |i| i + 1,
            );
        let tail = &tail[skip..];
        let mut out = Vec::with_capacity(self.head.len() + tail.len() + MARKER_RESERVE);
        out.extend_from_slice(&self.head);
        if !out.is_empty() && !out.ends_with(b"\n") {
            out.push(b'\n');
        }
        out.extend_from_slice(marker(omitted + skip).as_bytes());
        out.push(b'\n');
        out.extend_from_slice(tail);
        out
    }
}

#[allow(
    clippy::naive_bytecount,
    reason = "not worth a crate for chunk-sized inputs"
)]
fn count_newlines(bytes: &[u8]) -> usize {
    bytes.iter().filter(|b| **b == b'\n').count()
}

fn is_continuation(b: u8) -> bool {
    b & 0b1100_0000 == 0b1000_0000
}

/// Drops a UTF-8 sequence that `bytes` ends in the middle of.
fn trim_incomplete_char(bytes: &mut Vec<u8>) {
    let Some(lead) = bytes.iter().rposition(|b| !is_continuation(*b)) else {
        return;
    };
    if bytes.len() - lead > 4 {
        return;
    }
    let expected = match bytes[lead] {
        b if b & 0b1000_0000 == 0 => 1,
        b if b & 0b1110_0000 == 0b1100_0000 => 2,
        b if b & 0b1111_0000 == 0b1110_0000 => 3,
        b if b & 0b1111_1000 == 0b1111_0000 => 4,
        _ => 1,
    };
    if bytes.len() - lead < expected {
        bytes.truncate(lead);
    }
}

/// Largest `i <= at` at which `bytes` can be cut without splitting a
/// UTF-8 sequence (0 when every byte up to `at` continues one).
fn char_floor(bytes: &[u8], at: usize) -> usize {
    let mut i = at.min(bytes.len());
    while i > 0 && i < bytes.len() && is_continuation(bytes[i]) {
        i -= 1;
    }
    i
}

/// The three captures of one run. Chunks are whole lines (the reader
/// splits at newlines); the combined transcript tags a change of
/// stream with a `[err]` / `[out]` line of its own, so a run that
/// only wrote to stdout reads exactly as stdout did.
#[derive(Debug)]
pub(super) struct Transcript {
    combined: Capture,
    stdout: Capture,
    stderr: Capture,
    current: Option<ProgressStream>,
}

impl Transcript {
    pub fn new(max_bytes: usize) -> Self {
        Self {
            combined: Capture::new(max_bytes),
            stdout: Capture::new(max_bytes),
            stderr: Capture::new(max_bytes),
            current: None,
        }
    }

    pub fn push(&mut self, stream: ProgressStream, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        match stream {
            ProgressStream::Stdout => self.stdout.push(bytes),
            ProgressStream::Stderr => self.stderr.push(bytes),
        }
        if self.current != Some(stream) {
            let leading_stdout = self.current.is_none() && stream == ProgressStream::Stdout;
            if !leading_stdout {
                if self.combined.total() > 0 && !self.combined.ends_with_newline() {
                    self.combined.push(b"\n");
                }
                self.combined.push(match stream {
                    ProgressStream::Stdout => b"[out]\n",
                    ProgressStream::Stderr => b"[err]\n",
                });
            }
            self.current = Some(stream);
        }
        self.combined.push(bytes);
    }

    pub fn stdout(&self) -> &Capture {
        &self.stdout
    }

    pub fn stderr(&self) -> &Capture {
        &self.stderr
    }

    /// The combined transcript as text (invalid UTF-8 shown as U+FFFD).
    pub fn text(&self) -> String {
        let raw = self.combined.render(|n| format!("[... {n} bytes omitted]"));
        String::from_utf8_lossy(&raw).into_owned()
    }

    /// One stream's bytes for its attachment, `None` when it stayed
    /// silent.
    pub fn stream_bytes(&self, stream: ProgressStream) -> Option<Vec<u8>> {
        let capture = match stream {
            ProgressStream::Stdout => &self.stdout,
            ProgressStream::Stderr => &self.stderr,
        };
        (capture.total() > 0)
            .then(|| capture.render(|n| format!("[... {n} bytes of {stream} omitted]")))
    }

    /// Something was left out of at least one capture.
    pub fn truncated(&self) -> bool {
        self.combined.truncated() || self.stdout.truncated() || self.stderr.truncated()
    }
}

/// Streams chunks to the call's progress channel. A consumer that
/// lags (the channel is full) costs the command nothing: chunks queue
/// up to [`PENDING_MAX`], then the oldest queued ones are dropped and
/// the next delivered chunk is preceded by a `[... N lines of output
/// not shown]` marker.
#[derive(Debug)]
pub(super) struct Reporter<'a> {
    ctx: &'a ToolContext,
    pending: VecDeque<(ProgressStream, String)>,
    dropped_lines: usize,
    closed: bool,
}

impl<'a> Reporter<'a> {
    pub fn new(ctx: &'a ToolContext) -> Self {
        Self {
            ctx,
            pending: VecDeque::new(),
            dropped_lines: 0,
            closed: ctx.progress.is_closed(),
        }
    }

    pub fn push(&mut self, stream: ProgressStream, text: String) {
        if self.closed {
            return;
        }
        self.pending.push_back((stream, text));
        if self.pending.len() > PENDING_MAX
            && let Some((_, old)) = self.pending.pop_front()
        {
            self.dropped_lines += count_lines(&old);
        }
        self.flush();
    }

    /// Delivers what the channel has room for; never waits.
    pub fn flush(&mut self) {
        while !self.closed {
            let text = if self.dropped_lines > 0 {
                format!("[... {} lines of output not shown]\n", self.dropped_lines)
            } else if let Some((_, text)) = self.pending.front() {
                text.clone()
            } else {
                return;
            };
            let stream = self
                .pending
                .front()
                .map_or(ProgressStream::Stdout, |(s, _)| *s);
            match self.ctx.progress.try_send(ToolProgress {
                call_id: self.ctx.call_id.clone(),
                stream,
                text,
            }) {
                Ok(()) => {
                    if self.dropped_lines > 0 {
                        self.dropped_lines = 0;
                    } else {
                        self.pending.pop_front();
                    }
                }
                Err(TrySendError::Full(_)) => return,
                Err(TrySendError::Closed(_)) => {
                    self.closed = true;
                    self.pending.clear();
                    self.dropped_lines = 0;
                }
            }
        }
    }

    /// Lines that never reached the consumer (so far).
    pub fn dropped_lines(&self) -> usize {
        self.dropped_lines
            + self
                .pending
                .iter()
                .map(|(_, t)| count_lines(t))
                .sum::<usize>()
    }
}

fn count_lines(text: &str) -> usize {
    count_newlines(text.as_bytes()) + usize::from(!text.is_empty() && !text.ends_with('\n'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_output_is_kept_whole() {
        let mut c = Capture::new(1024);
        c.push(b"hello\n");
        c.push(b"world");
        assert_eq!(c.total(), 11);
        assert_eq!(c.lines(), 2);
        assert!(!c.truncated());
        assert_eq!(c.render(|_| unreachable!()), b"hello\nworld");
    }

    #[test]
    fn big_output_keeps_head_and_tail_around_a_marker() {
        // 96 reserved, 928 usable: 696 head, 232 tail.
        let mut c = Capture::new(1024);
        for i in 0..1000 {
            c.push(format!("line {i:04}\n").as_bytes());
        }
        assert_eq!(c.total(), 10_000);
        assert_eq!(c.lines(), 1000);
        assert!(c.truncated());
        let out = String::from_utf8(c.render(|n| format!("[... {n} omitted]"))).unwrap();
        assert!(out.starts_with("line 0000\nline 0001\n"), "{out}");
        assert!(out.ends_with("line 0998\nline 0999\n"), "{out}");
        let marker_at = out.find("[... ").unwrap();
        assert_eq!(marker_at, 690, "{out}");
        assert!(out.len() <= 1024, "{}", out.len());
        // Head bytes + tail bytes + omitted == total.
        let omitted: usize = out[marker_at + 5..]
            .split(' ')
            .next()
            .unwrap()
            .parse()
            .unwrap();
        let marker_len = out[marker_at..].find('\n').unwrap() + 1;
        assert_eq!(out.len() - marker_len + omitted, 10_000);
    }

    #[test]
    fn cuts_never_split_utf8() {
        let mut c = Capture::new(200);
        let s = "😀".repeat(100); // 400 bytes
        for chunk in s.as_bytes().chunks(7) {
            c.push(chunk);
        }
        let out = c.render(|n| format!("[{n}]"));
        let text = String::from_utf8(out).expect("valid utf-8");
        assert!(text.starts_with('😀'));
        assert!(text.ends_with('😀'));
    }

    #[test]
    fn transcript_tags_stream_changes_only() {
        let mut t = Transcript::new(4096);
        t.push(ProgressStream::Stdout, b"one\n");
        t.push(ProgressStream::Stdout, b"two\n");
        assert_eq!(t.text(), "one\ntwo\n");
        t.push(ProgressStream::Stderr, b"oops\n");
        t.push(ProgressStream::Stdout, b"three");
        t.push(ProgressStream::Stderr, b"bad");
        assert_eq!(t.text(), "one\ntwo\n[err]\noops\n[out]\nthree\n[err]\nbad");
        assert_eq!(t.stdout().lines(), 3);
        assert_eq!(t.stderr().lines(), 2);
        assert_eq!(
            t.stream_bytes(ProgressStream::Stdout).unwrap(),
            b"one\ntwo\nthree"
        );

        let mut only_err = Transcript::new(4096);
        only_err.push(ProgressStream::Stderr, b"warn\n");
        assert_eq!(only_err.text(), "[err]\nwarn\n");
        assert!(only_err.stream_bytes(ProgressStream::Stdout).is_none());
    }

    #[tokio::test]
    async fn reporter_drops_the_oldest_pending_chunks_with_a_marker() {
        use std::sync::Arc;

        use crate::tools::{SeenFiles, ToolEnv};
        use crate::trace::{AgentId, SessionId};

        let (tx, mut rx) = tokio::sync::mpsc::channel(2);
        let ctx = ToolContext {
            workspace: None,
            session_id: SessionId::generate(),
            agent_id: AgentId::generate(),
            call_id: "c1".into(),
            env: ToolEnv::default(),
            config: Arc::default(),
            seen: Arc::new(SeenFiles::new()),
            progress: tx,
        };
        let mut r = Reporter::new(&ctx);
        for i in 0..20 {
            r.push(ProgressStream::Stdout, format!("chunk {i}\n"));
        }
        // Two delivered, eight pending, ten dropped.
        assert_eq!(r.dropped_lines(), 18);
        assert_eq!(rx.recv().await.unwrap().text, "chunk 0\n");
        assert_eq!(rx.recv().await.unwrap().text, "chunk 1\n");
        r.flush();
        assert_eq!(
            rx.recv().await.unwrap().text,
            "[... 10 lines of output not shown]\n"
        );
        assert_eq!(rx.recv().await.unwrap().text, "chunk 12\n");
        r.flush();
        assert_eq!(rx.recv().await.unwrap().text, "chunk 13\n");
        drop(rx);
        r.push(ProgressStream::Stdout, "late\n".into());
        assert_eq!(r.dropped_lines(), 0);
    }
}
