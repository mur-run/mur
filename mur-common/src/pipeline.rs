//! Pipeline expression AST and parser.
//!
//! Supports three composition operators with precedence (high → low):
//! - `|`  Pipe — pass output of left as input to right
//! - `&&` Sequential — run right only if left succeeds (exit code 0)
//! - `,`  Parallel — run all branches concurrently

use serde::{Deserialize, Serialize};

/// Pipeline expression AST.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum PipelineExpr {
    /// A single workflow reference (by name or ID).
    Single(String),
    /// `w1 | w2` — pipe output of left into right.
    Pipe(Box<PipelineExpr>, Box<PipelineExpr>),
    /// `w1 && w2` — run right only if left succeeds.
    Sequential(Box<PipelineExpr>, Box<PipelineExpr>),
    /// `w1, w2, w3` — run all concurrently.
    Parallel(Vec<PipelineExpr>),
}

/// Status of a pipeline stage execution.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PipelineStatus {
    Success,
    Failed,
    Skipped,
}

/// Output produced by a single pipeline stage.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineOutput {
    pub workflow_id: String,
    pub status: PipelineStatus,
    pub output_text: Option<String>,
    pub output_data: Option<serde_json::Value>,
    pub exit_code: i32,
    pub duration_ms: u64,
}

/// Replace `{{input}}` placeholders in a template string with the given value.
/// If `input` is `None`, replaces with empty string.
pub fn inject_input(template: &str, input: Option<&str>) -> String {
    let replacement = input.unwrap_or("");
    template.replace("{{input}}", replacement)
}

/// Returns `true` if the input string contains pipeline operators.
pub fn has_pipeline_syntax(input: &str) -> bool {
    // Check for `|` (but not inside workflow names), `&&`, or `,`
    input.contains(" | ") || input.contains(" && ") || input.contains(", ") || input.contains(',')
}

/// Parse a pipeline expression string into an AST.
///
/// Operator precedence (high → low): `|` > `&&` > `,`
///
/// Parsing strategy: split by lowest-precedence operator first (`,`),
/// then by `&&`, then by `|`.
pub fn parse_pipeline_expr(input: &str) -> Result<PipelineExpr, PipelineParseError> {
    let input = input.trim();
    if input.is_empty() {
        return Err(PipelineParseError::EmptyInput);
    }
    parse_parallel(input)
}

/// Errors from pipeline expression parsing.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum PipelineParseError {
    #[error("empty pipeline expression")]
    EmptyInput,
    #[error("empty segment in pipeline expression")]
    EmptySegment,
}

/// Parse comma-separated parallel branches (lowest precedence).
fn parse_parallel(input: &str) -> Result<PipelineExpr, PipelineParseError> {
    let parts: Vec<&str> = input.split(',').collect();
    if parts.len() == 1 {
        return parse_sequential(input);
    }

    let mut exprs = Vec::with_capacity(parts.len());
    for part in parts {
        let part = part.trim();
        if part.is_empty() {
            return Err(PipelineParseError::EmptySegment);
        }
        exprs.push(parse_sequential(part)?);
    }

    if exprs.len() == 1 {
        Ok(exprs.into_iter().next().unwrap())
    } else {
        Ok(PipelineExpr::Parallel(exprs))
    }
}

/// Parse `&&`-separated sequential chains (middle precedence).
fn parse_sequential(input: &str) -> Result<PipelineExpr, PipelineParseError> {
    let parts: Vec<&str> = input.split("&&").collect();
    if parts.len() == 1 {
        return parse_pipe(input);
    }

    let mut iter = parts.into_iter();
    let first = iter.next().unwrap().trim();
    if first.is_empty() {
        return Err(PipelineParseError::EmptySegment);
    }
    let mut expr = parse_pipe(first)?;

    for part in iter {
        let part = part.trim();
        if part.is_empty() {
            return Err(PipelineParseError::EmptySegment);
        }
        let right = parse_pipe(part)?;
        expr = PipelineExpr::Sequential(Box::new(expr), Box::new(right));
    }

    Ok(expr)
}

/// Parse `|`-separated pipe chains (highest precedence).
fn parse_pipe(input: &str) -> Result<PipelineExpr, PipelineParseError> {
    let parts: Vec<&str> = input.split('|').collect();
    if parts.len() == 1 {
        return parse_single(input);
    }

    let mut iter = parts.into_iter();
    let first = iter.next().unwrap().trim();
    if first.is_empty() {
        return Err(PipelineParseError::EmptySegment);
    }
    let mut expr = parse_single(first)?;

    for part in iter {
        let part = part.trim();
        if part.is_empty() {
            return Err(PipelineParseError::EmptySegment);
        }
        let right = parse_single(part)?;
        expr = PipelineExpr::Pipe(Box::new(expr), Box::new(right));
    }

    Ok(expr)
}

/// Parse a single workflow identifier.
fn parse_single(input: &str) -> Result<PipelineExpr, PipelineParseError> {
    let input = input.trim();
    if input.is_empty() {
        return Err(PipelineParseError::EmptySegment);
    }
    Ok(PipelineExpr::Single(input.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_single() {
        assert_eq!(
            parse_pipeline_expr("w1").unwrap(),
            PipelineExpr::Single("w1".into())
        );
    }

    #[test]
    fn test_pipe() {
        assert_eq!(
            parse_pipeline_expr("w1 | w2").unwrap(),
            PipelineExpr::Pipe(
                Box::new(PipelineExpr::Single("w1".into())),
                Box::new(PipelineExpr::Single("w2".into())),
            )
        );
    }

    #[test]
    fn test_sequential() {
        assert_eq!(
            parse_pipeline_expr("w1 && w2").unwrap(),
            PipelineExpr::Sequential(
                Box::new(PipelineExpr::Single("w1".into())),
                Box::new(PipelineExpr::Single("w2".into())),
            )
        );
    }

    #[test]
    fn test_parallel() {
        assert_eq!(
            parse_pipeline_expr("w1, w2").unwrap(),
            PipelineExpr::Parallel(vec![
                PipelineExpr::Single("w1".into()),
                PipelineExpr::Single("w2".into()),
            ])
        );
    }

    #[test]
    fn test_pipe_then_sequential() {
        // "w1 | w2 && w3" → Sequential(Pipe(w1, w2), w3)
        assert_eq!(
            parse_pipeline_expr("w1 | w2 && w3").unwrap(),
            PipelineExpr::Sequential(
                Box::new(PipelineExpr::Pipe(
                    Box::new(PipelineExpr::Single("w1".into())),
                    Box::new(PipelineExpr::Single("w2".into())),
                )),
                Box::new(PipelineExpr::Single("w3".into())),
            )
        );
    }

    #[test]
    fn test_full_combo() {
        // "w1 | w2 && w3, w4" →
        // Parallel([
        //   Sequential(Pipe(w1, w2), w3),
        //   w4,
        // ])
        assert_eq!(
            parse_pipeline_expr("w1 | w2 && w3, w4").unwrap(),
            PipelineExpr::Parallel(vec![
                PipelineExpr::Sequential(
                    Box::new(PipelineExpr::Pipe(
                        Box::new(PipelineExpr::Single("w1".into())),
                        Box::new(PipelineExpr::Single("w2".into())),
                    )),
                    Box::new(PipelineExpr::Single("w3".into())),
                ),
                PipelineExpr::Single("w4".into()),
            ])
        );
    }

    #[test]
    fn test_empty_input() {
        assert_eq!(
            parse_pipeline_expr(""),
            Err(PipelineParseError::EmptyInput)
        );
    }

    #[test]
    fn test_triple_pipe() {
        // "w1 | w2 | w3" → Pipe(Pipe(w1, w2), w3)
        assert_eq!(
            parse_pipeline_expr("w1 | w2 | w3").unwrap(),
            PipelineExpr::Pipe(
                Box::new(PipelineExpr::Pipe(
                    Box::new(PipelineExpr::Single("w1".into())),
                    Box::new(PipelineExpr::Single("w2".into())),
                )),
                Box::new(PipelineExpr::Single("w3".into())),
            )
        );
    }

    #[test]
    fn test_has_pipeline_syntax() {
        assert!(!has_pipeline_syntax("simple-workflow"));
        assert!(has_pipeline_syntax("w1 | w2"));
        assert!(has_pipeline_syntax("w1 && w2"));
        assert!(has_pipeline_syntax("w1, w2"));
    }

    #[test]
    fn test_inject_input_with_value() {
        let result = inject_input("Analyze this: {{input}}\nDone.", Some("hello world"));
        assert_eq!(result, "Analyze this: hello world\nDone.");
    }

    #[test]
    fn test_inject_input_none() {
        let result = inject_input("Prefix {{input}} suffix", None);
        assert_eq!(result, "Prefix  suffix");
    }

    #[test]
    fn test_inject_input_no_placeholder() {
        let result = inject_input("no placeholder here", Some("data"));
        assert_eq!(result, "no placeholder here");
    }

    #[test]
    fn test_inject_input_multiple() {
        let result = inject_input("{{input}} and {{input}}", Some("x"));
        assert_eq!(result, "x and x");
    }
}
