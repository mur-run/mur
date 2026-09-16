//! The channel of last resort: always registered, cannot be switched off.
//! A monitor that went unhealthy must leave a trace somewhere even when a
//! user has turned every other channel off, and the daemon log is where an
//! operator already looks.

use mur_monitor::notify::Notification;

use super::Channel;

pub struct LogChannel;

impl Channel for LogChannel {
    fn name(&self) -> &'static str {
        "log"
    }

    fn deliver(&self, n: &Notification) -> Result<(), String> {
        tracing::info!(
            title = %n.title,
            next_step = %n.next_step,
            "{}",
            n.body.replace('\n', " · ")
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_monitor::notify::Notification;

    #[test]
    fn the_log_channel_is_named_log_and_never_fails() {
        // It must never fail: it is the channel of last resort, the one that
        // records a notable event even when every other channel is off.
        let c = LogChannel;
        assert_eq!(c.name(), "log");
        assert!(
            c.deliver(&Notification {
                title: "MUR monitor: t — stalled".into(),
                body: "body".into(),
                next_step: "step".into(),
            })
            .is_ok()
        );
    }
}
