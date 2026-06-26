//! Buffer-account helpers for the settlement integration tests.

use litesvm::LiteSVM;
use settlement_client::instructions::CreateBuffers;
use settlement_client::settlement_interface::pda::buffer::find_buffer_pda;
use solana_sdk::{
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    transaction::Transaction,
};

use super::token;

/// The canonical buffer PDA for `mint`.
pub fn buffer_pda(program_id: &Pubkey, mint: &Pubkey) -> Pubkey {
    find_buffer_pda(program_id, mint).0
}

/// Create the canonical buffer for `mint`, paid for by `payer`, unless it
/// already exists, and return its address. Idempotent so several orders can
/// share one buy mint.
pub fn ensure(svm: &mut LiteSVM, program_id: &Pubkey, payer: &Keypair, mint: &Pubkey) -> Pubkey {
    let pda = buffer_pda(program_id, mint);
    if svm.get_account(&pda).is_some() {
        return pda;
    }
    let ix = CreateBuffers {
        program_id: *program_id,
        payer: payer.pubkey(),
        mints: &[*mint],
    }
    .instruction();
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
    let pda = ensure(svm, program_id, payer, mint);
    if amount > 0 {
        token::mint_to(svm, payer, mint, &pda, amount);
    }
    pda
}
