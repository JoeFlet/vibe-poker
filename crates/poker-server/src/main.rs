//! TCP entry point for the live poker server.
//!
//! Bootstraps a [`Registry`], a [`TableManager`] with one default
//! table, then loops on `listener.accept()` spawning a session per
//! connection.

use std::path::PathBuf;
use std::sync::Arc;

use clap::Parser;
use tokio::net::TcpListener;
use tokio::signal;
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

use poker_engine::game::BettingRules;
use poker_server::{
    handle_connection, shutdown_all, Registry, ServerContext, Table, TableConfig, TableManager,
};

#[derive(Parser, Debug)]
#[command(
    name = "poker-server",
    about = "Live poker server",
)]
struct Cli {
    /// Address to bind on. Pass `0.0.0.0:<port>` to accept off-host.
    #[arg(long, default_value = "127.0.0.1:7878")]
    bind: String,

    /// Directory for persistent state (`users/<name>.mp` records).
    #[arg(long, default_value = "data")]
    data_dir: PathBuf,

    /// Small blind for the default table.
    #[arg(long, default_value_t = 1)]
    small_blind: u32,

    /// Big blind for the default table.
    #[arg(long, default_value_t = 2)]
    big_blind: u32,

    /// Maximum seats at the default table.
    #[arg(long, default_value_t = 2)]
    max_seats: u8,

    /// Default buy-in chips suggested to clients.
    #[arg(long, default_value_t = 200)]
    buy_in: u32,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info,poker_server=debug")),
        )
        .init();

    let cli = Cli::parse();
    if let Err(e) = run(cli).await {
        error!(error = %e, "server exited with error");
        std::process::exit(1);
    }
}

async fn run(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    let registry = Arc::new(Registry::open(&cli.data_dir).await?);
    let tables = TableManager::new();

    // Default heads-up cash table. Future steps will let admins
    // create / configure additional tables at runtime.
    let cfg = TableConfig {
        name: "Main".into(),
        max_seats: cli.max_seats,
        min_seats: 2,
        small_blind: cli.small_blind,
        big_blind: cli.big_blind,
        default_buy_in: cli.buy_in,
        ..TableConfig::default()
    };
    let rules = BettingRules::no_limit_holdem(cfg.small_blind, cfg.big_blind, cfg.max_seats as usize);
    let table = Table::new(1, cfg);
    tables.install(table, rules, Arc::clone(&registry)).await;

    let ctx = ServerContext {
        registry,
        tables: tables.clone(),
        limits: Default::default(),
    };

    let listener = TcpListener::bind(&cli.bind).await?;
    let local = listener.local_addr()?;
    info!(addr = %local, data_dir = %cli.data_dir.display(), "poker-server listening");

    loop {
        tokio::select! {
            res = listener.accept() => {
                let (stream, peer) = match res {
                    Ok(v) => v,
                    Err(e) => {
                        error!(error = %e, "accept failed");
                        continue;
                    }
                };
                info!(%peer, "accepted connection");
                let ctx = ctx.clone();
                tokio::spawn(async move {
                    handle_connection(stream, peer, ctx).await;
                });
            }
            _ = signal::ctrl_c() => {
                info!("shutdown requested");
                shutdown_all(&tables).await;
                break;
            }
        }
    }
    Ok(())
}
