//! `ReclaimOrder` instruction builder.
//!
//! Closes an expired order PDA and returns its rent lamports to the
//! `created_by` account recorded in the order body. The instruction may only be
//! executed after the order's `valid_to` timestamp has elapsed.
//!
//! Wire format: `[discriminator=5]`, 1 byte.
//! Required accounts:
//! `[order_pda (W), reclaim_recipient (W)]`.

use solana_account_view::AccountView;
use solana_instruction::{AccountMeta, Instruction};
use solana_program_error::ProgramError;
use solana_pubkey::Pubkey;

use super::InstructionInputParsing;
use crate::SettlementInstruction;

/// Builder for a `ReclaimOrder` instruction.
///
/// `order_pda` is the canonical order PDA to close. `reclaim_recipient` must
/// be the account recorded as `created_by` in the order PDA; it receives the
/// recovered rent lamports. The instruction enforces no signature requirement:
/// anyone may reclaim an expired order on behalf of its reclaim_recipient.
pub struct ReclaimOrder {
    pub program_id: Pubkey,
    pub order_pda: Pubkey,
    pub reclaim_recipient: Pubkey,
}

impl ReclaimOrder {
    pub fn instruction(self) -> Instruction {
        Instruction {
            program_id: self.program_id,
            accounts: vec![
                AccountMeta::new(self.order_pda, false),
                AccountMeta::new(self.reclaim_recipient, false),
            ],
            data: vec![SettlementInstruction::ReclaimOrder.discriminator()],
        }
    }
}

/// Parsed inputs of a `ReclaimOrder` instruction.
pub struct ReclaimOrderInput<'a> {
    pub order_pda: &'a mut AccountView,
    pub reclaim_recipient: &'a mut AccountView,
}

impl<'a> InstructionInputParsing<'a> for ReclaimOrderInput<'a> {
    const DISCRIMINATOR: SettlementInstruction = SettlementInstruction::ReclaimOrder;

    fn parse_body(
        instruction_data: &'a [u8],
        accounts: &'a mut [AccountView],
    ) -> Result<Self, ProgramError> {
        // No instruction body: only the discriminator, already stripped.
        if !instruction_data.is_empty() {
            return Err(ProgramError::InvalidInstructionData);
        }
        let [order_pda, reclaim_recipient, ..] = accounts else {
            return Err(ProgramError::NotEnoughAccountKeys);
        };
        Ok(Self {
            order_pda,
            reclaim_recipient,
        })
    }
}

/// Test scaffolding for `ReclaimOrder` parsing, shared by this crate's tests
/// and the settlement program's via the `test-fixtures` feature.
#[cfg(any(test, feature = "test-fixtures"))]
pub mod fixtures {
    use solana_address::Address;

    use super::ReclaimOrder;

    /// Number of accounts `ReclaimOrder` expects: order PDA and reclaim
    /// recipient.
    pub const NUM_ACCOUNTS: usize = 2;

    /// `ReclaimOrder` instruction data with placeholder addresses.
    pub fn default_reclaim_data() -> Vec<u8> {
        let zero = Address::new_from_array([0; 32]);
        ReclaimOrder {
            program_id: zero,
            order_pda: zero,
            reclaim_recipient: zero,
        }
        .instruction()
        .data
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::{default_reclaim_data, NUM_ACCOUNTS};
    use super::*;
    use crate::instruction::fixtures::{fake_account, fake_sequential_accounts};
    use solana_address::Address;

    #[test]
    fn reclaim_order_input_parses_valid_input() {
        let program_id = Address::new_from_array([1; 32]);
        let order_pda = Address::new_from_array([2; 32]);
        let reclaim_recipient = Address::new_from_array([3; 32]);

        let data = ReclaimOrder {
            program_id,
            order_pda,
            reclaim_recipient,
        }
        .instruction()
        .data;
        let mut accounts = [fake_account(order_pda), fake_account(reclaim_recipient)];

        let ReclaimOrderInput {
            order_pda: derived_order_pda,
            reclaim_recipient: derived_reclaim_recipient,
        } = ReclaimOrderInput::parse(&data, &mut accounts).expect("parse should succeed");

        assert_eq!(*derived_order_pda.address(), order_pda);
        assert_eq!(*derived_reclaim_recipient.address(), reclaim_recipient);
    }

    #[test]
    fn reclaim_order_input_rejects_nonempty_body() {
        let mut data = default_reclaim_data();
        data.push(0);
        let mut accounts = fake_sequential_accounts::<NUM_ACCOUNTS>();
        assert_eq!(
            ReclaimOrderInput::parse(&data, &mut accounts).err(),
            Some(ProgramError::InvalidInstructionData),
        );
    }

    #[test]
    fn reclaim_order_input_rejects_missing_accounts() {
        let data = default_reclaim_data();
        let mut accounts: Vec<AccountView> = fake_sequential_accounts::<NUM_ACCOUNTS>().into();
        accounts.pop();
        assert_eq!(
            ReclaimOrderInput::parse(&data, &mut accounts).err(),
            Some(ProgramError::NotEnoughAccountKeys),
        );
    }

    #[test]
    fn instruction_data_has_expected_layout() {
        let program_id = Address::new_from_array([1; 32]);
        let order_pda = Address::new_from_array([2; 32]);
        let reclaim_recipient = Address::new_from_array([3; 32]);

        let ix = ReclaimOrder {
            program_id,
            order_pda,
            reclaim_recipient,
        }
        .instruction();

        assert_eq!(
            ix.data,
            vec![SettlementInstruction::ReclaimOrder.discriminator()]
        );
    }

    #[test]
    fn instruction_data_has_expected_accounts() {
        let program_id = Address::new_from_array([1; 32]);
        let order_pda = Address::new_from_array([2; 32]);
        let reclaim_recipient = Address::new_from_array([3; 32]);

        let ix = ReclaimOrder {
            program_id,
            order_pda,
            reclaim_recipient,
        }
        .instruction();

        assert_eq!(ix.accounts.len(), 2);
        // order_pda: writable, not signer (the PDA being closed)
        assert_eq!(ix.accounts[0].pubkey, order_pda);
        assert!(ix.accounts[0].is_writable);
        assert!(!ix.accounts[0].is_signer);
        // reclaim_recipient: writable, not signer (receives the recovered rent)
        assert_eq!(ix.accounts[1].pubkey, reclaim_recipient);
        assert!(ix.accounts[1].is_writable);
        assert!(!ix.accounts[1].is_signer);
    }
}
