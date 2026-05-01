# client-transport

## Purpose

Define transport abstraction, session persistence, and runtime facade requirements. Applies to poker-client-transport-native and any alternative transport implementation.

## Requirements

### Requirement: Transport trait abstraction
All network I/O SHALL be owned by a `Transport` implementor. The `Transport` trait SHALL expose exactly one method: `async fn open(&self, addr: String) -> Result<TransportHandle, Self::OpenError>`. The core SHALL remain agnostic of the transport implementation.

#### Scenario: In-memory transport implements trait
- **GIVEN** `InMemoryTransport` in poker-client-headless
- **WHEN** it implements `Transport::open`
- **THEN** `NativeClient::with_transport` SHALL accept it without modification
- **AND** the core SHALL emit identical `Effect`s compared to a TCP transport

### Requirement: Session key atomic persistence
`SessionStore` SHALL write session keys atomically (write-then-rename) to prevent corrupt partial writes. An empty or missing file SHALL be interpreted as `None`.

#### Scenario: Crash during session save
- **GIVEN** a `SessionStore::save` call in progress
- **WHEN** the process crashes after writing but before renaming
- **THEN** on next load, the old session key SHALL still be valid (no partial write visible)
- **AND** the temporary file SHALL not be mistaken for a valid session

### Requirement: Structured logging with sequence numbers
The transport SHALL emit structured `tracing` events at every wire send and receive, including a monotonic sequence number. Stringly-typed `format!` logs SHALL NOT be used for wire events.

#### Scenario: Send event logged
- **GIVEN** a `ClientMessage` sent over the transport
- **WHEN** the write completes
- **THEN** a `tracing` event SHALL fire with fields `direction = "outbound"`, `seq`, and `msg_type`
- **AND** the sequence number SHALL strictly increase per event direction

### Requirement: Transport handle channel contract
`TransportHandle` SHALL expose two channels: an outbound `mpsc::Sender<ClientMessage>` and an inbound `mpsc::Receiver<TransportIn>`. The inbound channel SHALL carry a `TransportIn` enum (`Connected`, `Message(ServerMessage)`, `Lost`) so the core can observe connection state changes.

#### Scenario: Connection drop surfaces as TransportIn::Lost
- **GIVEN** an active `TransportHandle`
- **WHEN** the underlying TCP stream receives EOF
- **THEN** the inbound channel SHALL yield `TransportIn::Lost`
- **AND** the core SHALL transition to `Phase::Ended` upon receiving it
