use std::io::{self, BufRead, Write};

use super::{Agent, Observation};
use crate::game::{Action, EngineEvent, HandId, HandResult};

/// An interactive agent that prompts a human player on stdin/stdout.
///
/// Not suitable for parallel simulation — use single-threaded mode only.
pub struct HumanAgent {
    my_seat: Option<usize>,
}

impl HumanAgent {
    pub fn new() -> Self {
        HumanAgent { my_seat: None }
    }
}

impl Default for HumanAgent {
    fn default() -> Self {
        HumanAgent::new()
    }
}

impl Agent for HumanAgent {
    fn on_hand_start(&mut self, hand_id: HandId) {
        println!("\n{}\n  Hand #{hand_id}\n{}", "═".repeat(50), "═".repeat(50));
    }

    fn on_event(&mut self, event: &EngineEvent) {
        match event {
            EngineEvent::BoardDealt { street, cards } => {
                let card_str: Vec<String> = cards.iter().map(|c| c.to_string()).collect();
                println!("\n  [{:?}] Board: {}", street, card_str.join(" "));
            }
            EngineEvent::ActionTaken { seat, action, pot_total } => {
                // Skip our own action — it's already visible at the prompt.
                if self.my_seat == Some(*seat) {
                    return;
                }
                println!("  Seat {}: {}  (pot: {})", seat, format_action(action), pot_total);
            }
            _ => {}
        }
    }

    fn act(&mut self, obs: &Observation<'_>) -> Action {
        self.my_seat = Some(obs.position);
        print_state(obs);
        read_action(obs)
    }

    fn on_hand_end(&mut self, result: &HandResult) {
        println!("\n── Result ──────────────────────────────────────");
        if !result.board.is_empty() {
            let board: Vec<String> = result.board.iter().map(|c| c.to_string()).collect();
            println!("  Board: {}", board.join(" "));
        }
        for o in &result.seats {
            let cards = match &o.hole_cards {
                Some(cs) => format!("  [{} {}]", cs[0], cs[1]),
                None => "  [folded]".to_string(),
            };
            let delta = if o.chip_delta >= 0 {
                format!("\x1b[32m{:+}\x1b[0m", o.chip_delta)
            } else {
                format!("\x1b[31m{:+}\x1b[0m", o.chip_delta)
            };
            println!("  Seat {}{}  {}", o.seat, cards, delta);
        }
        println!("{}", "─".repeat(50));
    }
}

// ── Display helpers ──────────────────────────────────────────────────────────

fn print_state(obs: &Observation<'_>) {
    let street = format!("{:?}", obs.street);
    println!("\n── {} ─────────────────────────────────────────", street);

    // Board
    if !obs.board.is_empty() {
        let cards: Vec<String> = obs.board.iter().map(|c| c.to_string()).collect();
        println!("  Board:  {}", cards.join(" "));
    }

    // Hole cards
    println!(
        "  Your hand:  {} {}  (seat {})",
        obs.hole_cards[0], obs.hole_cards[1], obs.position
    );

    // Players table
    println!();
    println!("  {:<6} {:>7} {:>7}  Status", "Seat", "Stack", "Bet");
    println!("  {}", "─".repeat(34));
    for p in obs.players {
        let you = if p.seat == obs.position { " ← you" } else { "" };
        let status = if p.is_folded {
            "folded"
        } else if p.is_all_in {
            "all-in"
        } else {
            "active"
        };
        println!(
            "  {:<6} {:>7} {:>7}  {}{}",
            p.seat, p.stack, p.bet_this_street, status, you
        );
    }

    // Pot
    println!("\n  Pot: {}", obs.pot.total());

    // Legal actions
    println!("\n  Actions:");
    let la = &obs.legal_actions;
    if la.can_check {
        println!("    [c]heck");
    }
    if la.can_call {
        println!("    [c]all  ({} chips)", la.call_amount);
    }
    if la.can_raise {
        println!("    [r]aise <amount>  ({} – {} total)", la.min_raise, la.max_raise);
    }
    if la.all_in_amount > 0 {
        println!("    [a]ll-in  ({} chips)", la.all_in_amount);
    }
    println!("    [f]old");
}

fn read_action(obs: &Observation<'_>) -> Action {
    let la = &obs.legal_actions;
    let stdin = io::stdin();
    loop {
        print!("> ");
        io::stdout().flush().ok();

        let mut line = String::new();
        if stdin.lock().read_line(&mut line).is_err() {
            // EOF or pipe closed — fall back to fold.
            return Action::Fold;
        }
        let input = line.trim().to_lowercase();

        match parse_input(&input, la) {
            Ok(action) => return action,
            Err(msg) => println!("  {msg}"),
        }
    }
}

fn parse_input(
    input: &str,
    la: &crate::game::LegalActions,
) -> Result<Action, String> {
    let mut parts = input.splitn(2, char::is_whitespace);
    let cmd = parts.next().unwrap_or("").trim();
    let arg = parts.next().map(str::trim);

    match cmd {
        "f" | "fold" => Ok(Action::Fold),

        "c" | "check" if la.can_check => Ok(Action::Check),
        "c" | "call" if la.can_call => Ok(Action::Call),
        "c" | "check" | "call" => Err("neither check nor call is legal here".to_string()),

        "a" | "allin" | "all-in" | "all_in" if la.all_in_amount > 0 => Ok(Action::AllIn),
        "a" | "allin" | "all-in" | "all_in" => Err("you have no chips to go all-in with".to_string()),

        "r" | "raise" => {
            if !la.can_raise {
                return Err("raise is not legal here".to_string());
            }
            let amount_str = arg.ok_or_else(|| {
                format!("usage: raise <amount>  ({} – {})", la.min_raise, la.max_raise)
            })?;
            let amount: u32 = amount_str.parse().map_err(|_| {
                format!("'{}' is not a valid chip amount", amount_str)
            })?;
            if amount < la.min_raise || amount > la.max_raise {
                return Err(format!(
                    "raise amount must be between {} and {} (got {})",
                    la.min_raise, la.max_raise, amount
                ));
            }
            Ok(Action::Raise(amount))
        }

        _ => Err(format!(
            "unknown command '{}' — try: fold, check, call, raise <amount>, allin",
            cmd
        )),
    }
}

fn format_action(action: &Action) -> String {
    match action {
        Action::Fold => "folds".to_string(),
        Action::Check => "checks".to_string(),
        Action::Call => "calls".to_string(),
        Action::Raise(amount) => format!("raises to {amount}"),
        Action::AllIn => "goes all-in".to_string(),
    }
}
