use anyhow::Context as _;
use clap::Args as ClapArgs;
use cow_settlement_client::{
    cow_settlement_interface::{pda::state::find_state_pda, Pubkey, Role},
    instruction::TransferAuthority,
};
use solana_sdk::{signature::Signer, transaction::Transaction};

use crate::cmd::Context;
use crate::utils::keypair::read_keypair_or;
use crate::utils::output::print_summary;

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

    let payer = ctx.payer.pubkey();
    let signer = read_keypair_or(signer, &ctx.payer)?;
    let signer_pubkey = signer.pubkey();

    let ix = TransferAuthority {
        program_id: ctx.program_id,
        signer: signer_pubkey,
        role,
        new_authority,
    };

    let blockhash = ctx
        .rpc
        .get_latest_blockhash()
        .context("failed to fetch blockhash")?;
    let tx = Transaction::new_signed_with_payer(
        &[ix.into()],
        Some(&payer),
        &[&ctx.payer, &*signer],
        blockhash,
    );
    let sig = ctx
        .rpc
        .send_and_confirm_transaction(&tx)
        .context("transaction failed")?;

    let (state_pda, _) = find_state_pda(&ctx.program_id);
    print_summary(&[
        ("signature", &sig),
        ("role", &format!("{role:?}")),
        ("newAuthority", &new_authority),
        ("signer", &signer_pubkey),
        ("statePda", &state_pda),
    ]);

    Ok(())
}
