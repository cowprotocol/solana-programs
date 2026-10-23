use anyhow::Context as _;
use solana_instruction::Instruction;
use solana_sdk::{
    signature::{Signature, Signer},
    signer::keypair::Keypair,
    transaction::Transaction,
};

use crate::cmd::Context;

/// Sends `ixs` in a transaction paid by the context's payer and signed by
/// `signers` on top.
pub fn send(ctx: &Context, ixs: &[Instruction], signers: &[&Keypair]) -> anyhow::Result<Signature> {
    let blockhash = ctx
        .rpc
        .get_latest_blockhash()
        .context("failed to fetch blockhash")?;
    let signers: Vec<&Keypair> = std::iter::once(&ctx.payer)
        .chain(signers.iter().copied())
        .collect();
    let tx =
        Transaction::new_signed_with_payer(ixs, Some(&ctx.payer.pubkey()), &signers, blockhash);
    ctx.rpc
        .send_and_confirm_transaction(&tx)
        .context("transaction failed")
}
