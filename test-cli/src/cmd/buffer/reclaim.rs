use std::collections::HashMap;

use anyhow::Context as _;
use clap::Args as ClapArgs;
use cow_settlement_client::{
    cow_settlement_interface::{
        instruction::reclaim_buffer::ReclaimBuffer, pda::buffer::find_buffer_pda, Pubkey,
    },
    instruction::TokenProgram,
};
use solana_instruction::Instruction;
use solana_sdk::{account::Account, signature::Signer};

use crate::utils::keypair::read_keypair_or;
use crate::utils::output::print_summary;
use crate::utils::parsing::{BufferArg, KeypairArg};
use crate::utils::pda::find_state_pda;
use crate::utils::token::{token_program_of, unpack_token_account};
use crate::{cmd::Context, utils::transaction::send};

#[derive(ClapArgs)]
pub struct ReclaimArgs {
    /// The account receiving the closed buffers' rent
    receiver: Pubkey,

    /// The buffer PDAs to close, all in one transaction.
    /// Buffers holding more tokens than their burn limit are left open.
    /// Specify just the buffer pubkey to close without burning, or if burning some
    /// tokens is desired, use the syntax `<pubkey>:<max burn in atoms>`.
    #[arg(required = true)]
    buffers: Vec<BufferArg>,

    /// Path to the reclaim-authority keypair, which must sign (defaults to the
    /// payer keypair)
    #[arg(long)]
    reclaim_authority: Option<KeypairArg>,
}

/// Issues a transaction to reclaim the specified buffers, optionally burning
/// up to the specified number of tokens in order to do so.
pub fn run(ctx: Context, args: ReclaimArgs) -> anyhow::Result<()> {
    let reclaim_authority = read_keypair_or(args.reclaim_authority, &ctx.payer)?;

    let addresses: Vec<Pubkey> = args.buffers.iter().map(|b| b.address).collect();
    let accounts = ctx
        .rpc
        .get_multiple_accounts(&addresses)
        .context("failed to fetch buffer accounts")?;

    let mut by_program: HashMap<TokenProgram, Vec<(Pubkey, Pubkey, u64)>> = HashMap::new();
    let mut to_close = Vec::new();
    let mut non_empty = Vec::new();
    for (i, (buffer, account)) in args.buffers.iter().zip(accounts).enumerate() {
        let address = buffer.address;
        anyhow::ensure!(
            !addresses[..i].contains(&address),
            "bad buffer {address}, nothing was reclaimed: buffer listed more than once"
        );
        let BufferState {
            amount,
            token_program,
            mint,
        } = account
            .context("buffer account not found")
            .and_then(|account| decode_buffer(&account, &address, &ctx.program_id))
            .with_context(|| format!("bad buffer {address}, nothing was reclaimed"))?;
        if amount <= buffer.max_burn {
            by_program
                .entry(token_program)
                .or_default()
                .push((address, mint, buffer.max_burn));
            to_close.push(address);
        } else {
            non_empty.push(address);
        }
    }

    let mut reclaimed = Vec::new();
    let sig = if to_close.is_empty() {
        None
    } else {
        let ixs = instructions(
            &ctx.program_id,
            &find_state_pda(&ctx.program_id),
            &reclaim_authority.pubkey(),
            &args.receiver,
            &by_program,
        );
        let sig = send(&ctx, &ixs, &[&reclaim_authority])?;
        // The program skips rather than rejects a buffer that received tokens
        // since it was fetched, so check which ones are gone.
        let accounts = ctx
            .rpc
            .get_multiple_accounts(&to_close)
            .with_context(|| format!("sent {sig}, but failed to check the outcome"))?;
        for (buffer, account) in to_close.iter().zip(accounts) {
            match account {
                None => reclaimed.push(*buffer),
                Some(_) => non_empty.push(*buffer),
            }
        }
        Some(sig)
    };

    let mut summary: Vec<(&str, &dyn ToString)> = Vec::new();
    if let Some(sig) = &sig {
        summary.push(("signature", sig));
    }
    summary.push(("reclaimRecipient", &args.receiver));
    summary.extend(
        reclaimed
            .iter()
            .map(|pda| ("bufferPda", pda as &dyn ToString)),
    );
    summary.extend(
        non_empty
            .iter()
            .map(|pda| ("nonEmptyBufferPda", pda as &dyn ToString)),
    );
    print_summary(&summary);

    Ok(())
}

/// One `ReclaimBuffer` instruction per token program, as each serves only one.
fn instructions(
    program_id: &Pubkey,
    state_pda: &Pubkey,
    reclaim_authority: &Pubkey,
    target: &Pubkey,
    by_program: &HashMap<TokenProgram, Vec<(Pubkey, Pubkey, u64)>>,
) -> Vec<Instruction> {
    by_program
        .iter()
        .map(|(token_program, buffers)| {
            ReclaimBuffer {
                program_id: *program_id,
                state_pda: *state_pda,
                reclaim_authority: *reclaim_authority,
                reclaim_recipient: *target,
                token_program: token_program.address(),
                buffers,
            }
            .into()
        })
        .collect()
}

/// What closing a buffer depends on.
struct BufferState {
    token_program: TokenProgram,
    mint: Pubkey,
    amount: u64,
}

/// Decodes `account`, stored at `address`. Fails unless it's a token account
/// of a supported token program at the buffer PDA of its mint under
/// `program_id`.
fn decode_buffer(
    account: &Account,
    address: &Pubkey,
    program_id: &Pubkey,
) -> anyhow::Result<BufferState> {
    let token_program = token_program_of(account)?;
    let token_account =
        unpack_token_account(&account.data).context("account is not a token account")?;
    let mint = token_account.mint;
    anyhow::ensure!(
        find_buffer_pda(program_id, &mint).0 == *address,
        "not the buffer of mint {mint} under {program_id}"
    );
    Ok(BufferState {
        token_program,
        mint,
        amount: token_account.amount,
    })
}
