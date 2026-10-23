use clap::{Parser, Subcommand};

use super::Context;

mod create;

use create::CreateArgs;

#[derive(Parser)]
pub struct BufferArgs {
    #[command(subcommand)]
    command: BufferCommand,
}

#[derive(Subcommand)]
enum BufferCommand {
    #[command(about = "Create the buffer PDA of each mint")]
    Create(CreateArgs),
}

pub fn run(ctx: Context, args: BufferArgs) -> anyhow::Result<()> {
    match args.command {
        BufferCommand::Create(args) => create::run(ctx, args),
    }
}
