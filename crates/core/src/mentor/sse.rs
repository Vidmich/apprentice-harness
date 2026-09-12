//! Minimal Server-Sent Events parser: `event:` / `data:` lines, blank-line
//! delimited, comments ignored, tolerant of chunk boundaries anywhere.

/// One parsed event.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SseEvent {
    pub event: Option<String>,
    pub data: String,
}

/// Incremental parser. Feed bytes as they arrive; complete events come out.
#[derive(Debug, Default)]
pub struct SseParser {
    buf: Vec<u8>,
    current: SseEvent,
    has_data: bool,
}

impl SseParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Consumes a chunk and returns every event completed by it.
    pub fn push(&mut self, chunk: &[u8]) -> Vec<SseEvent> {
        self.buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        while let Some(nl) = self.buf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.buf.drain(..=nl).collect();
            let line = String::from_utf8_lossy(&line);
            let line = line.trim_end_matches(['\n', '\r']);
            if let Some(ev) = self.line(line) {
                out.push(ev);
            }
        }
        out
    }

    /// Flushes a trailing event that was not terminated by a blank line.
    pub fn finish(&mut self) -> Option<SseEvent> {
        if !self.buf.is_empty() {
            let rest = std::mem::take(&mut self.buf);
            let line = String::from_utf8_lossy(&rest).to_string();
            if let Some(ev) = self.line(line.trim_end_matches(['\n', '\r'])) {
                return Some(ev);
            }
        }
        self.dispatch()
    }

    fn line(&mut self, line: &str) -> Option<SseEvent> {
        if line.is_empty() {
            return self.dispatch();
        }
        if line.starts_with(':') {
            return None;
        }
        let (field, value) = match line.split_once(':') {
            Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
            None => (line, ""),
        };
        match field {
            "event" => self.current.event = Some(value.to_owned()),
            "data" => {
                if self.has_data {
                    self.current.data.push('\n');
                }
                self.current.data.push_str(value);
                self.has_data = true;
            }
            _ => {} // id, retry, unknown: ignored
        }
        None
    }

    fn dispatch(&mut self) -> Option<SseEvent> {
        if !self.has_data && self.current.event.is_none() {
            return None;
        }
        self.has_data = false;
        Some(std::mem::take(&mut self.current))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(event: &str, data: &str) -> SseEvent {
        SseEvent {
            event: Some(event.into()),
            data: data.into(),
        }
    }

    #[test]
    fn parses_whole_and_split_streams() {
        let stream = "event: ping\ndata: {\"type\": \"ping\"}\n\n: comment\nevent: message_stop\r\ndata: {\"a\":1}\r\ndata: {\"b\":2}\r\n\r\n";
        let mut p = SseParser::new();
        let all = p.push(stream.as_bytes());
        assert_eq!(
            all,
            vec![
                ev("ping", "{\"type\": \"ping\"}"),
                ev("message_stop", "{\"a\":1}\n{\"b\":2}")
            ]
        );

        // Byte-by-byte delivery yields the same events.
        let mut p = SseParser::new();
        let mut got = Vec::new();
        for b in stream.as_bytes() {
            got.extend(p.push(&[*b]));
        }
        assert_eq!(got, all);
    }

    #[test]
    fn finish_flushes_unterminated_event() {
        let mut p = SseParser::new();
        assert!(p.push(b"data: tail").is_empty());
        assert_eq!(
            p.finish(),
            Some(SseEvent {
                event: None,
                data: "tail".into()
            })
        );
        assert_eq!(p.finish(), None);
    }

    #[test]
    fn data_without_space_and_empty_data() {
        let mut p = SseParser::new();
        let got = p.push(b"data:x\n\ndata\n\n");
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].data, "x");
        assert_eq!(got[1].data, "");
    }
}
