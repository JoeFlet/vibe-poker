use rand::{rngs::SmallRng, Rng, SeedableRng};
use smallvec::SmallVec;

use poker_engine::agent::{Agent, Observation};
use poker_engine::game::{Action, EngineEvent, HandId, LegalActions};
use crate::solver::action::AbstractAction;
use crate::solver::info_set::InfoSet;

/// Probability distribution over `AbstractAction`. Indexed by variant.
///
/// Illegal actions must have probability `0.0` in any well-formed `ActionProbs`;
/// legal probabilities must sum to `1.0` (within float tolerance).
#[derive(Clone, Copy, Debug)]
pub struct ActionProbs {
    pub probs: [f32; AbstractAction::COUNT],
}

impl ActionProbs {
    pub fn zero() -> Self {
        ActionProbs { probs: [0.0; AbstractAction::COUNT] }
    }

    /// Uniform distribution over the provided legal subset. The caller must
    /// supply at least one legal action.
    pub fn uniform_over(legal: &[AbstractAction]) -> Self {
        debug_assert!(!legal.is_empty(), "at least one legal action required");
        let p = 1.0 / legal.len() as f32;
        let mut probs = [0.0f32; AbstractAction::COUNT];
        for &a in legal {
            probs[a.index()] = p;
        }
        ActionProbs { probs }
    }

    /// Sample an action from this distribution.
    pub fn sample(&self, rng: &mut SmallRng) -> AbstractAction {
        let r: f32 = rng.r#gen();
        let mut acc = 0.0;
        for a in AbstractAction::ALL {
            acc += self.probs[a.index()];
            if r <= acc {
                return a;
            }
        }
        // Float drift; return the last non-zero legal action.
        AbstractAction::ALL
            .iter()
            .rev()
            .copied()
            .find(|a| self.probs[a.index()] > 0.0)
            .expect("ActionProbs had no legal action")
    }
}

/// A policy that returns action probabilities given an info set and the
/// engine's legal-action mask. Implementors range from a uniform random
/// baseline (`UniformRandomStrategy`) to trained MCCFR blueprints.
pub trait StrategyAgent: Send + Sync {
    fn strategy(&self, info: &InfoSet, legal: &LegalActions) -> ActionProbs;
}

/// Set of abstract actions that are legal given `LegalActions`. Fold is
/// excluded when the player can check — folding a free check is strictly
/// dominated and a uniform random policy should not burn probability on it.
pub fn legal_abstract_actions(legal: &LegalActions) -> SmallVec<[AbstractAction; 4]> {
    let mut out: SmallVec<[AbstractAction; 4]> = SmallVec::new();
    if !legal.can_check {
        out.push(AbstractAction::Fold);
    }
    if legal.can_check || legal.can_call {
        out.push(AbstractAction::Call);
    }
    if legal.can_raise {
        out.push(AbstractAction::Raise);
    }
    if legal.all_in_amount > 0 {
        out.push(AbstractAction::AllIn);
    }
    out
}

/// Concretize an abstract action into an engine `Action` given the current
/// observation. `Raise` is interpreted as a pot-sized raise: total bet =
/// `current_bet + (pot + street bets)`, clamped to `[min_raise, max_raise]`.
///
/// If the abstract action is not legal in context (e.g. `Raise` when
/// `!can_raise`), it falls back to the safest legal alternative (Call or
/// Check). Trained strategies should set probability 0 on illegal actions;
/// the fallback is a defensive last line.
pub fn concretize(action: AbstractAction, obs: &Observation<'_>) -> Action {
    let legal = &obs.legal_actions;
    match action {
        AbstractAction::Fold => {
            if legal.can_check { Action::Check } else { Action::Fold }
        }
        AbstractAction::Call => {
            if legal.can_check { Action::Check } else { Action::Call }
        }
        AbstractAction::Raise => {
            if !legal.can_raise {
                return if legal.can_check { Action::Check } else { Action::Call };
            }
            let street_bets: u32 = obs.players.iter().map(|p| p.bet_this_street).sum();
            let pot_after_call = obs.pot.total() + street_bets;
            let target = legal
                .min_raise
                .saturating_add(pot_after_call)
                .min(legal.max_raise)
                .max(legal.min_raise);
            Action::Raise(target)
        }
        AbstractAction::AllIn => {
            if legal.all_in_amount > 0 { Action::AllIn }
            else if legal.can_check { Action::Check }
            else { Action::Call }
        }
    }
}

/// Engine `Agent` adapter over a `StrategyAgent`. Tracks the abstract action
/// history across the hand so the solver sees a consistent info-set key.
pub struct StrategyAdapter<S: StrategyAgent> {
    pub strategy: S,
    rng: SmallRng,
    history: SmallVec<[AbstractAction; 12]>,
}

impl<S: StrategyAgent> StrategyAdapter<S> {
    pub fn new(strategy: S, seed: u64) -> Self {
        StrategyAdapter {
            strategy,
            rng: SmallRng::seed_from_u64(seed),
            history: SmallVec::new(),
        }
    }

    /// The abstract action history recorded during the current hand. Cleared
    /// at `on_hand_start`. Useful for tests and for solver training code that
    /// wants to inspect the trajectory the adapter produced.
    pub fn history(&self) -> &[AbstractAction] {
        &self.history
    }
}

impl<S: StrategyAgent> Agent for StrategyAdapter<S> {
    fn act(&mut self, obs: &Observation<'_>) -> Action {
        let info = InfoSet::from_observation(obs, &self.history);
        let probs = self.strategy.strategy(&info, &obs.legal_actions);
        let abstract_action = probs.sample(&mut self.rng);
        // Note: history is not pushed here. Every seat (including us) receives
        // an `ActionTaken` event after the action is applied; we push from
        // there so the history reflects the full *public* action sequence.
        concretize(abstract_action, obs)
    }

    fn on_hand_start(&mut self, _hand_id: HandId) {
        self.history.clear();
    }

    fn on_event(&mut self, event: &EngineEvent) {
        if let EngineEvent::ActionTaken { action, .. } = event {
            self.history.push(AbstractAction::from_action(*action));
        }
    }
}

/// Minimal `StrategyAgent` for scaffolding: uniform over legal abstract
/// actions at every info set. Useful as a smoke test and as a starting
/// strategy for MCCFR regret minimization.
pub struct UniformRandomStrategy;

impl StrategyAgent for UniformRandomStrategy {
    fn strategy(&self, _info: &InfoSet, legal: &LegalActions) -> ActionProbs {
        let actions = legal_abstract_actions(legal);
        ActionProbs::uniform_over(&actions)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use poker_engine::agent::Agent;
    use poker_engine::core::NaiveEvaluator;
    use poker_engine::game::{BettingRules, Engine, NullSink};

    #[test]
    fn uniform_over_selects_only_legal_actions() {
        let actions = [AbstractAction::Call, AbstractAction::Raise];
        let probs = ActionProbs::uniform_over(&actions);
        assert!((probs.probs[AbstractAction::Call.index()] - 0.5).abs() < 1e-6);
        assert!((probs.probs[AbstractAction::Raise.index()] - 0.5).abs() < 1e-6);
        assert_eq!(probs.probs[AbstractAction::Fold.index()], 0.0);
        assert_eq!(probs.probs[AbstractAction::AllIn.index()], 0.0);
    }

    #[test]
    fn strategy_adapter_plays_a_hand_without_panic() {
        let engine = Engine::new(BettingRules::no_limit_holdem(1, 2, 3), NaiveEvaluator);
        let stacks = [200u32; 3];
        let mut agents: Vec<Box<dyn Agent>> = vec![
            Box::new(StrategyAdapter::new(UniformRandomStrategy, 1)),
            Box::new(StrategyAdapter::new(UniformRandomStrategy, 2)),
            Box::new(StrategyAdapter::new(UniformRandomStrategy, 3)),
        ];
        let result = engine.run_hand(1, 42, &stacks, 0, None, &mut agents, &mut NullSink);
        let sum: i32 = result.seats.iter().map(|s| s.chip_delta).sum();
        assert_eq!(sum, 0, "chip conservation broken by StrategyAdapter");
    }

    #[test]
    fn history_clears_between_hands() {
        let strategy = UniformRandomStrategy;
        let mut adapter = StrategyAdapter::new(strategy, 7);
        adapter.history.push(AbstractAction::Call);
        adapter.history.push(AbstractAction::Raise);
        assert_eq!(adapter.history().len(), 2);
        adapter.on_hand_start(1);
        assert_eq!(adapter.history().len(), 0);
    }
}
