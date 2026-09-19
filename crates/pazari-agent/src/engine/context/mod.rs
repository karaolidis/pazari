use std::{mem, num::NonZeroU64, time::Duration};

use pazari_proto::GuestMessage;

use crate::engine::{
    action::Action,
    config::EngineConfig,
    counters::CounterTracker,
    event::{BootInfo, Snapshot},
    idle::IdleTracker,
    limits::{Band, Limits},
    time::Timestamp,
};

#[derive(Debug, Clone)]
pub struct Context {
    pub config: EngineConfig,
    pub limits: Limits,
    pub idle: IdleTracker,
    pub last: Snapshot,
    pub connected: bool,

    boot_mem: u64,
    block_size: NonZeroU64,

    counters: CounterTracker,
    status_interval: Duration,
    next_status: Option<Timestamp>,
    next_id: u64,

    out: Vec<Action>,
}

impl Context {
    pub fn new(config: EngineConfig, boot: BootInfo) -> Self {
        Self {
            config,
            limits: Limits::new(&config, boot.plugged),
            idle: IdleTracker::default(),
            last: Snapshot {
                plugged: boot.plugged,
                ..Snapshot::default()
            },
            connected: false,
            boot_mem: boot.boot_mem.get(),
            block_size: boot.block_size,
            counters: CounterTracker::default(),
            status_interval: Duration::ZERO,
            next_status: None,
            next_id: 1,
            out: Vec::new(),
        }
    }

    pub fn emit(&mut self, action: Action) {
        self.out.push(action);
    }

    pub fn send(&mut self, message: GuestMessage) {
        if self.connected {
            self.out.push(Action::Send(message));
        }
    }

    pub fn nak(&mut self, id: u64, reason: &str) {
        self.send(GuestMessage::ReclaimNak {
            id,
            reason: reason.to_owned(),
        });
    }

    pub fn drain(&mut self) -> Vec<Action> {
        mem::take(&mut self.out)
    }

    pub fn apply_limits(&mut self, band: Band) {
        self.limits.apply(band, &mut self.out);
    }

    pub fn reapply_limits(&mut self) {
        self.limits.reapply(&mut self.out);
    }

    pub fn set_high(&mut self, value: u64) {
        self.limits.set_high(value, &mut self.out);
    }

    pub const fn connect(&mut self, status_interval: Duration) {
        self.connected = true;
        self.status_interval = status_interval;
        self.next_status = None;
        self.next_id = 1;
    }

    pub const fn disconnect(&mut self) {
        self.connected = false;
        self.next_status = None;
    }

    pub const fn take_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        id
    }

    pub fn observe(&mut self, snapshot: Snapshot) -> bool {
        let throttled = self
            .counters
            .observe(snapshot.high_events, snapshot.max_events, false);

        self.last = snapshot;

        throttled
    }

    pub fn observe_wake(&mut self, high_events: u64, max_events: u64) -> bool {
        self.counters.observe(high_events, max_events, true)
    }

    pub fn quiet_streak(&mut self, throttled: bool) -> bool {
        let usage = self.last.workload_usage;
        let budget = self.limits.budget();
        let threshold = self.config.idle_threshold;
        let intervals = self.config.idle_intervals;

        self.idle
            .note(throttled, usage, budget, threshold, intervals)
    }

    pub fn emit_status(&mut self, now: Timestamp) {
        if !self.connected || self.next_status.is_some_and(|at| now < at) {
            return;
        }

        self.next_status = Some(now.saturating_add(self.status_interval));

        let counters = self.counters.seen();

        self.send(GuestMessage::Status {
            plugged: self.limits.plugged(),
            available: self.last.available,
            high_events: counters.high,
            max_events: counters.max,
            psi_some10: self.last.psi_some10,
            psi_full10: self.last.psi_full10,
            swap_used: self.last.swap_used,
            workload_usage: self.last.workload_usage,
        });
    }

    pub const fn round_up(&self, value: u64) -> u64 {
        value
            .div_ceil(self.block_size.get())
            .saturating_mul(self.block_size.get())
    }

    pub const fn round_down(&self, value: u64) -> u64 {
        (value / self.block_size.get()).saturating_mul(self.block_size.get())
    }

    pub fn grow_target(&self) -> Option<u64> {
        let plugged = self.limits.plugged();

        let need = self
            .last
            .workload_usage
            .saturating_add(self.limits.service_reserve())
            .saturating_add(self.limits.burst_reserve());

        let deficit = need.saturating_sub(plugged);
        let step = deficit.max(self.config.grow_step.get());

        let mut target = self.round_up(plugged.saturating_add(step));

        if let Some(ceiling) = self.limits.ceiling()
            && target > ceiling
        {
            target = self.round_down(ceiling);
        }

        (target > plugged).then_some(target)
    }

    pub fn release_budget(&self, desired: u64) -> u64 {
        let plugged = self.limits.plugged();

        let floor = self
            .boot_mem
            .max(self.limits.reserves().saturating_add(self.block_size.get()));

        let by_floor = plugged.saturating_sub(floor);

        #[expect(
            clippy::cast_precision_loss,
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "byte counts stay far below the f64 mantissa, and the cast back saturates"
        )]
        let unpressured =
            (self.last.workload_usage as f64 / self.config.idle_threshold.get()) as u64;

        let by_pressure =
            plugged.saturating_sub(unpressured.saturating_add(self.limits.service_reserve()));

        self.round_down(desired.min(by_floor).min(by_pressure))
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU64;

    use super::*;
    use crate::engine::{
        BootInfo,
        testing::{BLOCK, BOOT, BURST, CALM, MIB, SERVICE, boot, config, snap},
    };

    fn context() -> Context {
        Context::new(config(), boot())
    }

    fn grown(usage: u64) -> Context {
        let mut ctx = context();

        ctx.limits.set_plugged(2560 * MIB);
        ctx.observe(snap(2560 * MIB, usage, 0));

        ctx
    }

    #[test]
    fn a_grow_target_asks_for_a_whole_step_when_the_workload_fits() {
        let mut ctx = context();
        ctx.observe(snap(BOOT, CALM, 0));

        assert_eq!(ctx.grow_target(), Some(2560 * MIB));
    }

    #[test]
    fn a_grow_target_covers_a_deficit_bigger_than_the_step() {
        let mut ctx = context();
        ctx.observe(snap(BOOT, 2200 * MIB, 0));

        // 2200 + 256 + 256 wanted, 2048 plugged: a 664 MiB deficit, rounded up
        // to the next 128 MiB block.
        assert_eq!(ctx.grow_target(), Some(2816 * MIB));
    }

    #[test]
    fn a_grow_target_is_clamped_and_aligned_under_the_ceiling() {
        let mut ctx = context();
        ctx.observe(snap(BOOT, CALM, 0));
        ctx.limits.raise_ceiling(2200 * MIB);

        assert_eq!(ctx.grow_target(), Some(2176 * MIB));
    }

    #[test]
    fn a_grow_target_runs_out_at_a_ceiling_on_the_plug() {
        let mut ctx = context();
        ctx.observe(snap(BOOT, CALM, 0));
        ctx.limits.raise_ceiling(BOOT);

        assert_eq!(ctx.grow_target(), None);
    }

    #[test]
    fn a_release_budget_keeps_the_guest_at_its_boot_memory() {
        let mut ctx = context();
        ctx.observe(snap(BOOT, CALM, 0));

        assert_eq!(ctx.release_budget(256 * MIB), 0);
    }

    #[test]
    fn a_release_budget_gives_back_what_the_floor_allows() {
        let ctx = grown(CALM);

        assert_eq!(ctx.release_budget(256 * MIB), 256 * MIB);
        // 2560 plugged against a 2048 floor leaves 512, whatever was asked for.
        assert_eq!(ctx.release_budget(1024 * MIB), 512 * MIB);
    }

    #[test]
    fn a_release_budget_stops_where_the_workload_would_feel_it() {
        // 1200 MiB at a deadband of one half wants 2400 MiB to sit in, which
        // the service reserve pushes past the plug.
        let ctx = grown(1200 * MIB);

        assert_eq!(ctx.release_budget(256 * MIB), 0);
    }

    #[test]
    fn a_release_budget_leaves_a_block_of_band() {
        let boot = BootInfo {
            boot_mem: NonZeroU64::new(128 * MIB).expect("a positive byte count"),
            block_size: NonZeroU64::new(BLOCK).expect("a positive byte count"),
            plugged: 4096 * MIB,
        };

        let mut ctx = Context::new(config(), boot);
        ctx.limits.set_plugged(4096 * MIB);
        ctx.observe(snap(4096 * MIB, 0, 0));

        let given = ctx.release_budget(u64::MAX);
        let left = 4096 * MIB - given;

        assert_eq!(left, SERVICE + BURST + BLOCK);
        assert_eq!(ctx.limits.reserves() + BLOCK, left);
    }

    #[test]
    fn a_release_budget_lands_on_a_block_boundary() {
        let ctx = grown(CALM);

        assert_eq!(ctx.release_budget(200 * MIB), 128 * MIB);
    }
}
