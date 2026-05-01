# server-protocol

## MODIFIED Requirements

### Requirement: Protocol version lockstep
`PROTOCOL_VERSION` SHALL be bumped on any backwards-incompatible change to `ClientMessage` or `ServerMessage` variants, field ordering, or semantics. The server SHALL reject connections whose claimed protocol version does not match.

#### Scenario: Client with mismatched version rejected
- **GIVEN** a client sending `Register` with `protocol_version` not equal to the server's `PROTOCOL_VERSION`
- **WHEN** the server processes the registration
- **THEN** the server SHALL respond with `Rejected { reason: "protocol mismatch" }`
- **AND** the connection SHALL be closed

### Requirement: Compact msgpack encoding
All enum variants SHALL encode by variant name. Struct-variant payloads SHALL serialize as positional arrays (not maps) in field declaration order. Unit variants SHALL be bare strings.

#### Scenario: Struct variant encoding
- **GIVEN** a `ServerMessage::Welcome` with known field values
- **WHEN** serialized to msgpack
- **THEN** the result SHALL be a single-key map `{"Welcome": [field1, field2, ...]}`
- **AND** the inner value SHALL be a fixarray containing the fields in declaration order

#### Scenario: RequestReplay encoding
- **GIVEN** a `ClientMessage::RequestReplay { hand_id }` with a known `HandId` (u64)
- **WHEN** serialized to msgpack
- **THEN** the result SHALL be a single-key map `{"RequestReplay": [hand_id]}`
- **AND** the `hand_id` SHALL encode as a bare msgpack integer inside a 1-element fixarray

## ADDED Requirements

### Requirement: RequestReplay verb
`ClientMessage` SHALL include a `RequestReplay { hand_id: HandId }` variant, where `HandId` is the protocol's `u64` hand identifier. A client MAY send this at any time after seating to request the event history for an in-flight hand.

#### Scenario: RequestReplay accepted
- **GIVEN** a seated client with an active hand in progress
- **WHEN** the client sends `RequestReplay { hand_id }` matching the current hand
- **THEN** the server SHALL reply with `ReplayEvents { hand_id, events }`
- **AND** the `events` list SHALL contain every `ServerMessage` that seat received since `HandStarted`

### Requirement: ReplayEvents response
`ServerMessage` SHALL include a `ReplayEvents { hand_id: HandId, events: Vec<ServerMessage> }` variant. The `events` field SHALL contain the ordered per-seat `ServerMessage` stream exactly as that seat originally received it, including hole-card masking.

#### Scenario: Replay preserves masking
- **GIVEN** a two-player hand where each player was dealt distinct hole cards
- **WHEN** seat 0 requests a replay
- **THEN** the replayed `HoleCardsDealt` for seat 0 SHALL contain only seat 0's cards
- **AND** the replayed `HoleCardsDealt` for seat 1 SHALL NOT appear in seat 0's replay

#### Scenario: Replay ordering before live events
- **GIVEN** a live hand with 10 events already broadcast to a seat
- **WHEN** that seat sends `RequestReplay`
- **THEN** the client SHALL receive all 10 replayed events before any subsequent live broadcast
- **AND** no live event SHALL be duplicated in the replay
