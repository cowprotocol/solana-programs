//! `CancelOrder` instruction builder.
//!
//! Cancels the order for a given intent by setting the `cancelled` flag on its
//! PDA (see [`crate::data::order::OrderAccount`]); the PDA is created already
//! cancelled if it doesn't exist yet.

use solana_instruction::{AccountMeta, Instruction};
use solana_program_error::ProgramError;
use solana_pubkey::Pubkey;

pub use solana_system_interface::program::ID as SYSTEM_PROGRAM_ID;

use super::InstructionInputParsing;
use crate::{data::intent::EncodedOrderIntent, SettlementInstruction};

/// Builder for a `CancelOrder` instruction.
///
/// `owner` signs the instruction and must match the intent owner; only the
/// owner can cancel their order.
///
/// `intent_bytes` is optional. When present, it's the encoded intent (the same
/// input as [`super::create_order::CreateOrder`]) and lets this instruction
/// create the order PDA already cancelled if it doesn't exist yet. When absent,
/// the order must already exist: its data is recovered from the `order_pda`
/// account and only its `cancelled` flag is set.
///
/// `created_by` funds the order PDA's rent on the create path, and must sign to
/// authorize that rent. When the order already exists no rent moves and
/// `created_by` is unused, so it doesn't need to sign (and doesn't need to be
/// meaningful when `intent_bytes` is absent).
///
/// Cancelling is idempotent: cancelling an already-cancelled order does
/// nothing.
///
/// The rent of a cancelled order can be recovered afterwards with
/// [`super::reclaim_order`], which reclaims a `created_on_chain` order early
/// once it is cancelled.
///
/// Wire format: `[discriminator=11]` to cancel an existing order, or
/// `[discriminator=11, ..intent bytes]` (1 + [`EncodedOrderIntent::SIZE`]
/// bytes) to also create it already cancelled when it's missing.
/// Required accounts:
/// `[owner (S), created_by (W,S), order_pda (W), system_program (R)]`.
/// When the PDA already exists, `created_by` neither signs nor is written and
/// the system program is unused, but the accounts must still occupy these
/// slots. The system program needs to be available but doesn't need to be at
/// that specific position in the instruction, unlike the others.
pub struct CancelOrder {
    pub program_id: Pubkey,
    pub owner: Pubkey,
    pub created_by: Pubkey,
    pub order_pda: Pubkey,
    pub intent_bytes: Option<[u8; EncodedOrderIntent::SIZE]>,
}

impl From<CancelOrder> for Instruction {
    fn from(builder: CancelOrder) -> Self {
        let mut data = Vec::with_capacity(1 + EncodedOrderIntent::SIZE);
        data.push(SettlementInstruction::CancelOrder.discriminator());

        // `created_by` only funds rent on the create path, so it signs (and is
        // written) only when the intent is supplied; recovering an existing
        // order touches neither the funder nor the system program.
        let created_by = match builder.intent_bytes {
            Some(intent_bytes) => {
                data.extend_from_slice(&intent_bytes);
                AccountMeta::new(builder.created_by, true)
            }
            None => AccountMeta::new_readonly(builder.created_by, false),
        };

        Instruction {
            program_id: builder.program_id,
            accounts: vec![
                AccountMeta::new_readonly(builder.owner, true),
                created_by,
                AccountMeta::new(builder.order_pda, false),
                AccountMeta::new_readonly(SYSTEM_PROGRAM_ID, false),
            ],
            data,
        }
    }
}

/// Parsed inputs of a `CancelOrder` instruction. Mirrors
/// [`super::create_order::CreateOrderInput`], except the intent is optional:
/// it's absent when the caller cancels an order that already exists on-chain.
pub struct CancelOrderInput<'a, A> {
    pub intent_bytes: Option<[u8; EncodedOrderIntent::SIZE]>,
    pub owner: &'a A,
    pub created_by: &'a A,
    pub order_pda: &'a A,
}

impl<'a, A> InstructionInputParsing<'a, A> for CancelOrderInput<'a, A> {
    const DISCRIMINATOR: SettlementInstruction = SettlementInstruction::CancelOrder;

    fn parse_body(instruction_data: &'a [u8], accounts: &'a [A]) -> Result<Self, ProgramError> {
        let intent_bytes = if instruction_data.is_empty() {
            None
        } else {
            Some(
                instruction_data
                    .try_into()
                    .map_err(|_| ProgramError::InvalidInstructionData)?,
            )
        };

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
    use crate::data::intent::EncodedOrderIntent;

    // `CancelOrder` takes the same intent input as `CreateOrder`, so reuse its
    // sample payload rather than duplicating it.
    pub use crate::instruction::create_order::fixtures::valid_intent_bytes;

    /// Number of accounts `CancelOrder` expects: owner, created_by, order PDA,
    /// and the system program.
    pub const NUM_ACCOUNTS: usize = 4;

    /// `CancelOrder` instruction data with placeholder addresses, for failure
    /// cases where the actual addresses don't matter. `intent_bytes` is `Some`
    /// for the create-cancelled form and `None` for the recover-from-PDA form.
    pub fn default_cancel_data(intent_bytes: Option<[u8; EncodedOrderIntent::SIZE]>) -> Vec<u8> {
        let zero = Address::new_from_array([0; 32]);
        Instruction::from(CancelOrder {
            program_id: zero,
            owner: zero,
            created_by: zero,
            order_pda: zero,
            intent_bytes,
        })
        .data
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::{default_cancel_data, valid_intent_bytes, NUM_ACCOUNTS};
    use super::*;
    use crate::fixtures::pubkey_from_seed;
    use crate::instruction::fixtures::{fake_account, fake_sequential_accounts};
    use crate::instruction::tests::{
        assert_readonly_nonsigner, assert_readonly_signer, assert_writable_nonsigner,
        assert_writable_signer,
    };
    use solana_account_view::AccountView;

    #[test]
    fn cancel_order_input_parses_valid_input() {
        let program_id = pubkey_from_seed("program id");
        let owner = pubkey_from_seed("owner");
        let created_by = pubkey_from_seed("created by");
        let order_pda = pubkey_from_seed("order pda");
        let intent_bytes = valid_intent_bytes();

        let data = Instruction::from(CancelOrder {
            program_id,
            owner,
            created_by,
            order_pda,
            intent_bytes: Some(intent_bytes),
        })
        .data;
        let accounts = [
            fake_account(owner),
            fake_account(created_by),
            fake_account(order_pda),
            fake_account(pubkey_from_seed("system program")),
        ];

        let CancelOrderInput {
            intent_bytes: derived_intent_bytes,
            owner: derived_owner,
            created_by: derived_created_by,
            order_pda: derived_order_pda,
        } = CancelOrderInput::parse(&data, &accounts).expect("parse should succeed");

        assert_eq!(derived_intent_bytes, Some(intent_bytes));
        assert_eq!(*derived_order_pda.address(), order_pda);
        assert_eq!(*derived_owner.address(), owner);
        assert_eq!(*derived_created_by.address(), created_by);
    }

    #[test]
    fn cancel_order_input_parses_without_intent() {
        let owner = pubkey_from_seed("owner");
        let created_by = pubkey_from_seed("created by");
        let order_pda = pubkey_from_seed("order pda");

        let data = default_cancel_data(None);
        let accounts = [
            fake_account(owner),
            fake_account(created_by),
            fake_account(order_pda),
            fake_account(pubkey_from_seed("system program")),
        ];

        let CancelOrderInput {
            intent_bytes: derived_intent_bytes,
            owner: derived_owner,
            order_pda: derived_order_pda,
            ..
        } = CancelOrderInput::parse(&data, &accounts).expect("parse should succeed");

        assert_eq!(
            derived_intent_bytes, None,
            "an empty body carries no intent"
        );
        assert_eq!(*derived_order_pda.address(), order_pda);
        assert_eq!(*derived_owner.address(), owner);
    }

    #[test]
    fn cancel_order_input_rejects_short_data() {
        let intent_bytes = valid_intent_bytes();
        let mut data = default_cancel_data(Some(intent_bytes));
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
        let mut data = default_cancel_data(Some(intent_bytes));
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
        let data = default_cancel_data(Some(intent_bytes));
        let mut accounts: Vec<AccountView> = fake_sequential_accounts::<NUM_ACCOUNTS>().into();
        accounts.pop();
        assert_eq!(
            CancelOrderInput::parse(&data, &accounts).err(),
            Some(ProgramError::NotEnoughAccountKeys),
        );
    }

    #[test]
    fn instruction_data_has_expected_layout() {
        let program_id = pubkey_from_seed("program id");
        let owner = pubkey_from_seed("owner");
        let created_by = pubkey_from_seed("created by");
        let order_pda = pubkey_from_seed("order pda");
        let intent_bytes = [0x42u8; EncodedOrderIntent::SIZE];

        let Instruction { data, .. } = CancelOrder {
            program_id,
            owner,
            created_by,
            order_pda,
            intent_bytes: Some(intent_bytes),
        }
        .into();

        assert_eq!(data.len(), 1 + EncodedOrderIntent::SIZE);
        assert_eq!(data[0], SettlementInstruction::CancelOrder.discriminator());
        assert_eq!(&data[1..], &intent_bytes);
    }

    #[test]
    fn instruction_data_without_intent_is_discriminator_only() {
        let Instruction { data, .. } = CancelOrder {
            program_id: pubkey_from_seed("program id"),
            owner: pubkey_from_seed("owner"),
            created_by: pubkey_from_seed("created by"),
            order_pda: pubkey_from_seed("order pda"),
            intent_bytes: None,
        }
        .into();

        assert_eq!(
            data,
            vec![SettlementInstruction::CancelOrder.discriminator()],
            "an intent-less cancellation carries only the discriminator"
        );
    }

    #[test]
    fn instruction_accounts_without_intent_leave_created_by_unsigned() {
        let program_id = pubkey_from_seed("program id");
        let owner = pubkey_from_seed("owner");
        let created_by = pubkey_from_seed("created by");
        let order_pda = pubkey_from_seed("order pda");

        let Instruction { accounts, .. } = CancelOrder {
            program_id,
            owner,
            created_by,
            order_pda,
            intent_bytes: None,
        }
        .into();

        // The account layout is unchanged, but with no rent to fund `created_by`
        // neither signs nor is written.
        assert_eq!(accounts.len(), 4);
        assert_readonly_signer(&accounts[0], owner);
        assert_readonly_nonsigner(&accounts[1], created_by);
        assert_writable_nonsigner(&accounts[2], order_pda);
        assert_readonly_nonsigner(&accounts[3], SYSTEM_PROGRAM_ID);
    }

    #[test]
    fn instruction_data_has_expected_accounts() {
        let program_id = pubkey_from_seed("program id");
        let owner = pubkey_from_seed("owner");
        let created_by = pubkey_from_seed("created by");
        let order_pda = pubkey_from_seed("order pda");
        let intent_bytes = [0u8; EncodedOrderIntent::SIZE];

        let Instruction { accounts, .. } = CancelOrder {
            program_id,
            owner,
            created_by,
            order_pda,
            intent_bytes: Some(intent_bytes),
        }
        .into();

        assert_eq!(accounts.len(), 4);
        // owner authenticates the cancellation without paying rent; created_by
        // funds the PDA's rent when the order is created cancelled; order_pda
        // is the order to cancel. The system program is only referenced.
        assert_readonly_signer(&accounts[0], owner);
        assert_writable_signer(&accounts[1], created_by);
        assert_writable_nonsigner(&accounts[2], order_pda);
        assert_readonly_nonsigner(&accounts[3], SYSTEM_PROGRAM_ID);
    }
}
