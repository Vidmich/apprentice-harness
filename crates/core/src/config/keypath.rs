//! Dotted key paths such as `mentor.model` or `pricing."my.model".input`.

use super::error::ConfigError;

/// Splits a dotted key into segments. Segments may be double-quoted to
/// contain dots; there is no other escaping.
pub fn parse(key: &str) -> Result<Vec<String>, ConfigError> {
    let bad = |reason| ConfigError::BadKey {
        key: key.to_owned(),
        reason,
    };
    if key.is_empty() {
        return Err(bad("empty key"));
    }
    let mut segments = Vec::new();
    let mut current = String::new();
    let mut chars = key.chars().peekable();
    let mut quoted = false;
    while let Some(c) = chars.next() {
        match c {
            '"' if current.is_empty() && !quoted => {
                quoted = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some(c) => current.push(c),
                        None => return Err(bad("unterminated quote")),
                    }
                }
                match chars.peek() {
                    None | Some('.') => {}
                    Some(_) => return Err(bad("text after closing quote")),
                }
            }
            '.' => {
                if current.is_empty() && !quoted {
                    return Err(bad("empty segment"));
                }
                segments.push(std::mem::take(&mut current));
                quoted = false;
            }
            '"' => return Err(bad("quote inside a bare segment")),
            c if c.is_whitespace() => return Err(bad("whitespace in key")),
            c => current.push(c),
        }
    }
    if current.is_empty() && !quoted {
        return Err(bad("empty segment"));
    }
    segments.push(current);
    Ok(segments)
}

/// Joins segments back into a dotted key, quoting segments that need it.
pub fn format(segments: &[String]) -> String {
    let mut out = String::new();
    for (i, s) in segments.iter().enumerate() {
        if i > 0 {
            out.push('.');
        }
        if s.is_empty() || s.contains(['.', '"']) || s.chars().any(char::is_whitespace) {
            out.push('"');
            out.push_str(s);
            out.push('"');
        } else {
            out.push_str(s);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_and_quoted() {
        assert_eq!(parse("mentor.model").unwrap(), ["mentor", "model"]);
        assert_eq!(
            parse(r#"pricing."my.model".input"#).unwrap(),
            ["pricing", "my.model", "input"]
        );
        assert_eq!(parse("a").unwrap(), ["a"]);
    }

    #[test]
    fn rejects_malformed() {
        for bad in [
            "", ".", "a.", ".a", "a..b", "a b", r#"a"b"#, r#""a"#, r#""a"b"#,
        ] {
            assert!(parse(bad).is_err(), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn format_round_trips() {
        for k in ["mentor.model", r#"pricing."my.model".input"#] {
            assert_eq!(format(&parse(k).unwrap()), k);
        }
    }
}
