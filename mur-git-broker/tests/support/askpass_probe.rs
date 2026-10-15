//! `GIT_ASKPASS` for the real-git test: the daemon's `main` dispatch, and nothing else.
//!
//! The crate is `cfg(unix)`, but `--workspace` still builds this bin on Windows when
//! `test-support` is unified in, so it needs a body there.
#[cfg(unix)]
fn main() {
    std::process::exit(mur_git_broker::askpass::run_from_env());
}

#[cfg(not(unix))]
fn main() {
    eprintln!("askpass-probe is unix-only");
    std::process::exit(2);
}
