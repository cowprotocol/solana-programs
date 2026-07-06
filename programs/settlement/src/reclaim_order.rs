//! `ReclaimOrder` instruction handler.

use pinocchio::{error::ProgramError, AccountView, ProgramResult};
use settlement_interface::{
    data::order::{EncodedOrderAccount, OrderAccount},
    instruction::{reclaim_order::ReclaimOrderInput, InstructionInputParsing},
    SettlementError,
};

use crate::processor::get_timestamp;

pub fn process_reclaim_order(
    _program_id: &pinocchio::Address,
    accounts: &mut [AccountView],
    instruction_data: &[u8],
) -> ProgramResult {
    let ReclaimOrderInput {
        order_pda,
        reclaim_recipient,
    } = ReclaimOrderInput::parse(instruction_data, accounts)?;

    let account = {
        let data = order_pda.try_borrow()?;
        let bytes: &[u8; EncodedOrderAccount::SIZE] = (&*data)
            .try_into()
            .map_err(|_| ProgramError::InvalidAccountData)?;
        OrderAccount::try_from(*bytes)?
    };

    // Verify the reclaim_recipient account matches the one recorded in the order.
    if reclaim_recipient.address().as_array() != &account.created_by.to_bytes() {
        return Err(SettlementError::ReclaimRecipientMismatch.into());
    }

    let now = get_timestamp()?;
    if now <= i64::from(account.intent.valid_to) {
        return Err(SettlementError::OrderNotExpired.into());
    }

    // Transfer the rent lamports to the reclaim_recipient account, then close the PDA.
    let order_lamports = order_pda.lamports();
    reclaim_recipient.set_lamports(
        reclaim_recipient
            .lamports()
            .checked_add(order_lamports)
            .ok_or(ProgramError::ArithmeticOverflow)?,
    );
    order_pda.close()?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use pinocchio::Address;
    use settlement_interface::instruction::{
        fixtures::{fake_account, fake_account_with_data, fake_sequential_accounts},
        reclaim_order::fixtures::{default_reclaim_data, NUM_ACCOUNTS},
    };

    use super::*;

    const PROGRAM_ID: pinocchio::Address = pinocchio::Address::new_from_array([1; 32]);

    #[test]
    fn process_reclaim_order_propagates_parse_error() {
        let mut data = default_reclaim_data();
        data.push(0); // trailing byte triggers parse error
        let mut accounts = fake_sequential_accounts::<NUM_ACCOUNTS>();

        assert_eq!(
            process_reclaim_order(&PROGRAM_ID, &mut accounts, &data),
            Err(ProgramError::InvalidInstructionData),
        );
    }

    #[test]
    fn process_reclaim_order_rejects_mismatched_reclaim_recipient() {
        let data = default_reclaim_data();

        let reclaim_recipient = fake_account(Address::new_unique());

        let order_data = OrderAccount {
            created_by: Address::new_unique(),
            ..Default::default()
        };

        let order_pda = fake_account_with_data(
            Address::new_unique(),
            &EncodedOrderAccount::from(order_data)[..],
        );

        assert_eq!(
            process_reclaim_order(&PROGRAM_ID, &mut [order_pda, reclaim_recipient], &data),
            Err(SettlementError::ReclaimRecipientMismatch.into()),
        );
    }

    #[cfg(feature = "settlement-test-clock")]
    #[test]
    fn process_reclaim_order_rejects_not_yet_expired() {
        let data = default_reclaim_data();

        let reclaim_recipient = fake_account(Address::new_unique());

        let order_data = OrderAccount {
            intent: settlement_interface::data::intent::OrderIntent {
                valid_to: 4_000_000_000,
                ..Default::default()
            },
            created_by: *reclaim_recipient.address(),
            ..Default::default()
        };

        let order_pda = fake_account_with_data(
            Address::new_unique(),
            &EncodedOrderAccount::from(order_data)[..],
        );

        assert_eq!(
            process_reclaim_order(&PROGRAM_ID, &mut [order_pda, reclaim_recipient], &data),
            Err(SettlementError::OrderNotExpired.into()),
        );
    }
}
