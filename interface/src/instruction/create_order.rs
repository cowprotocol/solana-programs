//! `CreateOrder` instruction builder.
//!
//! Allocates a per-order PDA (see [`crate::pda::order`]) and writes the
//! initial body bytes; the PDA's storage layout lives in
//! [`crate::data::order::EncodedOrderAccount`].

#[cfg(test)]
use solana_account_view::AccountView;
use solana_instruction::{AccountMeta, Instruction};
use solana_program_error::ProgramError;
use solana_pubkey::Pubkey;

pub use solana_system_interface::program::ID as SYSTEM_PROGRAM_ID;

use super::InstructionInputParsing;
use crate::{data::intent::EncodedOrderIntent, SettlementInstruction};

/// Builder for a `CreateOrder` instruction.
///
/// `intent_bytes` is the canonical byte encoding (see
/// [`EncodedOrderIntent`]). `order_pda` must be the canonical PDA returned
/// by [`crate::pda::order::find_order_pda`] for the same UID; the program
/// derives the bump itself and rejects any other address.
///
/// `owner` signs the instruction and must match the intent owner; this is
/// what authenticates the order. It may be a normal user account or a PDA,
/// the program does not check `is_on_curve`. A parent program that wants to
/// create orders on behalf of its own PDA can `invoke_signed` into the
/// settlement program using this instruction directly.
///
/// `created_by` funds the new order PDA's rent and is recorded as the
/// `created_by` address in its body. Keeping it separate from `owner` lets
/// the rent cost be delegated to another account, which is handy when
/// `owner` is a PDA. `created_by` and `owner` may be the same account.
///
/// The order doesn't need to be executable for it to be created. For
/// example, the sell token account doesn't need to belong to the user or
/// be a TOKEN account in the first place. This is checked at execution
/// time.
///
/// Wire format: `[discriminator=2, ..150 intent bytes]`, 151 bytes.
/// Required accounts:
/// `[owner (S), created_by (W,S), order_pda (W), system_program (R)]`.
/// The system program needs to be available but doesn't need to be at that
/// specific position in the instruction, unlike the others.
pub struct CreateOrder {
    pub program_id: Pubkey,
    pub owner: Pubkey,
    pub created_by: Pubkey,
    pub order_pda: Pubkey,
    pub intent_bytes: [u8; EncodedOrderIntent::SIZE],
}

impl From<CreateOrder> for Instruction {
    fn from(builder: CreateOrder) -> Self {
        let mut data = Vec::with_capacity(1 + EncodedOrderIntent::SIZE);
        data.push(SettlementInstruction::CreateOrder.discriminator());
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

/// Parsed inputs of a `CreateOrder` instruction.
pub struct CreateOrderInput<'a, A> {
    pub intent_bytes: [u8; EncodedOrderIntent::SIZE],
    pub owner: &'a A,
    pub created_by: &'a A,
    pub order_pda: &'a mut A,
}

impl<'a, A> InstructionInputParsing<'a, A> for CreateOrderInput<'a, A> {
    const DISCRIMINATOR: SettlementInstruction = SettlementInstruction::CreateOrder;

    fn parse_body(instruction_data: &'a [u8], accounts: &'a mut [A]) -> Result<Self, ProgramError> {
        // Body (discriminator already stripped): exactly the 150 intent bytes.
        if instruction_data.len() != EncodedOrderIntent::SIZE {
            return Err(ProgramError::InvalidInstructionData);
        }
        // Accounts: [owner (S), created_by (W,S), order_pda (W), some other
        // account]. We check that there are four accounts because the
        // instruction needs to specify `SYSTEM_PROGRAM_ID` as one of the
        // signers. It doesn't have to be the fourth though.
        let [owner, created_by, order_pda, _, ..] = accounts else {
            return Err(ProgramError::NotEnoughAccountKeys);
        };

        let intent_bytes: [u8; EncodedOrderIntent::SIZE] =
            instruction_data.try_into().expect("length checked above");

        Ok(Self {
            intent_bytes,
            owner,
            created_by,
            order_pda,
        })
    }
}

/// Test scaffolding for `CreateOrder` parsing and handling, shared by this
/// crate's tests and the settlement program's via the `test-fixtures` feature.
#[cfg(any(test, feature = "test-fixtures"))]
pub mod fixtures {
    use solana_address::Address;

    use super::{CreateOrder, Instruction};
    use crate::data::intent::{
        fixtures::sample_intent, EncodedOrderIntent, OrderIntent, OrderKind,
    };

    /// Owner baked into [`valid_intent_bytes`]' sample intent.
    pub const DEFAULT_OWNER: Address = Address::new_from_array([0x11; 32]);

    /// Number of accounts `CreateOrder` expects: owner, created_by, order PDA,
    /// and the system program.
    pub const NUM_ACCOUNTS: usize = 4;

    /// Canonical 150-byte intent payload for a valid sell order owned by
    /// [`DEFAULT_OWNER`].
    pub fn valid_intent_bytes() -> [u8; EncodedOrderIntent::SIZE] {
        (&EncodedOrderIntent::from(&OrderIntent {
            owner: DEFAULT_OWNER,
            ..sample_intent(OrderKind::Sell, true)
        }))
            .into()
    }

    /// `CreateOrder` instruction data carrying `intent_bytes`, with placeholder
    /// addresses for failure cases where the actual addresses don't matter.
    pub fn default_order_data(intent_bytes: &[u8; EncodedOrderIntent::SIZE]) -> Vec<u8> {
        let zero = Address::new_from_array([0; 32]);
        Instruction::from(CreateOrder {
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
    use super::fixtures::{default_order_data, valid_intent_bytes, NUM_ACCOUNTS};
    use super::*;
    use crate::instruction::fixtures::{
        fake_account, fake_account_from_array, fake_sequential_accounts,
    };
    use solana_address::Address;

    #[test]
    fn create_order_input_parses_valid_input() {
        let program_id = Address::new_from_array([21; 32]);
        let owner = Address::new_from_array([22; 32]);
        let created_by = Address::new_from_array([24; 32]);
        let order_pda = Address::new_from_array([23; 32]);
        let intent_bytes = valid_intent_bytes();

        let data = Instruction::from(CreateOrder {
            program_id,
            owner,
            created_by,
            order_pda,
            intent_bytes,
        })
        .data;
        let mut accounts = [
            fake_account(owner),
            fake_account(created_by),
            fake_account(order_pda),
            fake_account_from_array([4; 32]),
        ];

        let CreateOrderInput {
            intent_bytes: derived_intent_bytes,
            owner: derived_owner,
            created_by: derived_created_by,
            order_pda: derived_order_pda,
        } = CreateOrderInput::parse(&data, &mut accounts).expect("parse should succeed");

        assert_eq!(derived_intent_bytes, intent_bytes);
        assert_eq!(*derived_order_pda.address(), order_pda);
        assert_eq!(*derived_owner.address(), owner);
        assert_eq!(*derived_created_by.address(), created_by);
    }

    #[test]
    fn create_order_input_rejects_short_data() {
        let intent_bytes = valid_intent_bytes();
        let mut data = default_order_data(&intent_bytes);
        data.pop();
        let mut accounts = fake_sequential_accounts::<NUM_ACCOUNTS>();
        assert_eq!(
            CreateOrderInput::parse(&data, &mut accounts).err(),
            Some(ProgramError::InvalidInstructionData),
        );
    }

    #[test]
    fn create_order_input_rejects_long_data() {
        let intent_bytes = valid_intent_bytes();
        let mut data = default_order_data(&intent_bytes);
        data.push(0); // trailing byte
        let mut accounts = fake_sequential_accounts::<NUM_ACCOUNTS>();
        assert_eq!(
            CreateOrderInput::parse(&data, &mut accounts).err(),
            Some(ProgramError::InvalidInstructionData),
        );
    }

    #[test]
    fn create_order_input_rejects_missing_accounts() {
        let intent_bytes = valid_intent_bytes();
        let data = default_order_data(&intent_bytes);
        let mut accounts: Vec<AccountView> = fake_sequential_accounts::<NUM_ACCOUNTS>().into();
        accounts.pop();
        assert_eq!(
            CreateOrderInput::parse(&data, &mut accounts).err(),
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

        let Instruction { data, .. } = CreateOrder {
            program_id,
            owner,
            created_by,
            order_pda,
            intent_bytes,
        }
        .into();

        assert_eq!(data.len(), 1 + EncodedOrderIntent::SIZE);
        assert_eq!(data[0], SettlementInstruction::CreateOrder.discriminator());
        assert_eq!(&data[1..], &intent_bytes);
    }

    #[test]
    fn instruction_data_has_expected_accounts() {
        let program_id = Pubkey::new_from_array([1; 32]);
        let owner = Pubkey::new_from_array([2; 32]);
        let created_by = Pubkey::new_from_array([4; 32]);
        let order_pda = Pubkey::new_from_array([3; 32]);
        let intent_bytes = [0u8; EncodedOrderIntent::SIZE];

        let Instruction { accounts, .. } = CreateOrder {
            program_id,
            owner,
            created_by,
            order_pda,
            intent_bytes,
        }
        .into();

        assert_eq!(accounts.len(), 4);
        // owner: read-only, signer (authenticates the order; doesn't pay rent)
        assert_eq!(accounts[0].pubkey, owner);
        assert!(!accounts[0].is_writable);
        assert!(accounts[0].is_signer);
        // created_by: writable, signer (funds the new PDA's rent)
        assert_eq!(accounts[1].pubkey, created_by);
        assert!(accounts[1].is_writable);
        assert!(accounts[1].is_signer);
        // order_pda: writable, not signer (the program signs via PDA seeds)
        assert_eq!(accounts[2].pubkey, order_pda);
        assert!(accounts[2].is_writable);
        assert!(!accounts[2].is_signer);
        // system program: read-only, not signer; the on-chain handler
        // doesn't dereference it but the runtime requires it in the
        // transaction's `account_keys` to dispatch the CreateAccount CPI.
        assert_eq!(accounts[3].pubkey, SYSTEM_PROGRAM_ID);
        assert!(!accounts[3].is_writable);
        assert!(!accounts[3].is_signer);
    }
}
