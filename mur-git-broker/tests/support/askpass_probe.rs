//! `GIT_ASKPASS` for the real-git test: the daemon's `main` dispatch, and nothing else.
fn main() {
    std::process::exit(mur_git_broker::askpass::run_from_env());
}
