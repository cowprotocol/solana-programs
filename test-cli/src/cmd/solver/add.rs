use anyhow::Context as _;
use clap::Args as ClapArgs;
use cow_settlement_client::{
    cow_settlement_interface::{pda::state::find_state_pda, Pubkey},
    instruction::AddSolver,
};
use solana_sdk::{signature::Signer, transaction::Transaction};

use crate::cmd::Context;
use crate::utils::keypair::read_keypair_or;
use crate::utils::output::print_summary;

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

pub fn run(ctx: Context, args: AddArgs) -> anyhow::Result<()> {
    let payer = ctx.payer.pubkey();
    let manager = read_keypair_or(args.manager, &ctx.payer)?;
    let manager_pubkey = manager.pubkey();

    let ix = AddSolver {
        program_id: ctx.program_id,
        manager: manager_pubkey,
        payer,
        solver: args.solver,
    };

    let blockhash = ctx
        .rpc
        .get_latest_blockhash()
        .context("failed to fetch blockhash")?;
    let tx = Transaction::new_signed_with_payer(
        &[ix.into()],
        Some(&payer),
        &[&ctx.payer, &*manager],
        blockhash,
    );
    let sig = ctx
        .rpc
        .send_and_confirm_transaction(&tx)
        .context("transaction failed")?;

    let (state_pda, _) = find_state_pda(&ctx.program_id);
    print_summary(&[
        ("signature", &sig),
        ("added solver", &args.solver),
        ("manager", &manager_pubkey),
        ("statePda", &state_pda),
    ]);

    Ok(())
}
