use clap::{Parser, Subcommand};

use super::Context;

mod add;
mod remove;

use add::AddArgs;
use remove::RemoveArgs;

#[derive(Parser)]
pub struct SolverArgs {
    #[command(subcommand)]
    command: SolverCommand,
}

#[derive(Subcommand)]
enum SolverCommand {
    #[command(about = "Authorize a solver to settle orders")]
    Add(AddArgs),
    #[command(about = "Revoke a solver's authorization to settle orders")]
    Remove(RemoveArgs),
}

pub fn run(ctx: Context, args: SolverArgs) -> anyhow::Result<()> {
    match args.command {
        SolverCommand::Add(args) => add::run(ctx, args),
        SolverCommand::Remove(args) => remove::run(ctx, args),
    }
}
