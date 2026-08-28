use anyhow::Context as _;
use clap::{Args as ClapArgs, Parser, Subcommand};
use cow_settlement_client::{
    cow_settlement_interface::{pda::state::find_state_pda, Pubkey},
    instructions::AddSolver,
};
use solana_sdk::{
    signature::{read_keypair_file, Signer},
    signer::keypair::Keypair,
    transaction::Transaction,
};

use crate::helpers::print_summary;

use super::Context;

#[derive(Parser)]
pub struct SolverArgs {
    #[command(subcommand)]
    command: SolverCommand,
}

#[derive(Subcommand)]
enum SolverCommand {
    #[command(about = "Authorize a solver to settle orders")]
    Add(AddArgs),
}

#[derive(ClapArgs)]
pub struct AddArgs {
    /// Address of the solver to authorize
    solver: Pubkey,

    /// Path to the manager keypair, which authorizes the change and must sign
    /// it (defaults to the payer keypair, which always funds the state PDA's
    /// growth)
    #[arg(long)]
    manager: Option<String>,
}

pub fn run(ctx: Context, args: SolverArgs) -> anyhow::Result<()> {
    match args.command {
        SolverCommand::Add(args) => add(ctx, args),
    }
}

fn add(ctx: Context, args: AddArgs) -> anyhow::Result<()> {
    let payer = ctx.payer.pubkey();
    // Without `--manager` the payer manages the program, so reuse the keypair
    // the context already holds instead of reading a file again.
    let from_file = args
        .manager
        .map(|path| {
            read_keypair_file(&path)
                .map_err(|e| anyhow::anyhow!("failed to read manager keypair from {path}: {e}"))
        })
        .transpose()?;
    let manager = from_file.as_ref().unwrap_or(&ctx.payer);
    let manager_pubkey = manager.pubkey();

    let ix = AddSolver {
        program_id: ctx.program_id,
        manager: manager_pubkey,
        payer,
        solver: args.solver,
    };

    // A manager that is the payer signs a single slot, so don't pass it twice.
    let mut signers: Vec<&Keypair> = vec![&ctx.payer];
    if manager_pubkey != payer {
        signers.push(manager);
    }

    let blockhash = ctx
        .rpc
        .get_latest_blockhash()
        .context("failed to fetch blockhash")?;
    let tx = Transaction::new_signed_with_payer(&[ix.into()], Some(&payer), &signers, blockhash);
    let sig = ctx
        .rpc
        .send_and_confirm_transaction(&tx)
        .context("transaction failed")?;

    let (state_pda, _) = find_state_pda(&ctx.program_id);
    print_summary(&[
        ("signature", &sig),
        ("solver", &args.solver),
        ("manager", &manager_pubkey),
        ("statePda", &state_pda),
    ]);

    Ok(())
}
