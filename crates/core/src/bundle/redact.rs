//! The redaction pass: every string of a bundle — row fields, payloads,
//! message content, text blobs — goes through the built-in secret
//! detectors, the user's patterns (`<config_dir>/redact.toml`, the
//! workspace's `.harness/redact.toml`) and, when asked, path redaction.
//! A secret becomes `<REDACTED:kind:n>` with one `n` per distinct value,
//! so structure survives; the report counts matches per rule and never
//! carries a value.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use apprentice_api::types::{RedactionReport, RedactionRule};
use regex::Regex;
use serde::Deserialize;
use serde_json::Value;

use super::BundleError;

/// The file with user patterns, under the config dir and under a
/// workspace's `.harness/`.
pub const REDACT_FILE: &str = "redact.toml";

/// Token for a workspace root under `--redact-paths`.
pub const WS_TOKEN: &str = "<WS>";
/// Token for the home directory under `--redact-paths`.
pub const HOME_TOKEN: &str = "<HOME>";

/// A named group that narrows a match to the secret part.
const SECRET_GROUP: &str = "secret";

/// `redact.toml`: `[[pattern]] name = "…", regex = "…", replace = "…"`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct RedactConfig {
    #[serde(default, rename = "pattern")]
    pub patterns: Vec<PatternSpec>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct PatternSpec {
    pub name: String,
    pub regex: String,
    /// A literal replacement; without it the match becomes
    /// `<REDACTED:name:n>`. A `(?P<secret>…)` group narrows what is
    /// replaced.
    #[serde(default)]
    pub replace: Option<String>,
}

impl RedactConfig {
    /// Reads `<dir>/redact.toml`; `None` when there is none.
    pub fn load(dir: &Path) -> Result<Option<Self>, BundleError> {
        let path = dir.join(REDACT_FILE);
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(BundleError::io("reading", &path, e)),
        };
        toml::from_str(&text)
            .map(Some)
            .map_err(|e| BundleError::Invalid(format!("{}: {e}", path.display())))
    }
}

/// One compiled rule.
#[derive(Debug, Clone)]
pub struct Rule {
    pub name: String,
    /// `builtin`, `user` or `workspace`.
    pub source: &'static str,
    regex: Regex,
    replace: Option<String>,
}

impl Rule {
    fn compile(spec: &PatternSpec, source: &'static str, file: &str) -> Result<Self, BundleError> {
        let regex = Regex::new(&spec.regex).map_err(|e| BundleError::BadPattern {
            name: spec.name.clone(),
            file: file.to_owned(),
            reason: e.to_string(),
        })?;
        if spec.name.is_empty() || spec.name.contains([':', '<', '>']) {
            return Err(BundleError::BadPattern {
                name: spec.name.clone(),
                file: file.to_owned(),
                reason: "the name goes into `<REDACTED:name:n>` (no `:`, `<`, `>`)".to_owned(),
            });
        }
        Ok(Self {
            name: spec.name.clone(),
            source,
            regex,
            replace: spec.replace.clone(),
        })
    }
}

/// The built-in detectors, in the order they run (an Anthropic key is
/// matched before the generic `sk-` form).
fn builtin_rules() -> Vec<Rule> {
    let rule = |name: &str, regex: &str| Rule {
        name: name.to_owned(),
        source: "builtin",
        regex: Regex::new(regex).expect("built-in pattern compiles"),
        replace: None,
    };
    vec![
        rule("anthropic_key", r"sk-ant-[A-Za-z0-9_\-]{20,}"),
        rule("openai_key", r"sk-(?:proj-|svcacct-)?[A-Za-z0-9_\-]{20,}"),
        rule(
            "github_token",
            r"(?:gh[pousr]_[A-Za-z0-9]{36,}|github_pat_[A-Za-z0-9_]{22,})",
        ),
        rule("aws_key", r"\b(?:AKIA|ASIA)[A-Z0-9]{16}\b"),
        rule(
            "bearer",
            r"(?i)bearer\s+(?P<secret>[A-Za-z0-9._~+/=\-]{16,})",
        ),
        rule(
            "pem",
            r"-----BEGIN [A-Z ]*PRIVATE KEY-----[\s\S]*?-----END [A-Z ]*PRIVATE KEY-----",
        ),
        rule(
            "env_secret",
            r"(?m)^[ \t]*(?:export[ \t]+)?[A-Za-z0-9_]*(?:_SECRET|_TOKEN|_KEY|PASSWORD)[A-Za-z0-9_]*[ \t]*=[ \t]*(?P<secret>[^\s#][^\s]*)",
        ),
        rule(
            "password",
            r#"(?i)\b(?:password|passwd|pwd)\b[ \t]*[=:][ \t]*["']?(?P<secret>[^\s"'&,;]{4,})"#,
        ),
    ]
}

/// A text that is not made of JSON string literals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotJson;

/// Blob media types the pass reads as text.
pub fn is_text(media_type: &str) -> bool {
    let m = media_type.trim().to_ascii_lowercase();
    m.starts_with("text/") || m.starts_with("application/json")
}

/// The pass with its state: the tokens given so far and the counts.
#[derive(Debug)]
pub struct Redactor {
    rules: Vec<Rule>,
    /// `(needle, token)`, longest needle first.
    paths: Vec<(String, &'static str)>,
    /// `(rule index, secret)` → `n`.
    seen: HashMap<(usize, String), u64>,
    counts: Vec<u64>,
    path_matches: u64,
}

impl Redactor {
    /// The built-in rules (when `builtins`), then `user` and
    /// `workspace` patterns.
    ///
    /// # Errors
    /// A user pattern that is not a regex.
    pub fn new(
        builtins: bool,
        user: Option<&RedactConfig>,
        workspace: &[(String, RedactConfig)],
    ) -> Result<Self, BundleError> {
        let mut rules = if builtins {
            builtin_rules()
        } else {
            Vec::new()
        };
        if let Some(cfg) = user {
            for spec in &cfg.patterns {
                rules.push(Rule::compile(spec, "user", REDACT_FILE)?);
            }
        }
        for (root, cfg) in workspace {
            let file = format!("{root}/.harness/{REDACT_FILE}");
            for spec in &cfg.patterns {
                rules.push(Rule::compile(spec, "workspace", &file)?);
            }
        }
        let counts = vec![0; rules.len()];
        Ok(Self {
            rules,
            paths: Vec::new(),
            seen: HashMap::new(),
            counts,
            path_matches: 0,
        })
    }

    /// Only the built-in rules.
    pub fn builtin() -> Self {
        Self::new(true, None, &[]).expect("built-in rules compile")
    }

    /// Adds path redaction: every `root` (with `/` or `\`) becomes
    /// `<WS>`, `home` `<HOME>`. Longer paths win over their prefixes.
    #[must_use]
    pub fn with_paths(mut self, roots: &[String], home: Option<&str>) -> Self {
        let mut paths: Vec<(String, &'static str)> = Vec::new();
        let mut add = |p: &str, token: &'static str| {
            let p = p.trim_end_matches(['/', '\\']);
            if p.is_empty() || p.len() < 3 {
                return;
            }
            for v in [p.replace('\\', "/"), p.replace('/', "\\")] {
                if !paths.iter().any(|(n, _)| *n == v) {
                    paths.push((v, token));
                }
            }
        };
        for root in roots {
            add(root, WS_TOKEN);
        }
        if let Some(h) = home {
            add(h, HOME_TOKEN);
        }
        paths.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then(a.0.cmp(&b.0)));
        self.paths = paths;
        self
    }

    pub fn redacts_paths(&self) -> bool {
        !self.paths.is_empty()
    }

    /// `text` with every match replaced; `None` when nothing matched.
    pub fn redact_str(&mut self, text: &str) -> Option<String> {
        let mut out: Option<String> = None;
        for i in 0..self.rules.len() {
            let regex = self.rules[i].regex.clone();
            let current = out.as_deref().unwrap_or(text);
            if !regex.is_match(current) {
                continue;
            }
            let mut n = 0;
            let replaced = regex
                .replace_all(current, |caps: &regex::Captures<'_>| {
                    n += 1;
                    let whole = caps.get(0).expect("a match");
                    let secret = caps.name(SECRET_GROUP).unwrap_or(whole);
                    let prefix = &current[whole.start()..secret.start()];
                    let suffix = &current[secret.end()..whole.end()];
                    let token = self.token(i, secret.as_str());
                    format!("{prefix}{token}{suffix}")
                })
                .into_owned();
            self.counts[i] += n;
            out = Some(replaced);
        }
        if !self.paths.is_empty() {
            let current = out.as_deref().unwrap_or(text);
            let mut replaced = current.to_owned();
            let mut n = 0;
            for (needle, token) in &self.paths {
                let hits = replaced.matches(needle.as_str()).count();
                if hits > 0 {
                    n += hits as u64;
                    replaced = replaced.replace(needle.as_str(), token);
                }
            }
            if n > 0 {
                self.path_matches += n;
                out = Some(replaced);
            }
        }
        out
    }

    /// Redacts every string inside `value` in place; whether any changed.
    pub fn redact_value(&mut self, value: &mut Value) -> bool {
        match value {
            Value::String(s) => match self.redact_str(s) {
                Some(r) => {
                    *s = r;
                    true
                }
                None => false,
            },
            Value::Array(items) => {
                let mut changed = false;
                for v in items {
                    changed |= self.redact_value(v);
                }
                changed
            }
            Value::Object(fields) => {
                let mut changed = false;
                for v in fields.values_mut() {
                    changed |= self.redact_value(v);
                }
                changed
            }
            _ => false,
        }
    }

    /// Redacts an optional string field in place.
    pub fn redact_opt(&mut self, field: &mut Option<String>) -> bool {
        match field {
            Some(s) => self.redact_field(s),
            None => false,
        }
    }

    pub fn redact_field(&mut self, field: &mut String) -> bool {
        match self.redact_str(field) {
            Some(r) => {
                *field = r;
                true
            }
            None => false,
        }
    }

    /// A text blob's redacted bytes, or `None` when nothing changed (or
    /// the blob is not text). JSON is redacted string literal by string
    /// literal ([`Self::redact_json_text`]), so it stays JSON whatever
    /// the patterns match and every other byte stays; other text is
    /// redacted as a whole.
    pub fn redact_blob(&mut self, bytes: &[u8], media_type: &str) -> Option<Vec<u8>> {
        if !is_text(media_type) {
            return None;
        }
        let text = std::str::from_utf8(bytes).ok()?;
        if media_type
            .trim()
            .to_ascii_lowercase()
            .starts_with("application/json")
            && let Ok(out) = self.redact_json_text(text)
        {
            return out.map(String::into_bytes);
        }
        self.redact_str(text).map(String::into_bytes)
    }

    /// Redacts the string literals of a JSON text in place, each
    /// unescaped, redacted and re-escaped; the bytes between them are
    /// untouched (field order, whitespace, numbers). `Err` when the text
    /// is not JSON-shaped (an unterminated string); `Ok(None)` when
    /// nothing matched.
    ///
    /// # Errors
    /// The text is not made of JSON string literals.
    pub fn redact_json_text(&mut self, text: &str) -> Result<Option<String>, NotJson> {
        let bytes = text.as_bytes();
        let mut out = String::new();
        let mut last = 0;
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] != b'"' {
                i += 1;
                continue;
            }
            let start = i;
            i += 1;
            loop {
                match bytes.get(i) {
                    None => return Err(NotJson),
                    Some(b'\\') => i += 2,
                    Some(b'"') => break,
                    Some(_) => i += 1,
                }
            }
            let literal = &text[start..=i];
            if let Ok(s) = serde_json::from_str::<String>(literal)
                && let Some(r) = self.redact_str(&s)
            {
                out.push_str(&text[last..start]);
                out.push_str(&serde_json::to_string(&r).expect("a string serialises"));
                last = i + 1;
            }
            i += 1;
        }
        if last == 0 {
            return Ok(None);
        }
        out.push_str(&text[last..]);
        Ok(Some(out))
    }

    fn token(&mut self, rule: usize, secret: &str) -> String {
        let r = &self.rules[rule];
        if let Some(literal) = &r.replace {
            return literal.clone();
        }
        let next = self.seen.len() as u64 + 1;
        let n = *self.seen.entry((rule, secret.to_owned())).or_insert(next);
        format!("<REDACTED:{}:{n}>", self.rules[rule].name)
    }

    /// The report so far. `blob_map` and `touched_requests` come from
    /// the export that ran the pass.
    pub fn report(
        &self,
        blob_map: BTreeMap<String, String>,
        touched_requests: u64,
    ) -> RedactionReport {
        let mut rules: Vec<RedactionRule> = self
            .rules
            .iter()
            .zip(&self.counts)
            .map(|(r, n)| RedactionRule {
                name: r.name.clone(),
                source: r.source.to_owned(),
                matches: *n,
            })
            .collect();
        if !self.paths.is_empty() {
            rules.push(RedactionRule {
                name: "paths".to_owned(),
                source: "paths".to_owned(),
                matches: self.path_matches,
            });
        }
        let replacements = self.counts.iter().sum::<u64>() + self.path_matches;
        RedactionReport {
            applied: replacements > 0,
            rules,
            replacements,
            secrets: self.seen.len() as u64,
            paths: !self.paths.is_empty(),
            blob_map,
            touched_requests,
            replayable: touched_requests == 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const KEY: &str = "sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123456789";
    const PEM: &str =
        "-----BEGIN RSA PRIVATE KEY-----\nMIIEow\nAB==\n-----END RSA PRIVATE KEY-----";

    #[test]
    fn builtins_replace_secrets_with_numbered_tokens() {
        let mut r = Redactor::builtin();
        let text = format!(
            "key={KEY} again {KEY}\nAuthorization: Bearer abcdefghijklmnop0123\n{PEM}\nAKIAIOSFODNN7EXAMPLE\nDB_PASSWORD=hunter22\nghp_{}\nsk-proj-{}",
            "a".repeat(36),
            "b".repeat(24)
        );
        let out = r.redact_str(&text).unwrap();
        assert_eq!(
            out,
            "key=<REDACTED:anthropic_key:1> again <REDACTED:anthropic_key:1>\nAuthorization: Bearer <REDACTED:bearer:5>\n<REDACTED:pem:6>\n<REDACTED:aws_key:4>\nDB_PASSWORD=<REDACTED:env_secret:7>\n<REDACTED:github_token:3>\n<REDACTED:openai_key:2>"
        );
        assert!(!out.contains(KEY));
        let report = r.report(BTreeMap::new(), 0);
        let by_name: HashMap<_, _> = report
            .rules
            .iter()
            .map(|x| (x.name.as_str(), x.matches))
            .collect();
        assert_eq!(by_name["anthropic_key"], 2);
        assert_eq!(
            by_name["openai_key"], 1,
            "the Anthropic key was taken first"
        );
        assert_eq!(by_name["bearer"], 1);
        assert_eq!(by_name["pem"], 1);
        assert_eq!(report.secrets, 7);
        assert_eq!(report.replacements, 8);
        assert!(report.applied && report.replayable);
        assert!(r.redact_str("nothing here").is_none());
    }

    #[test]
    fn user_patterns_and_paths() {
        let cfg: RedactConfig = toml::from_str(
            r#"
            [[pattern]]
            name = "ticket"
            regex = "ACME-[0-9]+"

            [[pattern]]
            name = "host"
            regex = "(?P<secret>[a-z]+)\\.internal"
            replace = "<INTERNAL>"
            "#,
        )
        .unwrap();
        let mut r = Redactor::new(true, Some(&cfg), &[])
            .unwrap()
            .with_paths(&["C:/src/demo".into()], Some("C:\\Users\\me"));
        let out = r
            .redact_str("ACME-12 at db.internal in C:\\src\\demo\\x.rs and C:/Users/me/.cfg")
            .unwrap();
        assert_eq!(
            out,
            "<REDACTED:ticket:1> at <INTERNAL>.internal in <WS>\\x.rs and <HOME>/.cfg"
        );
        let report = r.report(BTreeMap::new(), 0);
        assert_eq!(report.rules.last().unwrap().name, "paths");
        assert_eq!(report.rules.last().unwrap().matches, 2);
        assert!(report.paths);

        let bad = RedactConfig {
            patterns: vec![PatternSpec {
                name: "x".into(),
                regex: "(".into(),
                replace: None,
            }],
        };
        assert!(matches!(
            Redactor::new(true, Some(&bad), &[]),
            Err(BundleError::BadPattern { .. })
        ));
    }

    #[test]
    fn json_blobs_are_redacted_string_by_string() {
        let mut r = Redactor::builtin();
        let body = format!(
            "{{\"z\": \"use {KEY}\\n{}\", \"a\": 1.50, \"q\": \"say \\\"hi\\\"\"}}",
            PEM.replace('\n', "\\n")
        );
        let out = r.redact_blob(body.as_bytes(), "application/json").unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "{\"z\": \"use <REDACTED:anthropic_key:1>\\n<REDACTED:pem:2>\", \"a\": 1.50, \"q\": \"say \\\"hi\\\"\"}",
            "order, spacing and untouched literals are kept; the PEM matched with real newlines"
        );
        assert_eq!(
            r.redact_str(PEM).unwrap(),
            "<REDACTED:pem:2>",
            "the same secret in raw text gets the same number"
        );
        assert!(r.redact_json_text("\"unterminated").is_err());
        let body =
            serde_json::to_vec(&json!({"messages": [{"content": format!("use {KEY}")}], "n": 1.5}))
                .unwrap();
        let out = r.redact_blob(&body, "application/json").unwrap();
        let v: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(
            v["messages"][0]["content"],
            "use <REDACTED:anthropic_key:1>"
        );
        assert_eq!(v["n"], 1.5);
        assert!(r.redact_blob(b"plain", "text/plain").is_none());
        assert!(r.redact_blob(KEY.as_bytes(), "image/png").is_none());
        assert_eq!(
            r.redact_blob(format!("x {KEY}").as_bytes(), "text/event-stream")
                .unwrap(),
            b"x <REDACTED:anthropic_key:1>"
        );
        let mut v = json!({"a": [KEY, 2], "b": {"c": "ok"}});
        assert!(r.redact_value(&mut v));
        assert_eq!(
            v,
            json!({"a": ["<REDACTED:anthropic_key:1>", 2], "b": {"c": "ok"}})
        );
        assert!(!r.redact_value(&mut json!({"b": "ok"})));
    }

    #[test]
    fn missing_config_is_none() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(RedactConfig::load(dir.path()).unwrap(), None);
        std::fs::write(
            dir.path().join(REDACT_FILE),
            "[[pattern]]\nname = \"a\"\nregex = \"b\"\n",
        )
        .unwrap();
        let cfg = RedactConfig::load(dir.path()).unwrap().unwrap();
        assert_eq!(cfg.patterns[0].name, "a");
        std::fs::write(dir.path().join(REDACT_FILE), "not = [toml").unwrap();
        assert!(matches!(
            RedactConfig::load(dir.path()),
            Err(BundleError::Invalid(_))
        ));
    }
}
