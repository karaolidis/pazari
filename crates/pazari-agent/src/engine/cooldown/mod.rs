use pazari_types::{Deadline, Delay, Timestamp};

/// A wait after a resize before the next release.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cooldown {
    delay: Delay,
    until: Option<Deadline>,
}

impl Cooldown {
    pub const fn new(delay: Delay) -> Self {
        Self { delay, until: None }
    }

    /// Starts the wait over from `now`.
    pub fn restart(&mut self, now: Timestamp) {
        self.until = Some(self.from(now));
    }

    /// Makes the wait last at least a whole delay from `now`.
    pub fn extend(&mut self, now: Timestamp) {
        self.until = Some(self.from(now).furthest(self.until));
    }

    pub const fn lift(&mut self) {
        self.until = None;
    }

    pub fn active(&self, now: Timestamp) -> bool {
        self.until.is_some_and(|until| !until.reached(now))
    }

    fn from(&self, now: Timestamp) -> Deadline {
        Deadline::after(now, self.delay.get())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::testing::{at, delay};

    #[test]
    fn a_restart_waits_a_whole_delay_from_now() {
        let mut cooldown = Cooldown::new(delay(30));

        assert!(!cooldown.active(at(0)));

        cooldown.restart(at(100));

        assert!(cooldown.active(at(129)));
        assert!(!cooldown.active(at(130)));
    }

    #[test]
    fn an_extension_never_shortens_the_wait() {
        let mut cooldown = Cooldown::new(delay(30));

        cooldown.restart(at(100));
        cooldown.extend(at(90));
        assert!(cooldown.active(at(129)));

        cooldown.extend(at(120));
        assert!(cooldown.active(at(149)));
        assert!(!cooldown.active(at(150)));
    }

    #[test]
    fn a_lift_ends_the_wait() {
        let mut cooldown = Cooldown::new(delay(30));

        cooldown.restart(at(100));
        cooldown.lift();

        assert!(!cooldown.active(at(101)));
    }
}
