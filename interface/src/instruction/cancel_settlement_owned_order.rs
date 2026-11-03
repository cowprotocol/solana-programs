//! `CancelSettlementOwnedOrder` instruction builder.

use solana_instruction::{AccountMeta, Instruction};
use solana_program_error::ProgramError;
use solana_pubkey::Pubkey;

use super::InstructionInputParsing;
use crate::SettlementInstruction;

/// Builder for a `CancelSettlementOwnedOrder` instruction.
///
/// Sets the `cancelled` flag on an existing order owned by the settlement state
/// PDA, so it can no longer be settled.
///
/// Similar to [`CancelOrder`](crate::instruction::cancel_order::CancelOrder),
/// but the instruction is gated by the
/// [`SettlementOwnedOrderAuthority`](crate::Role::SettlementOwnedOrderAuthority),
/// since the state PDA owning the order can't sign. The order must already
/// exist: a settlement-owned order can only be created on-chain through
/// [`CreateSettlementOwnedOrder`](crate::instruction::create_settlement_owned_order::CreateSettlementOwnedOrder),
/// so there's no intent to cancel ahead of its creation.
///
/// Cancelling is idempotent: cancelling an already-cancelled order does
/// nothing.
///
/// Wire format: `[discriminator=13]`, 1 byte. Required accounts:
/// `[authority (S), state_pda (R), order_pda (W)]`.
pub struct CancelSettlementOwnedOrder {
    pub program_id: Pubkey,
    pub authority: Pubkey,
    pub state_pda: Pubkey,
    pub order_pda: Pubkey,
}

impl From<CancelSettlementOwnedOrder> for Instruction {
    fn from(builder: CancelSettlementOwnedOrder) -> Self {
        Instruction {
            program_id: builder.program_id,
            accounts: vec![
                AccountMeta::new_readonly(builder.authority, true),
                AccountMeta::new_readonly(builder.state_pda, false),
                AccountMeta::new(builder.order_pda, false),
            ],
            data: vec![SettlementInstruction::CancelSettlementOwnedOrder.discriminator()],
        }
    }
}

/// Parsed inputs of a `CancelSettlementOwnedOrder` instruction.
pub struct CancelSettlementOwnedOrderInput<'a, A> {
    pub authority: &'a A,
    pub state_pda: &'a A,
    pub order_pda: &'a A,
}

impl<'a, A> InstructionInputParsing<'a, A> for CancelSettlementOwnedOrderInput<'a, A> {
    const DISCRIMINATOR: SettlementInstruction = SettlementInstruction::CancelSettlementOwnedOrder;

    fn parse_body(instruction_data: &'a [u8], accounts: &'a [A]) -> Result<Self, ProgramError> {
        // The body is empty once the discriminator has been stripped.
        if !instruction_data.is_empty() {
            return Err(ProgramError::InvalidInstructionData);
        }

        // Accounts: [authority (S), state_pda (R), order_pda (W)].
        let [authority, state_pda, order_pda, ..] = accounts else {
            return Err(ProgramError::NotEnoughAccountKeys);
        };

        Ok(Self {
            authority,
            state_pda,
            order_pda,
        })
    }
}

/// Test scaffolding for `CancelSettlementOwnedOrder` parsing and handling,
/// shared by this crate's tests and the settlement program's via the
/// `test-fixtures` feature.
#[cfg(any(test, feature = "test-fixtures"))]
pub mod fixtures {
    use solana_address::Address;

    use super::{CancelSettlementOwnedOrder, Instruction};

    /// Number of accounts `CancelSettlementOwnedOrder` expects: authority,
    /// state PDA, and order PDA.
    pub const NUM_ACCOUNTS: usize = 3;

    /// `CancelSettlementOwnedOrder` instruction data, with placeholder
    /// addresses since they don't end up in the data.
    pub fn cancel_settlement_owned_order_data() -> Vec<u8> {
        let zero = Address::new_from_array([0; 32]);
        Instruction::from(CancelSettlementOwnedOrder {
            program_id: zero,
            authority: zero,
            state_pda: zero,
            order_pda: zero,
        })
        .data
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::{cancel_settlement_owned_order_data, NUM_ACCOUNTS};
    use super::*;
    use crate::fixtures::pubkey_from_seed;
    use crate::instruction::fixtures::{fake_account, fake_sequential_accounts};
    use crate::instruction::tests::{
        assert_readonly_nonsigner, assert_readonly_signer, assert_writable_nonsigner,
    };
    use solana_account_view::AccountView;

    #[test]
    fn cancel_settlement_owned_order_input_parses_valid_input() {
        let program_id = pubkey_from_seed("program id");
        let authority = pubkey_from_seed("authority");
        let state_pda = pubkey_from_seed("state pda");
        let order_pda = pubkey_from_seed("order pda");

        let data = Instruction::from(CancelSettlementOwnedOrder {
            program_id,
            authority,
            state_pda,
            order_pda,
        })
        .data;
        let accounts = [
            fake_account(authority),
            fake_account(state_pda),
            fake_account(order_pda),
        ];

        let CancelSettlementOwnedOrderInput {
            authority: derived_authority,
            state_pda: derived_state_pda,
            order_pda: derived_order_pda,
        } = CancelSettlementOwnedOrderInput::parse(&data, &accounts).expect("parse should succeed");

        assert_eq!(*derived_authority.address(), authority);
        assert_eq!(*derived_state_pda.address(), state_pda);
        assert_eq!(*derived_order_pda.address(), order_pda);
    }

    #[test]
    fn cancel_settlement_owned_order_input_rejects_trailing_data() {
        let mut data = cancel_settlement_owned_order_data();
        data.push(0); // trailing byte
        let accounts = fake_sequential_accounts::<NUM_ACCOUNTS>();
        assert_eq!(
            CancelSettlementOwnedOrderInput::parse(&data, &accounts).err(),
            Some(ProgramError::InvalidInstructionData),
        );
    }

    #[test]
    fn cancel_settlement_owned_order_input_rejects_missing_accounts() {
        let data = cancel_settlement_owned_order_data();
        let mut accounts: Vec<AccountView> = fake_sequential_accounts::<NUM_ACCOUNTS>().into();
        accounts.pop();
        assert_eq!(
            CancelSettlementOwnedOrderInput::parse(&data, &accounts).err(),
            Some(ProgramError::NotEnoughAccountKeys),
        );
    }

    #[test]
    fn instruction_data_is_discriminator_only() {
        assert_eq!(
            cancel_settlement_owned_order_data(),
            vec![SettlementInstruction::CancelSettlementOwnedOrder.discriminator()],
        );
    }

    #[test]
    fn instruction_data_has_expected_accounts() {
        let program_id = pubkey_from_seed("program id");
        let authority = pubkey_from_seed("authority");
        let state_pda = pubkey_from_seed("state pda");
        let order_pda = pubkey_from_seed("order pda");

        let Instruction { accounts, .. } = CancelSettlementOwnedOrder {
            program_id,
            authority,
            state_pda,
            order_pda,
        }
        .into();

        assert_eq!(accounts.len(), 3);
        // authority gates the cancellation; state_pda is only read; order_pda
        // has its cancelled flag set.
        assert_readonly_signer(&accounts[0], authority);
        assert_readonly_nonsigner(&accounts[1], state_pda);
        assert_writable_nonsigner(&accounts[2], order_pda);
    }
}
