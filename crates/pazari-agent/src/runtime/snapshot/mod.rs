use std::sync::Arc;

use pazari_types::{Clock, Interval};
use tokio::time::{self, MissedTickBehavior};

use crate::{
    engine::{Event, Snapshot},
    runtime::{cgroup::Cgroup, error::RuntimeError, topology::Topology},
};

/// Composes the engine's view of the guest out of the workload cgroup and
/// `/proc/meminfo`.
#[derive(Debug)]
pub struct Snapshots<C> {
    cgroup: Arc<C>,
    topology: Topology,
}

impl<C: Cgroup> Snapshots<C> {
    #[must_use]
    pub const fn new(cgroup: Arc<C>, topology: Topology) -> Self {
        Self { cgroup, topology }
    }

    pub async fn take(&self) -> Result<Snapshot, RuntimeError> {
        let meminfo = self.topology.meminfo().await?;
        let events = self.cgroup.events().await?;
        let pressure = self.cgroup.pressure().await?;

        Ok(Snapshot {
            plugged: meminfo.total,
            available: meminfo.available,
            workload_usage: self.cgroup.current().await?,
            swap_used: self.cgroup.swap_current().await?,
            high_events: events.high,
            max_events: events.max,
            psi_some10: pressure.some10,
            psi_full10: pressure.full10,
        })
    }
}

/// Reads the guest on a cadence.
#[derive(Debug)]
pub struct Ticker<C> {
    snapshots: Snapshots<C>,
    interval: time::Interval,
    clock: Clock,
}

impl<C: Cgroup> Ticker<C> {
    #[must_use]
    pub fn new(snapshots: Snapshots<C>, period: Interval, clock: Clock) -> Self {
        let mut interval = time::interval(period.get());
        interval.set_missed_tick_behavior(MissedTickBehavior::Delay);

        Self {
            snapshots,
            interval,
            clock,
        }
    }

    pub async fn tick(&mut self) -> Result<Event, RuntimeError> {
        self.interval.tick().await;

        Ok(Event::Tick {
            now: self.clock.now(),
            snapshot: self.snapshots.take().await?,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, time::Duration};

    use tempfile::TempDir;

    use super::*;
    use crate::runtime::{
        cgroup::{MemoryEvents, Pressure},
        config::Paths,
        testing::{FakeCgroup, MEMINFO, Reads},
    };

    fn topology(dir: &TempDir) -> Topology {
        let paths = Paths::under(dir.path());

        fs::create_dir_all(paths.meminfo.parent().unwrap()).unwrap();
        fs::write(&paths.meminfo, MEMINFO).unwrap();

        Topology::new(&paths)
    }

    fn reads() -> Reads {
        Reads {
            events: MemoryEvents { high: 37, max: 2 },
            current: 1_610_612_736,
            swap: 65_536,
            pressure: Pressure {
                some10: 12.5,
                full10: 0.25,
            },
        }
    }

    #[tokio::test]
    async fn a_snapshot_is_the_cgroup_and_meminfo_side_by_side() {
        let dir = TempDir::new().unwrap();
        let cgroup = Arc::new(FakeCgroup::new(reads()));

        let snapshot = Snapshots::new(cgroup, topology(&dir)).take().await.unwrap();

        assert_eq!(
            snapshot,
            Snapshot {
                plugged: 2_096_756 * 1024,
                available: 1_729_396 * 1024,
                workload_usage: 1_610_612_736,
                swap_used: 65_536,
                high_events: 37,
                max_events: 2,
                psi_some10: 12.5,
                psi_full10: 0.25,
            }
        );
    }

    #[tokio::test]
    async fn the_first_tick_is_due_at_once() {
        let dir = TempDir::new().unwrap();
        let cgroup = Arc::new(FakeCgroup::new(reads()));
        let snapshots = Snapshots::new(cgroup, topology(&dir));

        let period = Interval::try_new(Duration::from_secs(3600)).unwrap();
        let mut ticker = Ticker::new(snapshots, period, Clock::start());

        let event = ticker.tick().await.unwrap();

        let Event::Tick { snapshot, .. } = event else {
            panic!("the ticker emitted {event:?}");
        };

        assert_eq!(snapshot.workload_usage, 1_610_612_736);
    }
}
