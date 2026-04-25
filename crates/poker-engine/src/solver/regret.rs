//! Regret storage and blueprint strategy for MCCFR.
//!
//! `RegretTable` is the persistent state the trainer builds up. At each
//! visited `InfoSet` it holds:
//!
//! * `regret[a]` — cumulative counterfactual regret for action `a`. Regret
//!   matching converts this into the trainer's *current* strategy for the
//!   next iteration.
//! * `strategy_sum[a]` — cumulative sum of the current strategy's
//!   probability on action `a` across all visits. Normalising yields the
//!   time-averaged strategy, which is what converges to the equilibrium.
//!
//! `BlueprintStrategy` wraps a trained `RegretTable` and implements
//! `StrategyAgent`, so solvers plug into the same `StrategyAdapter` pipeline
//! as `UniformRandomStrategy`.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::game::LegalActions;
use crate::solver::action::AbstractAction;
use crate::solver::info_set::InfoSet;
use crate::solver::strategy::{legal_abstract_actions, ActionProbs, StrategyAgent};

/// Per-info-set regret + strategy-sum accumulator.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RegretEntry {
    pub regret: [f32; AbstractAction::COUNT],
    pub strategy_sum: [f32; AbstractAction::COUNT],
    pub visits: u32,
}

/// A hash-map-backed regret table. One entry per distinct `InfoSet` visited.
///
/// Keyed by a clone of `InfoSet`, which includes a `SmallVec` history — the
/// clone is cheap (inline up to 12 actions) but not free. If profiling shows
/// this to matter we can swap in a specialized interning layer.
///
/// On-disk form (see `solver::persistence`) serializes the entries as a
/// sequence of `(InfoSet, RegretEntry)` pairs rather than a map, both for
/// portability across formats that restrict map-key shapes and for stable
/// byte output when callers pre-sort.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(from = "RegretTableWire", into = "RegretTableWire")]
pub struct RegretTable {
    entries: HashMap<InfoSet, RegretEntry>,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct RegretTableWire {
    pub(crate) entries: Vec<(InfoSet, RegretEntry)>,
}

impl From<RegretTableWire> for RegretTable {
    fn from(wire: RegretTableWire) -> Self {
        RegretTable {
            entries: wire.entries.into_iter().collect(),
        }
    }
}

impl From<RegretTable> for RegretTableWire {
    fn from(table: RegretTable) -> Self {
        RegretTableWire {
            entries: table.entries.into_iter().collect(),
        }
    }
}

impl RegretTable {
    pub fn new() -> Self {
        Self { entries: HashMap::new() }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Get an entry by reference; missing entries return `None`.
    pub fn get(&self, info: &InfoSet) -> Option<&RegretEntry> {
        self.entries.get(info)
    }

    /// Look up an entry mutably, inserting a zero-initialised one if absent.
    pub fn entry_mut(&mut self, info: &InfoSet) -> &mut RegretEntry {
        // `entry(info.clone())` always clones the key, even on hits. The
        // explicit split below lets hits avoid the clone, which is the
        // common case once training has run for a while.
        if !self.entries.contains_key(info) {
            self.entries.insert(info.clone(), RegretEntry::default());
        }
        self.entries
            .get_mut(info)
            .expect("just inserted above if missing")
    }

    /// Reset all counters. Useful for running fresh training from the same
    /// allocations in benchmarks.
    pub fn clear(&mut self) {
        self.entries.clear();
    }
}

/// Regret matching: convert cumulative regret into a strategy distribution.
///
/// Positive-part normalisation — actions with positive cumulative regret get
/// probability proportional to that regret; if every action has ≤0 regret,
/// fall back to uniform over the legal subset. Illegal actions always get 0.
pub fn regret_matching(regrets: &[f32; AbstractAction::COUNT], legal: &[AbstractAction]) -> ActionProbs {
    debug_assert!(!legal.is_empty(), "at least one legal action required");
    let mut pos = [0.0f32; AbstractAction::COUNT];
    let mut sum = 0.0f32;
    for &a in legal {
        let r = regrets[a.index()].max(0.0);
        pos[a.index()] = r;
        sum += r;
    }
    let mut probs = [0.0f32; AbstractAction::COUNT];
    if sum > 0.0 {
        for &a in legal {
            probs[a.index()] = pos[a.index()] / sum;
        }
    } else {
        let p = 1.0 / legal.len() as f32;
        for &a in legal {
            probs[a.index()] = p;
        }
    }
    ActionProbs { probs }
}

/// Trained policy wrapping a `RegretTable`. Returns the time-averaged
/// strategy at each info set, falling back to uniform over legal actions
/// when the info set was never visited during training.
///
/// `Clone` is cheap *enough* for tests (the table is a small HashMap during
/// scaffolding-era training), but once we train seriously the table will
/// grow and callers should share it via `Arc<BlueprintStrategy>`.
#[derive(Clone)]
pub struct BlueprintStrategy {
    table: RegretTable,
}

impl BlueprintStrategy {
    pub fn from_table(table: RegretTable) -> Self {
        Self { table }
    }

    /// Number of info sets in the underlying table.
    pub fn size(&self) -> usize {
        self.table.len()
    }

    /// Average strategy at `info` over `legal` abstract actions.
    pub fn average_strategy(&self, info: &InfoSet, legal: &[AbstractAction]) -> ActionProbs {
        match self.table.get(info) {
            None => ActionProbs::uniform_over(legal),
            Some(entry) => {
                let sum: f32 = legal.iter().map(|a| entry.strategy_sum[a.index()]).sum();
                if sum <= 0.0 {
                    return ActionProbs::uniform_over(legal);
                }
                let mut probs = [0.0f32; AbstractAction::COUNT];
                for &a in legal {
                    probs[a.index()] = entry.strategy_sum[a.index()] / sum;
                }
                ActionProbs { probs }
            }
        }
    }
}

impl StrategyAgent for BlueprintStrategy {
    fn strategy(&self, info: &InfoSet, legal: &LegalActions) -> ActionProbs {
        let actions = legal_abstract_actions(legal);
        self.average_strategy(info, &actions)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::Street;
    use smallvec::SmallVec;

    fn info(actions: &[AbstractAction]) -> InfoSet {
        InfoSet {
            street: Street::Preflop,
            position: 0,
            bucket: 0,
            history: SmallVec::from_slice(actions),
        }
    }

    #[test]
    fn regret_matching_proportional_to_positive_regret() {
        let regrets = [0.0, 3.0, 1.0, 0.0];
        let legal = [AbstractAction::Call, AbstractAction::Raise];
        let probs = regret_matching(&regrets, &legal);
        assert!((probs.probs[AbstractAction::Call.index()] - 0.75).abs() < 1e-6);
        assert!((probs.probs[AbstractAction::Raise.index()] - 0.25).abs() < 1e-6);
        assert_eq!(probs.probs[AbstractAction::Fold.index()], 0.0);
        assert_eq!(probs.probs[AbstractAction::AllIn.index()], 0.0);
    }

    #[test]
    fn regret_matching_uniform_when_no_positive_regret() {
        let regrets = [-1.0, -2.0, -0.5, -3.0];
        let legal = [AbstractAction::Fold, AbstractAction::Call, AbstractAction::AllIn];
        let probs = regret_matching(&regrets, &legal);
        let expected = 1.0 / 3.0;
        assert!((probs.probs[AbstractAction::Fold.index()] - expected).abs() < 1e-6);
        assert!((probs.probs[AbstractAction::Call.index()] - expected).abs() < 1e-6);
        assert!((probs.probs[AbstractAction::AllIn.index()] - expected).abs() < 1e-6);
        assert_eq!(probs.probs[AbstractAction::Raise.index()], 0.0);
    }

    #[test]
    fn table_inserts_on_first_access_then_retains() {
        let mut table = RegretTable::new();
        assert!(table.is_empty());
        let i = info(&[]);
        {
            let entry = table.entry_mut(&i);
            entry.regret[AbstractAction::Raise.index()] = 2.5;
        }
        assert_eq!(table.len(), 1);
        assert_eq!(
            table.get(&i).unwrap().regret[AbstractAction::Raise.index()],
            2.5
        );
    }

    #[test]
    fn blueprint_falls_back_to_uniform_for_unknown_info() {
        let bp = BlueprintStrategy::from_table(RegretTable::new());
        let legal = [AbstractAction::Fold, AbstractAction::Call];
        let probs = bp.average_strategy(&info(&[]), &legal);
        assert!((probs.probs[AbstractAction::Fold.index()] - 0.5).abs() < 1e-6);
        assert!((probs.probs[AbstractAction::Call.index()] - 0.5).abs() < 1e-6);
    }
}
