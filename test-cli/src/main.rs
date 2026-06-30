use clap::{Parser, Subcommand};
use settlement_client::settlement_interface::Pubkey;

mod cmd;
mod instructions;
mod token;

/// Default program ID declared in `settlement_interface`.
const DEFAULT_PROGRAM_ID: &str = "MooohhPEAAHwAwEozL7JPEmnDvaahuUpccYN4Yb8ccK";

#[derive(Parser)]
#[command(name = "cow", about = "CoW Protocol Solana dev/testing CLI")]
struct Cli {
    /// Solana RPC endpoint
    #[arg(
        long,
        global = true,
        env = "RPC_URL",
        default_value = "https://api.devnet.solana.com"
    )]
    rpc_url: String,

    /// Path to the payer keypair (JSON format as written by `solana-keygen`)
    #[arg(
        long,
        global = true,
        env = "KEYPAIR",
        default_value = "~/.config/solana/id.json"
    )]
    keypair: String,

    /// Settlement program ID
    #[arg(long, global = true, default_value = DEFAULT_PROGRAM_ID)]
    program_id: Pubkey,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Sell a token for another (e.g. `cow sell 1.0 SOL for USDC`)
    Sell(cmd::create_order::BuyOrSellArgs),
    /// Buy a token using another (e.g. `cow buy 1.0 SOL with USDC`)
    Buy(cmd::create_order::BuyOrSellArgs),
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let ctx = cmd::Context {
        rpc_url: cli.rpc_url,
        keypair: cli.keypair,
        program_id: cli.program_id,
    };
    match cli.command {
        Commands::Sell(args) => cmd::create_order::run_sell(ctx, args),
        Commands::Buy(args) => cmd::create_order::run_buy(ctx, args),
    }
}
