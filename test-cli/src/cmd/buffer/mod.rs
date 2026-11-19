use clap::{Parser, Subcommand};

use super::Context;

mod create;
mod reclaim;

use create::CreateArgs;
use reclaim::ReclaimArgs;

#[derive(Parser)]
pub struct BufferArgs {
    #[command(subcommand)]
    command: BufferCommand,
}

#[derive(Subcommand)]
enum BufferCommand {
    #[command(about = "Create the buffer PDA of each mint")]
    Create(CreateArgs),
    #[command(
        about = "Close buffer PDAs, burning up to a per-buffer limit, and send their rent to a target account"
    )]
    Reclaim(ReclaimArgs),
}

pub fn run(ctx: Context, args: BufferArgs) -> anyhow::Result<()> {
    match args.command {
        BufferCommand::Create(args) => create::run(ctx, args),
        BufferCommand::Reclaim(args) => reclaim::run(ctx, args),
    }
}
