use clap::Args as ClapArgs;
use cow_settlement_client::instruction::Initialize;
use solana_sdk::pubkey::Pubkey;

use crate::utils::output::print_summary;
use crate::utils::pda::find_state_pda;

use super::Context;

#[derive(ClapArgs)]
pub struct InitializeArgs {
    /// Account authorized to reassign every role (defaults to the payer)
    #[arg(long)]
    manager: Option<Pubkey>,
    /// Account authorized to add and remove solvers (defaults to the payer)
    #[arg(long)]
    solver_authority: Option<Pubkey>,
    /// Account authorized to reclaim buffer rent (defaults to the payer)
    #[arg(long)]
    reclaim_authority: Option<Pubkey>,
    /// Account authorized to place settlement-owned orders (defaults to the
    /// payer)
    #[arg(long)]
    settlement_owned_order_authority: Option<Pubkey>,
}

pub fn run(ctx: Context, args: InitializeArgs) -> anyhow::Result<()> {
    let payer = ctx.payer();
    let state_pda = find_state_pda(&ctx.program_id);

    if matches!(ctx.rpc.get_account(&state_pda), Ok(account) if account.owner == ctx.program_id) {
        print_summary(&[("statePda", &state_pda), ("status", &"already initialized")]);
        return Ok(());
    }

    let ix = Initialize {
        program_id: ctx.program_id,
        payer,
        manager: args.manager.unwrap_or(payer),
        solver_authority: args.solver_authority.unwrap_or(payer),
        reclaim_authority: args.reclaim_authority.unwrap_or(payer),
        settlement_owned_order_authority: args.settlement_owned_order_authority.unwrap_or(payer),
    };

    let submission = ctx.submit(&[ix.into()], &[])?;

    print_summary(&submission.summary(&[("statePda", &state_pda)]));

    Ok(())
}
