//! `ReclaimOrder` instruction handler.

use pinocchio::{
    error::ProgramError,
    sysvars::{clock::Clock, Sysvar},
    AccountView, ProgramResult,
};
use settlement_interface::{
    data::order::OrderAccount,
    instruction::{reclaim_order::ReclaimOrderInput, InstructionInputParsing},
    SettlementError,
};

pub fn process_reclaim_order(
    program_id: &pinocchio::Address,
    accounts: &mut [AccountView],
    instruction_data: &[u8],
) -> ProgramResult {
    let ReclaimOrderInput {
        order_pda,
        reclaim_recipient,
    } = ReclaimOrderInput::parse(instruction_data, accounts)?;

    let account = OrderAccount::load_from_pda(order_pda, program_id)?;

    if reclaim_recipient.address() != &account.created_by {
        return Err(SettlementError::ReclaimRecipientMismatch.into());
    }

    let now = Clock::get()?.unix_timestamp;
    if now <= i64::from(account.intent.valid_to) {
        return Err(SettlementError::OrderNotExpired.into());
    }

    // Transfer the rent lamports to the reclaim_recipient account, then close the PDA.
    // Copied `AccountView` handles write through to the same runtime accounts.
    let (mut order_pda, mut reclaim_recipient) = (*order_pda, *reclaim_recipient);
    let order_lamports = order_pda.lamports();
    reclaim_recipient.set_lamports(
        reclaim_recipient
            .lamports()
            .checked_add(order_lamports)
            .ok_or(ProgramError::ArithmeticOverflow)?,
    );
    // Closing an order also sets the lamport balance to zero, so we don't need to
    // explicitly zero the account SOL balance.
    order_pda.close()?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use pinocchio::Address;
    use settlement_interface::data::order::EncodedOrderAccount;
    use settlement_interface::instruction::{
        fixtures::{fake_account, fake_account_with_data, fake_sequential_accounts},
        reclaim_order::fixtures::{default_reclaim_data, NUM_ACCOUNTS},
    };
    use settlement_interface::pda::order::find_order_pda;
    use settlement_interface::SettlementInstruction;

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
        let reclaim_recipient = fake_account(Address::new_from_array([2; 32]));

        let (order_pda_address, bump) =
            find_order_pda(&PROGRAM_ID, &OrderAccount::default().intent.uid());
        let order_data = OrderAccount {
            bump,
            created_by: Address::new_from_array([3; 32]),
            ..Default::default()
        };
        let data = vec![SettlementInstruction::ReclaimOrder.discriminator()];

        let order_pda = fake_account_with_data(
            order_pda_address,
            &EncodedOrderAccount::from(order_data)[..],
        );

        assert_eq!(
            process_reclaim_order(&PROGRAM_ID, &mut [order_pda, reclaim_recipient], &data),
            Err(SettlementError::ReclaimRecipientMismatch.into()),
        );
    }
}
