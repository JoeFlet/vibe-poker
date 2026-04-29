//! Play a trained blueprint against bot opponents and print stats.
//!
//! Loads a blueprint file produced by `poker_train`, wraps it in a
//! `StrategyAdapter`, and runs it head-to-head with the configured
//! opponents through `SimRunner`. Output is the same `print_report`
//! used by `poker_report`, so per-seat win-rate / VPIP / PFR / AF
//! comparisons drop in directly.

use std::path::PathBuf;

use clap::Parser;

use poker_engine::agent::builtin::{CallingStation, RandomAgent};
use poker_engine::agent::Agent;
use poker_engine::core::RsPokerEvaluator;
use poker_engine::game::{BettingRules, Engine, EngineEvent, EventSink, FileSink};
use poker_engine::sim::{SeedMode, SimConfig, SimRunner, StackPolicy};
use poker_trainer::solver::{
    load_blueprint, BlueprintStrategy, StrategyAdapter,
};
use poker_engine::stats::{print_report, StatsSink};

#[derive(Parser, Debug)]
#[command(
    name = "poker_play",
    about = "Play a trained MCCFR blueprint against bots and print stats",
)]
struct Cli {
    /// Path to a blueprint produced by `poker_train`.
    #[arg(long)]
    blueprint: PathBuf,

    /// Number of hands to play.
    #[arg(long, default_value_t = 5_000)]
    hands: usize,

    /// Comma-separated agent specs, one per seat. Use `blueprint` for the
    /// trained policy, `calling` for the calling station, or `random:<n>`
    /// for a random agent with that seed. Example:
    ///   --agents blueprint,calling
    ///   --agents calling,blueprint
    #[arg(long, default_value = "blueprint,calling")]
    agents: String,

    /// Per-seat starting stacks, comma-separated. Must match agent count.
    #[arg(long, default_value = "200,200")]
    stacks: String,

    /// Blind levels as small/big.
    #[arg(long, default_value = "1/2")]
    blinds: String,

    /// Base seed for deterministic deck dealing across the run.
    #[arg(long, default_value_t = 0)]
    seed: u64,

    /// Optional path to write a `FileSink` event log (MessagePack framing).
    /// Open with `poker-client --replay <path>`.
    #[arg(long)]
    log: Option<PathBuf>,
}

/// Fans a single event stream to two sinks. Used when we want both live
/// stats and a durable log from one run.
struct TeeSink<'a, A: EventSink + ?Sized, B: EventSink + ?Sized> {
    a: &'a mut A,
    b: &'a mut B,
}

impl<'a, A: EventSink + ?Sized, B: EventSink + ?Sized> EventSink for TeeSink<'a, A, B> {
    fn on_event(&mut self, event: &EngineEvent) {
        self.a.on_event(event);
        self.b.on_event(event);
    }
}

#[derive(Debug, Clone)]
enum AgentSpec {
    Blueprint,
    Calling,
    Random(u64),
}

impl AgentSpec {
    fn parse(s: &str) -> Result<Self, String> {
        let s = s.trim();
        if s == "blueprint" {
            return Ok(AgentSpec::Blueprint);
        }
        if s == "calling" {
            return Ok(AgentSpec::Calling);
        }
        if let Some(seed) = s.strip_prefix("random:") {
            let seed: u64 = seed
                .parse()
                .map_err(|_| format!("invalid random seed in {s:?}"))?;
            return Ok(AgentSpec::Random(seed));
        }
        Err(format!(
            "unknown agent {s:?} — valid: blueprint, calling, random:<seed>"
        ))
    }

    fn label(&self) -> String {
        match self {
            AgentSpec::Blueprint => "blueprint".to_string(),
            AgentSpec::Calling => "calling".to_string(),
            AgentSpec::Random(s) => format!("random:{s}"),
        }
    }
}

fn parse_stacks(s: &str) -> Result<Vec<u32>, String> {
    s.split(',')
        .map(|p| p.trim().parse::<u32>().map_err(|_| format!("bad stack {p:?}")))
        .collect()
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

fn main() {
    let cli = Cli::parse();

    let specs: Vec<AgentSpec> = cli
        .agents
        .split(',')
        .map(AgentSpec::parse)
        .collect::<Result<_, _>>()
        .unwrap_or_else(|e| {
            eprintln!("error: {e}");
            std::process::exit(2);
        });

    let stacks = parse_stacks(&cli.stacks).unwrap_or_else(|e| {
        eprintln!("error: {e}");
        std::process::exit(2);
    });
    if stacks.len() != specs.len() {
        eprintln!(
            "error: stacks count ({}) != agents count ({})",
            stacks.len(),
            specs.len()
        );
        std::process::exit(2);
    }

    let (sb, bb) = parse_blinds(&cli.blinds).unwrap_or_else(|e| {
        eprintln!("error: {e}");
        std::process::exit(2);
    });

    let blueprint = load_blueprint(&cli.blueprint).unwrap_or_else(|e| {
        eprintln!("error loading blueprint: {e}");
        std::process::exit(1);
    });
    let blueprint = BlueprintStrategy::from_table(blueprint);
    println!(
        "loaded blueprint: {} info sets from {}",
        blueprint.size(),
        cli.blueprint.display()
    );

    let agents: Vec<Box<dyn Agent>> = specs
        .iter()
        .enumerate()
        .map(|(i, spec)| -> Box<dyn Agent> {
            match spec {
                AgentSpec::Blueprint => Box::new(StrategyAdapter::new(
                    blueprint.clone(),
                    cli.seed.wrapping_mul(31).wrapping_add(i as u64),
                )),
                AgentSpec::Calling => Box::new(CallingStation),
                AgentSpec::Random(s) => Box::new(RandomAgent::new(*s)),
            }
        })
        .collect();

    let labels: Vec<String> = specs.iter().map(|s| s.label()).collect();

    let rules = BettingRules::no_limit_holdem(sb, bb, specs.len());
    let stack_label = if stacks.iter().all(|&s| s == stacks[0]) {
        stacks[0].to_string()
    } else {
        stacks
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join("/")
    };
    let engine = Engine::new(rules.clone(), RsPokerEvaluator);
    let mut runner = SimRunner::new(engine, agents, stacks);
    let cfg = SimConfig {
        seed: SeedMode::Sequential(cli.seed),
        stack_policy: StackPolicy::Reset,
    };
    let mut sink = StatsSink::new(specs.len());
    match cli.log.as_ref() {
        Some(path) => {
            let mut file_sink = FileSink::create(path).unwrap_or_else(|e| {
                eprintln!("error creating log {}: {e}", path.display());
                std::process::exit(1);
            });
            let mut tee = TeeSink { a: &mut sink, b: &mut file_sink };
            let _ = runner.run(cli.hands, &cfg, &mut tee);
            let _ = file_sink.flush();
            println!("wrote event log: {}", path.display());
        }
        None => {
            let _ = runner.run(cli.hands, &cfg, &mut sink);
        }
    }

    print_report(&sink, &labels, &rules, 1, &stack_label);
}
