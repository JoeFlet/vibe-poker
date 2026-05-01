# server-behavior

## ADDED Requirements

### Requirement: RequestReplay preconditions
The server SHALL accept `RequestReplay` only from a client that is currently seated at the named table and the named hand is in-flight. If the hand_id does not match the current hand, or the client is not seated, the server SHALL reply with `ActionRejected { reason: "not seated at this hand" }`.

#### Scenario: Unseated client rejected
- **GIVEN** a client in the lobby (not seated at any table)
- **WHEN** the client sends `RequestReplay { hand_id }`
- **THEN** the server SHALL respond with `ActionRejected { reason: "not seated at this hand" }`
- **AND** no replay payload SHALL be sent

#### Scenario: Mismatched hand_id rejected
- **GIVEN** a seated client at a table where the current hand has id `H1`
- **WHEN** the client sends `RequestReplay { hand_id: H2 }` where `H2 != H1`
- **THEN** the server SHALL respond with `ActionRejected { reason: "not seated at this hand" }`

### Requirement: Per-seat replay buffering
During an in-flight hand, the server SHALL maintain an ordered buffer of `ServerMessage`s per seat, populated as events are fanned by `BroadcastSink`. The buffer for a seat SHALL contain exactly the messages that seat received.

#### Scenario: Buffer populated during hand
- **GIVEN** a hand that emits `HandStarted`, `HoleCardsDealt`, and `FlopDealt`
- **WHEN** these events are broadcast
- **THEN** each seat's replay buffer SHALL contain exactly the `ServerMessage`s it received
- **AND** no other seat's messages SHALL appear in that buffer

#### Scenario: Buffer cleared on hand end
- **GIVEN** a hand that has completed and emitted `HandEnded`
- **WHEN** the server finalizes the hand
- **THEN** the replay buffers for that hand SHALL be dropped
- **AND** a subsequent `RequestReplay` for that hand_id SHALL be rejected

### Requirement: Replay event fidelity
Replayed events SHALL be byte-identical to the original live events for that seat. The server SHALL NOT recompute or regenerate events at replay time.

#### Scenario: Byte-identical replay
- **GIVEN** a seat received a specific `ServerMessage` sequence during live play
- **WHEN** the same seat requests a replay
- **THEN** the serialized bytes of each replayed event SHALL equal the serialized bytes of the live event
