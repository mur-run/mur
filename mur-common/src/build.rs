//! Compile-time build identity. `SHORT_SHA` is the git commit the binary was
//! built from (set by build.rs), or "unknown" for git-less builds (crates.io).
//! Used to detect when a running agent's binary differs from the installed one.

/// 12-char git sha of this build, or "unknown".
pub const SHORT_SHA: &str = env!("MUR_GIT_SHA");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_sha_is_set() {
        // Either a real 12-char hex sha, or the "unknown" fallback.
        assert!(SHORT_SHA == "unknown" || SHORT_SHA.len() == 12, "got {SHORT_SHA:?}");
    }
}
