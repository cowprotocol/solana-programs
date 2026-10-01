use clap::Args as ClapArgs;
use cow_settlement_client::{cow_settlement_interface::Pubkey, instruction::AddSolver};

use crate::cmd::Context;
use crate::utils::output::print_summary;
use crate::utils::pda::find_state_pda;

#[derive(ClapArgs)]
pub struct AddArgs {
    /// Address of the solver to authorize
    solver: Pubkey,

    /// Path to the solver-authority keypair, which authorizes the change and
    /// must sign it (defaults to the payer keypair, which always funds the
    /// state PDA's growth)
    #[arg(long)]
    solver_authority: Option<String>,
}

pub fn run(ctx: Context, args: AddArgs) -> anyhow::Result<()> {
    let payer = ctx.payer();
    let solver_authority = ctx.signer_or(args.solver_authority)?;
    let solver_authority_pubkey = solver_authority.pubkey();

    let ix = AddSolver {
        program_id: ctx.program_id,
        authority: solver_authority_pubkey,
        payer,
        solver: args.solver,
    };

    let submission = ctx.submit(&[ix.into()], &[&solver_authority])?;

    let state_pda = find_state_pda(&ctx.program_id);
    print_summary(&submission.summary(&[
        ("added solver", &args.solver),
        ("solverAuthority", &solver_authority_pubkey),
        ("statePda", &state_pda),
    ]));

    Ok(())
}
