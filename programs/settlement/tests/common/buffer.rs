//! Buffer-account helpers for the settlement integration tests.

use cow_settlement_client::cow_settlement_interface::pda::buffer::find_buffer_pda;
use cow_settlement_client::cow_settlement_interface::token_program::SPL_TOKEN_PROGRAM_ID;
use cow_settlement_client::cow_settlement_interface::Instruction;
use cow_settlement_client::instructions::CreateBuffers;
use litesvm::LiteSVM;
use solana_sdk::{
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    transaction::Transaction,
};

use super::{replace_first_matching_account, token};

/// The canonical buffer PDA for `mint`.
pub fn buffer_pda(program_id: &Pubkey, mint: &Pubkey) -> Pubkey {
    find_buffer_pda(program_id, mint).0
}

/// Create the canonical buffer for `mint`, paid for by `payer`, unless it
/// already exists, and return its address. Idempotent so several orders can
/// share one buy mint.
pub fn ensure_buffer_exists(
    svm: &mut LiteSVM,
    program_id: &Pubkey,
    payer: &Keypair,
    mint: &Pubkey,
) -> Pubkey {
    let pda = buffer_pda(program_id, mint);
    if svm.get_account(&pda).is_some() {
        return pda;
    }
    let mut ix = Instruction::from(CreateBuffers {
        program_id: *program_id,
        payer: payer.pubkey(),
        mints: &[*mint],
    });
    // A buffer is a token account of its mint, so it has to be created under the
    // mint's own program. The builder can only name the legacy one, so point the
    // instruction at whichever program the mint actually lives under — a no-op
    // for a legacy mint.
    replace_first_matching_account(&mut ix, &SPL_TOKEN_PROGRAM_ID, token::program_of(svm, mint));
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&payer.pubkey()),
        &[payer],
        svm.latest_blockhash(),
    );
    svm.send_transaction(tx)
        .expect("create_buffer should succeed");
    pda
}

/// Ensure the buffer for `mint` exists and mint `amount` of `mint` into it, so a
/// push can draw from it. Returns the buffer address.
pub fn ensure_funded(
    svm: &mut LiteSVM,
    program_id: &Pubkey,
    payer: &Keypair,
    mint: &Pubkey,
    amount: u64,
) -> Pubkey {
    let pda = ensure_buffer_exists(svm, program_id, payer, mint);
    if amount > 0 {
        token::mint_to(svm, payer, mint, &pda, amount);
    }
    pda
}
