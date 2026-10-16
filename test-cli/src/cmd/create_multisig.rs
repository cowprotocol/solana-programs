use clap::Args as ClapArgs;
use cow_settlement_client::cow_settlement_interface::Pubkey;
use solana_sdk::signature::{read_keypair_file, Signer};
use solana_sdk::signer::keypair::Keypair;

use crate::cmd::{Context, TxSigner};
use crate::utils::keypair::MaybeKeypair;
use crate::utils::output::print_summary;
use crate::utils::squads::{
    fetch_program_config, multisig_address, multisig_create_ix, vault_address,
};

#[derive(ClapArgs)]
pub struct CreateMultisigArgs {
    /// A member of the multisig, allowed to propose, vote and execute. Repeat
    /// for each member
    #[arg(long = "member", required = true)]
    members: Vec<Pubkey>,

    /// Number of approvals a transaction needs to execute
    #[arg(long)]
    threshold: u16,

    /// Path to the keypair the multisig address is derived from, such as one
    /// written by `grind-multisig` (defaults to a new random key)
    #[arg(long)]
    create_key: Option<String>,
}

pub fn run(ctx: Context, args: CreateMultisigArgs) -> anyhow::Result<()> {
    anyhow::ensure!(
        ctx.squads.is_none(),
        "a multisig can't be created through --squads-multisig, as the create key must co-sign"
    );
    let create_key = match args.create_key {
        Some(path) => read_keypair_file(&path)
            .map_err(|e| anyhow::anyhow!("failed to read keypair from {path}: {e}"))?,
        None => Keypair::new(),
    };
    let create_key_pubkey = create_key.pubkey();

    let config = fetch_program_config(&ctx.rpc)?;
    let ix = multisig_create_ix(
        &config,
        &create_key_pubkey,
        &ctx.payer(),
        &args.members,
        args.threshold,
    )?;
    let submission = ctx.submit(
        &[ix],
        &[&TxSigner::Keypair(MaybeKeypair::Owned(create_key))],
    )?;

    let multisig = multisig_address(&create_key_pubkey);
    print_summary(&submission.summary(&[
        ("createKey", &create_key_pubkey),
        ("squadsMultisig", &multisig),
        ("squadsVault", &vault_address(&multisig, 0)),
        ("squadsCreationFee", &config.multisig_creation_fee),
    ]));
    Ok(())
}
