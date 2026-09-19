use std::future;

use sd_notify::NotifyState;
use tokio::time::{self, MissedTickBehavior};
use tracing::warn;

/// Talks to the service manager.
#[derive(Debug)]
pub struct Notifier {
    watchdog: Option<time::Interval>,
}

impl Notifier {
    #[must_use]
    pub fn new() -> Self {
        let watchdog = sd_notify::watchdog_enabled()
            .map(|timeout| timeout / 2)
            .filter(|period| !period.is_zero())
            .map(|period| {
                let mut interval = time::interval(period);
                interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
                interval
            });

        Self { watchdog }
    }

    pub fn ready(&self) {
        tell(&[NotifyState::Ready]);
    }

    pub fn stopping(&self) {
        tell(&[NotifyState::Stopping]);
    }

    pub fn ping(&self) {
        tell(&[NotifyState::Watchdog]);
    }

    pub async fn due(&mut self) {
        match &mut self.watchdog {
            Some(interval) => {
                interval.tick().await;
            }
            None => future::pending::<()>().await,
        }
    }
}

fn tell(state: &[NotifyState]) {
    if let Err(error) = sd_notify::notify(state) {
        warn!(%error, "failed to tell the service manager");
    }
}

impl Default for Notifier {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::time::timeout;

    use super::*;

    #[tokio::test]
    async fn a_notifier_with_no_service_manager_says_nothing_and_never_pings() {
        let mut notifier = Notifier::new();

        notifier.ready();
        notifier.ping();
        notifier.stopping();

        assert!(
            timeout(Duration::from_millis(50), notifier.due())
                .await
                .is_err(),
            "the watchdog fired without a service manager asking for it"
        );
    }
}
