/// Compile-time check that `cmd_init` accepts the new `refresh_discovery` flag.
#[test]
fn cmd_init_accepts_refresh_discovery() {
    let _ = super::cmd_init as fn(bool, bool) -> anyhow::Result<()>;
}
