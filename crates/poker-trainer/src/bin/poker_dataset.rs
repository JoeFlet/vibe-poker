//! Round-robin opponent dataset generator.
//!
//! Runs every persona against every other persona heads-up for `--hands`
//! hands per matchup, accumulates the resulting `HandStats` rows, and
//! prints two views:
//!
//! 1. **Class-conditional** — for each (own, opponent) class pair, the
//!    rolled-up profile (VPIP / PFR / AF / WTSD / chip EV). This is the
//!    core "how does X behave against Y" table.
//! 2. **Windowed** — rolling N-hand windows per persona, surfacing
//!    time-correlated drift (e.g., post-loss VPIP inflation in the
//!    `tilt` persona).
//!
//! With `--out <dir>`, raw matchup logs are written as `FileSink` files so
//! the same data can be replayed in `poker-client` or re-analysed offline.

use std::fs;
use std::path::PathBuf;

use clap::Parser;

use poker_engine::agent::personas::{Lag, Maniac, Nit, Tag, TiltProne};
use poker_engine::agent::Agent;
use poker_engine::core::RsPokerEvaluator;
use poker_trainer::dataset::{
    class_conditional, extract_hand_stats, windowed, HandStats, StatRollup,
};
use poker_engine::game::{BettingRules, Engine, EventSink, FileSink, VecSink};
use poker_engine::sim::{SeedMode, SimConfig, SimRunner, StackPolicy};

#[derive(Parser, Debug)]
#[command(
    name = "poker_dataset",
    about = "Run round-robin persona matchups and print class-conditional + windowed stats",
)]
struct Cli {
    /// Comma-separated personas to include in the round-robin.
    /// Available: nit, tag, lag, maniac, tilt.
    #[arg(long, default_value = "nit,tag,lag,maniac,tilt")]
    personas: String,

    /// Hands per matchup (heads-up).
    #[arg(long, default_value_t = 2000)]
    hands: usize,

    /// Window size for the windowed report (hands).
    #[arg(long, default_value_t = 500)]
    window: u32,

    /// Step between window starts. Defaults to `--window` (non-overlapping).
    #[arg(long)]
    step: Option<u32>,

    /// Base seed; each matchup gets a deterministic offset.
    #[arg(long, default_value_t = 1)]
    seed: u64,

    /// Per-seat starting stacks.
    #[arg(long, default_value_t = 200)]
    stack: u32,

    /// Blind levels.
    #[arg(long, default_value = "1/2")]
    blinds: String,

    /// Optional output directory. Each matchup writes `<a>_vs_<b>.mp` here.
    #[arg(long)]
    out: Option<PathBuf>,
}

fn parse_blinds(s: &str) -> Result<(u32, u32), String> {
    let parts: Vec<&str> = s.split('/').map(str::trim).collect();
    if parts.len() != 2 {
        return Err(format!("blinds must look like SMALL/BIG, got {s:?}"));
    }
    let sb: u32 = parts[0].parse().map_err(|_| format!("bad small blind {:?}", parts[0]))?;
    let bb: u32 = parts[1].parse().map_err(|_| format!("bad big blind {:?}", parts[1]))?;
    Ok((sb, bb))
}

fn build_agent(name: &str, seed: u64) -> Result<Box<dyn Agent>, String> {
    match name {
        "nit" => Ok(Box::new(Nit::new(seed))),
        "tag" => Ok(Box::new(Tag::new(seed))),
        "lag" => Ok(Box::new(Lag::new(seed))),
        "maniac" => Ok(Box::new(Maniac::new(seed))),
        "tilt" => Ok(Box::new(TiltProne::new(seed))),
        other => Err(format!("unknown persona {other:?}")),
    }
}

/// Run one heads-up matchup and append its rows to `rows`.
/// Optionally tee events to a `FileSink` at `<out_dir>/<a>_vs_<b>.mp`.
fn run_matchup(
    a: &str,
    b: &str,
    hands: usize,
    seed: u64,
    stack: u32,
    rules: &BettingRules,
    out_dir: Option<&PathBuf>,
    rows: &mut Vec<HandStats>,
) -> Result<(), String> {
    let agents: Vec<Box<dyn Agent>> = vec![
        build_agent(a, seed.wrapping_mul(31).wrapping_add(0))?,
        build_agent(b, seed.wrapping_mul(31).wrapping_add(1))?,
    ];
    let stacks = vec![stack, stack];
    let engine = Engine::new(rules.clone(), RsPokerEvaluator);
    let mut runner = SimRunner::new(engine, agents, stacks);
    let cfg = SimConfig {
        seed: SeedMode::Sequential(seed),
        stack_policy: StackPolicy::Reset,
    };

    let mut vec_sink = VecSink::default();
    if let Some(dir) = out_dir {
        let path = dir.join(format!("{a}_vs_{b}.mp"));
        let mut file_sink = FileSink::create(&path)
            .map_err(|e| format!("creating {}: {e}", path.display()))?;
        let mut tee = TeeSink { a: &mut vec_sink, b: &mut file_sink };
        let _ = runner.run(hands, &cfg, &mut tee);
    } else {
        let _ = runner.run(hands, &cfg, &mut vec_sink);
    }

    let labels = vec![a.to_string(), b.to_string()];
    rows.extend(extract_hand_stats(&vec_sink.events, &labels));
    Ok(())
}

struct TeeSink<'a, A: EventSink + ?Sized, B: EventSink + ?Sized> {
    a: &'a mut A,
    b: &'a mut B,
}

impl<'a, A: EventSink + ?Sized, B: EventSink + ?Sized> EventSink for TeeSink<'a, A, B> {
    fn on_event(&mut self, event: &poker_engine::game::EngineEvent) {
        self.a.on_event(event);
        self.b.on_event(event);
    }
}

fn fmt_af(af: Option<f64>) -> String {
    match af {
        Some(v) if v.is_finite() => format!("{v:>6.2}"),
        _ => "    —".to_string(),
    }
}

fn print_class_conditional(rows: &[HandStats], personas: &[String]) {
    let cc = class_conditional(rows);
    println!();
    println!(
        "Class-conditional profile  ·  {} matchups  ·  {} rows",
        personas.len() * (personas.len() - 1) / 2,
        rows.len(),
    );
    println!();
    println!(
        " {:<8}  {:<8}  {:>6}  {:>6}  {:>6}  {:>6}  {:>6}  {:>8}",
        "own", "vs", "hands", "VPIP%", "PFR%", "AF", "WTSD%", "Chip EV"
    );
    println!(" {}", "─".repeat(72));
    for own in personas {
        for opp in personas {
            if own == opp {
                continue;
            }
            if let Some(r) = cc.get(&(own.clone(), opp.clone())) {
                println!(
                    " {:<8}  {:<8}  {:>6}  {:>5.1}  {:>5.1}  {}  {:>5.1}  {:>+8.2}",
                    own,
                    opp,
                    r.hands,
                    r.vpip_pct,
                    r.pfr_pct,
                    fmt_af(r.aggression_factor),
                    r.showdown_pct,
                    r.chip_ev,
                );
            }
        }
    }
}

fn print_per_persona_summary(rows: &[HandStats], personas: &[String]) {
    println!();
    println!("Per-persona summary (over all matchups)");
    println!();
    println!(
        " {:<8}  {:>6}  {:>6}  {:>6}  {:>6}  {:>6}  {:>8}",
        "persona", "hands", "VPIP%", "PFR%", "AF", "WTSD%", "Chip EV"
    );
    println!(" {}", "─".repeat(60));
    for p in personas {
        let subset: Vec<&HandStats> = rows.iter().filter(|r| &r.own_class == p).collect();
        let s = StatRollup::from_rows(subset);
        println!(
            " {:<8}  {:>6}  {:>5.1}  {:>5.1}  {}  {:>5.1}  {:>+8.2}",
            p,
            s.hands,
            s.vpip_pct,
            s.pfr_pct,
            fmt_af(s.aggression_factor),
            s.showdown_pct,
            s.chip_ev,
        );
    }
}

fn print_windowed(rows: &[HandStats], personas: &[String], window: u32, step: u32) {
    let w = windowed(rows, window, step);
    if w.is_empty() {
        return;
    }
    println!();
    println!(
        "Windowed stats  ·  window={window} hands  ·  step={step} hands",
    );
    for p in personas {
        let series: Vec<_> = w.iter().filter(|r| &r.class == p).collect();
        if series.is_empty() {
            continue;
        }
        // Aggregate by window_start across all matchups (a persona shows up
        // in multiple matchups; pool their windows by start hand_id).
        use std::collections::BTreeMap;
        let mut grouped: BTreeMap<u64, Vec<&HandStats>> = BTreeMap::new();
        for win in &series {
            let start = win.window_start;
            grouped
                .entry(start)
                .or_default()
                .extend(rows.iter().filter(|r| {
                    &r.own_class == p
                        && r.hand_id >= start
                        && r.hand_id < start + window as u64
                }));
        }

        println!();
        println!("  {p}");
        println!(
            "    {:<14}  {:>6}  {:>6}  {:>6}  {:>6}  {:>8}",
            "hands", "n", "VPIP%", "PFR%", "AF", "Chip EV"
        );
        for (start, batch) in grouped {
            let r = StatRollup::from_rows(batch);
            println!(
                "    {:>5}-{:<8}  {:>6}  {:>5.1}  {:>5.1}  {}  {:>+8.2}",
                start,
                start + window as u64,
                r.hands,
                r.vpip_pct,
                r.pfr_pct,
                fmt_af(r.aggression_factor),
                r.chip_ev,
            );
        }
    }
}

fn main() {
    let cli = Cli::parse();
    let personas: Vec<String> = cli
        .personas
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if personas.len() < 2 {
        eprintln!("error: need at least 2 personas");
        std::process::exit(2);
    }

    let (sb, bb) = parse_blinds(&cli.blinds).unwrap_or_else(|e| {
        eprintln!("error: {e}");
        std::process::exit(2);
    });
    let rules = BettingRules::no_limit_holdem(sb, bb, 2);

    if let Some(dir) = cli.out.as_ref() {
        if let Err(e) = fs::create_dir_all(dir) {
            eprintln!("error creating {}: {e}", dir.display());
            std::process::exit(1);
        }
    }

    let step = cli.step.unwrap_or(cli.window);
    let total_matchups = personas.len() * (personas.len() - 1) / 2;
    println!(
        "round-robin: {} personas, {total_matchups} matchups, {} hands each ({} total hands)",
        personas.len(),
        cli.hands,
        total_matchups * cli.hands,
    );

    let mut rows: Vec<HandStats> = Vec::new();
    for i in 0..personas.len() {
        for j in (i + 1)..personas.len() {
            let a = &personas[i];
            let b = &personas[j];
            let seed = cli.seed
                .wrapping_add((i * personas.len() + j) as u64 * 1009);
            print!("  {a} vs {b} … ");
            if let Err(e) = run_matchup(a, b, cli.hands, seed, cli.stack, &rules, cli.out.as_ref(), &mut rows) {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
            println!("done");
        }
    }

    print_per_persona_summary(&rows, &personas);
    print_class_conditional(&rows, &personas);
    print_windowed(&rows, &personas, cli.window, step);
}
