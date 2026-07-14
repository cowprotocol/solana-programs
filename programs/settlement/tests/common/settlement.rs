//! The canonical settlement transaction shape shared by the integration tests.
//!
//! Every settlement is a `[BeginSettle, FinalizeSettle]` pair with begin at
//! [`BEGIN_INDEX`] and finalize at [`FINALIZE_INDEX`], each instruction
//! referencing its counterpart by that position. [`settlement_tx`] assembles the
//! pair in that fixed order; callers submit the result with [`send`](super::send).

use litesvm::LiteSVM;
use settlement_interface::Instruction;
use solana_sdk::{
    signature::{Keypair, Signer},
    transaction::Transaction,
};

/// Position of the `BeginSettle` instruction in the transactions built by
/// [`settlement_tx`].
pub const BEGIN_INDEX: u8 = 0;
/// Position of the `FinalizeSettle` instruction in those transactions.
pub const FINALIZE_INDEX: u8 = 1;

/// Assemble the canonical two-instruction settlement transaction: `begin` at
/// [`BEGIN_INDEX`], `finalize` at [`FINALIZE_INDEX`], signed by `payer`. Callers
/// build the two instructions and submit the returned transaction with
/// [`send`](super::send).
pub fn settlement_tx(
    svm: &LiteSVM,
    payer: &Keypair,
    begin: impl Into<Instruction>,
    finalize: impl Into<Instruction>,
) -> Transaction {
    Transaction::new_signed_with_payer(
        &[begin.into(), finalize.into()],
        Some(&payer.pubkey()),
        &[payer],
        svm.latest_blockhash(),
    )
}
