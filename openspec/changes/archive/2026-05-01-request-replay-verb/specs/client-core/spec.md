# client-core

## ADDED Requirements

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
