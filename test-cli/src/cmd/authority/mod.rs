use clap::{Parser, Subcommand};

use super::Context;

mod list;
mod transfer;

use transfer::TransferArgs;

#[derive(Parser)]
pub struct AuthorityArgs {
    #[command(subcommand)]
    command: AuthorityCommand,
}

#[derive(Subcommand)]
enum AuthorityCommand {
    #[command(about = "Transfer a role to a new authority")]
    Transfer(TransferArgs),
    #[command(about = "List the current authority for each role")]
    List,
}

pub fn run(ctx: Context, args: AuthorityArgs) -> anyhow::Result<()> {
    match args.command {
        AuthorityCommand::Transfer(args) => transfer::run(ctx, args),
        AuthorityCommand::List => list::run(ctx),
    }
}
