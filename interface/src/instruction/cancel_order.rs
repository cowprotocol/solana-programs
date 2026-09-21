//! `CancelOrder` instruction builder.
//!
//! Cancels the order for a given intent by setting the `cancelled` flag on its
//! PDA (see [`crate::data::order::EncodedOrderAccount`]); the PDA is created
//! already cancelled if it doesn't exist yet.

use solana_instruction::{AccountMeta, Instruction};
use solana_program_error::ProgramError;
use solana_pubkey::Pubkey;

pub use solana_system_interface::program::ID as SYSTEM_PROGRAM_ID;

use super::InstructionInputParsing;
use crate::{data::intent::EncodedOrderIntent, SettlementInstruction};

/// Builder for a `CancelOrder` instruction.
///
/// Takes the same inputs as [`super::create_order::CreateOrder`]: the encoded
/// `intent_bytes` and the canonical `order_pda` for its UID (see
/// [`crate::pda::order::find_order_pda`]).
///
/// `owner` signs the instruction and must match the intent owner; only the
/// owner can cancel their order. When the order already exists, that is what
/// authenticates the cancellation and the passed intent is otherwise unused.
///
/// `created_by` funds the order PDA's rent when the order doesn't exist yet and
/// this instruction creates it already cancelled. When the order already
/// exists, no rent moves and `created_by` is unused, though it must still be
/// supplied (and signs) to keep the account layout identical to `CreateOrder`.
/// `created_by` and `owner` may be the same account.
///
/// Cancelling is idempotent: cancelling an already-cancelled order does
/// nothing. The rent of a cancelled order can be recovered afterwards with
/// [`super::reclaim_order`], which reclaims a `created_on_chain` order early
/// once it is cancelled.
///
/// Wire format: `[discriminator=11, ..149 intent bytes]`, 150 bytes.
/// Required accounts:
/// `[owner (S), created_by (W,S), order_pda (W), system_program (R)]`.
/// The system program needs to be available but doesn't need to be at that
/// specific position in the instruction, unlike the others.
pub struct CancelOrder {
    pub program_id: Pubkey,
    pub owner: Pubkey,
    pub created_by: Pubkey,
    pub order_pda: Pubkey,
    pub intent_bytes: [u8; EncodedOrderIntent::SIZE],
}

impl From<CancelOrder> for Instruction {
    fn from(builder: CancelOrder) -> Self {
        let mut data = Vec::with_capacity(1 + EncodedOrderIntent::SIZE);
        data.push(SettlementInstruction::CancelOrder.discriminator());
        data.extend_from_slice(&builder.intent_bytes);

        Instruction {
            program_id: builder.program_id,
            accounts: vec![
                AccountMeta::new_readonly(builder.owner, true),
                AccountMeta::new(builder.created_by, true),
                AccountMeta::new(builder.order_pda, false),
                AccountMeta::new_readonly(SYSTEM_PROGRAM_ID, false),
            ],
            data,
        }
    }
}

/// Parsed inputs of a `CancelOrder` instruction. Same layout as
/// [`super::create_order::CreateOrderInput`].
pub struct CancelOrderInput<'a, A> {
    pub intent_bytes: [u8; EncodedOrderIntent::SIZE],
    pub owner: &'a A,
    pub created_by: &'a A,
    pub order_pda: &'a A,
}

impl<'a, A> InstructionInputParsing<'a, A> for CancelOrderInput<'a, A> {
    const DISCRIMINATOR: SettlementInstruction = SettlementInstruction::CancelOrder;

    fn parse_body(instruction_data: &'a [u8], accounts: &'a [A]) -> Result<Self, ProgramError> {
        // Body (discriminator already stripped): exactly the intent bytes.
        let intent_bytes: [u8; EncodedOrderIntent::SIZE] = instruction_data
            .try_into()
            .map_err(|_| ProgramError::InvalidInstructionData)?;

        // Accounts: [owner (S), created_by (W,S), order_pda (W), some other
        // account]. We check that there are four accounts because the
        // instruction needs to specify `SYSTEM_PROGRAM_ID` as one of the
        // signers. It doesn't have to be the fourth though.
        let [owner, created_by, order_pda, _, ..] = accounts else {
            return Err(ProgramError::NotEnoughAccountKeys);
        };

        Ok(Self {
            intent_bytes,
            owner,
            created_by,
            order_pda,
        })
    }
}

/// Test scaffolding for `CancelOrder` parsing and handling, shared by this
/// crate's tests and the settlement program's via the `test-fixtures` feature.
#[cfg(any(test, feature = "test-fixtures"))]
pub mod fixtures {
    use solana_address::Address;

    use super::{CancelOrder, Instruction};
    use crate::data::intent::{fixtures::sample_intent, EncodedOrderIntent, OrderIntent};

    /// Owner baked into [`valid_intent_bytes`]' sample intent.
    pub const DEFAULT_OWNER: Address = Address::new_from_array([0x11; 32]);

    /// Number of accounts `CancelOrder` expects: owner, created_by, order PDA,
    /// and the system program.
    pub const NUM_ACCOUNTS: usize = 4;

    /// Canonical 149-byte intent payload for a valid sell order owned by
    /// [`DEFAULT_OWNER`].
    pub fn valid_intent_bytes() -> [u8; EncodedOrderIntent::SIZE] {
        (&EncodedOrderIntent::from(&OrderIntent {
            owner: DEFAULT_OWNER,
            ..sample_intent(Default::default())
        }))
            .into()
    }

    /// `CancelOrder` instruction data carrying `intent_bytes`, with placeholder
    /// addresses for failure cases where the actual addresses don't matter.
    pub fn default_cancel_data(intent_bytes: &[u8; EncodedOrderIntent::SIZE]) -> Vec<u8> {
        let zero = Address::new_from_array([0; 32]);
        Instruction::from(CancelOrder {
            program_id: zero,
            owner: zero,
            created_by: zero,
            order_pda: zero,
            intent_bytes: *intent_bytes,
        })
        .data
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::{default_cancel_data, valid_intent_bytes, NUM_ACCOUNTS};
    use super::*;
    use crate::instruction::fixtures::{
        fake_account, fake_account_from_array, fake_sequential_accounts,
    };
    use crate::instruction::tests::{
        assert_readonly_nonsigner, assert_readonly_signer, assert_writable_nonsigner,
        assert_writable_signer,
    };
    use solana_account_view::AccountView;
    use solana_address::Address;

    #[test]
    fn cancel_order_input_parses_valid_input() {
        let program_id = Address::new_from_array([21; 32]);
        let owner = Address::new_from_array([22; 32]);
        let created_by = Address::new_from_array([24; 32]);
        let order_pda = Address::new_from_array([23; 32]);
        let intent_bytes = valid_intent_bytes();

        let data = Instruction::from(CancelOrder {
            program_id,
            owner,
            created_by,
            order_pda,
            intent_bytes,
        })
        .data;
        let accounts = [
            fake_account(owner),
            fake_account(created_by),
            fake_account(order_pda),
            fake_account_from_array([4; 32]),
        ];

        let CancelOrderInput {
            intent_bytes: derived_intent_bytes,
            owner: derived_owner,
            created_by: derived_created_by,
            order_pda: derived_order_pda,
        } = CancelOrderInput::parse(&data, &accounts).expect("parse should succeed");

        assert_eq!(derived_intent_bytes, intent_bytes);
        assert_eq!(*derived_order_pda.address(), order_pda);
        assert_eq!(*derived_owner.address(), owner);
        assert_eq!(*derived_created_by.address(), created_by);
    }

    #[test]
    fn cancel_order_input_rejects_short_data() {
        let intent_bytes = valid_intent_bytes();
        let mut data = default_cancel_data(&intent_bytes);
        data.pop();
        let accounts = fake_sequential_accounts::<NUM_ACCOUNTS>();
        assert_eq!(
            CancelOrderInput::parse(&data, &accounts).err(),
            Some(ProgramError::InvalidInstructionData),
        );
    }

    #[test]
    fn cancel_order_input_rejects_long_data() {
        let intent_bytes = valid_intent_bytes();
        let mut data = default_cancel_data(&intent_bytes);
        data.push(0); // trailing byte
        let accounts = fake_sequential_accounts::<NUM_ACCOUNTS>();
        assert_eq!(
            CancelOrderInput::parse(&data, &accounts).err(),
            Some(ProgramError::InvalidInstructionData),
        );
    }

    #[test]
    fn cancel_order_input_rejects_missing_accounts() {
        let intent_bytes = valid_intent_bytes();
        let data = default_cancel_data(&intent_bytes);
        let mut accounts: Vec<AccountView> = fake_sequential_accounts::<NUM_ACCOUNTS>().into();
        accounts.pop();
        assert_eq!(
            CancelOrderInput::parse(&data, &accounts).err(),
            Some(ProgramError::NotEnoughAccountKeys),
        );
    }

    #[test]
    fn instruction_data_has_expected_layout() {
        let program_id = Pubkey::new_from_array([1; 32]);
        let owner = Pubkey::new_from_array([2; 32]);
        let created_by = Pubkey::new_from_array([4; 32]);
        let order_pda = Pubkey::new_from_array([3; 32]);
        let intent_bytes = [0x42u8; EncodedOrderIntent::SIZE];

        let Instruction { data, .. } = CancelOrder {
            program_id,
            owner,
            created_by,
            order_pda,
            intent_bytes,
        }
        .into();

        assert_eq!(data.len(), 1 + EncodedOrderIntent::SIZE);
        assert_eq!(data[0], SettlementInstruction::CancelOrder.discriminator());
        assert_eq!(&data[1..], &intent_bytes);
    }

    #[test]
    fn instruction_data_has_expected_accounts() {
        let program_id = Pubkey::new_from_array([1; 32]);
        let owner = Pubkey::new_from_array([2; 32]);
        let created_by = Pubkey::new_from_array([4; 32]);
        let order_pda = Pubkey::new_from_array([3; 32]);
        let intent_bytes = [0u8; EncodedOrderIntent::SIZE];

        let Instruction { accounts, .. } = CancelOrder {
            program_id,
            owner,
            created_by,
            order_pda,
            intent_bytes,
        }
        .into();

        assert_eq!(accounts.len(), 4);
        // owner authenticates the cancellation without paying rent; created_by
        // funds the PDA's rent when the order is created cancelled; order_pda is
        // the order to cancel. The system program is only referenced.
        assert_readonly_signer(&accounts[0], owner);
        assert_writable_signer(&accounts[1], created_by);
        assert_writable_nonsigner(&accounts[2], order_pda);
        assert_readonly_nonsigner(&accounts[3], SYSTEM_PROGRAM_ID);
    }
}
