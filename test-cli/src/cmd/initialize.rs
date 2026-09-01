use anyhow::Context as _;
use clap::Args as ClapArgs;
use cow_settlement_client::{
    cow_settlement_interface::pda::state::find_state_pda, instruction::Initialize,
};
use solana_sdk::{pubkey::Pubkey, signature::Signer, transaction::Transaction};

use crate::utils::output::print_summary;

use super::Context;

#[derive(ClapArgs)]
pub struct InitializeArgs {
    /// Account authorized to add and remove solvers and to reassign every role
    /// (defaults to the payer)
    #[arg(long)]
    manager: Option<Pubkey>,
    /// Account authorized to reclaim buffer rent (defaults to the payer)
    #[arg(long)]
    reclaim_authority: Option<Pubkey>,
}

pub fn run(ctx: Context, args: InitializeArgs) -> anyhow::Result<()> {
    let payer = ctx.payer.pubkey();
    let (state_pda, _) = find_state_pda(&ctx.program_id);

    let ix = Initialize {
        program_id: ctx.program_id,
        payer,
        manager: args.manager.unwrap_or(payer),
        reclaim_authority: args.reclaim_authority.unwrap_or(payer),
    };

    let blockhash = ctx
        .rpc
        .get_latest_blockhash()
        .context("failed to fetch blockhash")?;
    let tx =
        Transaction::new_signed_with_payer(&[ix.into()], Some(&payer), &[&ctx.payer], blockhash);
    let sig = ctx
        .rpc
        .send_and_confirm_transaction(&tx)
        .context("transaction failed")?;

    print_summary(&[("signature", &sig), ("statePda", &state_pda)]);

    Ok(())
}
