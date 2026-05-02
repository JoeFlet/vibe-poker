## ADDED Requirements

### Requirement: Force-fold on mid-hand leave
When a seated player sends `ClientMessage::LeaveTable` while a hand is in progress, the server SHALL cancel any pending action prompt for that player and treat the leaver as having folded for the remainder of the hand. The seat SHALL remain in the table state with `leave_pending = true` until `HandEnded`, at which point the seat vacates. The leaving client SHALL receive `ServerMessage::LeftTable` immediately; subsequent hand events for the leaver SHALL NOT be suppressed on the wire but MAY be safely discarded by the client.

This requirement also governs graceful session-close when the session is not superseded by a handoff. When a session closes and the revoked flag is false, the server SHALL apply the same force-fold and `leave_pending` sequence before vacating the seat.

When a session is superseded by a new login for the same player (the registry sets the revoked flag), the server SHALL NOT force-fold — the new session inherits the seat via `SeatLink::rebind_to` and continues the hand normally.

#### Scenario: LeaveTable during pending prompt folds the player
- **GIVEN** a seated player with an active `Prompt` (connection's `pending` slot is `Some`)
- **WHEN** the player sends `ClientMessage::LeaveTable`
- **THEN** the server SHALL clear the `pending` slot
- **AND** `RemoteAgent::act` SHALL return `Action::Fold` to the engine
- **AND** the engine SHALL broadcast `EngineEvent::ActionTaken { seat, action: Fold, .. }` to remaining seats
- **AND** the leaver SHALL receive `ServerMessage::LeftTable { table_id }`
- **AND** no action-deadline timeout SHALL need to fire for this player on this hand

#### Scenario: LeaveTable while not the current actor still force-folds
- **GIVEN** a seated player with no pending prompt (it is another player's turn)
- **WHEN** the player sends `ClientMessage::LeaveTable` mid-hand
- **THEN** the seat SHALL be marked `leave_pending = true`
- **AND** when the engine next prompts this seat, the `pending` slot SHALL be empty (leaver's connection has gone)
- **AND** the engine SHALL receive Fold from `RemoteAgent::act` on the first attempt

#### Scenario: Mid-hand leave preserves committed chips
- **GIVEN** a seated player who has committed 30 chips to the pot on the current street
- **WHEN** the player sends `ClientMessage::LeaveTable` before the hand ends
- **THEN** those 30 chips SHALL remain in the pot and be awarded per normal engine rules at showdown

#### Scenario: Seat vacates at HandEnded
- **GIVEN** a player who sent `LeaveTable` mid-hand (seat marked `leave_pending`)
- **WHEN** the engine emits `HandEnded`
- **THEN** the server SHALL remove the seat from `inner.seats[]`
- **AND** the post-hand `TableState` broadcast SHALL reflect the empty seat

#### Scenario: Session handoff does NOT force-fold
- **GIVEN** a seated player whose connection has just been superseded by a new login (revoked flag set on the old session)
- **WHEN** the old session reaches its cleanup path
- **THEN** the server SHALL NOT call `force_leave` on the table
- **AND** the new session's `SeatLink` SHALL own the seat
- **AND** any pending prompt SHALL have been migrated to the new connection via `SeatLink::rebind_to`
- **AND** the hand SHALL continue normally for the new session

#### Scenario: Graceful disconnect mid-hand force-folds
- **GIVEN** a seated player with an active hand and a pending prompt
- **WHEN** the player's connection closes gracefully (receives `Disconnect` or the socket closes) and the session is not superseded
- **THEN** `cancel_pending` SHALL clear the pending slot
- **AND** `force_leave` SHALL be invoked with the session id
- **AND** the seat SHALL be marked `leave_pending`
- **AND** the engine SHALL receive Fold from `RemoteAgent::act`
- **AND** the seat SHALL vacate at `HandEnded`
