# Poker server wire protocol

This document specifies the wire protocol implemented by [`poker-server`](../../../poker-server/) and consumed by clients (the in-tree `poker-client` plus any future client). It is sufficient to build a non-Rust client without reading server source.

It is versioned in lockstep with [`PROTOCOL_VERSION`](protocol.rs) (defined in [`protocol.rs`](protocol.rs)). The current version is **3**.

The Rust types in [`protocol.rs`](protocol.rs) are the canonical schema. This file describes how those types are framed, sequenced, and interpreted on the wire.

---

## 1. Transport and framing

The protocol is transport-agnostic in principle, but the only transport currently shipped is **TCP** with `set_nodelay(true)`. Each direction carries an ordered, length-prefixed stream of msgpack-encoded messages.

### 1.1 Frame layout

```
+-------------------+--------------------------+
| u32 length (LE)   | msgpack payload (length) |
+-------------------+--------------------------+
```

- The 4-byte length prefix is **little-endian** and gives the size of the payload in bytes.
- The payload is a single [`rmp-serde`](https://crates.io/crates/rmp-serde)-encoded value. The value is a `ClientMessage` (client → server) or a `ServerMessage` (server → client); see § 4 for shapes.
- Frames are written and read whole. Partial frames are an error.

This is identical to the format `FileSink` writes for replay logs, so a server can spool its event broadcasts straight into a log without re-encoding.

### 1.2 Size cap

Payloads larger than `MAX_FRAME_BYTES` (4 MiB, see [`frame.rs`](frame.rs)) are rejected at the framing layer. The server inspects the length prefix *before* allocating a payload buffer — an oversized prefix tears the connection down without an allocation.

A real-world payload is typically tens to hundreds of bytes; the cap exists purely as a safety bound.

### 1.3 Encoding rules

The wire format is the **compact** (default) `rmp-serde` encoding — `to_vec`, **not** `to_vec_named`. Concretely:

- **Structs and struct-variant payloads** serialize as **positional msgpack arrays of field values in declaration order**. There are no field-name strings on the wire. For example, `LifetimeStats { hands, voluntary_pf, raised_pf, aggressive_actions, passive_actions, showdowns, chip_delta }` is a 7-element array `[u64, u64, u64, u64, u64, u64, i64]`.
- **Enum variants** are encoded by variant name:
  - **Unit variants** (`Action::Fold`, `ClientMessage::Heartbeat`): the **bare string** `"Fold"` / `"Heartbeat"`.
  - **Struct variants** (`ServerMessage::Welcome { … }`, `EngineEvent::HandStarted { … }`, …): a single-key map `{"VariantName": [<positional fields>]}` where the inner array is the struct payload encoded by the rule above.
  - **Newtype tuple variants** with exactly one field (`Action::Raise(u32)`, `AuthMode::Session { key }` is *not* this — it's a struct variant): a single-key map `{"VariantName": <inner-value>}`. The inner value is **not** wrapped in a 1-element array.
  - **Tuple variants with ≥ 2 fields**: a single-key map `{"VariantName": [<fields>]}`.
- **Optional fields** (`device_label: Option<String>`) serialize as `nil` when absent and as the bare value when present.
- **`Card`** is a single byte 0–51: `(rank << 2) | suit`. Rank 0 = Two, …, 12 = Ace; suit 0 = Clubs, 1 = Diamonds, 2 = Hearts, 3 = Spades.
- **Chip amounts** are unsigned 32-bit big-blind-denominated integers. There are no floats anywhere in the protocol. `chip_delta` (signed 32-bit) is the only signed chip field.

> **Concrete example.** `ServerMessage::Welcome { protocol_version: 3, player_id: 1, username: "alice", session_key: "k", stats: LifetimeStats::default() }` encodes as the 28-byte sequence `81 a7 'Welcome' 95 03 01 a5 'alice' a1 'k' 97 00 00 00 00 00 00 00`: a one-key map (`81`) with key `"Welcome"`, whose value is a 5-element fixarray (`95`) holding the five struct fields, the last of which is itself a 7-element fixarray (`97`) for `LifetimeStats`. **No field-name strings appear on the wire.** A non-Rust client must decode by position, not by name. The invariant is pinned by `structs_serialize_as_positional_arrays_not_maps` in [`protocol.rs`](protocol.rs).

---

## 2. Connection lifecycle

```
                ┌─────────────────┐
   open TCP ─→  │ Awaiting auth   │
                │  (handshake)    │
                └────────┬────────┘
                         │ Register | Authenticate
                ┌────────▼────────┐
   Rejected  ←─ │  Authenticating │
                └────────┬────────┘
                         │ Welcome
                ┌────────▼────────┐
                │     Lobby       │ ←── ListTables / TableList,
                │                 │     JoinTable / JoinedTable …
                └────────┬────────┘
                         │ JoinedTable
                ┌────────▼────────┐
                │     Seated      │ ←── TableEvent, Prompt,
                │                 │     SubmitAction, TableState …
                └────────┬────────┘
                         │ LeaveTable / LeftTable
                         ▼
                       Lobby
```

Server-initiated termination at any state: `Goodbye { reason }` followed by socket close. Client-initiated termination: `Disconnect`, after which the server may send `Goodbye` and then close.

### 2.1 Handshake state machine

The first message on a fresh connection **must** be one of:

- `Register` — create a new account (§ 3.1).
- `Authenticate` — log in to an existing account by password or session key (§ 3.2).

Any other first message is a protocol violation; the server closes without sending a response.

Both succeed with `Welcome` and fail with `Rejected`. After `Welcome`, the connection is in the **Lobby** state and may exchange any non-handshake messages.

The server enforces an idle timeout on the handshake (default 60s, see [`limits.rs`](../../../poker-server/src/limits.rs)). A peer that opens a TCP connection and sends no frame is reaped silently.

### 2.2 Idle timeout and rate limit

After `Welcome`, the server applies two per-connection bounds:

- **Idle timeout.** No inbound frame for `idle_timeout` (default 60s) → the server queues `Goodbye { reason: "idle timeout" }` and closes. Clients can keep a connection alive indefinitely with `Heartbeat`.
- **Rate limit.** A token bucket (default capacity 30, refill 20/s) caps inbound frame rate. Over-budget peers receive `Goodbye { reason: "rate limit exceeded" }` and a teardown.

Defaults are deliberately generous; one human action every few seconds is well within budget.

---

## 3. Authentication

### 3.1 `Register` — account creation

```
ClientMessage::Register {
    protocol_version: u32,
    email: String,
    username: String,
    password: String,
    device_label: Option<String>,
}
```

Server-side validation:

- `protocol_version` must equal the server's `PROTOCOL_VERSION` (currently 3). Mismatch → `Rejected { reason: "protocol version mismatch" }`.
- `username` is 3–24 ASCII bytes, alphanumeric / `_` / `-` / `.`, must start alphanumeric. See `is_valid_username`. Failure → `Rejected { reason: "invalid username" }`.
- `email` matches a lightweight shape check (`is_valid_email`): non-empty local part, exactly one `@`, dot in domain, no whitespace, length 3–254. Failure → `Rejected { reason: "invalid email" }`.
- `password` length is 8–128 bytes (`is_valid_password`). Failure → `Rejected { reason: "invalid password" }`.
- `username` and `email` must not collide with an existing account: `Rejected { reason: "username already in use" }` / `Rejected { reason: "email already in use" }`.

On success the server hashes the password with Argon2id, persists the user record, allocates a new `PlayerId`, mints a fresh `session_key`, and replies with `Welcome`.

### 3.2 `Authenticate` — login

```
ClientMessage::Authenticate {
    protocol_version: u32,
    mode: AuthMode,
    device_label: Option<String>,
}
```

`AuthMode` is one of:

```
AuthMode::Password { identifier: String, password: String }
AuthMode::Session  { key: String }
```

- `Password` mode: `identifier` is the user's username **or** email. The server hashes the supplied password and compares against the stored Argon2id hash.
- `Session` mode: `key` is a previously-issued `session_key` from a `Welcome`.

Failure cases:

| `RejectReason`     | `reason` string             | When                                                        |
|--------------------|-----------------------------|-------------------------------------------------------------|
| `BadCredentials`   | `"bad credentials"`         | Unknown user, wrong password.                               |
| `SessionRevoked`   | `"session revoked"`         | Session key was valid but has since been superseded.        |
| `UnknownSession`   | `"unknown session"`         | Session key has never existed.                              |
| `ProtocolMismatch` | `"protocol version mismatch"` | `protocol_version` ≠ server.                              |
| `InternalError`    | `"internal error"`          | Database / unexpected failure (server logs the underlying). |

On success the server replies with `Welcome` and revokes any prior live session for the same user (see § 5.2).

### 3.3 `Welcome` — handshake accepted

```
ServerMessage::Welcome {
    protocol_version: u32,
    player_id: PlayerId,             // u64, stable across sessions
    username: String,                // canonical username (case as registered)
    session_key: String,             // retain for AuthMode::Session reconnect
    stats: LifetimeStats,
}
```

`LifetimeStats` is a flat record of cumulative counters (see § 4.5).

Clients **should** persist `session_key` between runs; presenting it on a future connection avoids re-prompting the user for a password.

### 3.4 `Rejected` — handshake refused

```
ServerMessage::Rejected {
    protocol_version: u32,
    reason: String,
}
```

The server sends `Rejected` and closes the connection. `reason` is human-readable but stable enough to match against (see the table in § 3.2).

---

## 4. Post-handshake messages

### 4.1 Lobby

#### `ListTables` → `TableList`

```
ClientMessage::ListTables
ServerMessage::TableList { tables: Vec<TableInfo> }

TableInfo {
    table_id: TableId,        // u32
    name: String,
    small_blind: u32,
    big_blind: u32,
    max_seats: u8,
    seated: u8,               // current occupant count
    default_buy_in: u32,
}
```

`TableList` is a snapshot at the moment of the request — there is no subscription. Clients should re-issue `ListTables` periodically if the lobby view needs to track joins/leaves.

#### `JoinTable` → `JoinedTable` | `ActionRejected`

```
ClientMessage::JoinTable { table_id: TableId, buy_in: u32 }

ServerMessage::JoinedTable {
    table_id: TableId,
    seat: SeatIndex,           // usize, 0-indexed
    seats: Vec<SeatInfo>,      // current full seat layout
}
```

Common rejection reasons (`ActionRejected.reason`):

- `"already seated at a table"` — the player is already at any table.
- `"no such table: <id>"` — invalid `table_id`.
- `"table full"` / `"buy-in too small"` etc. — table-specific reasons surfaced by `Table::sit`.

A successful `JoinTable` also triggers a broadcast `TableState` (§ 4.2.2) to existing seats; the joiner already has the layout via `JoinedTable` and is suppressed from this broadcast.

#### `LeaveTable` → `LeftTable` | `ActionRejected`

```
ClientMessage::LeaveTable { table_id: TableId }
ServerMessage::LeftTable   { table_id: TableId }
```

Effect is at the next hand boundary; the current hand (if the player is in one) plays out. Existing seats receive a follow-up `TableState`.

### 4.2 Seated

A client at a seated table will receive a stream of `TableEvent`s describing the engine's progress, plus periodic `TableState`s and per-action `Prompt`s.

#### 4.2.1 `TableEvent` — engine event broadcast

```
ServerMessage::TableEvent { table_id: TableId, event: EngineEvent }
```

`EngineEvent` is the same type the engine emits to its `EventSink` (see [`event.rs`](../game/event.rs)):

| Variant            | When                                                           | Carries |
|--------------------|----------------------------------------------------------------|---------|
| `HandStarted`      | Start of every hand.                                           | `hand_id`, `dealer`, `deck_seed`. |
| `HoleCardsDealt`   | After deal. **Per-recipient** — only the seat owner sees its own. | `seat`, `cards: [Card; 2]`. |
| `BoardDealt`       | Flop / turn / river.                                           | `street: Street`, `cards: Vec<Card>`. |
| `ActionTaken`      | Every voluntary action.                                        | `seat`, `action: Action`, `pot_total`. |
| `PlayerAllIn`      | A seat goes all-in.                                            | `seat`, `total_committed`. |
| `HandEnded`        | End of every hand. **Filtered per recipient** (see § 4.2.3).   | `hand_id`, `result: HandResult`. |

Ordering within a hand is deterministic and matches engine emission order:

```
HandStarted
HoleCardsDealt × N      (one per seat, only the owner sees each)
ActionTaken × …          (preflop)
BoardDealt(Flop)
ActionTaken × …          (flop)
BoardDealt(Turn)
ActionTaken × …          (turn)
BoardDealt(River)
ActionTaken × …          (river)
HandEnded
```

Streets prior to a hand's natural end are skipped if it ends earlier (e.g. everyone folds preflop).

#### 4.2.2 `TableState` — periodic snapshot

```
ServerMessage::TableState {
    table_id: TableId,
    seats: Vec<SeatInfo>,
    button: SeatIndex,
}
```

Sent on join, after each hand, and whenever the seat layout changes outside of a hand. **`JoinedTable` always precedes the first `TableState`** for a newly-seated player.

`SeatInfo`:

```
SeatInfo { seat: SeatIndex, player_id: PlayerId, username: String, stack: u32 }
```

#### 4.2.3 Hole-card visibility

Hole cards are private. The server enforces this at the broadcast layer ([`BroadcastSink`](../../../poker-server/src/table.rs)):

- `HoleCardsDealt` is sent only to the owning seat.
- `HandEnded.result.seats[i].hole_cards` is `Some(_)` to recipient `i` for their own seat. For *other* seats, it is `Some(_)` only at a **proper showdown**: `BoardDealt(River)` was reached *and* ≥ 2 non-folded contenders remain. Folded seats are always masked even at a showdown.

The persisted server-side log carries the unfiltered truth; only the wire is filtered.

#### 4.2.4 `Prompt` — server is asking for an action

```
ServerMessage::Prompt {
    table_id: TableId,
    hand_id: HandId,
    seat: SeatIndex,
    legal: LegalActions,
    deadline_ms: u32,
}

LegalActions {
    can_check: bool,
    can_call:  bool,
    call_amount: u32,            // total chips needed to call
    can_raise: bool,
    min_raise: u32,              // min legal Raise(amount); inclusive
    max_raise: u32,              // max legal Raise(amount); inclusive
    all_in_amount: u32,          // chips moved by AllIn; 0 ⇒ AllIn illegal
}
```

The matching reply is `SubmitAction { table_id, hand_id, action }` with `hand_id` echoed verbatim. Stale or duplicate replies are dropped silently by the server.

If `deadline_ms` elapses without a reply, the server folds the seat for the player. Clients that disconnect mid-prompt get the same auto-fold.

#### 4.2.5 `SubmitAction` — client response

```
ClientMessage::SubmitAction {
    table_id: TableId,
    hand_id: HandId,
    action: Action,
}

Action ::= Fold | Check | Call | Raise(u32) | AllIn
```

The server re-validates `action` against the live `LegalActions` for the seat (`LegalActions::is_legal`). A forged or stale action is rejected with `ActionRejected { reason: "no matching pending action" | "illegal action" }` and the prompt remains live until the deadline.

### 4.3 Heartbeat

```
ClientMessage::Heartbeat
ServerMessage::Heartbeat   // echoed by the server
```

Used purely to keep the idle timer fresh. Either side may send heartbeats at any cadence; the recommended client cadence is one every 30s.

### 4.4 Disconnect / Goodbye

```
ClientMessage::Disconnect
ServerMessage::Goodbye { reason: String }
```

`Disconnect` requests a graceful close. The server replies with `Goodbye { reason: "client requested" }` and tears the connection down.

The server may *also* send `Goodbye` unprompted, e.g.:

| `reason`                  | Meaning                                                |
|---------------------------|--------------------------------------------------------|
| `"idle timeout"`          | No inbound frame within `idle_timeout`.                |
| `"rate limit exceeded"`   | Token bucket exhausted (see § 2.2).                    |
| `"session revoked"`       | A newer login for the same user superseded this one.  |
| `"client requested"`      | Reply to `Disconnect`.                                 |

After `Goodbye` the server closes the socket; clients should not expect further messages.

### 4.5 `LifetimeStats`

```
LifetimeStats {
    hands: u64,
    voluntary_pf: u64,
    raised_pf: u64,
    aggressive_actions: u64,
    passive_actions: u64,
    showdowns: u64,
    chip_delta: i64,
}
```

Cumulative counters across every persisted hand the player has been seated at. Sent in `Welcome`. Clients use these to render lifetime VPIP / PFR / aggression without paging through the hand log.

---

## 5. Error model

The protocol distinguishes three failure surfaces:

| Message            | Fatal? | When                                                                 |
|--------------------|--------|----------------------------------------------------------------------|
| `Rejected`         | Yes    | Handshake refused. Server closes after sending.                      |
| `ActionRejected`   | No     | A non-handshake request the server could not satisfy. Connection stays open. |
| `Goodbye`          | Yes    | Server is closing the connection cleanly. May be unsolicited.        |

`Rejected.reason` and `Goodbye.reason` are stable enough to match against (see the tables in §§ 3.2 and 4.4); `ActionRejected.reason` is free-form human-readable text.

---

## 6. Reconnect procedure

A client that retains its `session_key` from a prior `Welcome` may reconnect at any time:

1. Open a fresh TCP connection.
2. Send `Authenticate { protocol_version, mode: AuthMode::Session { key }, device_label }`.
3. Wait for `Welcome`. If the prior session is still valid the server returns the same `player_id` and (typically) the same `session_key`.

### 6.1 Mid-hand seat takeover (step 22c)

If the player is **currently seated and in a live hand** at the moment of the new login, the server hands the seat over to the new connection:

1. The prior session's outbound queue is closed; that connection sees `Goodbye { reason: "session revoked" }`.
2. The new connection receives `Welcome`.
3. The server immediately follows with `JoinedTable { table_id, seat, seats }` so the new client knows where it is.
4. If the engine was blocked on a prompt for this seat, the server re-issues the matching `Prompt { table_id, hand_id, seat, legal, deadline_ms }` over the new connection. The original `oneshot` is migrated, so submitting an action against this prompt resolves the engine's blocked `act()` and the hand keeps progressing.

**Known limitation (as of protocol v3).** The current hand's `HoleCardsDealt` is **not** replayed to the reconnecting client. The new device plays the rest of the hand with cards face-down on its UI, but is otherwise fully synced (subsequent `BoardDealt`, `ActionTaken`, and the eventual `HandEnded` all arrive normally, and `HandEnded`'s per-recipient filter still surfaces the player's own hole cards). Full state-replay is on the roadmap.

### 6.2 Concurrent-session policy

Only one live session per user. A second successful `Authenticate` (in any mode) revokes the prior session. The replaced connection receives `Goodbye { reason: "session revoked" }`. There is no message-level "kick another device" verb; reconnecting from the new device is itself the signal.

---

## 7. Versioning and compatibility

- `PROTOCOL_VERSION` is bumped on any backwards-incompatible change to message shapes — this includes adding required fields, removing fields, renaming variants, and changing field types. Adding optional fields that default cleanly under `serde_derive` is also a bump unless explicitly designed to be wire-compatible.
- The server sends its `PROTOCOL_VERSION` in `Welcome` and `Rejected`. Clients **should** refuse to proceed against a mismatched version: a client built for v3 talking to a v4 server will see `Rejected { reason: "protocol version mismatch" }`; a v4 client talking to a v3 server will likewise be told.
- Within a single major version, no message reorderings or semantic shifts are permitted.

---

## 8. msgpack schema appendix

Field-by-field msgpack shape for every message variant.

> **Reading the notation.** Per § 1.3, structs and struct-variant payloads are positional arrays. Each entry below uses `[…]` to mean a msgpack array whose elements are the struct's fields in declaration order — **never** a map keyed by field name. Comments after `//` annotate which struct field each position corresponds to but do **not** appear on the wire. `null | T` denotes a serde `Option<T>` (nil when absent). `<TypeName>` references a named shape defined elsewhere in this appendix.

### 8.1 `ClientMessage`

```
{"Register":     [ u32,            // protocol_version
                   string,         // email
                   string,         // username
                   string,         // password
                   null | string ] // device_label
}
{"Authenticate": [ u32,            // protocol_version
                   <AuthMode>,     // mode
                   null | string ] // device_label
}
"ListTables"
{"JoinTable":    [ u32,            // table_id
                   u32 ]           // buy_in
}
{"LeaveTable":   [ u32 ]           // table_id
}
{"SubmitAction": [ u32,            // table_id
                   u64,            // hand_id
                   <Action> ]      // action
}
"Heartbeat"
"Disconnect"
```

#### `AuthMode`

```
{"Password": [ string,             // identifier (username or email)
               string ]            // password
}
{"Session":  [ string ]            // key
}
```

#### `Action`

```
"Fold" | "Check" | "Call" | "AllIn"
{"Raise": u32}                      // bare u32 — NOT [u32]
```

`Raise` is a Rust newtype-style tuple variant (`Action::Raise(u32)`). The inner `u32` is **not** wrapped in a 1-element array.

### 8.2 `ServerMessage`

```
{"Welcome":        [ u32,                  // protocol_version
                     u64,                  // player_id
                     string,               // username
                     string,               // session_key
                     <LifetimeStats> ]     // stats
}
{"Rejected":       [ u32,                  // protocol_version
                     string ]              // reason
}
{"TableList":      [ [<TableInfo>] ]       // tables
}
{"JoinedTable":    [ u32,                  // table_id
                     uint,                 // seat (SeatIndex; usize on the wire — see § 1.3)
                     [<SeatInfo>] ]        // seats
}
{"LeftTable":      [ u32 ]                 // table_id
}
{"TableEvent":     [ u32,                  // table_id
                     <EngineEvent> ]       // event
}
{"TableState":     [ u32,                  // table_id
                     [<SeatInfo>],         // seats
                     uint ]                // button (SeatIndex)
}
{"Prompt":         [ u32,                  // table_id
                     u64,                  // hand_id
                     uint,                 // seat
                     <LegalActions>,       // legal
                     u32 ]                 // deadline_ms
}
{"ActionRejected": [ string ]              // reason
}
"Heartbeat"
{"Goodbye":        [ string ]              // reason
}
```

#### `TableInfo`, `SeatInfo`, `LifetimeStats`, `LegalActions`

```
TableInfo:
  [ u32,    // table_id
    string, // name
    u32,    // small_blind
    u32,    // big_blind
    u8,     // max_seats
    u8,     // seated
    u32 ]   // default_buy_in

SeatInfo:
  [ uint,   // seat
    u64,    // player_id
    string, // username
    u32 ]   // stack

LifetimeStats:
  [ u64,    // hands
    u64,    // voluntary_pf
    u64,    // raised_pf
    u64,    // aggressive_actions
    u64,    // passive_actions
    u64,    // showdowns
    i64 ]   // chip_delta

LegalActions:
  [ bool,   // can_check
    bool,   // can_call
    u32,    // call_amount
    bool,   // can_raise
    u32,    // min_raise
    u32,    // max_raise
    u32 ]   // all_in_amount
```

#### `EngineEvent`

```
{"HandStarted":    [ u64,             // hand_id
                     uint,            // dealer
                     u64 ]            // deck_seed
}
{"HoleCardsDealt": [ uint,            // seat
                     [u8, u8] ]       // cards (fixed-2 array of Card bytes)
}
{"BoardDealt":     [ <Street>,        // street
                     [u8] ]           // cards (variable-length Card bytes)
}
{"ActionTaken":    [ uint,            // seat
                     <Action>,        // action
                     u32 ]            // pot_total
}
{"PlayerAllIn":    [ uint,            // seat
                     u32 ]            // total_committed
}
{"HandEnded":      [ u64,             // hand_id
                     <HandResult> ]   // result
}

HandResult:
  [ u64,                              // hand_id
    [u8],                             // board (Card bytes)
    [<SeatOutcome>] ]                 // seats

SeatOutcome:
  [ uint,                             // seat
    null | [u8, u8],                  // hole_cards
    i32,                              // chip_delta
    bool ]                            // sat_out (optional in serde, but always
                                      //         emitted by the server)

Street ::= "Preflop" | "Flop" | "Turn" | "River" | "Showdown"
```

`Card` is encoded as a single `u8` 0–51: `(rank << 2) | suit`.
