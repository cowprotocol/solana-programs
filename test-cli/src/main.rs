use clap::{Parser, Subcommand};
use cow_settlement_client::cow_settlement_interface::Pubkey;

mod cmd;
mod helpers;
mod instructions;
mod token;

fn home_dir() -> String {
    std::env::var("HOME").expect("`HOME` env not available")
}

fn default_keypair_path() -> String {
    format!("{}/.config/solana/id.json", home_dir())
}

/// The endpoint configured with `solana config set --url`, if one is readable.
///
/// The Solana CLI config is flat YAML, so we scrape the single field we care
/// about rather than depend on a YAML parser.
fn solana_config_rpc_url() -> Option<String> {
    let config =
        std::fs::read_to_string(format!("{}/.config/solana/cli/config.yml", home_dir())).ok()?;
    config.lines().find_map(|line| {
        let url = line
            .strip_prefix("json_rpc_url:")?
            .trim()
            .trim_matches(['\'', '"']);
        (!url.is_empty()).then(|| url.to_owned())
    })
}

fn default_rpc_url() -> String {
    solana_config_rpc_url().unwrap_or_else(|| "https://api.mainnet-beta.solana.com".to_owned())
}

#[derive(Parser)]
#[command(name = "cow", about = "CoW Protocol Solana dev/testing CLI")]
struct Cli {
    /// Solana RPC endpoint (defaults to the `solana config` endpoint, or mainnet-beta if not available.)
    #[arg(
        long,
        global = true,
        env = "RPC_URL",
        default_value_t = default_rpc_url()
    )]
    rpc_url: String,

    /// Path to the payer keypair (JSON format as written by `solana-keygen`)
    #[arg(
        long,
        global = true,
        env = "KEYPAIR",
        default_value_t = default_keypair_path()
    )]
    keypair: String,

    /// Settlement program ID
    #[arg(long, global = true, default_value_t = cow_settlement_client::cow_settlement_interface::ID)]
    program_id: Pubkey,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    #[command(about = "Create the singleton settlement state PDA")]
    Initialize(cmd::initialize::InitializeArgs),
    #[command(about = "Sell a token for another (e.g. `cow sell 1.0 SOL for USDC`)")]
    Sell(cmd::create_order::BuyOrSellArgs),
    #[command(about = "Buy a token using another (e.g. `cow buy 1.0 SOL with USDC`)")]
    Buy(cmd::create_order::BuyOrSellArgs),
    #[command(about = "Settle one or more orders")]
    Settle(cmd::settle::SettleArgs),
    #[command(about = "Manage the program's solvers (e.g. `cow solver add <address>`)")]
    Solver(cmd::solver::SolverArgs),
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let ctx = cmd::Context::from_args(&cli)?;
    match cli.command {
        Commands::Initialize(args) => cmd::initialize::run(ctx, args),
        Commands::Sell(args) => cmd::create_order::run_sell(ctx, args),
        Commands::Buy(args) => cmd::create_order::run_buy(ctx, args),
        Commands::Settle(args) => cmd::settle::run(ctx, args),
        Commands::Solver(args) => cmd::solver::run(ctx, args),
    }
}
