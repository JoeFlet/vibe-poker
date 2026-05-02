## ADDED Requirements

### Requirement: Track per-seat street bet
`CurrentHand` SHALL maintain a `bet_this_street: Vec<(SeatIndex, u32)>` projection reflecting the amount each seat has committed this street, reconstructed from `EngineEvent::ActionTaken` and `EngineEvent::PlayerAllIn`. The projection SHALL reset on `HandStarted` and `BoardDealt`.

Update rules on `ActionTaken { seat, action, pot_total }`:
- `Fold`: no change to any seat's entry.
- `Check`: set or keep the seat's entry at the current maximum across entries (0 if none).
- `Call`: set the seat's entry to the current maximum across entries.
- `Raise(to)`: set the seat's entry to `to`.
- `AllIn`: set the seat's entry to the subsequent `PlayerAllIn.total_committed` minus any already-recorded committed-on-earlier-streets for that seat (best-effort; the entry is a UI affordance, not a settlement source).

The projection SHALL NOT be used for any settlement or correctness decision — the authoritative `pot_total` on `CurrentHand` remains the only source of pot truth. Views MAY render the per-seat bet alongside each seat's username and stack.

#### Scenario: Reset on new hand
- **GIVEN** a `CurrentHand` with several `(seat, bet)` entries populated during a prior street
- **WHEN** the core receives `EngineEvent::HandStarted`
- **THEN** `bet_this_street` SHALL be empty

#### Scenario: Reset on new street
- **GIVEN** a `CurrentHand` with flop bets populated
- **WHEN** the core receives `EngineEvent::BoardDealt { street: Turn, .. }`
- **THEN** `bet_this_street` SHALL be empty

#### Scenario: Raise sets exact amount
- **GIVEN** a `CurrentHand` with seat 0 bet = 10, seat 1 bet = 10
- **WHEN** the core receives `ActionTaken { seat: 0, action: Raise(30), .. }`
- **THEN** `bet_this_street` SHALL contain `(0, 30)` and `(1, 10)`

#### Scenario: Call matches current max
- **GIVEN** a `CurrentHand` with seat 0 bet = 30, seat 1 bet = 10
- **WHEN** the core receives `ActionTaken { seat: 1, action: Call, .. }`
- **THEN** `bet_this_street[seat=1]` SHALL equal 30

#### Scenario: Fold does not remove entry
- **GIVEN** a `CurrentHand` with seat 1 bet = 10
- **WHEN** the core receives `ActionTaken { seat: 1, action: Fold, .. }`
- **THEN** `bet_this_street` SHALL still contain `(1, 10)`
- **AND** the seat index SHALL also appear in `folded_seats`
