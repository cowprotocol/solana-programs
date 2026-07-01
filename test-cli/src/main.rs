use clap::{Parser, Subcommand};
use settlement_client::settlement_interface::Pubkey;

mod cmd;
mod instructions;
mod token;

fn default_keypair_path() -> String {
    let home = std::env::var("HOME").expect("`HOME` env not available");
    format!("{home}/.config/solana/id.json")
}

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
        default_value_t = default_keypair_path()
    )]
    keypair: String,

    /// Settlement program ID
    #[arg(long, global = true, default_value = None)]
    program_id: Option<Pubkey>,

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
    let ctx = cmd::Context::from_args(&cli)?;
    match cli.command {
        Commands::Sell(args) => cmd::create_order::run_sell(ctx, args),
        Commands::Buy(args) => cmd::create_order::run_buy(ctx, args),
    }
}
