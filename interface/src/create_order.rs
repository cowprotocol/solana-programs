//! `CreateOrder` instruction builder.
//!
//! Allocates a per-order PDA (see [`crate::pda::order`]) and writes the
//! initial body bytes; the PDA's storage layout lives in
//! [`crate::data::order::EncodedOrderAccount`].

use solana_instruction::{AccountMeta, Instruction};
use solana_pubkey::Pubkey;

pub use solana_system_interface::program::ID as SYSTEM_PROGRAM_ID;

use crate::{data::intent::EncodedOrderIntent, SettlementInstruction};

/// Build a `CreateOrder` instruction for `created_by`/`order_pda`.
///
/// `intent_bytes` is the canonical byte encoding (see
/// [`EncodedOrderIntent`]). `order_pda` must be the canonical PDA returned
/// by [`crate::pda::order::find_order_pda`] for the same UID; the program
/// derives the bump itself and rejects any other address.
///
/// `created_by` funds the new order PDA's rent and is recorded as the
/// `created_by` address in its body. It may be a normal user account or a
/// PDA, the program does not check `is_on_curve`. A parent program that
/// wants to create orders on behalf of its own PDA can `invoke_signed`
/// into the settlement program using this instruction directly.
///
/// Wire format: `[discriminator=2, ..150 intent bytes]`, 151 bytes.
/// Required accounts: `[created_by (W,S), order_pda (W), system_program (R)]`.
/// The third account needs to be available but doesn't need to be at that
/// specific position in the instruction, unlike the other two.
pub fn create_order(
    program_id: &Pubkey,
    created_by: &Pubkey,
    order_pda: &Pubkey,
    intent_bytes: &[u8; EncodedOrderIntent::SIZE],
) -> Instruction {
    let mut data = Vec::with_capacity(1 + EncodedOrderIntent::SIZE);
    data.push(SettlementInstruction::CreateOrder.discriminator());
    data.extend_from_slice(intent_bytes);

    Instruction {
        program_id: *program_id,
        accounts: vec![
            AccountMeta::new(*created_by, true),
            AccountMeta::new(*order_pda, false),
            AccountMeta::new_readonly(SYSTEM_PROGRAM_ID, false),
        ],
        data,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instruction_data_has_expected_layout() {
        let program_id = Pubkey::new_from_array([1; 32]);
        let payer = Pubkey::new_from_array([2; 32]);
        let order_pda = Pubkey::new_from_array([3; 32]);
        let intent_bytes = [0x42u8; EncodedOrderIntent::SIZE];

        let ix = create_order(&program_id, &payer, &order_pda, &intent_bytes);

        assert_eq!(ix.data.len(), 1 + EncodedOrderIntent::SIZE);
        assert_eq!(
            ix.data[0],
            SettlementInstruction::CreateOrder.discriminator()
        );
        assert_eq!(&ix.data[1..], &intent_bytes);
    }

    #[test]
    fn instruction_data_has_expected_accounts() {
        let program_id = Pubkey::new_from_array([1; 32]);
        let payer = Pubkey::new_from_array([2; 32]);
        let order_pda = Pubkey::new_from_array([3; 32]);
        let intent_bytes = [0u8; EncodedOrderIntent::SIZE];

        let ix = create_order(&program_id, &payer, &order_pda, &intent_bytes);

        assert_eq!(ix.accounts.len(), 3);
        // payer: writable, signer
        assert_eq!(ix.accounts[0].pubkey, payer);
        assert!(ix.accounts[0].is_writable);
        assert!(ix.accounts[0].is_signer);
        // order_pda: writable, not signer (the program signs via PDA seeds)
        assert_eq!(ix.accounts[1].pubkey, order_pda);
        assert!(ix.accounts[1].is_writable);
        assert!(!ix.accounts[1].is_signer);
        // system program: read-only, not signer; the on-chain handler
        // doesn't dereference it but the runtime requires it in the
        // transaction's `account_keys` to dispatch the CreateAccount CPI.
        assert_eq!(ix.accounts[2].pubkey, SYSTEM_PROGRAM_ID);
        assert!(!ix.accounts[2].is_writable);
        assert!(!ix.accounts[2].is_signer);
    }
}
