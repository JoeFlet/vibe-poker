//! Heads-up MCCFR blueprint trainer.
//!
//! Runs `MccfrTrainer::iterate` for the requested number of iterations
//! against itself, then writes the resulting `RegretTable` to a versioned
//! blueprint file. Read it back with `poker_play --blueprint <file>`.

use std::path::PathBuf;
use std::time::Instant;

use clap::Parser;

use poker_engine::core::RsPokerEvaluator;
use poker_engine::game::{BettingRules, Engine, SeatIndex};
use poker_engine::solver::{save_blueprint, MccfrTrainer};

#[derive(Parser, Debug)]
#[command(
    name = "poker_train",
    about = "Train a heads-up MCCFR blueprint and save it to disk",
)]
struct Cli {
    /// Number of training iterations. Each iteration plays two traversals
    /// (one per traverser) of one randomly-dealt heads-up hand.
    #[arg(long, default_value_t = 10_000)]
    iters: u64,

    /// Output blueprint path. The file is overwritten if it exists.
    #[arg(long)]
    out: PathBuf,

    /// Base seed. Iteration `i` uses deck seed `seed + i + 1`. Pin this for
    /// fully reproducible runs.
    #[arg(long, default_value_t = 0)]
    seed: u64,

    /// Heads-up starting stacks (one value per seat, two seats).
    /// Example: --stacks 200,200
    #[arg(long, default_value = "200,200")]
    stacks: String,

    /// Blind levels as small/big.
    #[arg(long, default_value = "1/2")]
    blinds: String,

    /// Print progress every N iterations.
    #[arg(long, default_value_t = 1_000)]
    progress: u64,
}

fn parse_stacks(s: &str) -> Result<[u32; 2], String> {
    let parts: Vec<&str> = s.split(',').map(str::trim).collect();
    if parts.len() != 2 {
        return Err(format!(
            "expected two stack values for heads-up training, got {}: {s:?}",
            parts.len()
        ));
    }
    let a: u32 = parts[0].parse().map_err(|_| format!("bad stack: {:?}", parts[0]))?;
    let b: u32 = parts[1].parse().map_err(|_| format!("bad stack: {:?}", parts[1]))?;
    Ok([a, b])
}

fn parse_blinds(s: &str) -> Result<(u32, u32), String> {
    let parts: Vec<&str> = s.split('/').map(str::trim).collect();
    if parts.len() != 2 {
        return Err(format!("blinds must look like SMALL/BIG, got {s:?}"));
    }
    let sb: u32 = parts[0].parse().map_err(|_| format!("bad small blind: {:?}", parts[0]))?;
    let bb: u32 = parts[1].parse().map_err(|_| format!("bad big blind: {:?}", parts[1]))?;
    Ok((sb, bb))
}

fn main() {
    let cli = Cli::parse();
    let stacks = parse_stacks(&cli.stacks).unwrap_or_else(|e| {
        eprintln!("error: {e}");
        std::process::exit(2);
    });
    let (sb, bb) = parse_blinds(&cli.blinds).unwrap_or_else(|e| {
        eprintln!("error: {e}");
        std::process::exit(2);
    });

    let engine = Engine::new(BettingRules::no_limit_holdem(sb, bb, 0), RsPokerEvaluator);
    let mut trainer = MccfrTrainer::new(cli.seed);

    println!(
        "training: iters={} stacks={:?} blinds={}/{} seed={} -> {}",
        cli.iters,
        stacks,
        sb,
        bb,
        cli.seed,
        cli.out.display(),
    );

    let start = Instant::now();
    for i in 0..cli.iters {
        let dealer: SeatIndex = (i % 2) as SeatIndex;
        let deck_seed = cli.seed.wrapping_add(i).wrapping_add(1);
        trainer.iterate(&engine, &stacks, dealer, deck_seed);
        if cli.progress > 0 && (i + 1) % cli.progress == 0 {
            let elapsed = start.elapsed().as_secs_f64();
            let rate = (i + 1) as f64 / elapsed;
            println!(
                "  iter {} / {}  ({:.0} it/s)  info_sets={}",
                i + 1,
                cli.iters,
                rate,
                trainer.table.len()
            );
        }
    }
    let elapsed = start.elapsed();

    save_blueprint(&trainer.table, &cli.out).unwrap_or_else(|e| {
        eprintln!("error saving blueprint: {e}");
        std::process::exit(1);
    });

    let bytes = std::fs::metadata(&cli.out).map(|m| m.len()).unwrap_or(0);
    println!(
        "done: {} info sets, {:.1} KiB on disk, {:.2} s wall ({:.0} it/s)",
        trainer.table.len(),
        bytes as f64 / 1024.0,
        elapsed.as_secs_f64(),
        cli.iters as f64 / elapsed.as_secs_f64().max(1e-9),
    );
}
