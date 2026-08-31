//! Instruction builders for operations the CLI needs to compose.

use crate::token;
use anyhow::Context as _;
use cow_settlement_client::cow_settlement_interface::{pda::state::find_state_pda, Pubkey};
use solana_instruction::Instruction;
use solana_rpc_client::rpc_client::RpcClient;
use spl_token_2022_interface::instruction::{self as token_ix};

/// Build instructions that wrap `amount` lamports into the payer's WSOL ATA.
///
/// Returns in order: create ATA (only when it does not exist yet), system transfer, sync_native.
/// The caller should use the returned `wsol_ata` as the input token parameter for any following operations
pub fn wrap_sol(
    rpc: &RpcClient,
    payer: &Pubkey,
    amount: u64,
) -> anyhow::Result<(Pubkey, Vec<Instruction>)> {
    let wsol =
        token::resolve(rpc, payer, "WSOL").context("failed to resolve WSOL token account")?;

    let mut ixs = Vec::with_capacity(3);
    ixs.extend(wsol.create_ata_ix(payer));

    ixs.push(solana_system_interface::instruction::transfer(
        payer, &wsol.ta, amount,
    ));

    ixs.push(
        token_ix::sync_native(&wsol.token_program, &wsol.ta)
            .context("failed to build SyncNative instruction")?,
    );

    Ok((wsol.ta, ixs))
}

/// Build an `Approve` instruction delegating `amount` of `token` to the PDA
/// derived from `program_id`.
pub fn approve(
    program_id: &Pubkey,
    token: &token::ResolvedToken,
    owner: &Pubkey,
    amount: u64,
) -> anyhow::Result<Instruction> {
    let (settlement_pda, _) = find_state_pda(program_id);

    token_ix::approve(
        &token.token_program,
        &token.ta,
        &settlement_pda,
        owner,
        &[],
        amount,
    )
    .context("failed to build Approve instruction")
}
