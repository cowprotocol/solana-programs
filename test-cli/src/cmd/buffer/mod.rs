use anyhow::Context as _;
use clap::{Parser, Subcommand};
use solana_instruction::Instruction;
use solana_sdk::{
    signature::{Signature, Signer},
    signer::keypair::Keypair,
    transaction::Transaction,
};

use super::Context;

mod create;
mod reclaim;

use create::CreateArgs;
use reclaim::ReclaimArgs;

#[derive(Parser)]
pub struct BufferArgs {
    #[command(subcommand)]
    command: BufferCommand,
}

#[derive(Subcommand)]
enum BufferCommand {
    #[command(about = "Create the buffer PDA of each mint")]
    Create(CreateArgs),
    #[command(about = "Close empty buffer PDAs and send their rent to a target account")]
    Reclaim(ReclaimArgs),
}

pub fn run(ctx: Context, args: BufferArgs) -> anyhow::Result<()> {
    match args.command {
        BufferCommand::Create(args) => create::run(ctx, args),
        BufferCommand::Reclaim(args) => reclaim::run(ctx, args),
    }
}

/// Sends `ixs` in a transaction paid by the context's payer and signed by
/// `signers` on top.
fn send(ctx: &Context, ixs: &[Instruction], signers: &[&Keypair]) -> anyhow::Result<Signature> {
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

/// `items` with repeats removed, keeping the first occurrence of each.
fn dedup<T: Copy + Eq + std::hash::Hash>(items: &[T]) -> Vec<T> {
    let mut seen = std::collections::HashSet::new();
    items
        .iter()
        .copied()
        .filter(|item| seen.insert(*item))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dedup_keeps_first_occurrences_in_order() {
        assert_eq!(dedup(&[3, 1, 3, 2, 1]), [3, 1, 2]);
    }
}
