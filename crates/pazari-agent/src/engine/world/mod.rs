use std::{num::NonZeroU64, time::Duration};

use pazari_proto::{GuestMessage, ReclaimId, RequestId};
use pazari_types::{Deadline, Timestamp};
use tracing::debug;

use crate::engine::{
    action::Action,
    config::{EngineConfig, ThrottleMode},
    counters::Pressure,
    effects::Effects,
    event::{BootInfo, Snapshot},
    grow::{Grow, GrowStrategy},
    limits::{Brake, Limits, Moved},
    refusal::Refusal,
    release::{Release, ReleaseStrategy},
    streak::Streak,
};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Guest {
    block_size: NonZeroU64,
    reading: Option<Snapshot>,
    hotplugged: Option<u64>,
}

impl Guest {
    const fn new(boot: BootInfo) -> Self {
        Self {
            block_size: boot.block_size,
            reading: None,
            hotplugged: None,
        }
    }

    pub const fn reading(self) -> Option<Snapshot> {
        self.reading
    }

    pub fn usage(self) -> Option<u64> {
        Some(self.reading?.workload_usage)
    }

    pub fn free(self) -> Option<u64> {
        Some(self.reading?.free)
    }

    pub fn available(self) -> Option<u64> {
        Some(self.reading?.available)
    }

    pub const fn round_up(self, value: u64) -> u64 {
        value
            .div_ceil(self.block_size.get())
            .saturating_mul(self.block_size.get())
    }

    pub const fn round_down(self, value: u64) -> u64 {
        (value / self.block_size.get()).saturating_mul(self.block_size.get())
    }

    fn boot_mem(self, total_mem: u64) -> Option<u64> {
        self.hotplugged
            .map(|hotplugged| total_mem.saturating_sub(hotplugged))
    }
}

/// A session with the host, past its handshake.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Session {
    status_interval: Duration,
    next_status: Option<Deadline>,
}

impl Session {
    const fn new(status_interval: Duration) -> Self {
        Self {
            status_interval,
            next_status: None,
        }
    }

    fn status_due(&mut self, now: Timestamp) -> bool {
        if self.next_status.is_some_and(|next| !next.reached(now)) {
            return false;
        }

        self.next_status = Some(Deadline::after(now, self.status_interval));

        true
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Link {
    next_id: RequestId,
    session: Option<Session>,
}

impl Link {
    const fn closed() -> Self {
        Self {
            next_id: RequestId(1),
            session: None,
        }
    }

    const fn open(status_interval: Duration) -> Self {
        Self {
            session: Some(Session::new(status_interval)),
            ..Self::closed()
        }
    }

    const fn connected(&self) -> bool {
        self.session.is_some()
    }

    const fn take_id(&mut self) -> RequestId {
        let id = self.next_id;
        self.next_id = id.next();
        id
    }
}

/// The workload cgroup's one reclaim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reclaim {
    Idle,
    Running,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct World {
    config: EngineConfig,
    limits: Limits,
    guest: Guest,
    link: Link,
    grow: GrowStrategy,
    release: ReleaseStrategy,
    calm: Streak,
    reclaim: Reclaim,
}

impl World {
    pub fn new(config: EngineConfig, boot: BootInfo) -> Self {
        Self {
            limits: Limits::new(&config, boot.total_mem),
            guest: Guest::new(boot),
            link: Link::closed(),
            grow: GrowStrategy::from(config.grow),
            release: ReleaseStrategy::from(config.release),
            calm: Streak::BROKEN,
            reclaim: Reclaim::Idle,
            config,
        }
    }

    pub const fn config(&self) -> &EngineConfig {
        &self.config
    }

    pub const fn limits(&self) -> Limits {
        self.limits
    }

    pub const fn guest(&self) -> Guest {
        self.guest
    }

    pub const fn grow(&self) -> GrowStrategy {
        self.grow
    }

    pub const fn connected(&self) -> bool {
        self.link.connected()
    }

    pub const fn take_id(&mut self) -> RequestId {
        self.link.take_id()
    }

    pub fn send(&self, message: GuestMessage, fx: &mut Effects) {
        if self.link.connected() {
            fx.emit(Action::Send(message));
        }
    }

    pub fn nak(&self, id: ReclaimId, refusal: Refusal, fx: &mut Effects) {
        self.send(
            GuestMessage::ReclaimNak {
                id,
                reason: refusal.to_string(),
            },
            fx,
        );
    }

    pub fn brake(&mut self, brake: Brake, fx: &mut Effects) {
        self.limits.apply(brake, fx);
    }

    pub fn rebrake(&mut self, fx: &mut Effects) {
        self.limits.reapply(fx);
    }

    pub fn start_reclaim(&mut self, bytes: NonZeroU64, fx: &mut Effects) {
        fx.emit(Action::StartReclaim {
            bytes: bytes.get(),
            swappiness: self.config.reclaim_swappiness,
        });

        self.reclaim = Reclaim::Running;
    }

    pub fn cancel_reclaim(&self, fx: &mut Effects) {
        if self.reclaiming() {
            fx.emit(Action::CancelReclaim);
        }
    }

    pub const fn reclaim_finished(&mut self) {
        self.reclaim = Reclaim::Idle;
    }

    /// Whether a reclaim has started and not yet reported.
    pub const fn reclaiming(&self) -> bool {
        matches!(self.reclaim, Reclaim::Running)
    }

    /// How much to reclaim before asking the host, if the guest has it to spare.
    pub fn throttle_reclaim(&self) -> Option<NonZeroU64> {
        match self.config.throttle {
            ThrottleMode::Grow => None,
            ThrottleMode::Reclaim { amount } => self
                .slack()
                .is_some_and(|slack| slack >= amount.get())
                .then_some(amount),
        }
    }

    pub const fn set_total_mem(&mut self, total_mem: u64) -> Moved {
        self.limits.set_total_mem(total_mem)
    }

    pub const fn set_ceiling(&mut self, ceiling: u64) -> Moved {
        self.limits.set_ceiling(ceiling)
    }

    pub const fn observe(&mut self, reading: Snapshot) {
        self.guest.reading = Some(reading);
    }

    pub fn connect(&mut self, status_interval: Duration, hotplugged: u64) {
        self.link = Link::open(status_interval);
        self.grow.reset();
        self.set_hotplugged(hotplugged);
    }

    pub const fn disconnect(&mut self) {
        self.link.session = None;
    }

    pub fn set_hotplugged(&mut self, hotplugged: u64) {
        if self.guest.hotplugged == Some(hotplugged) {
            return;
        }

        debug!(
            hotplugged,
            was = ?self.guest.hotplugged,
            total_mem = self.limits.total_mem(),
            "host reported the plugged size",
        );

        self.guest.hotplugged = Some(hotplugged);
    }

    pub fn status_due(&mut self, now: Timestamp) -> bool {
        self.link
            .session
            .as_mut()
            .is_some_and(|session| session.status_due(now))
    }

    pub fn note_interval(&mut self, now: Timestamp, pressure: Pressure) {
        let slack = self.slack();

        self.calm = self.calm.then(
            pressure.calm() && slack.is_some_and(|slack| slack >= self.guest.block_size.get()),
        );

        self.release.observe(now, slack, pressure);
    }

    /// The workload hit the brake at `at`.
    pub fn note_braked(&mut self, at: Timestamp) {
        self.grow.braked(at);
    }

    /// A grow has landed.
    pub fn note_grown(&mut self, now: Timestamp) {
        self.grow.settled(now);
    }

    /// The guest settled after a resize, or after an attempt at one.
    pub fn note_rested(&mut self, now: Timestamp) {
        self.release.rested(now);
    }

    /// The workload came back under its ceiling after exhausting it.
    pub fn note_recovered(&mut self) {
        self.grow.reset();
        self.release.recovered();
    }

    /// How much to give back this interval, if anything.
    pub fn release_due(&mut self, now: Timestamp, pressure: Pressure) -> Option<NonZeroU64> {
        self.release.due(now, pressure)
    }

    /// Whether the workload has been calm long enough to end an exhaustion.
    pub const fn calm(&self) -> bool {
        self.calm.reached(self.config.recover_after)
    }

    pub fn slack(&self) -> Option<u64> {
        Some(
            self.guest
                .available()?
                .saturating_sub(self.limits.reserves()),
        )
    }

    /// How far the workload's usage reaches past the memory the guest has.
    fn deficit(&self) -> Option<u64> {
        let need = self
            .guest
            .usage()?
            .saturating_add(self.limits.service_reserve())
            .saturating_add(self.limits.burst_reserve());

        Some(need.saturating_sub(self.limits.total_mem()))
    }

    pub fn grow_target(&self) -> Option<u64> {
        let total_mem = self.limits.total_mem();
        let step = self.grow.step().get();

        let reach = self.deficit().map_or(step, |deficit| deficit.max(step));

        let mut target = self.guest.round_up(total_mem.saturating_add(reach));

        if let Some(ceiling) = self.limits.ceiling()
            && target > ceiling
        {
            target = self.guest.round_down(ceiling);
        }

        (target > total_mem).then_some(target)
    }

    /// What a release needs reclaimed beyond free memory.
    pub fn reclaim_for(&self, bytes: NonZeroU64) -> Option<NonZeroU64> {
        let bytes = bytes.get();

        NonZeroU64::new(
            self.guest
                .free()
                .map_or(bytes, |free| bytes.saturating_sub(free)),
        )
    }

    pub fn release_budget(&self, desired: u64) -> Option<NonZeroU64> {
        self.budget_for(desired, self.slack()?)
    }

    pub fn release_budget_after_reclaim(
        &self,
        desired: NonZeroU64,
        reclaimed: u64,
    ) -> Option<NonZeroU64> {
        self.budget_for(desired.get(), self.slack()?.saturating_add(reclaimed))
    }

    fn budget_for(&self, desired: u64, slack: u64) -> Option<NonZeroU64> {
        let total_mem = self.limits.total_mem();

        let floor = self.guest.boot_mem(total_mem).unwrap_or(total_mem).max(
            self.limits
                .reserves()
                .saturating_add(self.guest.block_size.get()),
        );

        let by_floor = total_mem.saturating_sub(floor);

        NonZeroU64::new(self.guest.round_down(desired.min(by_floor).min(slack)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::testing::{
        BLOCK, BOOT, BURST, CALM, MIB, SERVICE, at, boot, bytes, config, snap, snap_available,
    };

    fn world() -> World {
        World::new(config(), boot())
    }

    fn grown(usage: u64) -> World {
        let mut world = world();

        world.set_total_mem(2560 * MIB);
        world.observe(snap(2560 * MIB, usage, 0));
        world.connect(Duration::ZERO, 512 * MIB);

        world
    }

    #[test]
    fn a_grow_target_asks_for_a_whole_step_when_the_workload_fits() {
        let mut world = world();
        world.observe(snap(BOOT, CALM, 0));

        assert_eq!(world.grow_target(), Some(2560 * MIB));
    }

    #[test]
    fn a_grow_target_reaches_as_far_as_the_strategy_steps() {
        let mut world = world();
        world.observe(snap(BOOT, CALM, 0));

        world.note_grown(at(100));
        world.note_braked(at(101));

        assert_eq!(world.grow_target(), Some(3072 * MIB));
    }

    #[test]
    fn a_reconnect_forgets_how_far_earlier_grows_reached() {
        let mut world = world();
        world.observe(snap(BOOT, CALM, 0));

        world.note_grown(at(100));
        world.note_braked(at(101));
        world.connect(Duration::ZERO, 0);

        assert_eq!(world.grow_target(), Some(2560 * MIB));
    }

    #[test]
    fn a_grow_target_covers_a_deficit_bigger_than_the_step() {
        let mut world = world();
        world.observe(snap(BOOT, 2200 * MIB, 0));

        // 2200 + 256 + 256 wanted, 2048 total_mem: a 664 MiB deficit, rounded up
        // to the next 128 MiB block.
        assert_eq!(world.grow_target(), Some(2816 * MIB));
    }

    #[test]
    fn a_grow_target_is_clamped_and_aligned_under_the_ceiling() {
        let mut world = world();
        world.observe(snap(BOOT, CALM, 0));
        world.set_ceiling(2200 * MIB);

        assert_eq!(world.grow_target(), Some(2176 * MIB));
    }

    #[test]
    fn a_grow_target_runs_out_at_a_ceiling_on_the_plug() {
        let mut world = world();
        world.observe(snap(BOOT, CALM, 0));
        world.set_ceiling(BOOT);

        assert_eq!(world.grow_target(), None);
    }

    #[test]
    fn a_guest_that_has_not_been_read_yet_has_no_budget_to_name() {
        let world = world();

        assert_eq!(world.slack(), None);
        assert_eq!(world.release_budget(u64::MAX), None);
        assert_eq!(
            world.release_budget_after_reclaim(bytes(u64::MAX), 256 * MIB),
            None
        );
    }

    #[test]
    fn a_release_budget_keeps_the_guest_at_its_boot_memory() {
        let mut world = world();
        world.observe(snap(BOOT, CALM, 0));
        world.connect(Duration::ZERO, 0);

        assert_eq!(world.release_budget(256 * MIB), None);
    }

    #[test]
    fn a_release_budget_gives_back_what_the_floor_allows() {
        let world = grown(CALM);

        assert_eq!(world.release_budget(256 * MIB), Some(bytes(256 * MIB)));
        // 2560 total_mem against a 2048 floor leaves 512, whatever was asked for.
        assert_eq!(world.release_budget(1024 * MIB), Some(bytes(512 * MIB)));
    }

    #[test]
    fn a_release_budget_stops_at_the_memory_the_guest_could_reclaim() {
        let mut world = world();

        world.set_total_mem(2560 * MIB);
        world.connect(Duration::ZERO, 512 * MIB);

        world.observe(snap_available(2560 * MIB, 1200 * MIB, 640 * MIB, 0));
        assert_eq!(world.release_budget(512 * MIB), Some(bytes(128 * MIB)));

        world.observe(snap_available(2560 * MIB, 0, 512 * MIB, 0));
        assert_eq!(world.release_budget(512 * MIB), None);
    }

    #[test]
    fn a_reclaim_credits_the_budget_with_what_it_freed() {
        let mut world = world();

        world.set_total_mem(2560 * MIB);
        world.connect(Duration::ZERO, 512 * MIB);
        world.observe(snap_available(2560 * MIB, 2000 * MIB, 512 * MIB, 0));

        assert_eq!(world.release_budget(512 * MIB), None);
        assert_eq!(
            world.release_budget_after_reclaim(bytes(512 * MIB), 256 * MIB),
            Some(bytes(256 * MIB))
        );
    }

    #[test]
    fn a_release_budget_leaves_a_block_of_band() {
        let boot = BootInfo {
            block_size: bytes(BLOCK),
            total_mem: 4096 * MIB,
        };

        let mut world = World::new(config(), boot);
        world.set_total_mem(4096 * MIB);
        world.observe(snap(4096 * MIB, 0, 0));
        world.connect(Duration::ZERO, 4096 * MIB - 128 * MIB);

        let given = world
            .release_budget(u64::MAX)
            .expect("a block to give")
            .get();
        let left = 4096 * MIB - given;

        assert_eq!(left, SERVICE + BURST + BLOCK);
        assert_eq!(world.limits().reserves() + BLOCK, left);
    }

    #[test]
    fn a_release_budget_lands_on_a_block_boundary() {
        let world = grown(CALM);

        assert_eq!(world.release_budget(200 * MIB), Some(bytes(128 * MIB)));
    }

    #[test]
    fn ids_run_from_one_again_after_a_reconnect() {
        let mut world = world();
        world.connect(Duration::ZERO, 0);

        assert_eq!(world.take_id().get(), 1);
        assert_eq!(world.take_id().get(), 2);

        world.connect(Duration::ZERO, 0);

        assert_eq!(world.take_id().get(), 1);
    }
}
