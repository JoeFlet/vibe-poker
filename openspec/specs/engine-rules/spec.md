# engine-rules

## Purpose

Define core game invariants for the poker-engine crate: determinism, chip arithmetic, hole-card privacy, and event stream exhaustiveness. These constraints apply to all crates that depend on poker-engine.

## Requirements

### Requirement: Deterministic dealing via deck_seed
Every hand SHALL reproduce the exact same card deal when given the same deck_seed. The engine SHALL NOT pull randomness from any source other than the seed parameter passed to Engine::run_hand.

#### Scenario: Identical seeds produce identical hands
- GIVEN two calls to Engine::run_hand with identical deck_seed and identical agent action sequences
- WHEN both calls complete
- THEN the sequence of EngineEvents SHALL be byte-identical
- AND the SeatOutcome chip deltas SHALL be identical

#### Scenario: Different seeds produce different deals
- GIVEN two calls to Engine::run_hand with different deck_seed values
- WHEN both calls reach HandStarted
- THEN the HoleCardsDealt events SHALL contain different cards for at least one seat

### Requirement: Integer chips, no floating-point
All chip values SHALL use u32 in big-blind-denominated units. SeatOutcome::chip_delta SHALL be i32. No floating-point arithmetic SHALL appear anywhere in the game logic.

#### Scenario: Pot split with odd remainder
- GIVEN a side pot with a remainder that does not divide evenly among winners
- WHEN the pot is awarded
- THEN the remainder chip(s) SHALL go to the first eligible winner left of the dealer index
- AND the sum of all awarded chip deltas SHALL equal the total pot exactly

#### Scenario: Negative chip delta on loss
- GIVEN a seat that lost chips during a hand
- WHEN HandEnded is emitted
- THEN SeatOutcome::chip_delta for that seat SHALL be negative
- AND the type SHALL be i32, not f32 or f64

### Requirement: Hole-card privacy
HoleCardsDealt events SHALL contain hole cards for exactly one seat per event. HandEnded SHALL reveal hole cards only when the hand reached a proper showdown (river dealt AND at least two non-folded contenders remain). Otherwise hole cards SHALL be absent from all non-owner views.

#### Scenario: Folded hand before showdown
- GIVEN a hand where all but one player folds before the river
- WHEN HandEnded is emitted
- THEN the hole cards for folded seats SHALL NOT be included in the HandResult for any non-owner recipient

#### Scenario: River showdown with two live players
- GIVEN a hand that reaches the river with at least two non-folded seats
- WHEN HandEnded is emitted
- THEN hole cards SHALL be visible in the HandResult for all showdown participants

### Requirement: Event stream exhaustiveness
Every meaningful hand moment SHALL emit exactly one corresponding EngineEvent. No EngineEvent variant SHALL be emitted spuriously. The event stream SHALL be the sole durable artifact of a hand.

#### Scenario: Single hand event sequence
- GIVEN a standard hand with no disconnections or timeouts
- WHEN the hand runs from start to finish
- THEN the event stream SHALL contain exactly one HandStarted, one HandEnded, and at least one ActionTaken per non-folded action
- AND no other event types SHALL appear in the stream except those required by game state changes
