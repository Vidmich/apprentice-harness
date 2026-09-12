//! What a file's bytes are: binary or text, which encoding, which BOM,
//! which line endings. Decoding is for showing and editing; encoding
//! puts a BOM back so an edit leaves those bytes as they were.

use std::path::Path;

/// Bytes sniffed for a NUL byte (and a magic number).
pub(crate) const SNIFF_BYTES: usize = 8 * 1024;

/// A byte-order mark found at the start of the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Bom {
    None,
    Utf8,
    Utf16Le,
    Utf16Be,
}

impl Bom {
    fn detect(bytes: &[u8]) -> Self {
        if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
            Self::Utf8
        } else if bytes.starts_with(&[0xFF, 0xFE]) {
            Self::Utf16Le
        } else if bytes.starts_with(&[0xFE, 0xFF]) {
            Self::Utf16Be
        } else {
            Self::None
        }
    }

    pub(crate) fn bytes(self) -> &'static [u8] {
        match self {
            Self::None => &[],
            Self::Utf8 => &[0xEF, 0xBB, 0xBF],
            Self::Utf16Le => &[0xFF, 0xFE],
            Self::Utf16Be => &[0xFE, 0xFF],
        }
    }
}

/// How the text was obtained from the bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Encoding {
    Utf8,
    /// Invalid UTF-8 sequences became U+FFFD.
    Utf8Lossy,
    Utf16Le,
    Utf16Be,
}

impl Encoding {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Utf8 => "utf-8",
            Self::Utf8Lossy => "utf-8-lossy",
            Self::Utf16Le => "utf-16le",
            Self::Utf16Be => "utf-16be",
        }
    }

    /// Text that can be written back byte-for-byte as UTF-8.
    pub(crate) fn is_exact_utf8(self) -> bool {
        self == Self::Utf8
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LineEnding {
    /// No line break at all.
    None,
    Lf,
    CrLf,
    /// Both kinds.
    Mixed,
}

impl LineEnding {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Lf => "lf",
            Self::CrLf => "crlf",
            Self::Mixed => "mixed",
        }
    }
}

/// A file decoded for display or editing.
#[derive(Debug, Clone)]
pub(crate) struct Decoded {
    /// Without the BOM.
    pub text: String,
    pub bom: Bom,
    pub encoding: Encoding,
    pub eol: LineEnding,
}

/// A NUL byte in the first [`SNIFF_BYTES`] means binary; UTF-16 with a
/// BOM is text despite its NULs.
pub(crate) fn is_binary(bytes: &[u8]) -> bool {
    if matches!(Bom::detect(bytes), Bom::Utf16Le | Bom::Utf16Be) {
        return false;
    }
    bytes[..bytes.len().min(SNIFF_BYTES)].contains(&0)
}

/// A media type for a binary file: by magic number, else by extension,
/// else `application/octet-stream`.
pub(crate) fn sniff_type(bytes: &[u8], path: &Path) -> &'static str {
    const MAGIC: &[(&[u8], &str)] = &[
        (b"\x89PNG\r\n\x1a\n", "image/png"),
        (b"\xFF\xD8\xFF", "image/jpeg"),
        (b"GIF87a", "image/gif"),
        (b"GIF89a", "image/gif"),
        (b"%PDF-", "application/pdf"),
        (b"PK\x03\x04", "application/zip"),
        (b"\x1F\x8B", "application/gzip"),
        (b"\x7FELF", "application/x-elf"),
        (b"MZ", "application/x-msdownload"),
        (b"SQLite format 3\0", "application/vnd.sqlite3"),
        (b"\0asm", "application/wasm"),
        (b"OggS", "audio/ogg"),
        (b"RIFF", "audio/x-riff"),
        (b"\xCA\xFE\xBA\xBE", "application/java-vm"),
    ];
    if let Some((_, t)) = MAGIC.iter().find(|(m, _)| bytes.starts_with(m)) {
        return t;
    }
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    match ext.as_deref() {
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("ico") => "image/x-icon",
        Some("pdf") => "application/pdf",
        Some("zip" | "jar") => "application/zip",
        Some("gz" | "tgz") => "application/gzip",
        Some("exe" | "dll") => "application/x-msdownload",
        Some("so" | "dylib" | "o" | "a") => "application/x-sharedlib",
        Some("wasm") => "application/wasm",
        Some("sqlite" | "db") => "application/vnd.sqlite3",
        Some("woff" | "woff2" | "ttf" | "otf") => "font/binary",
        Some("mp3") => "audio/mpeg",
        Some("mp4") => "video/mp4",
        _ => "application/octet-stream",
    }
}

/// Decodes text bytes (see [`is_binary`] first).
pub(crate) fn decode(bytes: &[u8]) -> Decoded {
    let bom = Bom::detect(bytes);
    let body = &bytes[bom.bytes().len()..];
    let (text, encoding) = match bom {
        Bom::Utf16Le | Bom::Utf16Be => {
            let units = body.chunks(2).map(|pair| {
                let (a, b) = (pair[0], *pair.get(1).unwrap_or(&0));
                if bom == Bom::Utf16Le {
                    u16::from_le_bytes([a, b])
                } else {
                    u16::from_be_bytes([a, b])
                }
            });
            let text: String = char::decode_utf16(units)
                .map(|r| r.unwrap_or(char::REPLACEMENT_CHARACTER))
                .collect();
            let encoding = if bom == Bom::Utf16Le {
                Encoding::Utf16Le
            } else {
                Encoding::Utf16Be
            };
            (text, encoding)
        }
        Bom::Utf8 | Bom::None => match std::str::from_utf8(body) {
            Ok(s) => (s.to_owned(), Encoding::Utf8),
            Err(_) => (
                String::from_utf8_lossy(body).into_owned(),
                Encoding::Utf8Lossy,
            ),
        },
    };
    let eol = line_ending(&text);
    Decoded {
        text,
        bom,
        encoding,
        eol,
    }
}

/// The file's line-ending convention.
pub(crate) fn line_ending(text: &str) -> LineEnding {
    let mut crlf = 0usize;
    let mut lf = 0usize;
    let bytes = text.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'\n' {
            if i > 0 && bytes[i - 1] == b'\r' {
                crlf += 1;
            } else {
                lf += 1;
            }
        }
    }
    match (crlf, lf) {
        (0, 0) => LineEnding::None,
        (_, 0) => LineEnding::CrLf,
        (0, _) => LineEnding::Lf,
        _ => LineEnding::Mixed,
    }
}

/// Lines as `split_inclusive('\n')` counts them: `"a\nb"` and `"a\nb\n"`
/// are both two lines, `""` is none.
pub(crate) fn count_lines(text: &str) -> usize {
    text.split_inclusive('\n').count()
}

/// The 1-based line number at which byte `offset` lies.
pub(crate) fn line_of(text: &str, offset: usize) -> usize {
    text[..offset.min(text.len())].matches('\n').count() + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binary_sniffing() {
        assert!(!is_binary(b"plain text\n"));
        assert!(is_binary(b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR"));
        assert!(!is_binary(b"\xFF\xFEh\0i\0"));
        let mut late = vec![b'a'; SNIFF_BYTES];
        late.push(0);
        assert!(!is_binary(&late));
        assert_eq!(
            sniff_type(b"\x89PNG\r\n\x1a\n", Path::new("x.bin")),
            "image/png"
        );
        assert_eq!(
            sniff_type(b"\0\0\0", Path::new("x.wasm")),
            "application/wasm"
        );
        assert_eq!(
            sniff_type(b"\0\0\0", Path::new("x")),
            "application/octet-stream"
        );
    }

    #[test]
    fn decoding_handles_boms_and_lossy_input() {
        let d = decode(b"\xEF\xBB\xBFhi\r\nthere\r\n");
        assert_eq!(d.text, "hi\r\nthere\r\n");
        assert_eq!(d.bom, Bom::Utf8);
        assert_eq!(d.encoding, Encoding::Utf8);
        assert_eq!(d.eol, LineEnding::CrLf);

        let d = decode(b"\xFF\xFEh\0i\0\n\0");
        assert_eq!(d.text, "hi\n");
        assert_eq!(d.encoding, Encoding::Utf16Le);
        let d = decode(b"\xFE\xFF\0h\0i");
        assert_eq!(d.text, "hi");
        assert_eq!(d.encoding, Encoding::Utf16Be);

        let d = decode(b"ok \xFF bad\n");
        assert_eq!(d.text, "ok \u{FFFD} bad\n");
        assert_eq!(d.encoding, Encoding::Utf8Lossy);
        assert!(!d.encoding.is_exact_utf8());
    }

    #[test]
    fn line_endings_and_counts() {
        assert_eq!(line_ending(""), LineEnding::None);
        assert_eq!(line_ending("a"), LineEnding::None);
        assert_eq!(line_ending("a\nb\n"), LineEnding::Lf);
        assert_eq!(line_ending("a\r\nb\r\n"), LineEnding::CrLf);
        assert_eq!(line_ending("a\r\nb\n"), LineEnding::Mixed);
        assert_eq!(count_lines(""), 0);
        assert_eq!(count_lines("a"), 1);
        assert_eq!(count_lines("a\n"), 1);
        assert_eq!(count_lines("a\nb"), 2);
        assert_eq!(line_of("a\nb\nc", 0), 1);
        assert_eq!(line_of("a\nb\nc", 2), 2);
        assert_eq!(line_of("a\nb\nc", 4), 3);
    }
}
