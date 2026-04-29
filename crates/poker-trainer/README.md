# poker-trainer

Agent-training scaffolding — split out of `poker-engine` in step 20 so the engine stays a focused rules library. Carries:

- **`solver`** — external-sampling MCCFR for heads-up no-limit, regret tables, blueprint persistence, and `BlueprintStrategy` / `StrategyAdapter` to plug a trained policy back into the engine's `Agent` trait.
- **`dataset`** — `extract_hand_stats` / `class_conditional` / `windowed` aggregators that turn `FileSink` event logs into per-hand stat rows for behavioural analysis.
- Three CLIs: `poker_train`, `poker_play`, `poker_dataset`.

The future plan is for serious AI development to eventually live in a separate (likely Python) project that consumes the same `poker_engine`. This crate is the in-tree reference trainer.

## Train → play loop

```sh
cargo run --release -p poker-trainer --bin poker_train -- \
    --iters 200000 --out blueprint.mp --seed 7

cargo run --release -p poker-trainer --bin poker_play -- \
    --blueprint blueprint.mp --hands 5000 \
    --agents blueprint,calling --log session.mp
```

`poker_train` runs MCCFR for the requested iterations and writes a versioned MessagePack blueprint (schema-tagged with the abstraction so loads fail loudly if either changes). `poker_play` loads it back via `BlueprintStrategy`, wraps it in `StrategyAdapter`, and runs head-to-head against built-in bots through `SimRunner`. Per-seat VPIP / PFR / AF / win-rate / chip EV print at the end via the same `print_report` table as `poker_report`. With `--log <path>` the run is also teed to a `FileSink`.

### Agent specs (one per seat, comma-separated)

| Spec | Behaviour |
|---|---|
| `blueprint` | The trained policy from `--blueprint` |
| `calling` | `CallingStation` — checks free, otherwise calls |
| `random:<seed>` | `RandomAgent` seeded at `<seed>` |

## Persona dataset

```sh
cargo run --release -p poker-trainer --bin poker_dataset -- \
    --personas nit,tag,lag,maniac,tilt --hands 2000 --window 500 --out logs/
```

Round-robins every persona heads-up against every other persona, then prints:

1. **Per-persona summary** — pooled VPIP / PFR / AF / WTSD% / chip EV across every matchup.
2. **Class-conditional matrix** — rows for each `(own, opponent)` pair: how does TAG behave against a Maniac vs against a Nit?
3. **Windowed time series** — rolling N-hand windows per persona, surfacing time-correlated drift (the `tilt` persona is the canonical example).

`--out <dir>` writes a `<a>_vs_<b>.mp` `FileSink` log per matchup that any downstream analysis (or the replay viewer in [poker-client](../poker-client/)) can re-aggregate offline.

The aggregator API (`extract_hand_stats`, `class_conditional`, `windowed`) is in [src/dataset/](src/dataset/) and works on any event log — your own bots and future server-recorded hands slot in unchanged.

## Persistence

| What | API | Format |
|---|---|---|
| Trained blueprint | `solver::save_blueprint` / `load_blueprint` | Schema-versioned + abstraction-tagged. Load fails if either differs. |

Custom agents can manage their own persistence via the lifecycle hooks on `Agent` — see the engine's README.

## Tests

```sh
cargo test -p poker-trainer
```
