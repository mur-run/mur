//! Upstream dial for the egress proxy: connect to one of the addresses a
//! single screened resolution produced.
//!
//! The no-rebinding guarantee needs the proxy to dial only addresses from that
//! one resolution — never to resolve again. It does not need a single address,
//! so a dual-stack host whose first address is unreachable still connects
//! through a later one (#2541).

use std::future::Future;
use std::net::SocketAddr;
use std::time::Duration;

use tokio::net::TcpStream;

/// How long one address may take before the next is tried. The last address is
/// not limited: with nothing left to fall back to, the OS connect timeout
/// decides, as it did when the proxy dialed a single address.
pub(super) const DIAL_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(5);

/// One address that did not connect, and why.
pub(super) type DialFailure = (SocketAddr, std::io::Error);

/// Connect to the first of `addrs` that answers, in order.
///
/// `Err` carries every address tried with its error, in the same order. It is
/// empty only when `addrs` is.
pub(super) async fn connect_first(
    addrs: &[SocketAddr],
) -> Result<(TcpStream, SocketAddr), Vec<DialFailure>> {
    connect_first_with(addrs, DIAL_ATTEMPT_TIMEOUT, TcpStream::connect).await
}

pub(super) async fn connect_first_with<T, F, Fut>(
    addrs: &[SocketAddr],
    attempt_timeout: Duration,
    mut dial: F,
) -> Result<(T, SocketAddr), Vec<DialFailure>>
where
    F: FnMut(SocketAddr) -> Fut,
    Fut: Future<Output = std::io::Result<T>>,
{
    let mut failures = Vec::new();
    for (i, &addr) in addrs.iter().enumerate() {
        let is_last = i + 1 == addrs.len();
        let attempt = if is_last {
            dial(addr).await
        } else {
            match tokio::time::timeout(attempt_timeout, dial(addr)).await {
                Ok(result) => result,
                Err(_) => Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    format!(
                        "no answer within {}s; trying the next address",
                        attempt_timeout.as_secs()
                    ),
                )),
            }
        };
        match attempt {
            Ok(stream) => return Ok((stream, addr)),
            Err(e) => failures.push((addr, e)),
        }
    }
    Err(failures)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    /// A loopback address nothing listens on: bound, then dropped.
    async fn dead_addr() -> SocketAddr {
        TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap()
            .local_addr()
            .unwrap()
    }

    /// #2541: the first screened address is unreachable, a later one works.
    #[tokio::test]
    async fn an_unreachable_first_address_falls_back_to_the_next() {
        let live = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let live_addr = live.local_addr().unwrap();
        let dead = dead_addr().await;

        let (_stream, connected) = connect_first(&[dead, live_addr])
            .await
            .unwrap_or_else(|f| panic!("no address connected: {f:?}"));
        assert_eq!(connected, live_addr);
    }

    #[tokio::test]
    async fn when_every_address_fails_each_one_is_reported_in_order() {
        let (a, b) = (dead_addr().await, dead_addr().await);
        let failures = connect_first(&[a, b]).await.map(|_| ()).unwrap_err();
        let tried: Vec<SocketAddr> = failures.iter().map(|(addr, _)| *addr).collect();
        assert_eq!(tried, [a, b]);
    }

    #[tokio::test]
    async fn no_addresses_is_an_empty_failure() {
        let failures = connect_first(&[]).await.map(|_| ()).unwrap_err();
        assert!(failures.is_empty());
    }

    /// An address that never answers must not hold up the ones after it.
    #[tokio::test(start_paused = true)]
    async fn a_hung_address_times_out_and_the_next_is_tried() {
        let hung: SocketAddr = "203.0.113.1:443".parse().unwrap();
        let good: SocketAddr = "203.0.113.2:443".parse().unwrap();
        let out = connect_first_with(&[hung, good], DIAL_ATTEMPT_TIMEOUT, |addr| async move {
            if addr == hung {
                std::future::pending::<()>().await;
            }
            Ok(())
        })
        .await;
        assert_eq!(out.unwrap().1, good);
    }

    /// The last address keeps the OS timeout: cutting it short would fail a
    /// slow single-address host that connects today.
    #[tokio::test(start_paused = true)]
    async fn the_last_address_is_not_cut_short() {
        let hung: SocketAddr = "203.0.113.1:443".parse().unwrap();
        let slow: SocketAddr = "203.0.113.2:443".parse().unwrap();
        let out = connect_first_with(&[hung, slow], DIAL_ATTEMPT_TIMEOUT, |addr| async move {
            if addr == hung {
                std::future::pending::<()>().await;
            }
            tokio::time::sleep(DIAL_ATTEMPT_TIMEOUT * 4).await;
            Ok(())
        })
        .await;
        assert_eq!(out.unwrap().1, slow);
    }

    #[tokio::test(start_paused = true)]
    async fn a_timed_out_address_is_reported_as_timed_out() {
        let hung: SocketAddr = "203.0.113.1:443".parse().unwrap();
        let dead: SocketAddr = "203.0.113.2:443".parse().unwrap();
        let failures = connect_first_with(&[hung, dead], DIAL_ATTEMPT_TIMEOUT, |addr| async move {
            if addr == hung {
                std::future::pending::<()>().await;
            }
            Err::<(), _>(std::io::Error::from(std::io::ErrorKind::ConnectionRefused))
        })
        .await
        .unwrap_err();
        let kinds: Vec<_> = failures.iter().map(|(a, e)| (*a, e.kind())).collect();
        assert_eq!(
            kinds,
            [
                (hung, std::io::ErrorKind::TimedOut),
                (dead, std::io::ErrorKind::ConnectionRefused)
            ]
        );
    }
}
