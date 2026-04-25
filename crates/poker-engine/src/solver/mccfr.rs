//! External-sampling Monte Carlo CFR for heads-up No-Limit Hold'em.
//!
//! One training iteration plays one hand per traverser (so two traversals
//! for heads-up) through `GameTree`. At the traverser's decision nodes we
//! enumerate all legal abstract actions, clone the tree down each branch,
//! and accumulate counterfactual regret. At the opponent's decision nodes
//! we sample one action from their current regret-matched strategy.
//!
//! Multi-player (3+) games are intentionally out of scope for this first
//! trainer: CFR in multi-player poker converges only to a correlated
//! equilibrium and the bookkeeping is substantially more complex. The
//! exploit layer is where multi-player play will eventually be handled, on
//! top of a heads-up blueprint.

use rand::{rngs::SmallRng, SeedableRng};
use smallvec::SmallVec;

use crate::core::HandEvaluator;
use crate::game::{
    tree::{GameTree, NodeKind},
    Engine, NullSink, SeatIndex,
};
use crate::solver::action::AbstractAction;
use crate::solver::info_set::InfoSet;
use crate::solver::regret::{regret_matching, RegretTable};
use crate::solver::strategy::{concretize, legal_abstract_actions, ActionProbs};

/// External-sampling MCCFR trainer for 2-player games.
pub struct MccfrTrainer {
    pub table: RegretTable,
    rng: SmallRng,
}

impl MccfrTrainer {
    pub fn new(seed: u64) -> Self {
        Self {
            table: RegretTable::new(),
            rng: SmallRng::seed_from_u64(seed),
        }
    }

    /// Run one training iteration. Plays a hand from each traverser's
    /// perspective (two traversals, sharing the regret table).
    ///
    /// `deck_seed` determines the cards for this iteration's hand.
    /// `stacks` must be length 2. `dealer` must be 0 or 1.
    pub fn iterate<E: HandEvaluator>(
        &mut self,
        engine: &Engine<E>,
        stacks: &[u32; 2],
        dealer: SeatIndex,
        deck_seed: u64,
    ) {
        debug_assert!(dealer < 2);
        for traverser in 0..2 {
            let tree = GameTree::new(
                engine,
                0, // hand_id unused by trainer
                deck_seed,
                stacks,
                dealer,
                None,
                &mut NullSink,
            );
            let mut history: SmallVec<[AbstractAction; 12]> = SmallVec::new();
            let _ = self.traverse(engine, tree, traverser, &mut history);
        }
    }

    /// Recursive external-sampling traversal. Returns the traverser's
    /// expected utility at the current node.
    fn traverse<E: HandEvaluator>(
        &mut self,
        engine: &Engine<E>,
        mut tree: GameTree,
        traverser: SeatIndex,
        history: &mut SmallVec<[AbstractAction; 12]>,
    ) -> f32 {
        match tree.current() {
            NodeKind::Terminal => {
                // Utility is the traverser's raw pot award minus what they
                // put in (net chip P/L). GameTree utilities are raw awards
                // from pot; subtract total_committed for net.
                let utils = tree.utilities().expect("terminal utilities available");
                let committed = tree.state.players[traverser].total_committed as i64;
                (utils[traverser] - committed) as f32
            }
            NodeKind::Decision { seat } => {
                let obs_owned = tree
                    .observation_owned(engine)
                    .expect("decision node must have an observation");
                let obs = obs_owned.as_observation();
                let info = InfoSet::from_observation(&obs, history);
                let legal = legal_abstract_actions(&obs_owned.legal_actions);

                // Snapshot regret to compute current-iteration strategy.
                // Drop the borrow before recursing — recursion mutates the table.
                let strategy: ActionProbs = {
                    let entry = self.table.entry_mut(&info);
                    entry.visits = entry.visits.saturating_add(1);
                    regret_matching(&entry.regret, &legal)
                };

                if seat == traverser {
                    // Enumerate all legal abstract actions.
                    let mut branch_values = [0.0f32; AbstractAction::COUNT];
                    let mut node_value = 0.0f32;
                    for &a in &legal {
                        let concrete = concretize(a, &obs);
                        let mut branch = tree.clone();
                        branch.apply_action(engine, concrete, &mut NullSink);
                        let mut branch_history = history.clone();
                        branch_history.push(a);
                        let v = self.traverse(engine, branch, traverser, &mut branch_history);
                        branch_values[a.index()] = v;
                        node_value += strategy.probs[a.index()] * v;
                    }

                    // Update regret and strategy-sum for the traverser.
                    let entry = self.table.entry_mut(&info);
                    for &a in &legal {
                        let regret_incr = branch_values[a.index()] - node_value;
                        entry.regret[a.index()] += regret_incr;
                        entry.strategy_sum[a.index()] += strategy.probs[a.index()];
                    }

                    node_value
                } else {
                    // Opponent: sample one action from their current strategy.
                    let sampled = strategy.sample(&mut self.rng);
                    let concrete = concretize(sampled, &obs);
                    tree.apply_action(engine, concrete, &mut NullSink);
                    history.push(sampled);
                    self.traverse(engine, tree, traverser, history)
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::NaiveEvaluator;
    use crate::agent::Agent;
    use crate::game::{BettingRules, NullSink};
    use crate::solver::regret::BlueprintStrategy;
    use crate::solver::strategy::{StrategyAdapter, UniformRandomStrategy};

    fn mk_engine() -> Engine<NaiveEvaluator> {
        Engine::new(BettingRules::no_limit_holdem(1, 2, 0), NaiveEvaluator)
    }

    #[test]
    fn single_iteration_populates_table() {
        let engine = mk_engine();
        let mut trainer = MccfrTrainer::new(7);
        let stacks = [200u32, 200];
        trainer.iterate(&engine, &stacks, 0, 1);
        assert!(trainer.table.len() > 0, "at least one info set should be visited");
    }

    #[test]
    fn many_iterations_run_without_panic() {
        let engine = mk_engine();
        let mut trainer = MccfrTrainer::new(11);
        let stacks = [200u32, 200];
        for i in 0..200u64 {
            let dealer = (i % 2) as SeatIndex;
            trainer.iterate(&engine, &stacks, dealer, i.wrapping_add(1));
        }
        assert!(trainer.table.len() > 10, "expected many info sets after training");
    }

    #[test]
    fn trained_blueprint_beats_uniform_random_over_many_hands() {
        // Crude convergence smoke test: train a modest number of iterations,
        // then play the resulting blueprint head-to-head against uniform
        // random over many hands. We expect the blueprint to show a
        // positive chip EV — even a poorly-trained policy should do better
        // than uniform random by, at minimum, folding junk hands less
        // stupidly and not burning chips on arbitrary all-ins.
        let engine = mk_engine();
        let mut trainer = MccfrTrainer::new(42);
        let stacks = [200u32, 200];
        for i in 0..1_000u64 {
            let dealer = (i % 2) as SeatIndex;
            trainer.iterate(&engine, &stacks, dealer, i.wrapping_add(100));
        }

        let blueprint = BlueprintStrategy::from_table(trainer.table);
        let mut bp_chip_delta: i64 = 0;
        let mut uniform_chip_delta: i64 = 0;
        let hands = 2_000u64;
        for hand in 0..hands {
            // Alternate which seat is the blueprint so dealer-position
            // bias cancels. Even hands: seat 0 blueprint; odd: seat 1.
            let (seat_bp, seat_ur): (SeatIndex, SeatIndex) = if hand % 2 == 0 { (0, 1) } else { (1, 0) };
            let mut agents: Vec<Box<dyn Agent>> = vec![
                Box::new(StrategyAdapter::new(
                    blueprint.clone(),
                    hand.wrapping_mul(2),
                )),
                Box::new(StrategyAdapter::new(
                    UniformRandomStrategy,
                    hand.wrapping_mul(2).wrapping_add(1),
                )),
            ];
            if seat_bp == 1 {
                agents.reverse();
            }
            let dealer = (hand % 2) as SeatIndex;
            let result = engine.run_hand(
                hand,
                hand.wrapping_add(777),
                &stacks,
                dealer,
                None,
                &mut agents,
                &mut NullSink,
            );
            bp_chip_delta += result.seats[seat_bp].chip_delta as i64;
            uniform_chip_delta += result.seats[seat_ur].chip_delta as i64;
        }

        assert!(
            bp_chip_delta > uniform_chip_delta,
            "blueprint should outperform uniform random: bp={bp_chip_delta} ur={uniform_chip_delta}"
        );
    }

}
