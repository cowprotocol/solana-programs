//! `CreateOrder` instruction handler.

use cow_settlement_interface::{
    data::{
        intent::EncodedOrderIntent,
        order::{self, EncodedOrderAccount},
    },
    instruction::{create_order::CreateOrderInput, InstructionInputParsing},
    pda::order::order_pda_seeds,
    SettlementError,
};
use pinocchio::{error::ProgramError, AccountView, Address, ProgramResult};

use crate::processor::CanonicalPda;

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
    // The intent commits to how it's authenticated, and this is the on-chain
    // creation flow.
    if !intent.flags.created_on_chain {
        return Err(SettlementError::OrderCreatedOnChainMismatch.into());
    }

    // We want a single order per uid; `CanonicalPda::create_new` derives the
    // canonical bump and, by signing the creation with the order seeds, rejects
    // any `order_pda` that isn't the canonical address. This is the owner's own
    // create flow, so recreating an order that already exists is a user error:
    // `create_new` then reverts. The rest of the code can assume that if an
    // account has data, then the bump is valid.
    let bump = CanonicalPda {
        program_id,
        payer: created_by,
        pda: order_pda,
        size: EncodedOrderAccount::SIZE as u64,
        owner: program_id,
        seeds: order_pda_seeds(&intent_uid),
    }
    .create_new()?;

    // A copied `AccountView` handle writes through to the same runtime account.
    let mut order_pda = *order_pda;
    let mut order_data = order_pda.try_borrow_mut()?;
    let order_data: &mut [u8; EncodedOrderAccount::SIZE] = (&mut *order_data)
        .try_into()
        .map_err(|_| ProgramError::AccountDataTooSmall)?;
    order::write_account(
        order_data,
        bump,
        false,
        0,
        0,
        created_by.address(),
        &intent_bytes,
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use cow_settlement_interface::data::intent::{Flags, OrderIntent, OrderKind};
    use cow_settlement_interface::fixtures::PROGRAM_ID;
    use cow_settlement_interface::instruction::create_order::fixtures::{
        default_order_data, valid_intent_bytes, DEFAULT_OWNER, NUM_ACCOUNTS,
    };
    use cow_settlement_interface::instruction::fixtures::{
        fake_account, fake_account_from, fake_sequential_accounts,
    };

    use pinocchio::account::RuntimeAccount;

    use super::*;

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
            flags: Flags {
                kind: OrderKind::Buy,
                ..intent.flags
            },
            ..intent
        });
        let intent_bytes_sell = EncodedOrderIntent::from(&OrderIntent {
            flags: Flags {
                kind: OrderKind::Sell,
                ..intent.flags
            },
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

    #[test]
    fn process_create_order_rejects_intent_not_created_on_chain() {
        let intent: OrderIntent = (&valid_intent_bytes()).try_into().expect("should be valid");
        let intent_bytes: [u8; EncodedOrderIntent::SIZE] =
            (&EncodedOrderIntent::from(&OrderIntent { ..intent })).into();
        let data = default_order_data(&intent_bytes);
        let owner_runtime_account = RuntimeAccount {
            address: DEFAULT_OWNER,
            is_signer: 1,
            ..Default::default()
        };

        let mut accounts = fake_sequential_accounts::<NUM_ACCOUNTS>();
        accounts[0] = fake_account_from(owner_runtime_account);

        assert_eq!(
            process_create_order(&PROGRAM_ID, &mut accounts, &data),
            Err(SettlementError::OrderCreatedOnChainMismatch.into()),
        );
    }
}
