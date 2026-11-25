//! Which shell command a gate check looks at — the prefixes agents put in
//! front of a build/test run — and which job a `bash_wait` names.

/// Strip one leading `cd <path> && ` — the one composition agents routinely
/// prepend. Nothing else: see `is_evidence` for why deeper shell parsing is
/// deliberately not attempted.
pub(crate) fn after_cd_prefix(command: &str) -> &str {
    let command = command.trim();
    command
        .strip_prefix("cd ")
        .and_then(|rest| rest.split_once("&&"))
        .map(|(_, after)| after.trim())
        .unwrap_or(command)
}

/// Strip leading `NAME=value` assignments (`MUR_TEST_SOCKETS=1 cargo test`),
/// the other prefix agents routinely put in front of a gate run (#2578).
/// Only unquoted values: a quoted one is left alone, so the command simply
/// fails to match a runner — the conservative direction.
pub(crate) fn after_env_prefix(command: &str) -> &str {
    let mut rest = command.trim_start();
    loop {
        let token = rest.split_whitespace().next().unwrap_or("");
        let Some((name, _)) = token.split_once('=') else {
            return rest;
        };
        let is_name = name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
        if !is_name || token.contains(['"', '\'']) {
            return rest;
        }
        rest = rest[token.len()..].trim_start();
    }
}

/// The command a gate check should look at: past one `cd <path> && ` and any
/// leading env assignments.
pub(crate) fn gate_command(command: &str) -> &str {
    after_env_prefix(after_cd_prefix(command))
}

/// The `job_id` a `bash_wait` target names. The target is the call's input
/// JSON (`describe_target_with_result` has no field for `bash_wait`).
pub(super) fn waited_job(target: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(target)
        .ok()?
        .get("job_id")?
        .as_str()
        .map(str::to_string)
}
