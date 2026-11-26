//! #2312: module-qualified privileged calls (`os.chmod(...)`).
//! #2581: the same for destructive calls (`shutil.rmtree(...)`).
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
//! call on an expression (`require('fs').chmodSync`), and a destructured
//! import (`const {chmodSync} = require('fs'); chmodSync(...)` — bare
//! `chmodsync` is not a deny-list head) are not seen. A bare `chmod(...)`
//! after `from os import *` is already caught by the plain head scan.

use super::{DESTRUCTIVE_HEADS, PRIVILEGED_HEADS};

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

/// Module-function spellings of a delete that differ from the destructive
/// heads (`rm`, `rmdir`, `truncate` already match as the bare function name).
/// Python `os.remove` / `os.unlink` / `os.removedirs` / `shutil.rmtree`,
/// Node `fs.unlink` / `fs.rmSync` / `fs.promises.rm` and friends. Compared
/// lowercase.
const DESTRUCTIVE_FUNCTIONS: &[&str] = &[
    "rmtree",
    "remove",
    "removedirs",
    "unlink",
    "rmsync",
    "rmdirsync",
    "unlinksync",
];

/// The first privileged function `cmd` (already lowercased) calls through a
/// system module, e.g. `os.chmod` or `fs.promises.chown`.
pub(super) fn privileged_call(cmd: &str) -> Option<&str> {
    module_call(cmd, |f| {
        PRIVILEGED_HEADS.contains(&f) || PRIVILEGED_FUNCTIONS.contains(&f)
    })
}

/// #2581: the first deleting function `cmd` (already lowercased) calls
/// through a system module, e.g. `shutil.rmtree` or `fs.rmSync`. An agent
/// refused `rm -rf` retried the same delete as a Python heredoc and it tiered
/// `Write`.
pub(super) fn destructive_call(cmd: &str) -> Option<&str> {
    module_call(cmd, |f| {
        DESTRUCTIVE_HEADS.contains(&f) || DESTRUCTIVE_FUNCTIONS.contains(&f)
    })
}

/// The first dotted name in `cmd` whose first segment is a system module and
/// whose last segment satisfies `is_func`.
fn module_call(cmd: &str, is_func: impl Fn(&str) -> bool) -> Option<&str> {
    // Split on anything that cannot be part of a dotted name, not just on
    // `tokens()`'s separators: `map(os.chmod,`, `f=os.chmod`, `{os.chmod(`
    // and `[os.chmod]` would otherwise glue punctuation to the module or the
    // function and miss the exact match. Whitespace is outside the class too,
    // so this is a superset of the token split.
    let is_name = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '.';
    cmd.split(|c: char| !is_name(c)).find(|t| {
        let Some((module, _)) = t.split_once('.') else {
            return false;
        };
        let func = t.rsplit('.').next().unwrap_or(t);
        SYSTEM_MODULES.contains(&module) && is_func(func)
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
            // Punctuation glued to the call by `tokens()` (review of #2314).
            "python3 -c 'import os; list(map(os.chmod, [\"x\"], [0o777]))'",
            "python3 -c 'import os; f=os.chmod; f(\"x\", 0o777)'",
            "node -e '{fs.chmodSync(\"x\", 0o777)}'",
            "python3 -c 'import os; [os.chmod][0](\"x\", 0o777)'",
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

    /// #2581: a delete through a system module is the delete it performs.
    #[test]
    fn module_qualified_deletes_are_destructive() {
        for cmd in [
            "python3 - <<'PY'\nimport shutil\nfor p in ['a','b']:\n    shutil.rmtree(p)\nPY",
            "python3 -c 'import shutil; shutil.rmtree(\"x\")'",
            "python3 -c 'import os; os.remove(\"x\")'",
            "python3 -c 'import os; os.unlink(\"x\")'",
            "python3 -c 'import os; os.rmdir(\"x\")'",
            "python3 -c 'import os; os.removedirs(\"a/b\")'",
            "node -e 'fs.rmSync(\"x\", {recursive: true})'",
            "node -e 'fs.unlinkSync(\"x\")'",
            "node -e 'fs.promises.rm(\"x\")'",
            "python3 -c 'import shutil; list(map(shutil.rmtree, [\"x\"]))'",
        ] {
            assert_eq!(tier(cmd), RiskTier::Destructive, "cmd: {cmd}");
        }
    }

    /// Ordinary scripts, look-alike methods, and a script merely WRITTEN to a
    /// file (heredoc body is data) keep their tier: no prompt storm.
    #[test]
    fn ordinary_scripts_and_lookalikes_are_not_destructive() {
        for cmd in [
            "python3 -c 'print(1)'",
            "python3 - <<'PY'\nimport os\nprint(os.listdir('.'))\nPY",
            "python3 -c 'xs=[1]; xs.remove(1)'",
            "python3 -c 'import os; os.path.exists(\"x\")'",
            "node -e 'cache.unlink(k)'",
            "cat > clean.py <<'PY'\nimport shutil\nshutil.rmtree('x')\nPY",
        ] {
            let got = tier(cmd);
            assert!(got < RiskTier::Destructive, "cmd: {cmd}\ngot {got:?}");
        }
    }
}
