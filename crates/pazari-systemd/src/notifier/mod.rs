use std::future;

use sd_notify::NotifyState;
use tokio::time::{self, MissedTickBehavior};
use tracing::warn;

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
        if let Err(error) = sd_notify::notify(&[NotifyState::Ready]) {
            warn!(%error, "failed to notify the service manager");
        }
    }

    pub fn stopping(&self) {
        if let Err(error) = sd_notify::notify(&[NotifyState::Stopping]) {
            warn!(%error, "failed to notify the service manager");
        }
    }

    pub fn ping(&self) {
        if let Err(error) = sd_notify::notify(&[NotifyState::Watchdog]) {
            warn!(%error, "failed to notify the service manager");
        }
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
