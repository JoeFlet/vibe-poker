## MODIFIED Requirements

### Requirement: Hand persistence atomicity
`Registry::record_hand` SHALL write the hand log and seat rows only after `HandEnded`. No chip movement SHALL be reflected in `lifetime_stats` or `hands` / `hand_seats` before `HandEnded`.

#### Scenario: Server crash mid-hand
- **GIVEN** a hand is in progress but has not yet emitted `HandEnded`
- **WHEN** the server process crashes and restarts
- **THEN** the database SHALL contain no evidence that the hand ever started
- **AND** player stacks and stats SHALL match the state from before the hand began

#### Scenario: Table actor aborted mid-hand leaves DB clean
- **GIVEN** a hand is in progress (clients have received `HandStarted`) but `HandEnded` has not yet been emitted
- **WHEN** the table actor task is aborted (simulating a mid-hand server crash)
- **THEN** the `hands` table SHALL contain zero rows for that hand
- **AND** the `hand_seats` table SHALL contain zero rows for that hand
- **AND** a subsequent authenticated login SHALL return `lifetime_stats` unchanged from before the hand started
