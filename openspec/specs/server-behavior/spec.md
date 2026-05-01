# server-behavior

## Purpose

Define server runtime behavior for authentication, persistence, table management, hole-card masking, and connection limits. Applies to the poker-server crate.

## Requirements

### Requirement: Argon2id password hashing
Password credentials SHALL be hashed using Argon2id. The raw password SHALL never be stored or logged.

#### Scenario: Registration stores hash
- **GIVEN** a new user registration with password `"hunter2hunter"`
- **WHEN** `Registry::register` succeeds
- **THEN** the `user_password` table SHALL contain an Argon2id hash, not the plaintext password
- **AND** subsequent authentication with the same password SHALL succeed

### Requirement: Session revocation on new login
Issuing a new session for a user SHALL revoke the prior live session atomically. The superseded connection SHALL receive `Goodbye { "session_revoked" }`.

#### Scenario: Duplicate login revokes first session
- **GIVEN** a user with an active connection
- **WHEN** the same user authenticates from a second connection
- **THEN** the first connection SHALL receive `Goodbye { reason: "session_revoked" }`
- **AND** the second connection SHALL receive `Welcome` with a fresh session key

### Requirement: Hand persistence atomicity
`Registry::record_hand` SHALL write the hand log and seat rows only after `HandEnded`. No chip movement SHALL be reflected in `lifetime_stats` or `hands` / `hand_seats` before `HandEnded`.

#### Scenario: Server crash mid-hand
- **GIVEN** a hand is in progress but has not yet emitted `HandEnded`
- **WHEN** the server process crashes and restarts
- **THEN** the database SHALL contain no evidence that the hand ever started
- **AND** player stacks and stats SHALL match the state from before the hand began

### Requirement: Hole-card masking per recipient
`BroadcastSink` SHALL fan `EngineEvent`s per-recipient. `HoleCardsDealt` SHALL travel only to its owning seat. `HandEnded` SHALL mask all hole cards for non-owner recipients except at proper showdown.

#### Scenario: Hole cards not leaked to opponent
- **GIVEN** a two-player hand
- **WHEN** `HoleCardsDealt` is emitted
- **THEN** player A's event stream SHALL contain only A's hole cards
- **AND** player B's event stream SHALL contain only B's hole cards

### Requirement: Mid-hand reconnect
A reconnect for an already-seated user SHALL swap the `SeatLink` in place. The blocked `RemoteAgent::act` SHALL resolve on the new connection, and any in-flight `Prompt` SHALL be re-issued to the new socket.

#### Scenario: Reconnect during prompted action
- **GIVEN** a seated player whose turn is active (a `Prompt` has been sent)
- **WHEN** the same user reconnects from a new connection
- **THEN** the new connection SHALL receive the same `Prompt` within the action deadline
- **AND** a `SubmitAction` on the new connection SHALL resolve the engine's blocked `Agent::act`

### Requirement: Connection limits
The server SHALL enforce idle timeout (60s default) and rate limits (30 burst / 20 per second default). Over-budget peers SHALL receive `Goodbye { "rate limit exceeded" }` or `Goodbye { "idle timeout" }` and be torn down.

#### Scenario: Idle connection dropped
- **GIVEN** an authenticated connection with no frames sent or received for longer than `idle_timeout`
- **WHEN** the timeout expires
- **THEN** the server SHALL send `Goodbye { reason: "idle timeout" }`
- **AND** close the TCP connection
