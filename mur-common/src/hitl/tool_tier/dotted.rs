//! #2312: module-qualified privileged calls (`os.chmod(...)`), and #2581:
//! module-qualified deletes (`shutil.rmtree(...)`).
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

/// Modules whose module-qualified calls can delete: the system modules plus
/// Python `pathlib` (`pathlib.Path.unlink(p)`). Kept apart from
/// [`SYSTEM_MODULES`] so #2581 does not widen the privileged rule.
const DELETE_MODULES: &[&str] = &["os", "posix", "shutil", "fs", "pathlib"];

/// Module-function spellings of a delete that are not themselves a
/// destructive head (`rm` / `rmdir` / `truncate` already match through
/// [`DESTRUCTIVE_HEADS`]). Lowercase, like the classifier's input.
const DELETE_FUNCTIONS: &[&str] = &[
    "rmtree",
    "remove",
    "unlink",
    "removedirs",
    "rmsync",
    "rmdirsync",
    "unlinksync",
];

/// The first privileged function `cmd` (already lowercased) calls through a
/// system module, e.g. `os.chmod` or `fs.promises.chown`.
pub(super) fn privileged_call(cmd: &str) -> Option<&str> {
    module_call(cmd, SYSTEM_MODULES, |f| {
        PRIVILEGED_HEADS.contains(&f) || PRIVILEGED_FUNCTIONS.contains(&f)
    })
}

/// #2581: the first delete `cmd` (already lowercased) calls through a module,
/// e.g. `shutil.rmtree` or `fs.promises.rm`. A delete through an interpreter
/// is the same delete as `rm -rf`, so it tiers the same.
///
/// Known gaps, accepted (the same as the privileged rule's, above): an
/// aliased import (`import shutil as s; s.rmtree`) and a method call on an
/// expression (`Path(p).unlink()`). The sandbox is the boundary for those,
/// not more patterns here.
pub(super) fn destructive_call(cmd: &str) -> Option<&str> {
    module_call(cmd, DELETE_MODULES, |f| {
        DESTRUCTIVE_HEADS.contains(&f) || DELETE_FUNCTIONS.contains(&f)
    })
}

/// The first dotted name in `cmd` whose FIRST segment is one of `modules`
/// and whose LAST segment satisfies `func`.
fn module_call<'a>(cmd: &'a str, modules: &[&str], func: impl Fn(&str) -> bool) -> Option<&'a str> {
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
        let last = t.rsplit('.').next().unwrap_or(t);
        modules.contains(&module) && func(last)
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

    /// #2581: a delete routed through an interpreter is the same delete as
    /// `rm -rf` — the incident was `shutil.rmtree` in a python heredoc.
    #[test]
    fn module_qualified_deletes_are_destructive() {
        for cmd in [
            "python3 - <<'PY'\nfrom pathlib import Path\nimport shutil\nshutil.rmtree(Path('x'))\nPY",
            "python3 -c 'import shutil; shutil.rmtree(\"x\")'",
            "python3 -c 'import os; os.remove(\"x\")'",
            "python3 -c 'import os; os.unlink(\"x\")'",
            "python3 -c 'import os; os.rmdir(\"x\")'",
            "python3 -c 'import os; os.removedirs(\"a/b\")'",
            "python3 -c 'import pathlib; pathlib.Path.unlink(p)'",
            "node -e 'fs.rmSync(\"x\",{recursive:true})'",
            "node -e 'fs.unlinkSync(\"x\")'",
            "node -e 'fs.rmdirSync(\"x\")'",
            "node -e 'fs.promises.rm(\"x\",{recursive:true})'",
            "python3 -c 'import shutil; list(map(shutil.rmtree, [\"x\"]))'",
        ] {
            assert_eq!(tier(cmd), RiskTier::Destructive, "cmd: {cmd}");
        }
    }

    /// Interpreter bodies without a module-qualified delete stay `Write`:
    /// raising every body is the prompt storm `opaque_shell` records.
    #[test]
    fn interpreter_bodies_without_deletes_stay_write() {
        for cmd in [
            "python3 -c 'print(1)'",
            "python3 - <<'PY'\nimport os\nprint(os.path.join('a', 'b'))\nPY",
            "python3 -c 'xs = [1]; xs.remove(1)'",
            "python3 -c 'import os; os.removexattr'",
            "node -e 'fs.readFileSync(\"x\")'",
            "node -e 'el.remove()'",
            // A delete-shaped method on a non-system object.
            "node -e 'cache.unlink(k)'",
            // A heredoc that only writes a script to disk never runs it.
            "cat > clean.py <<'PY'\nimport shutil\nshutil.rmtree('x')\nPY",
        ] {
            assert_eq!(tier(cmd), RiskTier::Write, "cmd: {cmd}");
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
