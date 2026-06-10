//! `CreateOrder` instruction handler.

use pinocchio::{error::ProgramError, AccountView, Address, ProgramResult};
use settlement_interface::{
    data::{
        intent::EncodedOrderIntent,
        order::{self, EncodedOrderAccount},
    },
    pda::order::order_pda_seeds,
    SettlementError, SettlementInstruction,
};

use crate::processor::{create_canonical_pda, InstructionInputParsing};

/// Parsed inputs of a `CreateOrder` instruction.
struct CreateOrderInput<'a> {
    intent_bytes: [u8; EncodedOrderIntent::SIZE],
    owner: &'a AccountView,
    created_by: &'a AccountView,
    order_pda: &'a mut AccountView,
}

impl<'a> InstructionInputParsing<'a> for CreateOrderInput<'a> {
    const DISCRIMINATOR: SettlementInstruction = SettlementInstruction::CreateOrder;

    fn parse_body(
        instruction_data: &[u8],
        accounts: &'a mut [AccountView],
    ) -> Result<Self, ProgramError> {
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

pub fn process_create_order(
    program_id: &Address,
    accounts: &mut [AccountView],
    instruction_data: &[u8],
) -> ProgramResult {
    let CreateOrderInput {
        intent_bytes,
        owner,
        created_by,
        order_pda,
    } = CreateOrderInput::parse(instruction_data, accounts)?;
    // Validate the intent payload and recover its UID before any allocation.
    let (intent, intent_uid) = EncodedOrderIntent::decode_and_hash(&intent_bytes)?;

    // The order must be authorized by its owner.
    if !owner.is_signer() {
        return Err(ProgramError::MissingRequiredSignature);
    }
    if owner.address() != &intent.owner {
        return Err(SettlementError::OwnerMismatch.into());
    }

    // We want a single order per uid; `create_canonical_pda` derives the
    // canonical bump and, by signing the creation with the order seeds, rejects
    // any `order_pda` that isn't the canonical address. The rest of the code
    // can assume that if an account has data, then the bump is valid.
    create_canonical_pda(
        program_id,
        created_by,
        order_pda,
        EncodedOrderAccount::SIZE as u64,
        program_id,
        order_pda_seeds(&intent_uid),
    )?;

    // Note: `intent_bytes` were validated before and are known to represent a valid intent.
    let mut buffer = order_pda.try_borrow_mut()?;
    let buffer: &mut [u8; EncodedOrderAccount::SIZE] = (&mut *buffer)
        .try_into()
        .map_err(|_| ProgramError::AccountDataTooSmall)?;
    order::write_account(buffer, false, 0, 0, created_by.address(), &intent_bytes);

    Ok(())
}

#[cfg(test)]
mod tests {
    use settlement_interface::data::intent::{fixtures::sample_intent, OrderIntent, OrderKind};

    use pinocchio::account::RuntimeAccount;

    use super::*;
    use crate::test_utils::{
        fake_account, fake_account_from, fake_account_from_array, fake_sequential_accounts,
    };

    const DEFAULT_OWNER: Address = Address::new_from_array([0x11; 32]);

    /// Number of accounts `CreateOrder` expects: owner, created_by, order PDA,
    /// and the system program.
    const NUM_ACCOUNTS: usize = 4;

    fn valid_intent_bytes() -> [u8; EncodedOrderIntent::SIZE] {
        (&EncodedOrderIntent::from(&OrderIntent {
            owner: DEFAULT_OWNER,
            ..sample_intent(OrderKind::Sell, true)
        }))
            .into()
    }

    fn default_order_data(intent_bytes: &[u8; EncodedOrderIntent::SIZE]) -> Vec<u8> {
        // We used this to test failure conditions where the actual addresses
        // don't matter.
        let zero = Address::new_from_array([0; 32]);
        settlement_interface::instruction::create_order::create_order(
            &zero,
            &zero,
            &zero,
            &zero,
            intent_bytes,
        )
        .data
    }

    #[test]
    fn create_order_input_parses_valid_input() {
        let program_id = Address::new_from_array([21; 32]);
        let owner = Address::new_from_array([22; 32]);
        let created_by = Address::new_from_array([24; 32]);
        let order_pda = Address::new_from_array([23; 32]);
        let intent_bytes = valid_intent_bytes();

        let data = settlement_interface::instruction::create_order::create_order(
            &program_id,
            &owner,
            &created_by,
            &order_pda,
            &intent_bytes,
        )
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

    /// Arbitrary placeholder program id for handler-level tests. The
    /// failure paths exercised below return before the program id is used
    /// for any syscall, so any 32-byte value works.
    const PROGRAM_ID: Address = Address::new_from_array([1; 32]);

    #[test]
    fn process_create_order_propagates_parse_error() {
        let intent_bytes = valid_intent_bytes();
        let mut data = default_order_data(&intent_bytes);
        // We generate a parse error by having less bytes than necessary.
        data.pop();
        let mut accounts = fake_sequential_accounts::<NUM_ACCOUNTS>();

        assert_eq!(
            process_create_order(&PROGRAM_ID, &mut accounts, &data),
            Err(ProgramError::InvalidInstructionData),
        );
    }

    #[test]
    fn process_create_order_rejects_invalid_encoded_intent() {
        let intent: OrderIntent = (&valid_intent_bytes()).try_into().expect("should be valid");
        let intent_bytes_buy = EncodedOrderIntent::from(&OrderIntent {
            kind: OrderKind::Buy,
            ..intent
        });
        let intent_bytes_sell = EncodedOrderIntent::from(&OrderIntent {
            kind: OrderKind::Sell,
            ..intent
        });
        fn first_differing_byte(lhs: &[u8], rhs: &[u8]) -> Option<usize> {
            lhs.iter().zip(rhs).position(|(l, r)| l != r)
        }
        let kind_offset =
            first_differing_byte(intent_bytes_buy.as_slice(), intent_bytes_sell.as_slice())
                .expect("kind is different");

        let mut intent_bytes: [u8; EncodedOrderIntent::SIZE] = (&intent_bytes_buy).into();
        // Invalid order kind
        intent_bytes[kind_offset] = 0x42;

        let data = default_order_data(&intent_bytes);
        let mut accounts = fake_sequential_accounts::<NUM_ACCOUNTS>();

        assert_eq!(
            process_create_order(&PROGRAM_ID, &mut accounts, &data),
            Err(ProgramError::InvalidInstructionData),
        );
    }

    #[test]
    fn process_create_order_rejects_nonsigner_owner() {
        let intent_bytes = valid_intent_bytes();
        let data = default_order_data(&intent_bytes);
        let owner_account = fake_account(DEFAULT_OWNER);

        // Test setup: owner is not a signer.
        assert!(!owner_account.is_signer());

        let mut accounts = fake_sequential_accounts::<NUM_ACCOUNTS>();
        accounts[0] = owner_account;

        assert_eq!(
            process_create_order(&PROGRAM_ID, &mut accounts, &data),
            Err(ProgramError::MissingRequiredSignature),
        );
    }

    #[test]
    fn process_create_order_rejects_owner_mismatch() {
        let intent_bytes = valid_intent_bytes();
        let data = default_order_data(&intent_bytes);
        let owner_runtime_account = RuntimeAccount {
            address: Address::new_from_array([0x67; 32]),
            is_signer: 1,
            ..Default::default()
        };

        // Test setup: owner doesn't match.
        assert_ne!(owner_runtime_account.address, DEFAULT_OWNER);

        let mut accounts = fake_sequential_accounts::<NUM_ACCOUNTS>();
        accounts[0] = fake_account_from(owner_runtime_account);

        assert_eq!(
            process_create_order(&PROGRAM_ID, &mut accounts, &data),
            Err(SettlementError::OwnerMismatch.into()),
        );
    }
}
