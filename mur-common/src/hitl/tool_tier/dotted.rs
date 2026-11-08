//! #2312: module-qualified privileged calls (`os.chmod(...)`).
//!
//! `tokens()` does not split on `.`, so `os.chmod` is one token and never
//! equals the deny-list's `chmod` — a Python heredoc or `python3 -c` body
//! that really changes permissions tiered as `Write`.
//!
//! Splitting every token on `.` would also fire on method calls such as a
//! test helper's `wrapper.mount(...)`, so only a token whose FIRST segment is
//! a known system module and whose LAST segment is a privileged function
//! counts. This can only RAISE a tier: it adds matches, removes none.
//!
//! Known gaps, accepted: an aliased import (`import os as o; o.chmod`) and a
//! call on an expression (`require('fs').chmodSync`) are not seen. A bare
//! `chmod(...)` after `from os import *` is already caught by the plain head
//! scan.

use super::{PRIVILEGED_HEADS, tokens};

/// Standard-library modules whose functions act on the real system:
/// Python `os` / `posix` / `shutil`, Node `fs` (incl. `fs.promises.*`).
const SYSTEM_MODULES: &[&str] = &["os", "posix", "shutil", "fs"];

/// Module-function spellings of the privileged heads that differ from the
/// head itself (descriptor/link variants and Node's `*Sync` forms). Compared
/// lowercase, as the classifier lowercases the command first.
const PRIVILEGED_FUNCTIONS: &[&str] = &[
    "fchmod",
    "lchmod",
    "fchown",
    "lchown",
    "chmodsync",
    "fchmodsync",
    "lchmodsync",
    "chownsync",
    "fchownsync",
    "lchownsync",
];

/// Does `cmd` (already lowercased) call a privileged function through a
/// system module, e.g. `os.chmod` or `fs.promises.chown`?
pub(super) fn privileged_call_in(cmd: &str) -> bool {
    tokens(cmd).any(|t| {
        let Some((module, _)) = t.split_once('.') else {
            return false;
        };
        let func = t.rsplit('.').next().unwrap_or(t);
        SYSTEM_MODULES.contains(&module)
            && (PRIVILEGED_HEADS.contains(&func) || PRIVILEGED_FUNCTIONS.contains(&func))
    })
}

#[cfg(test)]
mod tests {
    use super::super::classify;
    use crate::hitl::RiskTier;

    fn tier(cmd: &str) -> RiskTier {
        classify("bash", Some(&serde_json::json!({ "command": cmd })))
    }

    #[test]
    fn module_qualified_privileged_calls_are_privileged() {
        for cmd in [
            "python3 - <<'EOF'\nimport os\nos.chmod('x', 0o755)\nEOF",
            "python3 - <<'EOF'\nimport os\nos.chown('x', 0, 0)\nEOF",
            "python3 -c 'import os; os.chmod(\"x\", 0o755)'",
            "python3 -c 'import os; os.chown(\"x\", 0, 0)'",
            "python3 -c 'import os; os.lchown(\"x\", 0, 0)'",
            "python3 -c 'import posix; posix.chmod(\"x\", 0o777)'",
            "python3 -c 'import shutil; shutil.chown(\"x\", \"root\")'",
            "node -e 'require(\"fs\"); fs.chmodSync(\"x\", 0o777)'",
            "node -e 'fs.promises.chown(\"x\", 0, 0)'",
        ] {
            assert_eq!(tier(cmd), RiskTier::Privileged, "cmd: {cmd}");
        }
    }

    /// Method calls and look-alikes outside a system module keep their tier:
    /// the rule must not turn every `x.mount(` in a test file into a prompt.
    #[test]
    fn method_calls_and_lookalikes_are_not_privileged() {
        for cmd in [
            "node -e 'wrapper.mount(App)'",
            "python3 -c 'obj.chmod(1)'",
            "python3 -c 'import os; os.path.join(\"a\", \"b\")'",
            "python3 -c 'import os; os.chdir(\"/tmp\")'",
        ] {
            let got = tier(cmd);
            assert!(got < RiskTier::Privileged, "cmd: {cmd}\ngot {got:?}");
        }
    }
}
