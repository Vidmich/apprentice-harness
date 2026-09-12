//! Request building: the exact layout of every mentor request, in the
//! order that keeps the cached prefix stable across calls (SPEC §6):
//! tools (none yet), the frozen system prompt with a cache breakpoint,
//! then the messages with a breakpoint on the last user block.
//!
//! The system prompt is a file embedded verbatim — no interpolation, so
//! its bytes (and the cache entry they key) never change between runs of
//! one build.

use apprentice_api::types::RunOptions;

use crate::config::Config;
use crate::mentor::{ContentBlock, MentorRequest, Message, Role, SystemBlock, Thinking};

/// System prompt v0 (replaced in M01-09).
pub const SYSTEM_PROMPT_V0: &str = include_str!("../../prompts/system_v0.md");

/// Builds the single-turn request for `prompt`. `opts` override the
/// model and effort from `config`; everything else comes from config.
pub fn build_request(config: &Config, opts: &RunOptions, prompt: &str) -> MentorRequest {
    let mentor = &config.mentor;
    MentorRequest {
        model: opts.model.clone().unwrap_or_else(|| mentor.model.clone()),
        max_tokens: mentor.max_tokens,
        system: vec![SystemBlock::new(SYSTEM_PROMPT_V0).cached()],
        messages: vec![Message {
            role: Role::User,
            // Below the cacheable minimum for a short prompt, but set for
            // uniformity with multi-turn requests (M01).
            content: vec![ContentBlock::text(prompt).cached()],
        }],
        tools: Vec::new(),
        thinking: Thinking::Adaptive {
            display: mentor.thinking_display,
        },
        effort: opts.effort.unwrap_or(mentor.effort),
        metadata: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mentor::{CacheFlag, Effort, ThinkingDisplay};

    #[test]
    fn system_prompt_is_short_and_frozen() {
        // Roughly 4 characters per token: well under the 300-token budget.
        assert!(SYSTEM_PROMPT_V0.len() < 1200, "{}", SYSTEM_PROMPT_V0.len());
        assert!(SYSTEM_PROMPT_V0.starts_with("You are the mentor model inside apprentice-harness"));
        assert!(!SYSTEM_PROMPT_V0.contains('{'), "no interpolation markers");
    }

    #[test]
    fn layout_follows_the_cache_rules() {
        let mut config = Config::default();
        config.mentor.thinking_display = ThinkingDisplay::Omitted;
        config.mentor.max_tokens = 4096;
        let req = build_request(&config, &RunOptions::default(), "hello");
        assert_eq!(req.model, config.mentor.model);
        assert_eq!(req.max_tokens, 4096);
        assert_eq!(req.effort, config.mentor.effort);
        assert!(req.tools.is_empty());
        assert_eq!(req.system.len(), 1);
        assert_eq!(req.system[0].text, SYSTEM_PROMPT_V0);
        assert_eq!(req.system[0].cache, CacheFlag(true));
        assert_eq!(req.messages.len(), 1);
        assert_eq!(req.messages[0].role, Role::User);
        assert_eq!(
            req.messages[0].content,
            vec![ContentBlock::text("hello").cached()]
        );
        assert_eq!(
            req.thinking,
            Thinking::Adaptive {
                display: ThinkingDisplay::Omitted
            }
        );
        assert!(req.metadata.is_none());
    }

    #[test]
    fn options_override_model_and_effort() {
        let config = Config::default();
        let opts = RunOptions {
            model: Some("claude-sonnet-5".into()),
            effort: Some(Effort::Low),
            apprentice: None,
        };
        let req = build_request(&config, &opts, "x");
        assert_eq!(req.model, "claude-sonnet-5");
        assert_eq!(req.effort, Effort::Low);
    }
}
