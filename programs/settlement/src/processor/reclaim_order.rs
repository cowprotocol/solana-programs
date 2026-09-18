//! `ReclaimOrder` instruction handler.

use cow_settlement_interface::{
    data::{
        intent::OrderIntent,
        order::{fill_progress, FillAmounts, OrderAccount},
    },
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

    // Read the fields the reclaim decision needs, then drop the borrow before
    // the lamport transfer and `close` below touch the account.
    let (created_by, reclaimable, valid_to) = {
        let order = OrderAccount::load_from_pda(order_pda, program_id)?;
        let intent = order.intent()?;
        let reclaimable =
            is_reclaimable_before_expiry(&intent, order.cancelled()?, order.filled_amounts());
        (order.created_by(), reclaimable, intent.valid_to)
    };

    if reclaim_recipient.address() != &created_by {
        return Err(SettlementError::ReclaimRecipientMismatch.into());
    }

    if !reclaimable {
        let now = Clock::get()?.unix_timestamp;
        if now <= i64::from(valid_to) {
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
fn is_reclaimable_before_expiry(intent: &OrderIntent, cancelled: bool, fill: FillAmounts) -> bool {
    intent.flags.created_on_chain
        && (cancelled || {
            let (filled, order_amount) = fill_progress(intent, fill);
            filled >= order_amount
        })
}

#[cfg(test)]
mod tests {
    use cow_settlement_interface::data::intent::Flags;
    use cow_settlement_interface::data::intent::{fixtures::sample_intent, OrderIntent, OrderKind};
    use cow_settlement_interface::data::order::fixtures::OrderFields;
    use cow_settlement_interface::fixtures::PROGRAM_ID;
    use cow_settlement_interface::instruction::{
        fixtures::{fake_account, fake_account_with_data, fake_sequential_accounts},
        reclaim_order::fixtures::{default_reclaim_data, NUM_ACCOUNTS},
    };
    use cow_settlement_interface::pda::order::find_order_pda;
    use cow_settlement_interface::SettlementInstruction;
    use pinocchio::Address;

    use super::*;

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

        let intent = OrderIntent::default();
        let (order_pda_address, bump) = find_order_pda(&PROGRAM_ID, &intent.uid());
        let order_bytes = OrderFields {
            bump,
            cancelled: false,
            amount_withdrawn: 0,
            amount_received: 0,
            created_by: Address::new_from_array([3; 32]),
            intent,
        }
        .encode();
        let data = vec![SettlementInstruction::ReclaimOrder.discriminator()];

        let order_pda = fake_account_with_data(order_pda_address, &order_bytes[..]);

        assert_eq!(
            process_reclaim_order(&PROGRAM_ID, &mut [order_pda, reclaim_recipient], &data),
            Err(SettlementError::ReclaimRecipientMismatch.into()),
        );
    }

    #[test]
    fn early_reclaim_conditions() {
        const SELL_AMOUNT: u64 = 1_000;

        let intent = |created_on_chain| OrderIntent {
            sell_amount: SELL_AMOUNT,
            ..sample_intent(Flags {
                created_on_chain,
                kind: OrderKind::Sell,
                partially_fillable: true,
            })
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
            (false, false, 0, false),
            // Created on-chain and not fully filled.
            (true, false, 0, false),
            (true, false, SELL_AMOUNT - 1, false),
        ];
        for (created_on_chain, cancelled, amount_withdrawn, expected) in cases {
            assert_eq!(
                is_reclaimable_before_expiry(
                    &intent(created_on_chain),
                    cancelled,
                    FillAmounts {
                        withdrawn: amount_withdrawn,
                        received: 0,
                    },
                ),
                expected,
                "created_on_chain={created_on_chain} cancelled={cancelled} \
                 amount_withdrawn={amount_withdrawn}",
            );
        }
    }
}
