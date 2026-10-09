//! `ReclaimOrder` instruction handler.

use cow_settlement_interface::{
    data::order::OrderAccount,
    instruction::{reclaim_order::ReclaimOrderInput, InstructionInputParsing},
    SettlementError, ID,
};
use pinocchio::{
    error::ProgramError,
    sysvars::{clock::Clock, Sysvar},
    AccountView, ProgramResult,
};

use crate::processor::utils::intent::{fill_progress, OrderIntentAccessor};

pub fn process_reclaim_order(
    accounts: &mut [AccountView],
    instruction_data: &[u8],
) -> ProgramResult {
    let ReclaimOrderInput {
        order_pda,
        reclaim_recipient,
        owner,
    } = ReclaimOrderInput::parse(instruction_data, accounts)?;

    // Determine whether this order is reclaimable or not.
    if !is_reclaimable(order_pda, reclaim_recipient, owner, || {
        Ok(Clock::get()?.unix_timestamp)
    })? {
        return Err(SettlementError::OrderNotReclaimable.into());
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

/// Determines whether the order may be closed now. `now` is only called once the
/// cheaper fill check has failed, and is injected so unit tests can fake the clock.
#[inline(always)]
fn is_reclaimable(
    order_pda: &AccountView,
    reclaim_recipient: &AccountView,
    owner: Option<&AccountView>,
    now: impl FnOnce() -> Result<i64, ProgramError>,
) -> Result<bool, ProgramError> {
    let order = OrderAccount::load_from_pda(order_pda, &ID)?;

    // 0. The reclaim recipient always has to be correct.
    if reclaim_recipient.address() != &order.created_by() {
        return Err(SettlementError::ReclaimRecipientMismatch.into());
    }

    let intent = OrderIntentAccessor::from_order(&order)?;

    // 1. Anyone may reclaim a fully filled order even before it expires.
    let (filled, order_amount) = fill_progress(&intent, order.filled_amounts());
    if filled >= order_amount.into() {
        return Ok(true);
    }

    // 2. Anyone may reclaim an expired order.
    if now()? > i64::from(intent.valid_to()) {
        return Ok(true);
    }

    // 3. Only the owner may reclaim a cancelled order. This protects against a delayed sponsored
    // order from reopening a cancelled order by reclaiming.
    if order.cancelled()? {
        let owner = owner.ok_or(ProgramError::MissingRequiredSignature)?;
        if !owner.is_signer() {
            return Err(ProgramError::MissingRequiredSignature);
        }
        if owner.address().as_array() != intent.owner() {
            return Err(SettlementError::OwnerMismatch.into());
        }
        return Ok(true);
    }

    // 4. Nothing else permits reclaim.
    Ok(false)
}

#[cfg(test)]
mod tests {
    use cow_settlement_interface::data::intent::{
        fixtures::sample_intent, Flags, OrderIntent, OrderKind,
    };
    use cow_settlement_interface::data::order::fixtures::OrderFields;
    use cow_settlement_interface::fixtures::IntoNonZero;
    use cow_settlement_interface::instruction::{
        fixtures::{fake_account, fake_account_with_data, fake_sequential_accounts, fake_signer},
        reclaim_order::fixtures::{default_reclaim_data, NUM_ACCOUNTS},
    };
    use cow_settlement_interface::pda::order::find_order_pda;
    use cow_settlement_interface::SettlementInstruction;
    use cow_settlement_interface::ID as PROGRAM_ID;
    use pinocchio::Address;

    use super::*;

    #[test]
    fn process_reclaim_order_propagates_parse_error() {
        let mut data = default_reclaim_data();
        data.push(0); // trailing byte triggers parse error
        let mut accounts = fake_sequential_accounts::<NUM_ACCOUNTS>();

        assert_eq!(
            process_reclaim_order(&mut accounts, &data),
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
            process_reclaim_order(&mut [order_pda, reclaim_recipient], &data),
            Err(SettlementError::ReclaimRecipientMismatch.into()),
        );
    }

    #[test]
    fn reclaim_conditions() {
        const SELL_AMOUNT: u64 = 1_000;

        let intent = OrderIntent {
            sell_amount: SELL_AMOUNT.nz(),
            ..sample_intent(Flags {
                kind: OrderKind::Sell,
                partially_fillable: true,
            })
        };
        let created_by = Address::new_from_array([3; 32]);
        let owner_address = Address::new_from_array(intent.owner.to_bytes());
        let valid_to = i64::from(intent.valid_to);
        let (order_pda_address, bump) = find_order_pda(&PROGRAM_ID, &intent.uid());

        let owner_signer = fake_signer(owner_address);
        let owner_nonsigner = fake_account(owner_address);
        let other_signer = fake_signer(Address::new_from_array([4; 32]));

        let not_reclaimable = Ok(false);
        let reclaimable = Ok(true);
        let missing_signature = Err(ProgramError::MissingRequiredSignature);
        let owner_mismatch = Err(SettlementError::OwnerMismatch.into());

        // (cancelled, amount_withdrawn, now, owner, expected)
        let cases = [
            // Active and not fully filled: only expiry makes it reclaimable.
            (false, 0, valid_to, None, &not_reclaimable),
            (false, SELL_AMOUNT - 1, valid_to, None, &not_reclaimable),
            // An owner signature doesn't help an active order.
            (false, 0, valid_to, Some(&owner_signer), &not_reclaimable),
            (false, 0, valid_to + 1, None, &reclaimable),
            // Fully filled: reclaimable by anyone before expiry.
            (false, SELL_AMOUNT, valid_to, None, &reclaimable),
            (true, SELL_AMOUNT, valid_to, None, &reclaimable),
            // Cancelled and expired: reclaimable by anyone.
            (true, 0, valid_to + 1, None, &reclaimable),
            // Cancelled before expiry: only the signing owner may reclaim.
            (true, 0, valid_to, Some(&owner_signer), &reclaimable),
            (true, 0, valid_to, None, &missing_signature),
            (
                true,
                0,
                valid_to,
                Some(&owner_nonsigner),
                &missing_signature,
            ),
            (true, 0, valid_to, Some(&other_signer), &owner_mismatch),
        ];
        for (cancelled, amount_withdrawn, now, owner, expected) in cases {
            let order_bytes = OrderFields {
                bump,
                cancelled,
                amount_withdrawn,
                amount_received: 0,
                created_by,
                intent: intent.clone(),
            }
            .encode();
            let order_pda = fake_account_with_data(order_pda_address, &order_bytes[..]);

            assert_eq!(
                &is_reclaimable(&order_pda, &fake_account(created_by), owner, || Ok(now)),
                expected,
                "cancelled={cancelled} amount_withdrawn={amount_withdrawn} now={now} owner={:?}",
                owner.map(|owner| (owner.address(), owner.is_signer())),
            );
        }
    }

    #[test]
    fn reclaim_of_filled_order_skips_clock() {
        let intent = sample_intent(Flags {
            kind: OrderKind::Sell,
            partially_fillable: false,
        });
        let created_by = Address::new_from_array([3; 32]);
        let (order_pda_address, bump) = find_order_pda(&PROGRAM_ID, &intent.uid());
        let order_bytes = OrderFields {
            bump,
            cancelled: false,
            amount_withdrawn: intent.sell_amount.get(),
            amount_received: 0,
            created_by,
            intent,
        }
        .encode();
        let order_pda = fake_account_with_data(order_pda_address, &order_bytes[..]);

        assert_eq!(
            is_reclaimable(&order_pda, &fake_account(created_by), None, || {
                unreachable!("clock must not be read for a fully filled order")
            }),
            Ok(true),
        );
    }
}
