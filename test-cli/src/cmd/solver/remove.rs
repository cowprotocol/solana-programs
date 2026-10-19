use clap::Args as ClapArgs;
use cow_settlement_client::{cow_settlement_interface::Pubkey, instruction::RemoveSolver};

use crate::cmd::Context;
use crate::utils::output::print_summary;
use crate::utils::pda::find_state_pda;

#[derive(ClapArgs)]
pub struct RemoveArgs {
    /// Address of the solver to revoke
    solver: Pubkey,

    /// Path to the solver-authority keypair, which authorizes the change and
    /// must sign it (defaults to the payer keypair)
    #[arg(long)]
    solver_authority: Option<String>,

    /// Account that receives the rent freed by removing the solver (defaults to
    /// the payer)
    #[arg(long)]
    rent_recipient: Option<Pubkey>,
}

pub fn run(ctx: Context, args: RemoveArgs) -> anyhow::Result<()> {
    let payer = ctx.payer();
    let solver_authority = ctx.signer_or(args.solver_authority)?;
    let solver_authority_pubkey = solver_authority.pubkey();
    // The freed rent lands on the payer unless another recipient is named.
    let rent_recipient = args.rent_recipient.unwrap_or(payer);

    let ix = RemoveSolver {
        program_id: ctx.program_id,
        authority: solver_authority_pubkey,
        rent_recipient,
        solver: args.solver,
    };

    let submission = ctx.submit(&[ix.into()], &[&solver_authority])?;

    let state_pda = find_state_pda(&ctx.program_id);
    print_summary(&submission.summary(&[
        ("removed solver", &args.solver),
        ("solverAuthority", &solver_authority_pubkey),
        ("rentRecipient", &rent_recipient),
        ("statePda", &state_pda),
    ]));

    Ok(())
}
