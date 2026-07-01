//! Instruction builders for operations the CLI needs to compose.

use anyhow::Context as _;
use settlement_client::settlement_interface::{pda::SETTLEMENT_SEED, Pubkey};
use solana_instruction::Instruction;
use spl_associated_token_account_interface::{
    address::get_associated_token_address_with_program_id,
    instruction::create_associated_token_account_idempotent,
};
use spl_token_interface::instruction::{self as token_ix};

/// Build instructions that wrap `amount` lamports into the payer's WSOL ATA.
///
/// Returns in order: create ATA (idempotent), system transfer, sync_native.
/// The caller should use the returned `wsol_ata` as the input token parameter for any following operations
pub fn wrap_sol(payer: &Pubkey, amount: u64) -> anyhow::Result<(Pubkey, Vec<Instruction>)> {
    let wsol_mint: Pubkey = spl_token_interface::native_mint::id();
    let token_program: Pubkey = spl_token_interface::id();

    let wsol_ata = get_associated_token_address_with_program_id(payer, &wsol_mint, &token_program);

    let create_ata =
        create_associated_token_account_idempotent(payer, payer, &wsol_mint, &token_program);

    let transfer = solana_system_interface::instruction::transfer(payer, &wsol_ata, amount);

    let sync = token_ix::sync_native(&token_program, &wsol_ata)
        .context("failed to build SyncNative instruction")?;

    Ok((wsol_ata, vec![create_ata, transfer, sync]))
}

/// Build an `Approve` instruction delegating `amount` tokens on `token_account`
/// to the PDA derived from `program_id`.
pub fn approve(
    program_id: &Pubkey,
    token_account: &Pubkey,
    owner: &Pubkey,
    amount: u64,
) -> anyhow::Result<Instruction> {
    let (settlement_pda, _) = Pubkey::find_program_address(&[SETTLEMENT_SEED], program_id);

    token_ix::approve(
        &spl_token_interface::id(),
        token_account,
        &settlement_pda,
        owner,
        &[],
        amount,
    )
    .context("failed to build Approve instruction")
}
