## MODIFIED Requirements

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
