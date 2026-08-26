//! `ReclaimOrder` instruction handler.

use cow_settlement_interface::{
    data::order::OrderAccount,
    instruction::{reclaim_order::ReclaimOrderInput, InstructionInputParsing},
    SettlementError,
};
use pinocchio::{
    error::ProgramError,
    sysvars::{clock::Clock, Sysvar},
    AccountView, ProgramResult,
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

    // Is this order eligible for reclaimation?
    if !is_reclaimable_before_expiry(&account) {
        let now = Clock::get()?.unix_timestamp;
        if now <= i64::from(account.intent.valid_to) {
            return Err(SettlementError::OrderNotReclaimable.into());
        }
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

/// Determines whether the order may be reclaimed despite being unexpired
fn is_reclaimable_before_expiry(account: &OrderAccount) -> bool {
    account.intent.created_on_chain && (account.cancelled || account.is_fully_filled())
}

#[cfg(test)]
mod tests {
    use cow_settlement_interface::data::intent::{fixtures::sample_intent, OrderIntent, OrderKind};
    use cow_settlement_interface::data::order::EncodedOrderAccount;
    use cow_settlement_interface::instruction::{
        fixtures::{fake_account, fake_account_with_data, fake_sequential_accounts},
        reclaim_order::fixtures::{default_reclaim_data, NUM_ACCOUNTS},
    };
    use cow_settlement_interface::pda::order::find_order_pda;
    use cow_settlement_interface::SettlementInstruction;
    use pinocchio::Address;

    use super::*;

    const PROGRAM_ID: pinocchio::Address = pinocchio::Address::new_from_array([0xc0; 32]);

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

    #[test]
    fn early_reclaim_conditions() {
        const SELL_AMOUNT: u64 = 1_000;

        let account = |created_on_chain, cancelled, amount_withdrawn| OrderAccount {
            cancelled,
            amount_withdrawn,
            intent: OrderIntent {
                sell_amount: SELL_AMOUNT,
                created_on_chain,
                ..sample_intent(OrderKind::Sell, true)
            },
            ..Default::default()
        };

        // (created_on_chain, cancelled, amount_withdrawn, expected)
        let cases = [
            // Created on-chain and either cancelled or fully settled.
            (true, true, 0, true),
            (true, false, SELL_AMOUNT, true),
            (true, true, SELL_AMOUNT, true),
            // Authenticated by signature: prior cancelled or fully settled cases no longer apply
            (false, true, 0, false),
            (false, false, SELL_AMOUNT, false),
            (false, true, SELL_AMOUNT, false),
            // Created on-chain and not fully filled.
            (true, false, 0, false),
            (true, false, SELL_AMOUNT - 1, false),
        ];
        for (created_on_chain, cancelled, amount_withdrawn, expected) in cases {
            assert_eq!(
                is_reclaimable_before_expiry(&account(
                    created_on_chain,
                    cancelled,
                    amount_withdrawn
                )),
                expected,
                "created_on_chain={created_on_chain} cancelled={cancelled} \
                 amount_withdrawn={amount_withdrawn}",
            );
        }
    }
}
