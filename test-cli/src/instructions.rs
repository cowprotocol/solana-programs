//! Instruction builders for operations the CLI needs to compose.

use crate::token;
use anyhow::Context as _;
use settlement_client::settlement_interface::Pubkey;
use solana_instruction::Instruction;
use solana_rpc_client::rpc_client::RpcClient;
use spl_token_interface::instruction::{self as token_ix};

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
        token_ix::sync_native(&spl_token_interface::id(), &wsol.ta)
            .context("failed to build SyncNative instruction")?,
    );

    Ok((wsol.ta, ixs))
}

/// Build an `Approve` instruction delegating `amount` tokens on `token_account`
/// to `delegate`.
pub fn approve(
    token_program_id: &Pubkey,
    token_account: &Pubkey,
    owner: &Pubkey,
    delegate: &Pubkey,
    amount: u64,
) -> anyhow::Result<Instruction> {
    token_ix::approve(
        token_program_id,
        token_account,
        delegate,
        owner,
        &[],
        amount,
    )
    .context("failed to build Approve instruction")
}

/// Build `SetAuthority` instruction which modifies the current CloseAuthority
/// for an account.
pub fn set_close_authority(
    token_program_id: &Pubkey,
    token_account: &Pubkey,
    owner: &Pubkey,
    new_close_authority: &Pubkey,
) -> anyhow::Result<Instruction> {
    token_ix::set_authority(
        token_program_id,
        token_account,
        Some(new_close_authority),
        token_ix::AuthorityType::CloseAccount,
        owner,
        &[],
    )
    .context("failed to build SetAuthority instruction")
}
