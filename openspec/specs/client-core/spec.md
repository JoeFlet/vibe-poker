# client-core

## Purpose

Define client state machine requirements for Intent/Effect transitions, ClientView authority, animation budget, and crash resilience. Applies to poker-client-core and all consuming hosts.
## Requirements
### Requirement: Pure synchronous state machine
`ClientCore` SHALL be fully synchronous and deterministic. It SHALL contain no async runtime, no I/O, no platform code, and no `Instant` or randomness. The same `(Intent | inbound ServerMessage)` sequence SHALL always produce the same `Vec<Effect>` and `ClientView`.

#### Scenario: Deterministic replay
- **GIVEN** two `ClientCore` instances with the same `initial_session_key`
- **WHEN** both receive the identical sequence of `Intent`s and `ServerMessage`s
- **THEN** `snapshot()` SHALL return identical `ClientView` values after every step
- **AND** `handle_intent` / `handle_inbound` SHALL emit identical `Effect` sequences

### Requirement: Intent-driven state transitions
All client state changes SHALL be initiated by `Intent` (host requests) or `ServerMessage` (server events). The core SHALL not autonomously transition state.

#### Scenario: Connection lifecycle
- **GIVEN** a fresh `ClientCore` in `Phase::Disconnected`
- **WHEN** the host calls `handle_intent(Intent::Connect { addr })`
- **THEN** the core SHALL emit `Effect::OpenConnection` and advance to `Phase::Connecting`
- **AND** upon receiving `ServerMessage::Welcome`, the core SHALL advance to `Phase::Lobby`

### Requirement: ClientView as sole authoritative state
`ClientView` SHALL represent the entire authoritative client UI state. Hosts SHALL NOT maintain a parallel copy of game state that can diverge from the core's snapshot.

#### Scenario: No host-side state cache
- **GIVEN** a host displaying a poker table using `client.snapshot()`
- **WHEN** the host receives a new `ClientView` after an inbound message
- **THEN** the host SHALL re-render from the new `ClientView` directly
- **AND** the host SHALL NOT compare against or update a separate cached game state

### Requirement: UI animation budget
The frontend MAY introduce visual delay for animations, but the displayed state SHALL NOT lag true state by more than 1000 ms.

#### Scenario: Animation within budget
- **GIVEN** a card flip animation that takes 600 ms
- **WHEN** the true state has already advanced
- **THEN** the animation is acceptable because it stays within the 1000 ms budget

#### Scenario: Animation exceeds budget
- **GIVEN** a proposed animation sequence lasting 1200 ms
- **WHEN** evaluated against the animation budget
- **THEN** the animation SHALL be dynamically sped up or skipped

### Requirement: Resilience to interruption
A client crash or device swap SHALL be recoverable solely by reconnecting and resyncing from the server. The session key SHALL be the only client-side persisted artifact required to resume.

#### Scenario: Force-quit and reconnect
- **GIVEN** a client that crashes mid-hand while seated at a table
- **WHEN** the user restarts the application and authenticates with the same session key
- **THEN** the core SHALL reconstruct lobby and table state from server events alone
- **AND** no local files other than the session key SHALL be required

### Requirement: ReplayEvents ingestion
`ClientCore` SHALL handle `ServerMessage::ReplayEvents { hand_id, events }` by applying each contained `ServerMessage` to its state machine in order, exactly as if the messages had arrived live.

#### Scenario: State reconstruction from replay
- **GIVEN** a `ClientCore` that has just reconnected and has no `CurrentHand`
- **WHEN** it receives `ReplayEvents` containing `HandStarted`, `HoleCardsDealt`, `FlopDealt`
- **THEN** after processing, `snapshot().current_hand` SHALL be `Some(CurrentHand)`
- **AND** the hole cards in the view SHALL match those in the replayed `HoleCardsDealt`

#### Scenario: Deterministic replay after reconnect
- **GIVEN** two clients that both reconnect mid-hand and receive identical `ReplayEvents`
- **WHEN** both cores process the replay sequence
- **THEN** their `snapshot()` outputs SHALL be identical after the replay
- **AND** subsequent live events SHALL keep them synchronized

### Requirement: No duplicate processing
`ClientCore` SHALL NOT double-apply events that were already received live before a replay. If a `ReplayEvents` contains events that overlap with already-processed state, the core SHALL process them anyway (overwriting with identical state) because the state machine is deterministic and idempotent.

#### Scenario: Overlapping replay after brief disconnect
- **GIVEN** a client received `HandStarted` and `HoleCardsDealt` live, then disconnected
- **WHEN** it reconnects and receives a replay containing `HandStarted`, `HoleCardsDealt`, and `FlopDealt`
- **THEN** the core SHALL process all three events in order
- **AND** the final `ClientView` SHALL reflect `FlopDealt`

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

