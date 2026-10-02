use super::*;

/// F5's regression guard. A `read_timeout` or a total `.timeout()` on this
/// client is a hard ceiling on how long a model may think — reqwest polls
/// the read timer while the response head is still outstanding, so before
/// the first byte it cannot tell "thinking" from "hung". Either one
/// reintroduces exactly the bug #1287 is about.
///
/// Asserted through `Debug`, which prints `read_timeout` and the total
/// timeout when they are set (`async_impl/client.rs`, `Config::fmt_fields`).
///
/// Honest limitation: that same `Debug` does **not** print
/// `connect_timeout`, so this test cannot prove the connect clock is
/// applied — only that the two forbidden ones are absent. Proving the
/// connect clock behaviourally needs a TCP connect that stalls rather than
/// refuses, which means an unroutable address and a flaky test.
#[test]
fn the_client_carries_neither_a_total_nor_a_read_timeout() {
    let printed = format!("{:?}", llm_client_builder().build().unwrap());
    assert!(
        !printed.contains("read_timeout"),
        "a read_timeout bounds server think time before the first byte: {printed}"
    );
    assert!(
        !printed.contains("timeout: Some"),
        "a total timeout kills a live streamed response: {printed}"
    );
}
