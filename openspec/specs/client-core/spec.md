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
