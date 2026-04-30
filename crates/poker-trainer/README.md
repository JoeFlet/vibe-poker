# poker-trainer

Agent-training scaffolding — split out of `poker-engine` in step 20 so the engine stays a focused rules library. Carries:

- **`solver`** — external-sampling MCCFR for heads-up no-limit, regret tables, blueprint persistence (`BlueprintStrategy` / `StrategyAdapter` to plug a trained policy back into the engine's `Agent` trait).
- **`dataset`** — `extract_hand_stats` / `class_conditional` / `windowed` aggregators that turn `FileSink` event logs into per-hand stat rows.
- Three CLIs: `poker_train`, `poker_play`, `poker_dataset`.

The longer-term plan is for serious AI development to live in a separate (likely Python) project consuming `poker_engine`. This crate is the in-tree reference trainer.

## Train → play loop

```sh
# Train a blueprint (MCCFR, 200 000 iterations)
cargo run --release -p poker-trainer --bin poker_train -- \
    --iters 200000 --out blueprint.mp --seed 7

# Play it against bots and write an event log
cargo run --release -p poker-trainer --bin poker_play -- \
    --blueprint blueprint.mp --hands 5000 \
    --agents blueprint,calling --log session.mp
```

`poker_train` writes a schema-versioned + abstraction-tagged MessagePack file; loads fail loudly if either changes. `poker_play` loads it via `BlueprintStrategy`, wraps it in `StrategyAdapter`, and runs head-to-head through `SimRunner`. Per-seat VPIP / PFR / AF / win-rate / chip EV print at the end.

### Agent specs (comma-separated)

| Spec | Behaviour |
|---|---|
| `blueprint` | Trained policy from `--blueprint` |
| `calling` | `CallingStation` — checks free, otherwise calls |
| `random:<seed>` | `RandomAgent` seeded at `<seed>` |

## Replaying a session

A `--log` file from `poker_play` or `poker_dataset` can be replayed:

- **Desktop client** ([client/](../../client/)): load via file picker once a replay UI lands (step 26+).
- **Deprecated egui viewer** ([poker-client](../poker-client/)): `cargo run -p poker-client -- --replay session.mp`

Both consume the same `[u32 LE length][rmp-serde EngineEvent]` frame format.

## Persona dataset

```sh
cargo run --release -p poker-trainer --bin poker_dataset -- \
    --personas nit,tag,lag,maniac,tilt --hands 2000 --window 500 --out logs/
```

Round-robins every persona heads-up, then prints:

1. **Per-persona summary** — VPIP / PFR / AF / WTSD% / chip EV pooled across all matchups.
2. **Class-conditional matrix** — (own × opponent) behaviour stats; e.g. how does TAG play against Maniac vs Nit?
3. **Windowed time series** — rolling N-hand windows per persona; `tilt` persona is the canonical drift example.

`--out <dir>` writes a `<a>_vs_<b>.mp` `FileSink` log per matchup for offline replay or re-aggregation.

The aggregator API (`extract_hand_stats`, `class_conditional`, `windowed`) works on any `FileSink` log — server-recorded hands from `poker-server` slot in unchanged.

## Persistence

| What | API | Format |
|---|---|---|
| Trained blueprint | `solver::save_blueprint` / `load_blueprint` | Schema-versioned + abstraction-tagged MessagePack |

Custom agents can manage their own persistence via the lifecycle hooks on `Agent` — see the [engine README](../poker-engine/README.md).

## Tests

```sh
cargo test -p poker-trainer
```
