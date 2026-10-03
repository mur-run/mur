//! Code-search tool tuning (`search:`). Today only `search.ast_grep`, which
//! bounds the `ast_grep_search` MCP tool (plan 2026-10-03 code-nav, phase 1).
//!
//! Every limit resolves in three layers: the value the agent asks for in a
//! call, the user's config value, and a compile-time `HARD_MAX_*` ceiling.
//! The effective value is the smallest of the three, so neither an agent nor
//! a config file can lift a limit past the ceiling. The ceilings exist
//! because ast-grep's JSON output is quadratic on long lines (one minified
//! line with 5k matches emits ~127 MB), so an unbounded cap is a memory bomb.

use super::*;

/// Default number of matches returned per call.
pub const AST_GREP_DEFAULT_MAX_RESULTS: u32 = 200;
/// Default context lines around each match (ast-grep `--context`).
pub const AST_GREP_DEFAULT_MAX_CONTEXT_LINES: u32 = 3;
/// Default wall-clock budget for one ast-grep run.
pub const AST_GREP_DEFAULT_TIMEOUT_SECS: u64 = 30;
/// Default cumulative stdout bytes read before the child is killed.
pub const AST_GREP_DEFAULT_MAX_OUTPUT_BYTES: u64 = 1024 * 1024;
/// Default bytes kept of each match's `lines` text before truncation.
pub const AST_GREP_DEFAULT_MAX_MATCH_BYTES: u64 = 2 * 1024;

/// Ceiling on `max_results`, whatever the agent or config asks for.
pub const AST_GREP_HARD_MAX_RESULTS: u32 = 2_000;
/// Ceiling on `max_context_lines`.
pub const AST_GREP_HARD_MAX_CONTEXT_LINES: u32 = 20;
/// Ceiling on `timeout_secs`.
pub const AST_GREP_HARD_MAX_TIMEOUT_SECS: u64 = 300;
/// Ceiling on `max_output_bytes`. Required, not defensive: see module docs.
pub const AST_GREP_HARD_MAX_OUTPUT_BYTES: u64 = 16 * 1024 * 1024;
/// Ceiling on `max_match_bytes`.
pub const AST_GREP_HARD_MAX_MATCH_BYTES: u64 = 64 * 1024;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchConfig {
    #[serde(default)]
    pub ast_grep: AstGrepConfig,
}

/// User-tunable bounds for `ast_grep_search`. Each field defaults on its own,
/// so a partial `search.ast_grep:` block keeps the defaults it omits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AstGrepConfig {
    #[serde(default = "ast_grep_default_max_results")]
    pub max_results: u32,
    #[serde(default = "ast_grep_default_max_context_lines")]
    pub max_context_lines: u32,
    #[serde(default = "ast_grep_default_timeout_secs")]
    pub timeout_secs: u64,
    #[serde(default = "ast_grep_default_max_output_bytes")]
    pub max_output_bytes: u64,
    #[serde(default = "ast_grep_default_max_match_bytes")]
    pub max_match_bytes: u64,
}

fn ast_grep_default_max_results() -> u32 {
    AST_GREP_DEFAULT_MAX_RESULTS
}
fn ast_grep_default_max_context_lines() -> u32 {
    AST_GREP_DEFAULT_MAX_CONTEXT_LINES
}
fn ast_grep_default_timeout_secs() -> u64 {
    AST_GREP_DEFAULT_TIMEOUT_SECS
}
fn ast_grep_default_max_output_bytes() -> u64 {
    AST_GREP_DEFAULT_MAX_OUTPUT_BYTES
}
fn ast_grep_default_max_match_bytes() -> u64 {
    AST_GREP_DEFAULT_MAX_MATCH_BYTES
}

impl Default for AstGrepConfig {
    fn default() -> Self {
        Self {
            max_results: AST_GREP_DEFAULT_MAX_RESULTS,
            max_context_lines: AST_GREP_DEFAULT_MAX_CONTEXT_LINES,
            timeout_secs: AST_GREP_DEFAULT_TIMEOUT_SECS,
            max_output_bytes: AST_GREP_DEFAULT_MAX_OUTPUT_BYTES,
            max_match_bytes: AST_GREP_DEFAULT_MAX_MATCH_BYTES,
        }
    }
}

/// The bounds one `ast_grep_search` call actually runs under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AstGrepLimits {
    pub max_results: u32,
    pub context_lines: u32,
    pub timeout_secs: u64,
    pub max_output_bytes: u64,
    pub max_match_bytes: u64,
}

/// `min(requested, configured, hard)`, floored at `floor`. A missing request
/// means "use the configured value". The floor keeps a zero in config from
/// turning the tool into a silent no-op (zero results, zero timeout).
fn clamp<T: Ord + Copy>(requested: Option<T>, configured: T, hard: T, floor: T) -> T {
    let cap = configured.min(hard);
    requested.map_or(cap, |r| r.min(cap)).max(floor)
}

impl AstGrepConfig {
    /// Resolve the per-call bounds. Only `max_results` and `context` are
    /// agent-selectable; the time and byte caps are config-and-ceiling only,
    /// because they are the guards against the agent's own input.
    pub fn resolve(&self, max_results: Option<u32>, context: Option<u32>) -> AstGrepLimits {
        AstGrepLimits {
            max_results: clamp(max_results, self.max_results, AST_GREP_HARD_MAX_RESULTS, 1),
            context_lines: clamp(
                context,
                self.max_context_lines,
                AST_GREP_HARD_MAX_CONTEXT_LINES,
                0,
            ),
            timeout_secs: clamp(None, self.timeout_secs, AST_GREP_HARD_MAX_TIMEOUT_SECS, 1),
            max_output_bytes: clamp(
                None,
                self.max_output_bytes,
                AST_GREP_HARD_MAX_OUTPUT_BYTES,
                1,
            ),
            max_match_bytes: clamp(None, self.max_match_bytes, AST_GREP_HARD_MAX_MATCH_BYTES, 1),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_without_search_block_uses_defaults() {
        let c: Config = serde_yaml::from_str("llm: {}\n").unwrap();
        assert_eq!(c.search.ast_grep, AstGrepConfig::default());
    }

    #[test]
    fn partial_ast_grep_block_keeps_omitted_defaults() {
        let c: Config =
            serde_yaml::from_str("search:\n  ast_grep:\n    max_results: 50\n").unwrap();
        let g = &c.search.ast_grep;
        assert_eq!(g.max_results, 50);
        assert_eq!(g.max_context_lines, AST_GREP_DEFAULT_MAX_CONTEXT_LINES);
        assert_eq!(g.timeout_secs, AST_GREP_DEFAULT_TIMEOUT_SECS);
        assert_eq!(g.max_output_bytes, AST_GREP_DEFAULT_MAX_OUTPUT_BYTES);
        assert_eq!(g.max_match_bytes, AST_GREP_DEFAULT_MAX_MATCH_BYTES);
    }

    #[test]
    fn defaults_sit_under_ceilings() {
        let l = AstGrepConfig::default().resolve(None, None);
        assert_eq!(l.max_results, AST_GREP_DEFAULT_MAX_RESULTS);
        assert_eq!(l.context_lines, AST_GREP_DEFAULT_MAX_CONTEXT_LINES);
        assert_eq!(l.timeout_secs, AST_GREP_DEFAULT_TIMEOUT_SECS);
        assert_eq!(l.max_output_bytes, AST_GREP_DEFAULT_MAX_OUTPUT_BYTES);
        assert_eq!(l.max_match_bytes, AST_GREP_DEFAULT_MAX_MATCH_BYTES);
    }

    #[test]
    fn agent_request_cannot_exceed_config() {
        let l = AstGrepConfig::default().resolve(Some(u32::MAX), Some(u32::MAX));
        assert_eq!(l.max_results, AST_GREP_DEFAULT_MAX_RESULTS);
        assert_eq!(l.context_lines, AST_GREP_DEFAULT_MAX_CONTEXT_LINES);
    }

    #[test]
    fn agent_request_below_config_wins() {
        let l = AstGrepConfig::default().resolve(Some(5), Some(0));
        assert_eq!((l.max_results, l.context_lines), (5, 0));
    }

    #[test]
    fn config_cannot_exceed_hard_ceiling() {
        let c = AstGrepConfig {
            max_results: u32::MAX,
            max_context_lines: u32::MAX,
            timeout_secs: u64::MAX,
            max_output_bytes: u64::MAX,
            max_match_bytes: u64::MAX,
        };
        let l = c.resolve(None, None);
        assert_eq!(l.max_results, AST_GREP_HARD_MAX_RESULTS);
        assert_eq!(l.context_lines, AST_GREP_HARD_MAX_CONTEXT_LINES);
        assert_eq!(l.timeout_secs, AST_GREP_HARD_MAX_TIMEOUT_SECS);
        assert_eq!(l.max_output_bytes, AST_GREP_HARD_MAX_OUTPUT_BYTES);
        assert_eq!(l.max_match_bytes, AST_GREP_HARD_MAX_MATCH_BYTES);
    }

    #[test]
    fn zero_config_is_floored_not_a_silent_noop() {
        let c = AstGrepConfig {
            max_results: 0,
            max_context_lines: 0,
            timeout_secs: 0,
            max_output_bytes: 0,
            max_match_bytes: 0,
        };
        let l = c.resolve(Some(0), None);
        assert_eq!(l.max_results, 1);
        assert_eq!(l.context_lines, 0);
        assert_eq!(l.timeout_secs, 1);
        assert_eq!(l.max_output_bytes, 1);
        assert_eq!(l.max_match_bytes, 1);
    }
}
