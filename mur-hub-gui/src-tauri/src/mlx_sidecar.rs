//! MLX inference sidecar — spawns the bundled `mlx-server` (frozen mlx-lm,
//! OpenAI-compatible) on an ephemeral port and publishes its base URL via the
//! shared file so launchd-managed agents can reach it.

use std::net::TcpListener;

/// Reserve a free localhost TCP port by binding to :0 and reading the assigned
/// port. The listener is dropped immediately; a tiny race window exists before
/// the sidecar binds, which is acceptable here.
pub fn pick_free_port() -> std::io::Result<u16> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    Ok(listener.local_addr()?.port())
}

/// OpenAI-compatible base URL for the sidecar on `port`.
pub fn base_url(port: u16) -> String {
    format!("http://127.0.0.1:{port}/v1")
}

/// Readiness probe URL (returns 200 once the model is loaded).
pub fn health_url(port: u16) -> String {
    format!("http://127.0.0.1:{port}/v1/models")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pick_free_port_is_nonzero() {
        let p = pick_free_port().unwrap();
        assert!(p > 0);
    }

    #[test]
    fn url_helpers_format_correctly() {
        assert_eq!(base_url(50320), "http://127.0.0.1:50320/v1");
        assert_eq!(health_url(50320), "http://127.0.0.1:50320/v1/models");
    }
}
