use pazari_types::Timestamp;

use crate::engine::{
    context::Context,
    phase::{Phase, State, steady::Steady},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Titrating {
    next_step: Option<Timestamp>,
}

impl Titrating {
    fn step(ctx: &mut Context, now: Timestamp) -> Phase {
        let floor = ctx.limits.steady_high();

        let quiet = !ctx.idle.throttled()
            && f64::from(ctx.last.psi_some10) < ctx.config.titrate_psi_threshold.get();

        if quiet {
            let next = ctx
                .limits
                .high()
                .saturating_sub(ctx.config.titrate_step.get())
                .max(floor);

            ctx.set_high(next);

            if next <= floor {
                return Steady::from_titration(ctx);
            }
        }

        Self::scheduled(ctx, now)
    }

    fn scheduled(ctx: &Context, now: Timestamp) -> Phase {
        Phase::Titrating(Self {
            next_step: Some(now.saturating_add(ctx.config.titrate_interval.get())),
        })
    }
}

impl State for Titrating {
    fn stay(self) -> Phase {
        Phase::Titrating(self)
    }

    fn busy(self) -> &'static str {
        "still titrating towards the workload's working set"
    }

    fn advance(self, ctx: &mut Context, now: Timestamp) -> Phase {
        match self.next_step {
            None => Self::scheduled(ctx, now),
            Some(at) if now >= at => Self::step(ctx, now),
            Some(_) => self.stay(),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::engine::{Action, Engine, Mode, testing::*};

    #[test]
    fn titration_descends_one_step_per_interval() {
        let (mut engine, _) = Engine::try_new(config(), boot()).expect("a workable boot");

        assert_eq!(tick(&mut engine, at(0), snap(BOOT, BUSY, 0)), vec![]);

        for (step, expected) in [1728_u64, 1664, 1600, 1536].into_iter().enumerate() {
            let now = at((step as u64 + 1) * 10);
            assert_eq!(
                tick(&mut engine, now, snap(BOOT, BUSY, 0)),
                vec![Action::SetHigh(expected * MIB)]
            );
        }

        assert_eq!(engine.mode(), Mode::Steady);
        assert_eq!(engine.high(), HIGH0);
        assert_eq!(engine.max(), MAX0);
    }

    #[test]
    fn titration_holds_while_psi_is_over_the_threshold() {
        let (mut engine, _) = Engine::try_new(config(), boot()).expect("a workable boot");

        tick(&mut engine, at(0), snap_psi(BOOT, BUSY, 50.0));

        assert_eq!(
            tick(&mut engine, at(10), snap_psi(BOOT, BUSY, 50.0)),
            vec![]
        );
        assert_eq!(engine.mode(), Mode::Titrating);
        assert_eq!(engine.high(), MAX0);

        assert_eq!(
            tick(&mut engine, at(20), snap_psi(BOOT, BUSY, 1.0)),
            vec![Action::SetHigh(1728 * MIB)]
        );
    }

    #[test]
    fn a_throttle_aborts_titration_and_takes_the_brake() {
        let (mut engine, _) = Engine::try_new(config(), boot()).expect("a workable boot");

        connect(&mut engine, at(0), CEILING, HOUR);
        tick(&mut engine, at(0), snap(BOOT, BUSY, 0));
        tick(&mut engine, at(10), snap(BOOT, BUSY, 0));
        assert_eq!(engine.high(), 1728 * MIB);

        assert_eq!(
            wake(&mut engine, at(15), 1),
            vec![request(1, BOOT, 2560 * MIB)]
        );
        assert_eq!(engine.mode(), Mode::AwaitingGrant);
        assert!(engine.band_open());
        assert_eq!(engine.high(), OPEN0);

        assert_eq!(tick(&mut engine, at(20), snap(BOOT, BUSY, 1)), vec![]);
    }
}
