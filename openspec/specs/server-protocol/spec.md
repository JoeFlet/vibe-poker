# server-protocol

## Purpose

Define wire protocol requirements for framing, message encoding, and connection lifecycle. Applies to poker-server and any connecting client.
## Requirements
### Requirement: Length-prefixed msgpack framing
Every message on the wire SHALL be encoded as a single `[u32 LE length][msgpack payload]` frame. The length SHALL give the exact payload size in bytes. Partial frames SHALL be treated as an error.

#### Scenario: Valid frame round-trip
- **GIVEN** a `ServerMessage::Welcome` value
- **WHEN** it is encoded via `frame::encode` and decoded via `frame::decode::<ServerMessage>`
- **THEN** the decoded value SHALL equal the original
- **AND** no field names SHALL appear in the wire bytes (positional array encoding)

#### Scenario: Oversized length prefix rejected
- **GIVEN** a 4-byte length prefix whose value exceeds `MAX_FRAME_BYTES` (4 MiB)
- **WHEN** the server parses the prefix
- **THEN** the connection SHALL be torn down immediately
- **AND** no payload buffer SHALL be allocated

### Requirement: Protocol version lockstep
`PROTOCOL_VERSION` SHALL be bumped on any backwards-incompatible change to `ClientMessage` or `ServerMessage` variants, field ordering, or semantics. The server SHALL reject connections whose claimed protocol version does not match. The current value SHALL be `5`.

#### Scenario: Client with mismatched version rejected
- **GIVEN** a client sending `Register` with `protocol_version` not equal to the server's `PROTOCOL_VERSION`
- **WHEN** the server processes the registration
- **THEN** the server SHALL respond with `Rejected { reason: "protocol mismatch" }`
- **AND** the connection SHALL be closed

#### Scenario: Version constant is 5
- **GIVEN** the poker-engine crate
- **WHEN** inspecting `poker_engine::net::protocol::PROTOCOL_VERSION`
- **THEN** the value SHALL equal `5`

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

### Requirement: Card encoding
A `Card` SHALL serialize as a single byte `0–51` computed as `(rank << 2) | suit`, where rank `0 = Two` through `12 = Ace` and suit `0 = Clubs`, `1 = Diamonds`, `2 = Hearts`, `3 = Spades`.

#### Scenario: Round-trip card encoding
- **GIVEN** the Ace of Spades (`rank = 12`, `suit = 3`)
- **WHEN** encoded and decoded
- **THEN** the byte SHALL be `51` (`0x33`)
- **AND** decoded back to the original rank and suit

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

