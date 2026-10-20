use std::{
    future,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use pazari_types::Clock;
use tokio::task::JoinHandle;

use crate::{
    engine::Event,
    runtime::{
        cgroup::{Cgroup, Reclaimed},
        config::ReclaimConfig,
        error::RuntimeError,
    },
};

/// Stops a running reclaim from outside the task that awaits it.
#[derive(Debug, Clone)]
pub struct ReclaimHandle {
    cancelled: Arc<AtomicBool>,
}

impl ReclaimHandle {
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }
}

#[derive(Debug)]
struct Run {
    bytes: u64,
    handle: ReclaimHandle,
    task: JoinHandle<Result<u64, RuntimeError>>,
}

/// Runs `memory.reclaim` one release at a time.
#[derive(Debug)]
pub struct Reclaimer<C> {
    cgroup: Arc<C>,
    config: ReclaimConfig,
    clock: Clock,
    run: Option<Run>,
}

impl<C: Cgroup + 'static> Reclaimer<C> {
    #[must_use]
    pub const fn new(cgroup: Arc<C>, config: ReclaimConfig, clock: Clock) -> Self {
        Self {
            cgroup,
            config,
            clock,
            run: None,
        }
    }

    #[must_use]
    pub const fn is_running(&self) -> bool {
        self.run.is_some()
    }

    #[must_use]
    pub fn handle(&self) -> Option<ReclaimHandle> {
        self.run.as_ref().map(|run| run.handle.clone())
    }

    pub fn start(&mut self, bytes: u64) -> Result<ReclaimHandle, RuntimeError> {
        if let Some(run) = &self.run {
            return Err(RuntimeError::ReclaimInFlight {
                bytes,
                running: run.bytes,
            });
        }

        let handle = ReclaimHandle {
            cancelled: Arc::new(AtomicBool::new(false)),
        };

        let cgroup = Arc::clone(&self.cgroup);
        let cancelled = Arc::clone(&handle.cancelled);
        let config = self.config;

        let task = tokio::spawn(async move { run(&*cgroup, bytes, config, &cancelled).await });

        self.run = Some(Run {
            bytes,
            handle: handle.clone(),
            task,
        });

        Ok(handle)
    }

    pub async fn finished(&mut self) -> Result<Event, RuntimeError> {
        let joined = match self.run.as_mut() {
            Some(run) => (&mut run.task).await,
            None => future::pending().await,
        };

        self.run = None;

        let reclaimed = joined.map_err(|source| RuntimeError::ReclaimLost { source })??;

        Ok(Event::ReclaimFinished {
            now: self.clock.now(),
            reclaimed,
        })
    }
}

/// Walks `bytes` in requests of at most one chunk.
async fn run<C: Cgroup>(
    cgroup: &C,
    bytes: u64,
    config: ReclaimConfig,
    cancelled: &AtomicBool,
) -> Result<u64, RuntimeError> {
    let mut reclaimed = 0_u64;
    let mut left = bytes;

    while left > 0 && !cancelled.load(Ordering::Relaxed) {
        let ask = left.min(config.chunk.get());

        if cgroup.reclaim(ask, config.swappiness).await? == Reclaimed::Partial {
            break;
        }

        reclaimed = reclaimed.saturating_add(ask);
        left = left.saturating_sub(ask);
    }

    Ok(reclaimed)
}

#[cfg(test)]
mod tests {
    use pazari_types::Swappiness;
    use tokio::task;

    use super::*;
    use crate::runtime::testing::{Answer, Call, FakeCgroup, MIB, bytes};

    const CHUNKS: ReclaimConfig = ReclaimConfig {
        chunk: bytes(64 * MIB),
        swappiness: None,
    };

    fn reclaimer(cgroup: &Arc<FakeCgroup>) -> Reclaimer<FakeCgroup> {
        Reclaimer::new(Arc::clone(cgroup), CHUNKS, Clock::start())
    }

    fn reclaimed(event: &Event) -> u64 {
        let Event::ReclaimFinished { reclaimed, .. } = *event else {
            panic!("the reclaimer emitted {event:?}");
        };

        reclaimed
    }

    fn asked(cgroup: &FakeCgroup) -> Vec<u64> {
        cgroup
            .calls()
            .into_iter()
            .filter_map(|call| match call {
                Call::Reclaim { bytes, .. } => Some(bytes),
                _ => None,
            })
            .collect()
    }

    async fn step(cgroup: &FakeCgroup, calls: usize) {
        for _ in 0..16 {
            if cgroup.reclaims() >= calls {
                return;
            }

            task::yield_now().await;
        }

        panic!("the reclaim never reached {calls} chunks");
    }

    #[tokio::test]
    async fn a_release_is_split_into_chunks_and_the_last_one_is_the_remainder() {
        let cgroup = Arc::new(FakeCgroup::default());
        let mut reclaimer = reclaimer(&cgroup);

        reclaimer.start(200 * MIB).unwrap();

        assert_eq!(reclaimed(&reclaimer.finished().await.unwrap()), 200 * MIB);
        assert_eq!(asked(&cgroup), vec![64 * MIB, 64 * MIB, 64 * MIB, 8 * MIB]);
    }

    #[tokio::test]
    async fn a_release_of_nothing_writes_nothing() {
        let cgroup = Arc::new(FakeCgroup::default());
        let mut reclaimer = reclaimer(&cgroup);

        reclaimer.start(0).unwrap();

        assert_eq!(reclaimed(&reclaimer.finished().await.unwrap()), 0);
        assert_eq!(asked(&cgroup), Vec::<u64>::new());
    }

    #[tokio::test]
    async fn the_swappiness_the_policy_wants_reaches_every_chunk() {
        let cgroup = Arc::new(FakeCgroup::default());
        let mut reclaimer = Reclaimer::new(
            Arc::clone(&cgroup),
            ReclaimConfig {
                swappiness: Some(Swappiness::NONE),
                ..CHUNKS
            },
            Clock::start(),
        );

        reclaimer.start(128 * MIB).unwrap();
        reclaimer.finished().await.unwrap();

        assert_eq!(
            cgroup.calls(),
            vec![
                Call::Reclaim {
                    bytes: 64 * MIB,
                    swappiness: Some(Swappiness::NONE),
                },
                Call::Reclaim {
                    bytes: 64 * MIB,
                    swappiness: Some(Swappiness::NONE),
                },
            ]
        );
    }

    #[tokio::test]
    async fn a_partial_chunk_ends_the_release_and_counts_for_nothing() {
        let cgroup = Arc::new(FakeCgroup::scripted([
            Answer::Whole,
            Answer::Whole,
            Answer::Partial,
        ]));
        let mut reclaimer = reclaimer(&cgroup);

        reclaimer.start(256 * MIB).unwrap();

        assert_eq!(reclaimed(&reclaimer.finished().await.unwrap()), 128 * MIB);
        assert_eq!(asked(&cgroup), vec![64 * MIB, 64 * MIB, 64 * MIB]);
    }

    #[tokio::test]
    async fn a_cancelled_release_still_reports_what_it_freed() {
        let cgroup = Arc::new(FakeCgroup::default());
        let mut reclaimer = reclaimer(&cgroup);

        let handle = reclaimer.start(512 * MIB).unwrap();

        step(&cgroup, 2).await;
        handle.cancel();

        assert!(handle.is_cancelled());
        assert_eq!(reclaimed(&reclaimer.finished().await.unwrap()), 128 * MIB);
        assert_eq!(asked(&cgroup), vec![64 * MIB, 64 * MIB]);
    }

    #[tokio::test]
    async fn a_second_release_is_refused_while_the_first_runs() {
        let cgroup = Arc::new(FakeCgroup::default());
        let mut reclaimer = reclaimer(&cgroup);

        reclaimer.start(512 * MIB).unwrap();
        assert!(reclaimer.is_running());

        let error = reclaimer
            .start(64 * MIB)
            .expect_err("one release at a time");

        assert_eq!(
            error.to_string(),
            "asked to reclaim 67108864 bytes while a reclaim of 536870912 bytes is still running"
        );

        reclaimer.finished().await.unwrap();

        assert!(!reclaimer.is_running());
        assert!(reclaimer.handle().is_none());
        reclaimer.start(64 * MIB).expect("the first is done");
    }

    #[tokio::test]
    async fn a_failing_write_ends_the_release_loudly() {
        let cgroup = Arc::new(FakeCgroup::scripted([Answer::Whole, Answer::Gone]));
        let mut reclaimer = reclaimer(&cgroup);

        reclaimer.start(256 * MIB).unwrap();

        let error = reclaimer
            .finished()
            .await
            .expect_err("the cgroup went away");

        assert!(
            error.to_string().contains("memory.reclaim"),
            "unexpected error: {error}"
        );
        assert!(!reclaimer.is_running());
    }
}
