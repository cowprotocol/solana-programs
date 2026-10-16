use clap::Args as ClapArgs;
use cow_settlement_client::{
    cow_settlement_interface::{Pubkey, Role},
    instruction::TransferAuthority,
};

use crate::cmd::Context;
use crate::utils::output::print_summary;
use crate::utils::pda::find_state_pda;

#[derive(ClapArgs)]
pub struct TransferArgs {
    /// Role to reassign
    role: Role,

    /// Address that becomes the role's new holder
    new_authority: Pubkey,

    /// Path to the keypair authorizing the transfer, which must be the manager
    /// or the role's current holder and must sign it (defaults to the payer
    /// keypair)
    #[arg(long)]
    signer: Option<String>,
}

pub fn run(ctx: Context, args: TransferArgs) -> anyhow::Result<()> {
    let TransferArgs {
        role,
        new_authority,
        signer,
    } = args;

    let signer = ctx.signer_or(signer)?;
    let signer_pubkey = signer.pubkey();

    let ix = TransferAuthority {
        program_id: ctx.program_id,
        signer: signer_pubkey,
        role,
        new_authority,
    };

    let submission = ctx.submit(&[ix.into()], &[&signer])?;

    let state_pda = find_state_pda(&ctx.program_id);
    print_summary(&submission.summary(&[
        ("role", &format!("{role:?}")),
        ("newAuthority", &new_authority),
        ("signer", &signer_pubkey),
        ("statePda", &state_pda),
    ]));

    Ok(())
}
