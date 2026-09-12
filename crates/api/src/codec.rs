//! Newline-delimited JSON framing with a hard line-length limit.

use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};

use crate::jsonrpc::Message;

/// Maximum accepted line length (64 MiB). Large payloads are referenced by
/// blob id, never inlined, so real messages are far smaller.
pub const MAX_LINE_BYTES: usize = 64 * 1024 * 1024;

/// Outcome of reading one line.
#[derive(Debug)]
pub enum Frame {
    /// A complete line (without the trailing newline).
    Line(Vec<u8>),
    /// A line exceeded [`MAX_LINE_BYTES`]; the rest of it has been discarded.
    TooLong { bytes_discarded: usize },
    /// The peer closed the connection.
    Eof,
}

/// Reads one `\n`-terminated line, enforcing the size limit. Lines longer
/// than the limit are consumed and discarded so the stream stays in sync.
///
/// # Errors
/// Propagates I/O errors from the reader.
pub async fn read_frame<R: AsyncBufRead + Unpin>(reader: &mut R) -> std::io::Result<Frame> {
    let mut buf: Vec<u8> = Vec::new();
    let mut discarding = false;
    let mut discarded = 0usize;
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            // EOF
            if discarding {
                return Ok(Frame::TooLong {
                    bytes_discarded: discarded,
                });
            }
            if buf.is_empty() {
                return Ok(Frame::Eof);
            }
            // Final line without newline.
            return Ok(Frame::Line(buf));
        }
        let (chunk, done) = match available.iter().position(|&b| b == b'\n') {
            Some(i) => (&available[..i], true),
            None => (available, false),
        };
        let consume = chunk.len() + usize::from(done);
        if discarding {
            discarded += chunk.len();
        } else if buf.len() + chunk.len() > MAX_LINE_BYTES {
            discarding = true;
            discarded = buf.len() + chunk.len();
            buf = Vec::new();
        } else {
            buf.extend_from_slice(chunk);
        }
        reader.consume(consume);
        if done {
            return Ok(if discarding {
                Frame::TooLong {
                    bytes_discarded: discarded,
                }
            } else {
                Frame::Line(strip_cr(buf))
            });
        }
    }
}

fn strip_cr(mut line: Vec<u8>) -> Vec<u8> {
    if line.last() == Some(&b'\r') {
        line.pop();
    }
    line
}

/// Serialises a message as one line and flushes it.
///
/// # Errors
/// Propagates I/O errors from the writer.
pub async fn write_message<W: AsyncWrite + Unpin>(
    writer: &mut W,
    message: &Message,
) -> std::io::Result<()> {
    let mut bytes = serde_json::to_vec(message).map_err(std::io::Error::other)?;
    bytes.push(b'\n');
    writer.write_all(&bytes).await?;
    writer.flush().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::BufReader;

    #[tokio::test]
    async fn reads_lines_and_eof() {
        let data = b"one\r\ntwo\nthree";
        let mut r = BufReader::new(&data[..]);
        assert!(matches!(read_frame(&mut r).await.unwrap(), Frame::Line(l) if l == b"one"));
        assert!(matches!(read_frame(&mut r).await.unwrap(), Frame::Line(l) if l == b"two"));
        assert!(matches!(read_frame(&mut r).await.unwrap(), Frame::Line(l) if l == b"three"));
        assert!(matches!(read_frame(&mut r).await.unwrap(), Frame::Eof));
    }

    #[tokio::test]
    async fn oversized_line_is_discarded_and_stream_resyncs() {
        // Use a tiny reader buffer so the long line arrives in many chunks.
        let mut data = vec![b'x'; MAX_LINE_BYTES + 10];
        data.push(b'\n');
        data.extend_from_slice(b"ok\n");
        let mut r = BufReader::with_capacity(4096, &data[..]);
        match read_frame(&mut r).await.unwrap() {
            Frame::TooLong { bytes_discarded } => assert_eq!(bytes_discarded, MAX_LINE_BYTES + 10),
            other => panic!("expected TooLong, got {other:?}"),
        }
        assert!(matches!(read_frame(&mut r).await.unwrap(), Frame::Line(l) if l == b"ok"));
    }

    #[tokio::test]
    #[allow(clippy::naive_bytecount)]
    async fn write_appends_newline() {
        let mut out = Vec::new();
        let msg = Message::notification("event", None);
        write_message(&mut out, &msg).await.unwrap();
        assert!(out.ends_with(b"\n"));
        assert_eq!(out.iter().filter(|&&b| b == b'\n').count(), 1);
    }
}
