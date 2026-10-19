//! Pure core of `edit_file`: parse the one-or-many edit list and apply it to
//! an in-memory string. No I/O here, so the all-or-nothing guarantee is just
//! "the caller writes only on `Ok`".

use crate::tools::ToolError;

/// One exact-literal replacement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Edit {
    pub old: String,
    pub new: String,
    /// `None` = must match exactly once.
    pub expected: Option<i64>,
}

fn edit_from(v: &serde_json::Value, ctx: &str) -> Result<Edit, ToolError> {
    let field = |k: &str| {
        v[k].as_str()
            .map(str::to_owned)
            .ok_or_else(|| ToolError::InvalidInput(format!("{ctx}missing '{k}' field")))
    };
    let old = field("old_string")?;
    let new = field("new_string")?;
    if old.is_empty() {
        return Err(ToolError::InvalidInput(format!(
            "{ctx}'old_string' must be non-empty"
        )));
    }
    Ok(Edit {
        old,
        new,
        expected: v["expected_count"].as_i64().filter(|n| *n >= 1),
    })
}

/// Accept either the single-edit form (`old_string`/`new_string` at the top
/// level) or the batch form (`edits: [...]`). Mixing the two is refused: it
/// is ambiguous whether the top-level pair runs first, last, or not at all.
pub(super) fn parse_edits(input: &serde_json::Value) -> Result<Vec<Edit>, ToolError> {
    let Some(list) = input.get("edits").filter(|v| !v.is_null()) else {
        return Ok(vec![edit_from(input, "")?]);
    };
    if input.get("old_string").is_some() || input.get("new_string").is_some() {
        return Err(ToolError::InvalidInput(
            "pass either 'edits' or old_string/new_string, not both".into(),
        ));
    }
    let arr = list
        .as_array()
        .ok_or_else(|| ToolError::InvalidInput("'edits' must be an array".into()))?;
    if arr.is_empty() {
        return Err(ToolError::InvalidInput("'edits' must be non-empty".into()));
    }
    arr.iter()
        .enumerate()
        .map(|(i, v)| edit_from(v, &format!("edit #{}: ", i + 1)))
        .collect()
}

/// Apply `edits` in order, each against the output of the one before.
/// Returns the new text and the total number of replacements. Any failure
/// returns `Err` and the caller writes nothing.
pub(super) fn apply_edits(text: &str, edits: &[Edit]) -> Result<(String, usize), ToolError> {
    let batch = edits.len() > 1;
    let mut cur = text.to_owned();
    let mut total = 0;
    for (i, e) in edits.iter().enumerate() {
        let ctx = if batch {
            format!("edit #{} of {}: ", i + 1, edits.len())
        } else {
            String::new()
        };
        let tail = if batch {
            " — no edits were written"
        } else {
            ""
        };
        let found = cur.match_indices(e.old.as_str()).count();
        match e.expected {
            Some(n) if found as i64 != n => {
                return Err(ToolError::InvalidInput(format!(
                    "{ctx}expected {n} occurrence(s) of old_string, found {found}{tail}"
                )));
            }
            None if found == 0 => {
                let hint = super::hint::not_found_hint(&cur, &e.old);
                return Err(ToolError::InvalidInput(format!(
                    "{ctx}old_string not found{tail}{hint}"
                )));
            }
            None if found > 1 => {
                return Err(ToolError::InvalidInput(format!(
                    "{ctx}ambiguous edit: old_string matches {found} times — pass expected_count or a longer anchor{tail}"
                )));
            }
            _ => {}
        }
        cur = cur.replace(e.old.as_str(), &e.new);
        total += found;
    }
    Ok((cur, total))
}
