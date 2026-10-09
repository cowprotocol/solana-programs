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

use crate::processor::utils::intent::{fill_progress, OrderIntentAccessor};

pub fn process_reclaim_order(
    program_id: &pinocchio::Address,
    accounts: &mut [AccountView],
    instruction_data: &[u8],
) -> ProgramResult {
    let ReclaimOrderInput {
        order_pda,
        reclaim_recipient,
        owner,
    } = ReclaimOrderInput::parse(instruction_data, accounts)?;

    // Decide reclaimability, then drop the borrow before the lamport transfer
    // and `close` below touch the account.
    let reclaimable = {
        let order = OrderAccount::load_from_pda(order_pda, program_id)?;
        if reclaim_recipient.address() != &order.created_by() {
            return Err(SettlementError::ReclaimRecipientMismatch.into());
        }
        let intent = OrderIntentAccessor::from_order(&order)?;
        is_reclaimable(&order, &intent, owner)?
    };

    if !reclaimable {
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

/// Determines whether the order may be closed now.
fn is_reclaimable<T: core::ops::Deref<Target = [u8]>>(
    order: &OrderAccount<T>,
    intent: &OrderIntentAccessor,
    owner: Option<&AccountView>,
) -> Result<bool, ProgramError> {
    // 1. Anyone may reclaim a fully filled order even before it expires.
    let (filled, order_amount) = fill_progress(intent, order.filled_amounts());
    if filled >= order_amount.into() {
        return Ok(true);
    }

    // 2. Anyone may reclaim an expired order.
    if Clock::get()?.unix_timestamp > i64::from(intent.valid_to()) {
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
    use cow_settlement_interface::data::intent::OrderIntent;
    use cow_settlement_interface::data::order::fixtures::OrderFields;
    use cow_settlement_interface::instruction::{
        fixtures::{fake_account, fake_account_with_data, fake_sequential_accounts},
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
}
