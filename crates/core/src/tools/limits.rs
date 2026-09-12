//! Output limits. The trace keeps the raw output whole (up to
//! `tools.max_capture_bytes`, past which it keeps the head and the tail
//! and says so); the mentor gets at most `tools.max_mentor_bytes` as head
//! + tail around an omission marker naming the blob that has the rest.
//!
//! Crude, and meant to be: the apprentice's output compressor (M03)
//! replaces this policy, and the raw blob is what it trains on.

/// Media type of a text tool result blob.
pub const OUTPUT_MEDIA_TYPE: &str = "text/plain; charset=utf-8";

/// Share of the budget given to the head; the tail gets the rest
/// (24 KiB + 8 KiB at the 32 KiB default).
const HEAD_SHARE: (usize, usize) = (3, 4);

/// A text cut to a byte budget.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Truncated {
    pub text: String,
    /// `true` when something was left out.
    pub truncated: bool,
    /// Bytes not present in `text` (0 when not truncated).
    pub omitted: usize,
}

/// Largest index `<= at` that is a char boundary of `s`.
pub(crate) fn floor_boundary(s: &str, at: usize) -> usize {
    let mut i = at.min(s.len());
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Smallest index `>= at` that is a char boundary of `s`.
pub(crate) fn ceil_boundary(s: &str, at: usize) -> usize {
    let mut i = at.min(s.len());
    while !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

/// Cuts `text` to about `max_bytes`: a head of three quarters of the
/// budget, the marker from `marker(omitted_bytes)`, then the tail. Cuts
/// fall on char boundaries, so the result is valid UTF-8 and never
/// splits a code point; it may exceed `max_bytes` by the marker.
pub fn truncate_utf8(text: &str, max_bytes: usize, marker: impl Fn(usize) -> String) -> Truncated {
    if text.len() <= max_bytes {
        return Truncated {
            text: text.to_owned(),
            truncated: false,
            omitted: 0,
        };
    }
    let head_budget = max_bytes * HEAD_SHARE.0 / HEAD_SHARE.1;
    let head_end = floor_boundary(text, head_budget);
    let tail_start = ceil_boundary(text, text.len() - (max_bytes - head_budget)).max(head_end);
    let omitted = tail_start - head_end;
    let mut out = String::with_capacity(max_bytes + 96);
    out.push_str(&text[..head_end]);
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&marker(omitted));
    out.push('\n');
    out.push_str(&text[tail_start..]);
    Truncated {
        text: out,
        truncated: true,
        omitted,
    }
}

/// The same head + tail policy on raw bytes, for the capture limit: the
/// stored blob is `head ++ tail` and the caller records the real length.
/// For text the cuts stay on char boundaries.
pub(crate) fn capture_bytes(raw: &[u8], max_bytes: usize) -> Option<Vec<u8>> {
    if raw.len() <= max_bytes {
        return None;
    }
    let head_budget = max_bytes * HEAD_SHARE.0 / HEAD_SHARE.1;
    let tail_budget = max_bytes - head_budget;
    let (head_end, tail_start) = match std::str::from_utf8(raw) {
        Ok(s) => (
            floor_boundary(s, head_budget),
            ceil_boundary(s, s.len() - tail_budget),
        ),
        Err(_) => (head_budget, raw.len() - tail_budget),
    };
    let mut out = Vec::with_capacity(max_bytes);
    out.extend_from_slice(&raw[..head_end]);
    out.extend_from_slice(&raw[tail_start.max(head_end)..]);
    Some(out)
}

/// One line of at most `max_chars` characters: newlines become spaces,
/// runs of whitespace collapse, an overlong line ends in `…`.
pub(crate) fn one_line(text: &str, max_chars: usize) -> String {
    let mut out = String::with_capacity(text.len().min(max_chars * 4));
    let mut pending_space = false;
    for c in text.chars() {
        if c.is_whitespace() {
            pending_space = !out.is_empty();
            continue;
        }
        if pending_space {
            out.push(' ');
            pending_space = false;
        }
        out.push(c);
    }
    if out.chars().count() > max_chars {
        let keep = out
            .char_indices()
            .nth(max_chars.saturating_sub(1))
            .map_or(out.len(), |(i, _)| i);
        out.truncate(keep);
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_text_is_untouched() {
        let t = truncate_utf8("hello", 32, |n| format!("[{n}]"));
        assert_eq!(t.text, "hello");
        assert!(!t.truncated);
        assert_eq!(t.omitted, 0);
    }

    #[test]
    fn long_text_keeps_head_and_tail_around_the_marker() {
        let text = (0..100).fold(String::new(), |mut s, i| {
            use std::fmt::Write as _;
            let _ = writeln!(s, "line {i:03}");
            s
        });
        assert_eq!(text.len(), 900);
        let t = truncate_utf8(&text, 400, |n| format!("[... {n} bytes omitted]"));
        assert!(t.truncated);
        // 300 bytes of head (33 full lines and a third), 100 of tail.
        assert!(t.text.starts_with("line 000\nline 001\n"));
        assert!(t.text.ends_with("line 098\nline 099\n"));
        assert!(t.text.contains("\n[... 500 bytes omitted]\n"), "{}", t.text);
        assert_eq!(t.omitted, 500);
        assert_eq!(t.text.len(), 400 + "\n[... 500 bytes omitted]\n".len());
    }

    #[test]
    fn cuts_never_split_a_code_point() {
        // 4-byte code points: no budget lands on a boundary by accident.
        let text = "😀".repeat(50);
        for max in [10, 13, 17, 33, 100] {
            let t = truncate_utf8(&text, max, |n| format!("[{n}]"));
            assert!(t.truncated, "max {max}");
            assert!(t.text.chars().all(|c| c == '😀'
                || c == '['
                || c == ']'
                || c == '\n'
                || c.is_ascii_digit()));
            assert_eq!(t.omitted % 4, 0);
            assert!(t.text.len() >= 4 + 4, "max {max}: {}", t.text);
        }
        let raw = capture_bytes(text.as_bytes(), 41).unwrap();
        assert!(std::str::from_utf8(&raw).is_ok());
        assert_eq!(raw.len() % 4, 0);
        assert!(raw.len() <= 41);
    }

    #[test]
    fn capture_keeps_head_and_tail_of_binary() {
        let raw: Vec<u8> = (0..=255u8).collect();
        assert!(capture_bytes(&raw, 256).is_none());
        let cut = capture_bytes(&raw, 8).unwrap();
        assert_eq!(cut, [0, 1, 2, 3, 4, 5, 254, 255]);
    }

    #[test]
    fn summaries_are_one_short_line() {
        assert_eq!(one_line("  a \n\n b\tc  ", 10), "a b c");
        assert_eq!(one_line("abcdefghij", 10), "abcdefghij");
        assert_eq!(one_line("abcdefghijk", 10), "abcdefghi…");
        assert_eq!(one_line("", 10), "");
    }
}
