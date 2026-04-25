use rand::rngs::SmallRng;
use rand::{Rng, SeedableRng};

use super::{Agent, Observation};
use crate::game::Action;

/// Acts uniformly at random over the legal action set.
pub struct RandomAgent {
    rng: SmallRng,
}

impl RandomAgent {
    pub fn new(seed: u64) -> Self {
        RandomAgent { rng: SmallRng::seed_from_u64(seed) }
    }
}

impl Agent for RandomAgent {
    fn act(&mut self, obs: &Observation<'_>) -> Action {
        let la = &obs.legal_actions;
        let mut choices: [Action; 5] = [Action::Fold; 5];
        let mut n = 0;

        // Never fold when a free check is available — it's strictly dominated.
        if !la.can_check {
            choices[n] = Action::Fold;
            n += 1;
        }

        if la.can_check {
            choices[n] = Action::Check;
            n += 1;
        }
        if la.can_call {
            choices[n] = Action::Call;
            n += 1;
        }
        if la.can_raise {
            choices[n] = Action::Raise(la.min_raise);
            n += 1;
        }
        if la.all_in_amount > 0 {
            choices[n] = Action::AllIn;
            n += 1;
        }

        choices[self.rng.gen_range(0..n)]
    }
}

/// Always calls (or checks if free).
pub struct CallingStation;

impl Agent for CallingStation {
    fn act(&mut self, obs: &Observation<'_>) -> Action {
        let la = &obs.legal_actions;
        if la.can_check {
            Action::Check
        } else {
            Action::Call
        }
    }
}

/// Follows a predetermined script of actions. Panics if the script runs out.
/// Useful for deterministic unit tests.
pub struct ScriptedAgent {
    script: Vec<Action>,
    cursor: usize,
}

impl ScriptedAgent {
    pub fn new(script: Vec<Action>) -> Self {
        ScriptedAgent { script, cursor: 0 }
    }
}

impl Agent for ScriptedAgent {
    fn act(&mut self, _obs: &Observation<'_>) -> Action {
        assert!(self.cursor < self.script.len(), "ScriptedAgent ran out of actions");
        let action = self.script[self.cursor];
        self.cursor += 1;
        action
    }
}
