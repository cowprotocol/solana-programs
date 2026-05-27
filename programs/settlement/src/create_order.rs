//! `CreateOrder` instruction handler.

use pinocchio::{
    cpi::{Seed, Signer},
    error::ProgramError,
    AccountView, Address, ProgramResult,
};
use pinocchio_system::instructions::CreateAccount;
use settlement_interface::{
    data::{intent::EncodedOrderIntent, order::EncodedOrderAccount},
    pda::order::{order_pda_seeds, order_pda_signer_seeds},
    SettlementInstruction,
};

use crate::processor::InstructionInputParsing;

/// Parsed inputs of a `CreateOrder` instruction.
struct CreateOrderInput<'a> {
    intent_bytes: [u8; EncodedOrderIntent::SIZE],
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
        // Accounts: [created_by (W,S), order_pda (W), some other account].
        // We check that there are three accounts because the instruction
        // needs to specify `SYSTEM_PROGRAM_ID` as one of the signers.
        // It doesn't have to be the third though.
        let [created_by, order_pda, _, ..] = accounts else {
            return Err(ProgramError::NotEnoughAccountKeys);
        };

        let intent_bytes: [u8; EncodedOrderIntent::SIZE] =
            instruction_data.try_into().expect("length checked above");

        Ok(Self {
            intent_bytes,
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
        created_by,
        order_pda,
    } = CreateOrderInput::parse(instruction_data, accounts)?;
    // Validate the intent payload and recover its UID before any allocation.
    let (_intent, intent_uid) = EncodedOrderIntent::decode_and_hash(&intent_bytes)?;
    // We want to have a single order per uid, so we need to derive the bump
    // here. The rest of the code can assume that if an account has data,
    // then the bump is valid.
    let (_, bump) = Address::find_program_address(&order_pda_seeds(&intent_uid), program_id);

    let bump_seed = [bump];
    let signer_seeds = order_pda_signer_seeds(&intent_uid, &bump_seed).map(Seed::from);
    let signer = Signer::from(&signer_seeds[..]);

    // Implicitly, this also checks that `order_pda.address()` matches the
    // bump generated above without needing to directly compare addresses.
    CreateAccount::with_minimum_balance(
        created_by,
        order_pda,
        EncodedOrderAccount::SIZE as u64,
        program_id,
        None,
    )?
    .invoke_signed(&[signer])?;

    // Note: `intent_bytes` were validated before and are known to represent a valid intent.
    let initial = EncodedOrderAccount::init(created_by.address(), &intent_bytes);
    let mut data = order_pda.try_borrow_mut()?;
    data.copy_from_slice(&initial[..]);

    Ok(())
}

#[cfg(test)]
mod tests {
    use settlement_interface::data::intent::{OrderIntent, OrderKind};

    use super::*;
    use crate::test_utils::fake_account;

    fn valid_intent_bytes() -> [u8; EncodedOrderIntent::SIZE] {
        (&EncodedOrderIntent::from(&OrderIntent {
            owner: Address::new_from_array([0x11; 32]),
            buy_token_account: Address::new_from_array([0x22; 32]),
            sell_token_account: Address::new_from_array([0x33; 32]),
            sell_amount: 0x0123_4567_89ab_cdef,
            buy_amount: 0xfedc_ba98_7654_3210,
            valid_to: 0xdead_beef,
            kind: OrderKind::Sell,
            partially_fillable: true,
            app_data: [0x44; 32],
        }))
            .into()
    }

    fn default_order_data(intent_bytes: &[u8; EncodedOrderIntent::SIZE]) -> Vec<u8> {
        // We used this to test failure conditions where the actual addresses
        // don't matter.
        let zero = Address::new_from_array([0; 32]);
        settlement_interface::create_order::create_order(&zero, &zero, &zero, intent_bytes).data
    }

    fn three_accounts() -> [AccountView; 3] {
        [
            fake_account(Address::new_from_array([1; 32])),
            fake_account(Address::new_from_array([2; 32])),
            fake_account(Address::new_from_array([3; 32])),
        ]
    }

    #[test]
    fn create_order_input_parses_valid_input() {
        let program_id = Address::new_from_array([21; 32]);
        let created_by = Address::new_from_array([22; 32]);
        let order_pda = Address::new_from_array([23; 32]);
        let intent_bytes = valid_intent_bytes();

        let data = settlement_interface::create_order::create_order(
            &program_id,
            &created_by,
            &order_pda,
            &intent_bytes,
        )
        .data;
        let mut accounts = [
            fake_account(created_by),
            fake_account(order_pda),
            fake_account(Address::new_from_array([3; 32])),
        ];

        let parsed = CreateOrderInput::parse(&data, &mut accounts).expect("parse should succeed");

        assert_eq!(parsed.intent_bytes, intent_bytes);
        assert_eq!(*parsed.order_pda.address(), order_pda);
        assert_eq!(*parsed.created_by.address(), created_by);
    }

    #[test]
    fn create_order_input_rejects_different_discriminator() {
        let intent_bytes = valid_intent_bytes();
        let mut data = default_order_data(&intent_bytes);
        data[0] = SettlementInstruction::BeginSettle.discriminator();
        let mut accounts = three_accounts();
        assert_eq!(
            CreateOrderInput::parse(&data, &mut accounts).err(),
            Some(ProgramError::InvalidInstructionData),
        );
    }

    #[test]
    fn create_order_input_rejects_short_data() {
        let intent_bytes = valid_intent_bytes();
        let mut data = default_order_data(&intent_bytes);
        data.pop();
        let mut accounts = three_accounts();
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
        let mut accounts = three_accounts();
        assert_eq!(
            CreateOrderInput::parse(&data, &mut accounts).err(),
            Some(ProgramError::InvalidInstructionData),
        );
    }

    #[test]
    fn create_order_input_rejects_missing_accounts() {
        let intent_bytes = valid_intent_bytes();
        let data = default_order_data(&intent_bytes);
        let mut accounts: [AccountView; 2] = [
            fake_account(Address::new_from_array([1; 32])),
            fake_account(Address::new_from_array([2; 32])),
        ];
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
        let mut accounts = three_accounts();

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
            lhs.iter()
                .zip(rhs)
                .enumerate()
                .find(|(_, (l, r))| l != r)
                .map(|(i, _)| i)
        }
        let kind_offset =
            first_differing_byte(intent_bytes_buy.as_slice(), intent_bytes_sell.as_slice())
                .expect("kind is different");

        let mut intent_bytes: [u8; EncodedOrderIntent::SIZE] = (&intent_bytes_buy).into();
        // Invalid order kind
        intent_bytes[kind_offset] = 0x42;

        let data = default_order_data(&intent_bytes);
        let mut accounts = three_accounts();

        assert_eq!(
            process_create_order(&PROGRAM_ID, &mut accounts, &data),
            Err(ProgramError::InvalidInstructionData),
        );
    }
}
