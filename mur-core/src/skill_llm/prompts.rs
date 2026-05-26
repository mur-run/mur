//! Prompt templates for skill-maintenance LLM checks.
//! Versioned so future revisions invalidate the cache (cache key hashes the
//! prompt body → any change auto-invalidates).

pub const API_DRIFT_V1: &str = r#"You are a skill-maintenance assistant. Decide whether a skill's procedure still matches recent observed tool usage.

## Skill procedure
{procedure}

## Recent traces (last {trace_count} executions)
{trace_summary}

## Output (JSON only, no prose)
{
  "verdict": "aligned" | "drifted" | "unknown",
  "evidence": "one short sentence",
  "drifted_steps": [<step indices, only if verdict == drifted>]
}
"#;

pub const API_DRIFT_VERSION: u32 = 1;

pub const COVERAGE_GAP_V1: &str = r#"You are a skill-maintenance assistant. Given a cluster of repeated failures, determine whether an existing skill should be extended or a new skill is needed.

## Failure cluster ({count} occurrences)
{error_signature}

## Sample failed steps
{sample_steps}

## Existing skills (name + abstract)
{skill_inventory}

## Output (JSON only)
{
  "recommendation": "extend" | "new" | "ignore",
  "target_skill": "<existing skill name if recommendation==extend>",
  "suggested_step": "<one-sentence step description if recommendation==extend or new>",
  "rationale": "<one sentence>"
}
"#;

pub const COVERAGE_GAP_VERSION: u32 = 1;

pub const CONTRADICTION_ADJUDICATE_V1: &str = r#"You are a skill-maintenance assistant. Two skills appear to overlap. Decide whether they contradict (one is wrong or duplicates the other) or coexist (they cover different cases).

## Skill A: {name_a}
{procedure_a}

## Skill B: {name_b}
{procedure_b}

## Reported overlap
{overlap_summary}

## Output (JSON only)
{
  "verdict": "contradict" | "coexist" | "duplicate",
  "rationale": "<one sentence>"
}
"#;

pub const CONTRADICTION_ADJUDICATE_VERSION: u32 = 1;
