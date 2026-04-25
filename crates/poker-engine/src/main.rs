use clap::Parser;

use poker_engine::agent::builtin::{CallingStation, RandomAgent};
use poker_engine::agent::human::HumanAgent;
use poker_engine::agent::Agent;
use poker_engine::core::RsPokerEvaluator;
use poker_engine::game::BettingRules;
use poker_engine::game::Engine;
use poker_engine::sim::{run_parallel, SeedMode, SimConfig, SimRunner, StackPolicy};
use poker_engine::stats::{print_report, StatsSink};

// -----------------------------------------------------------------------
// CLI argument parsing
// -----------------------------------------------------------------------

#[derive(Parser, Debug)]
#[command(
    name = "poker_report",
    about = "Run AI agents against each other and print statistics",
    long_about = None,
)]
struct Cli {
    /// Number of hands to simulate.
    #[arg(long, default_value_t = 1000)]
    hands: usize,

    /// Comma-separated agent specs for each seat.
    /// Available: calling, random:<seed>
    /// Example: --agents calling,random:1,random:2
    #[arg(long, default_value = "random:1,random:2")]
    agents: String,

    /// Starting chip stack for every seat (uniform).
    #[arg(long, default_value_t = 200)]
    stack: u32,

    /// Per-seat starting stacks, comma-separated (overrides --stack).
    /// Must have exactly one value per agent. Example: --stacks 500,200,50
    #[arg(long)]
    stacks: Option<String>,

    /// Blind levels as small/big.
    #[arg(long, default_value = "1/2")]
    blinds: String,

    /// Base deck seed for deterministic runs. Omit for random seeds.
    #[arg(long)]
    seed: Option<u64>,

    /// Number of parallel threads.
    #[arg(long, default_value_t = 1)]
    threads: usize,
}

// -----------------------------------------------------------------------
// Agent parsing
// -----------------------------------------------------------------------

#[derive(Debug, Clone)]
enum AgentSpec {
    Calling,
    Random(u64),
    Human,
}

impl AgentSpec {
    fn parse(s: &str) -> Result<Self, String> {
        let s = s.trim();
        if s == "calling" {
            return Ok(AgentSpec::Calling);
        }
        if s == "human" {
            return Ok(AgentSpec::Human);
        }
        if let Some(seed_str) = s.strip_prefix("random:") {
            let seed: u64 = seed_str
                .parse()
                .map_err(|_| format!("invalid random seed in '{s}'"))?;
            return Ok(AgentSpec::Random(seed));
        }
        Err(format!(
            "unknown agent '{s}' — valid options: calling, random:<seed>, human"
        ))
    }

    fn name(&self) -> String {
        match self {
            AgentSpec::Calling => "calling".to_string(),
            AgentSpec::Random(s) => format!("random:{s}"),
            AgentSpec::Human => "human".to_string(),
        }
    }

    fn build(&self) -> Box<dyn Agent> {
        match self {
            AgentSpec::Calling => Box::new(CallingStation),
            AgentSpec::Random(seed) => Box::new(RandomAgent::new(*seed)),
            AgentSpec::Human => Box::new(HumanAgent::new()),
        }
    }

    fn is_human(&self) -> bool {
        matches!(self, AgentSpec::Human)
    }
}

// -----------------------------------------------------------------------
// Blind parsing
// -----------------------------------------------------------------------

fn parse_blinds(s: &str) -> Result<(u32, u32), String> {
    let parts: Vec<&str> = s.splitn(2, '/').collect();
    if parts.len() != 2 {
        return Err(format!("expected small/big format, got '{s}'"));
    }
    let sb: u32 = parts[0]
        .trim()
        .parse()
        .map_err(|_| format!("invalid small blind '{}'", parts[0]))?;
    let bb: u32 = parts[1]
        .trim()
        .parse()
        .map_err(|_| format!("invalid big blind '{}'", parts[1]))?;
    if sb == 0 || bb == 0 || sb >= bb {
        return Err(format!("blinds must satisfy 0 < sb < bb, got {sb}/{bb}"));
    }
    Ok((sb, bb))
}

// -----------------------------------------------------------------------
// Entry point
// -----------------------------------------------------------------------

fn main() {
    let cli = Cli::parse();

    // Parse agent specs.
    let specs: Vec<AgentSpec> = cli
        .agents
        .split(',')
        .map(|s| AgentSpec::parse(s).unwrap_or_else(|e| { eprintln!("Error: {e}"); std::process::exit(1); }))
        .collect();

    if specs.len() < 2 {
        eprintln!("Error: need at least 2 agents");
        std::process::exit(1);
    }

    let human_count = specs.iter().filter(|s| s.is_human()).count();
    if human_count > 1 {
        eprintln!("Error: at most one human seat is supported (multiple humans share a terminal)");
        std::process::exit(1);
    }
    if human_count > 0 && cli.threads > 1 {
        eprintln!("Error: human agent is not compatible with --threads > 1");
        std::process::exit(1);
    }

    // Parse blinds.
    let (sb, bb) = parse_blinds(&cli.blinds).unwrap_or_else(|e| {
        eprintln!("Error: {e}");
        std::process::exit(1);
    });

    let n = specs.len();
    let rules = BettingRules::no_limit_holdem(sb, bb, n);
    let stacks: Vec<u32> = match cli.stacks {
        Some(ref s) => {
            let parsed: Vec<u32> = s
                .split(',')
                .map(|v| {
                    v.trim().parse().unwrap_or_else(|_| {
                        eprintln!("Error: invalid stack value '{v}' in --stacks");
                        std::process::exit(1);
                    })
                })
                .collect();
            if parsed.len() != n {
                eprintln!(
                    "Error: --stacks has {} values but there are {} agents",
                    parsed.len(),
                    n
                );
                std::process::exit(1);
            }
            parsed
        }
        None => vec![cli.stack; n],
    };
    let agent_names: Vec<String> = specs.iter().map(|s| s.name()).collect();

    let seed_config = match cli.seed {
        Some(base) => SeedMode::Sequential(base),
        None => SeedMode::Random,
    };
    let config = SimConfig {
        seed: seed_config,
        stack_policy: StackPolicy::Reset,
    };

    let mut stats = StatsSink::new(n);

    if cli.threads <= 1 {
        // Single-threaded: collect events into StatsSink.
        let agents: Vec<Box<dyn Agent>> = specs.iter().map(|s| s.build()).collect();
        let mut runner = SimRunner::new(
            Engine::new(rules.clone(), RsPokerEvaluator),
            agents,
            stacks.clone(),
        );
        runner.run(cli.hands, &config, &mut stats);
    } else {
        // Parallel: aggregate chip deltas from all threads, then re-run a single
        // thread to capture stats. (Parallel run discards events for throughput.)
        let parallel_result = run_parallel(
            cli.hands,
            cli.threads,
            &stacks,
            &|| Engine::new(rules.clone(), RsPokerEvaluator),
            &|| specs.iter().map(|s| s.build()).collect(),
            &config,
        );

        // Re-run a representative single pass for per-hand stats (uses same seed).
        // This is a small fraction of the total work (1/threads overhead).
        let sample_hands = (cli.hands / cli.threads).max(1);
        let sample_config = SimConfig {
            seed: match config.seed {
                SeedMode::Sequential(base) => SeedMode::Sequential(base),
                SeedMode::Random => SeedMode::Random,
            },
            stack_policy: StackPolicy::Reset,
        };
        let agents: Vec<Box<dyn Agent>> = specs.iter().map(|s| s.build()).collect();
        let mut runner = SimRunner::new(
            Engine::new(rules.clone(), RsPokerEvaluator),
            agents,
            stacks.clone(),
        );
        runner.run(sample_hands, &sample_config, &mut stats);

        // Override chip_delta_total with the accurate parallel totals.
        for (seat, outcome_delta) in parallel_result.chip_deltas.iter().enumerate() {
            if let Some(s) = stats.seats.get_mut(seat) {
                // Scale the sampled per-hand chip deltas to the full run.
                // Accurate chip totals come from the parallel run.
                let ratio = cli.hands as f64 / sample_hands as f64;
                s.chip_delta_total = *outcome_delta;
                s.hands_dealt = (s.hands_dealt as f64 * ratio).round() as u64;
                s.hands_won   = (s.hands_won   as f64 * ratio).round() as u64;
                s.vpip_hands  = (s.vpip_hands  as f64 * ratio).round() as u64;
                s.pfr_hands   = (s.pfr_hands   as f64 * ratio).round() as u64;
                s.total_pot   = (s.total_pot   as f64 * ratio).round() as u64;
            }
        }
    }

    let stack_label = if stacks.windows(2).all(|w| w[0] == w[1]) {
        stacks[0].to_string()
    } else {
        stacks.iter().map(|s| s.to_string()).collect::<Vec<_>>().join("/")
    };
    print_report(&stats, &agent_names, &rules, cli.threads, &stack_label);
}
