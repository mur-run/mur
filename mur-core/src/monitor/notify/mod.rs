//! Delivery channels. The queue and the message live in `mur-monitor`
//! (below this crate, deliberately — it must not spawn processes); the
//! channels live here because they do.

pub mod log;

use mur_monitor::notify::Notification;

pub trait Channel: Send + Sync {
    fn name(&self) -> &'static str;
    /// `Err(reason)` is recorded and retried on the queue's backoff. A
    /// channel must not panic and must not block for long: the drain runs
    /// on the daemon's tick.
    fn deliver(&self, n: &Notification) -> Result<(), String>;
}

#[derive(Default)]
pub struct ChannelRegistry {
    channels: Vec<Box<dyn Channel>>,
}

impl ChannelRegistry {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn register(&mut self, c: Box<dyn Channel>) {
        self.channels.push(c);
    }
    pub fn iter(&self) -> impl Iterator<Item = &dyn Channel> {
        self.channels.iter().map(|b| b.as_ref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_monitor::notify::Notification;

    struct Fake(&'static str, std::sync::atomic::AtomicUsize);
    impl Channel for Fake {
        fn name(&self) -> &'static str {
            self.0
        }
        fn deliver(&self, _n: &Notification) -> Result<(), String> {
            self.1.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }
    }

    #[test]
    fn registry_iterates_in_registration_order() {
        let mut r = ChannelRegistry::new();
        r.register(Box::new(Fake("a", Default::default())));
        r.register(Box::new(Fake("b", Default::default())));
        assert_eq!(
            r.iter().map(|c| c.name()).collect::<Vec<_>>(),
            vec!["a", "b"]
        );
    }

    #[test]
    fn a_channel_that_errors_returns_its_reason() {
        struct Broken;
        impl Channel for Broken {
            fn name(&self) -> &'static str {
                "broken"
            }
            fn deliver(&self, _n: &Notification) -> Result<(), String> {
                Err("no display".into())
            }
        }
        assert_eq!(Broken.deliver(&n()).unwrap_err(), "no display");
    }

    fn n() -> Notification {
        Notification {
            title: "MUR monitor: t — stalled".into(),
            body: "b".into(),
            next_step: "s".into(),
        }
    }
}
