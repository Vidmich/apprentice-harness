//! The system prompt: a file embedded verbatim — no interpolation, so
//! its bytes (and the cache entry they key) never change between runs
//! of one build. M01-09 replaces it with the assembled prompt; the
//! request layout itself lives in [`super::conversation`].

use crate::mentor::SystemBlock;

/// System prompt v0 (replaced in M01-09).
pub const SYSTEM_PROMPT_V0: &str = include_str!("../../prompts/system_v0.md");

/// The frozen system blocks of a new session (breakpoints are placed
/// by the conversation when it builds a request).
pub fn system_blocks() -> Vec<SystemBlock> {
    vec![SystemBlock::new(SYSTEM_PROMPT_V0)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_prompt_is_short_and_frozen() {
        // Roughly 4 characters per token: well under the 300-token budget.
        assert!(SYSTEM_PROMPT_V0.len() < 1200, "{}", SYSTEM_PROMPT_V0.len());
        assert!(SYSTEM_PROMPT_V0.starts_with("You are the mentor model inside apprentice-harness"));
        assert!(!SYSTEM_PROMPT_V0.contains('{'), "no interpolation markers");
        assert_eq!(system_blocks(), vec![SystemBlock::new(SYSTEM_PROMPT_V0)]);
    }
}
