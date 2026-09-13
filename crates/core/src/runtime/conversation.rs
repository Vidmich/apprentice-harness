//! The conversation of one session as the mentor sees it (task M01-08):
//! the messages so far, the frozen system prompt, the sorted tool set,
//! and the settings of the current run. One per open session, held by
//! the [`AgentRegistry`](super::AgentRegistry); every `agent.run` on
//! the session appends a user turn and runs the loop over it.
//!
//! Cache breakpoints are placed when a request is built, never stored:
//! every system block (the core shared across sessions, then the
//! workspace block; see [`super::prompt`]), the last tool, and the last
//! block of the last user message (SPEC §6). Messages are kept exactly as exchanged —
//! assistant content with its thinking blocks and signatures, tool
//! results as the mentor received them — so a request is the same bytes
//! the API already saw plus the new tail.

use std::fmt::Write as _;

use apprentice_api::types::{RunOptions, Usage};
use serde_json::Value;

use super::prompt::SystemPrompt;
use crate::config::Config;
use crate::mentor::{
    ContentBlock, Effort, MentorRequest, Message, Role, SystemBlock, Thinking, ToolDef,
};
use crate::tools::ToolCall;
use crate::trace::{SessionId, sha256_hex};

/// The user turn appended when the model stopped at `max_tokens`
/// without asking for a tool, once per run.
pub const CONTINUE_MESSAGE: &str = "[continue — output was cut off]";

#[derive(Debug, Clone)]
pub struct Conversation {
    session_id: SessionId,
    messages: Vec<Message>,
    system: Vec<SystemBlock>,
    /// [`super::prompt::PROMPT_VERSION`] of the system blocks, once set.
    prompt_version: Option<String>,
    tools: Vec<ToolDef>,
    /// `None` until the first [`Self::set_tools`].
    tools_hash: Option<String>,
    pub model: String,
    pub effort: Effort,
    pub max_tokens: u32,
    pub thinking: Thinking,
    /// Usage of the last completed call: the size of the context the
    /// next call starts from.
    last_usage: Option<Usage>,
    totals: Usage,
    /// Running cost; `None` once a call was unpriced.
    cost_micros: Option<i64>,
    priced: bool,
    /// Mentor calls of the session so far (every kind).
    calls: u64,
}

impl Conversation {
    /// An empty conversation; the runtime sets the system prompt, the
    /// tools and the settings ([`Self::configure`]) at the first run.
    pub fn new(session_id: SessionId) -> Self {
        Self {
            session_id,
            messages: Vec::new(),
            system: Vec::new(),
            prompt_version: None,
            tools_hash: None,
            tools: Vec::new(),
            model: String::new(),
            effort: Effort::High,
            max_tokens: 4096,
            thinking: Thinking::default(),
            last_usage: None,
            totals: Usage::default(),
            cost_micros: Some(0),
            priced: true,
            calls: 0,
        }
    }

    /// A conversation over an existing history (a resumed session, or
    /// a request body under test). Everything else is set as for
    /// [`Self::new`].
    pub fn load(session_id: SessionId, messages: Vec<Message>) -> Self {
        Self {
            messages,
            ..Self::new(session_id)
        }
    }

    /// Seeds the tool-set hash a resumed session started under, so the
    /// first [`Self::set_tools`] reports whether the set changed.
    #[must_use]
    pub fn with_tools_hash(mut self, hash: Option<String>) -> Self {
        self.tools_hash = hash;
        self
    }

    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    /// Applies `config` and the run's `opts` (model and effort
    /// override the config).
    pub fn configure(&mut self, config: &Config, opts: &RunOptions) {
        let mentor = &config.mentor;
        self.model = opts.model.clone().unwrap_or_else(|| mentor.model.clone());
        self.effort = opts.effort.unwrap_or(mentor.effort);
        self.max_tokens = mentor.max_tokens;
        self.thinking = Thinking::Adaptive {
            display: mentor.thinking_display,
        };
    }

    /// The system prompt, frozen for the session: set once, at the
    /// first run ([`super::prompt::build_system`] assembles it).
    pub fn set_system(&mut self, prompt: SystemPrompt) {
        self.system = prompt.blocks;
        self.prompt_version = Some(prompt.version);
    }

    /// The version of the system prompt in use, once set.
    pub fn prompt_version(&self) -> Option<&str> {
        self.prompt_version.as_deref()
    }

    /// Replaces the tool set. Returns the previous hash when an
    /// earlier set differs (a cache miss for the next call — worth a
    /// warning); `None` for the first set and for the same set again.
    pub fn set_tools(&mut self, tools: Vec<ToolDef>) -> Option<String> {
        let tools = sorted(tools);
        let hash = hash_tools(&tools);
        let old = if self.tools_hash.as_deref() == Some(hash.as_str()) {
            None
        } else {
            self.tools_hash.replace(hash)
        };
        // Always: a loaded conversation knows its hash before its tools.
        self.tools = tools;
        old
    }

    /// SHA-256 of the serialised tool set, once set.
    pub fn tools_hash(&self) -> Option<&str> {
        self.tools_hash.as_deref()
    }

    pub fn tools(&self) -> &[ToolDef] {
        &self.tools
    }

    pub fn system(&self) -> &[SystemBlock] {
        &self.system
    }

    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    pub fn message_count(&self) -> usize {
        self.messages.len()
    }

    /// The last message with its row number in the store (index + 1;
    /// task M01-10 mirrors every message as it is appended).
    pub fn last_row(&self) -> Option<(u64, &Message)> {
        let last = self.messages.last()?;
        Some((self.messages.len() as u64, last))
    }

    /// Seeds the running totals (a resumed session starts from what
    /// the store has).
    pub fn seed_totals(&mut self, totals: Usage, cost_micros: Option<i64>, calls: u64) {
        self.totals = totals;
        self.priced = cost_micros.is_some();
        self.cost_micros = cost_micros;
        self.calls = calls;
    }

    /// Adds a completed call to the totals and remembers it as the
    /// last one.
    pub fn record_usage(&mut self, usage: Usage, cost_micros: Option<i64>) {
        self.last_usage = Some(usage);
        self.calls += 1;
        self.totals.input_tokens += usage.input_tokens;
        self.totals.output_tokens += usage.output_tokens;
        self.totals.cache_read_input_tokens += usage.cache_read_input_tokens;
        self.totals.cache_creation_input_tokens += usage.cache_creation_input_tokens;
        if let (true, Some(c)) = (self.priced, cost_micros) {
            self.cost_micros = Some(self.cost_micros.unwrap_or(0) + c);
        } else {
            self.priced = false;
            self.cost_micros = None;
        }
    }

    pub fn last_usage(&self) -> Option<Usage> {
        self.last_usage
    }

    /// Input tokens the last call consumed — prompt, cache reads and
    /// cache writes together: the size of the context the next call
    /// starts from. `None` before the first call.
    pub fn context_tokens(&self) -> Option<u64> {
        self.last_usage
            .map(|u| u.input_tokens + u.cache_read_input_tokens + u.cache_creation_input_tokens)
    }

    pub fn totals(&self) -> Usage {
        self.totals
    }

    /// Mentor calls of the session so far, the seeded ones included.
    pub fn calls(&self) -> u64 {
        self.calls
    }

    /// Running cost of the session in USD × 1e6; `None` when a call
    /// had no price.
    pub fn cost_micros(&self) -> Option<i64> {
        self.cost_micros
    }

    /// Appends a user text turn. When the last message is already the
    /// user's (the previous run ended before the mentor answered) the
    /// text joins it: the API merges consecutive same-role turns anyway,
    /// and one message keeps the history readable.
    pub fn push_user_text(&mut self, text: impl Into<String>) {
        let block = ContentBlock::text(text);
        match self.messages.last_mut() {
            Some(m) if m.role == Role::User => m.content.push(block),
            _ => self.messages.push(Message {
                role: Role::User,
                content: vec![block],
            }),
        }
    }

    /// Appends the assistant's content, every block verbatim.
    pub fn push_assistant(&mut self, content: Vec<ContentBlock>) {
        self.messages.push(Message::assistant(content));
    }

    /// Appends the tool results of the last assistant message as one
    /// user message, in the order given.
    pub fn push_tool_results(&mut self, results: Vec<ContentBlock>) {
        self.messages.push(Message {
            role: Role::User,
            content: results,
        });
    }

    /// The `tool_use` blocks of the last message when it is the
    /// assistant's and they are not answered yet.
    pub fn pending_tool_uses(&self) -> Vec<ToolCall> {
        match self.messages.last() {
            Some(m) if m.role == Role::Assistant => m
                .content
                .iter()
                .filter_map(|b| match b {
                    ContentBlock::ToolUse {
                        id, name, input, ..
                    } => Some(ToolCall::new(id.clone(), name.clone(), input.clone())),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        }
    }

    /// Drops a trailing assistant message whose `tool_use` blocks have
    /// no results (a run cancelled between the call and its tools), so
    /// the next request is valid. Returns the dropped message.
    pub fn repair(&mut self) -> Option<Message> {
        if self.pending_tool_uses().is_empty() {
            return None;
        }
        self.messages.pop()
    }

    /// Checks the invariants the API enforces: roles alternate (system
    /// messages aside), every `tool_use` is answered by the next message
    /// and every `tool_result` answers one.
    ///
    /// # Errors
    /// A description of the first violation, with the message index.
    pub fn validate(&self) -> Result<(), String> {
        let mut last_role: Option<Role> = None;
        let mut open: Vec<String> = Vec::new();
        for (i, m) in self.messages.iter().enumerate() {
            if m.role != Role::System && last_role == Some(m.role) {
                return Err(format!("message {i}: two {:?} turns in a row", m.role));
            }
            let results: Vec<&str> = m
                .content
                .iter()
                .filter_map(|b| match b {
                    ContentBlock::ToolResult { tool_use_id, .. } => Some(tool_use_id.as_str()),
                    _ => None,
                })
                .collect();
            if !open.is_empty() {
                if m.role != Role::User {
                    return Err(format!(
                        "message {i}: tool_use {} unanswered",
                        open.join(", ")
                    ));
                }
                for id in &open {
                    if !results.contains(&id.as_str()) {
                        return Err(format!("message {i}: no tool_result for {id}"));
                    }
                }
            }
            for id in &results {
                if !open.iter().any(|o| o == id) {
                    return Err(format!("message {i}: tool_result {id} answers nothing"));
                }
            }
            open = m
                .content
                .iter()
                .filter_map(|b| match b {
                    ContentBlock::ToolUse { id, .. } => Some(id.clone()),
                    _ => None,
                })
                .collect();
            if m.role != Role::System {
                last_role = Some(m.role);
            }
        }
        if !open.is_empty() {
            return Err(format!(
                "tool_use {} unanswered at the end",
                open.join(", ")
            ));
        }
        Ok(())
    }

    /// The next request: the frozen prefix (tools, system) with its
    /// breakpoints, then the messages with one on the last block of the
    /// last user message.
    pub fn request(&self) -> MentorRequest {
        let mut messages: Vec<Message> = self
            .messages
            .iter()
            .map(|m| Message {
                role: m.role,
                content: m.content.iter().cloned().map(uncached).collect(),
            })
            .collect();
        if let Some(last) = messages.last_mut()
            && last.role == Role::User
            && let Some(block) = last.content.pop()
        {
            last.content.push(block.cached());
        }
        let mut tools = self.tools.clone();
        for t in &mut tools {
            t.cache = crate::mentor::CacheFlag(false);
        }
        if let Some(t) = tools.last_mut() {
            t.cache = crate::mentor::CacheFlag(true);
        }
        let mut system = self.system.clone();
        for s in &mut system {
            s.cache = crate::mentor::CacheFlag(true);
        }
        MentorRequest {
            model: self.model.clone(),
            max_tokens: self.max_tokens,
            system,
            messages,
            tools,
            thinking: self.thinking,
            effort: self.effort,
            metadata: None,
        }
    }

    /// A one-line summary for logs: `12 messages, 3 tools, 41k context`.
    pub fn describe(&self) -> String {
        let mut s = format!(
            "{} messages, {} tools",
            self.messages.len(),
            self.tools.len()
        );
        if let Some(ctx) = self.context_tokens() {
            let _ = write!(s, ", {}k context", ctx / 1000);
        }
        s
    }
}

fn uncached(block: ContentBlock) -> ContentBlock {
    match block {
        ContentBlock::Text { text, .. } => ContentBlock::text(text),
        ContentBlock::ToolUse {
            id, name, input, ..
        } => ContentBlock::ToolUse {
            id,
            name,
            input,
            cache: crate::mentor::CacheFlag(false),
        },
        ContentBlock::ToolResult {
            tool_use_id,
            content,
            is_error,
            ..
        } => ContentBlock::ToolResult {
            tool_use_id,
            content,
            is_error,
            cache: crate::mentor::CacheFlag(false),
        },
        other => other,
    }
}

fn sorted(mut tools: Vec<ToolDef>) -> Vec<ToolDef> {
    tools.sort_by(|a, b| a.name.cmp(&b.name));
    tools
}

/// SHA-256 of the tool set as it is sent (cache flags excluded), the
/// stable identity of the prefix.
pub fn hash_tools(tools: &[ToolDef]) -> String {
    let plain: Vec<Value> = tools
        .iter()
        .map(|t| {
            serde_json::json!({
                "name": t.name,
                "description": t.description,
                "input_schema": t.input_schema,
            })
        })
        .collect();
    sha256_hex(&serde_json::to_vec(&plain).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::mentor::CacheFlag;

    fn tool(name: &str) -> ToolDef {
        ToolDef {
            name: name.into(),
            description: format!("{name} does things"),
            input_schema: json!({"type": "object"}),
            cache: CacheFlag(false),
        }
    }

    fn conv() -> Conversation {
        let mut c = Conversation::new(SessionId::from("s1"));
        c.set_system(SystemPrompt {
            version: "test_v1".into(),
            blocks: vec![SystemBlock::new("You are"), SystemBlock::new("Rules")],
        });
        assert_eq!(c.prompt_version(), Some("test_v1"));
        assert_eq!(
            c.set_tools(vec![tool("write_file"), tool("read_file")]),
            None
        );
        c.configure(&Config::default(), &RunOptions::default());
        c
    }

    #[test]
    fn requests_carry_the_breakpoints_and_sorted_tools() {
        let mut c = conv();
        c.push_user_text("hello");
        let req = c.request();
        assert_eq!(req.model, "claude-opus-5");
        assert_eq!(req.max_tokens, 64_000);
        let names: Vec<&str> = req.tools.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["read_file", "write_file"]);
        assert_eq!(req.tools[0].cache, CacheFlag(false));
        assert_eq!(req.tools[1].cache, CacheFlag(true));
        assert_eq!(req.system[0].cache, CacheFlag(true));
        assert_eq!(req.system[1].cache, CacheFlag(true));
        assert_eq!(
            req.messages,
            vec![Message {
                role: Role::User,
                content: vec![ContentBlock::text("hello").cached()],
            }]
        );

        // The stored messages carry no flags; the breakpoint moves.
        c.push_assistant(vec![
            ContentBlock::text("reading"),
            ContentBlock::ToolUse {
                id: "t1".into(),
                name: "read_file".into(),
                input: json!({"path": "a"}),
                cache: CacheFlag(false),
            },
        ]);
        c.push_tool_results(vec![ContentBlock::tool_result("t1", "fn main() {}", false)]);
        let req = c.request();
        assert_eq!(req.messages.len(), 3);
        assert_eq!(
            req.messages[0].content,
            vec![ContentBlock::text("hello")],
            "the old breakpoint is gone"
        );
        assert_eq!(
            req.messages[2].content,
            vec![ContentBlock::tool_result("t1", "fn main() {}", false).cached()]
        );
        assert!(c.validate().is_ok());

        // A trailing assistant message gets no breakpoint (pause_turn).
        c.push_assistant(vec![ContentBlock::text("done")]);
        let req = c.request();
        assert_eq!(req.messages[3].content, vec![ContentBlock::text("done")]);
    }

    #[test]
    fn tool_set_changes_are_noticed_by_hash() {
        let mut c = conv();
        let hash = c.tools_hash().unwrap().to_owned();
        assert_eq!(
            c.set_tools(vec![tool("read_file"), tool("write_file")]),
            None
        );
        assert_eq!(c.tools_hash(), Some(hash.as_str()), "order does not matter");
        let old = c.set_tools(vec![tool("read_file")]).expect("changed");
        assert_eq!(old, hash);
        assert_ne!(c.tools_hash(), Some(hash.as_str()));
        assert_eq!(c.tools().len(), 1);
        assert_eq!(Conversation::new(SessionId::from("s2")).tools_hash(), None);

        // A loaded conversation starts from its stored hash: the same
        // set is no change but still the set to send.
        let mut loaded = Conversation::load(SessionId::from("s3"), Vec::new())
            .with_tools_hash(Some(hash.clone()));
        assert!(loaded.tools().is_empty());
        assert_eq!(
            loaded.set_tools(vec![tool("read_file"), tool("write_file")]),
            None
        );
        assert_eq!(loaded.tools().len(), 2);
        let mut loaded = Conversation::load(SessionId::from("s4"), Vec::new())
            .with_tools_hash(Some(hash.clone()));
        assert_eq!(loaded.set_tools(vec![tool("read_file")]), Some(hash));
    }

    #[test]
    fn user_turns_merge_and_unanswered_tool_uses_are_repaired() {
        let mut c = conv();
        c.push_user_text("one");
        c.push_user_text("two");
        assert_eq!(c.message_count(), 1);
        assert_eq!(c.messages()[0].content.len(), 2);
        c.push_assistant(vec![ContentBlock::ToolUse {
            id: "t1".into(),
            name: "read_file".into(),
            input: json!({}),
            cache: CacheFlag(false),
        }]);
        assert_eq!(c.pending_tool_uses().len(), 1);
        assert!(c.validate().unwrap_err().contains("t1 unanswered"));
        assert_eq!(c.last_row().unwrap().0, 2);
        assert!(c.repair().is_some());
        assert!(c.repair().is_none());
        assert_eq!(c.last_row().unwrap().0, 1);
        assert_eq!(c.message_count(), 1);
        assert!(c.validate().is_ok());

        // Wrong results are caught too.
        c.push_assistant(vec![ContentBlock::text("x")]);
        c.push_tool_results(vec![ContentBlock::tool_result("t9", "?", true)]);
        assert!(c.validate().unwrap_err().contains("answers nothing"));
    }

    #[test]
    fn usage_accumulates_and_prices_stop_at_the_first_unpriced_call() {
        let mut c = conv();
        assert_eq!(c.context_tokens(), None);
        let u = Usage {
            input_tokens: 100,
            output_tokens: 10,
            cache_read_input_tokens: 400,
            cache_creation_input_tokens: 50,
        };
        c.record_usage(u, Some(1_000));
        assert_eq!(c.context_tokens(), Some(550));
        assert_eq!(c.cost_micros(), Some(1_000));
        assert_eq!(c.calls(), 1);
        c.record_usage(u, Some(500));
        assert_eq!(c.totals().input_tokens, 200);
        assert_eq!(c.cost_micros(), Some(1_500));
        c.record_usage(u, None);
        assert_eq!(c.cost_micros(), None);
        c.record_usage(u, Some(1));
        assert_eq!(c.cost_micros(), None, "stays unpriced");
        assert_eq!(c.describe(), "0 messages, 2 tools, 0k context");

        let mut d = conv();
        d.seed_totals(u, None, 3);
        d.record_usage(u, Some(1));
        assert_eq!(d.cost_micros(), None);
        assert_eq!(d.totals().output_tokens, 20);
        assert_eq!(d.calls(), 4);
    }
}
