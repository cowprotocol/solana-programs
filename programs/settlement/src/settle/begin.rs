//! `BeginSettle` instruction handler.

use std::ops::Deref;

use pinocchio::{
    cpi::{Seed, Signer},
    error::ProgramError,
    sysvars::{clock::Clock, instructions::Instructions, Sysvar},
    AccountView, Address, ProgramResult,
};
use pinocchio_token::{instructions::Transfer, state::Account as TokenAccount};
use settlement_interface::{
    data::order::EncodedOrderAccount,
    instruction::{
        create_buffer::SPL_TOKEN_PROGRAM_ID,
        settle::{BeginSettleInput, SettledOrder},
        InstructionInputParsing,
    },
    pda::{order::order_pda_signer_seeds, state::state_pda_seeds},
    recover_discriminator, Pubkey, SettlementError, SettlementInstruction,
};

use crate::processor::is_cpi_call;

use super::validate_counterpart;

pub fn process_begin_settle(
    program_id: &Address,
    accounts: &mut [AccountView],
    instruction_data: &[u8],
) -> ProgramResult {
    if is_cpi_call() {
        return Err(SettlementError::CalledViaCpi.into());
    }

    let input = BeginSettleInput::parse(instruction_data, accounts)?;

    // We use `instructions_sysvar_account` from the input but this could be
    // any address since parsing doesn't validate the input. We rely on the
    // fact that the Pinocchio library already checks that the input account
    // is the expected one.
    let instructions = Instructions::try_from(input.instructions_sysvar_account)?;
    let current_index = instructions.load_current_index();

    // Reciprocity: the input index is a finalize_settle instruction and that
    // instruction points to the current one.
    validate_counterpart(
        program_id,
        &instructions,
        current_index,
        input.finalize_ix_index,
        SettlementInstruction::FinalizeSettle,
    )?;

    validate_no_nested_settlement(
        program_id,
        &instructions,
        current_index,
        input.finalize_ix_index,
    )?;

    pull_funds(
        program_id,
        input.token_program_account,
        input.state_pda_account,
        input.orders.iter(),
    )?;

    Ok(())
}

/// Reject a `BeginSettle` whose pair encloses another settlement: no
/// `BeginSettle`/`FinalizeSettle` of this program may appear strictly between
/// `current_index` and `finalize_ix_index`. The bounds themselves are excluded.
#[must_use = "ignoring the output may lead to an unintended on-chain state"]
fn validate_no_nested_settlement<T: Deref<Target = [u8]>>(
    program_id: &Address,
    instructions: &Instructions<T>,
    current_index: u16,
    finalize_ix_index: u16,
) -> ProgramResult {
    if finalize_ix_index <= current_index {
        return Err(SettlementError::FinalizeBeforeInitialize.into());
    }

    let search_start = current_index
        .checked_add(1)
        .expect("the finalize index is tested to be larger, no overflow can happen");
    for i in search_start..finalize_ix_index {
        let inner = instructions.load_instruction_at(usize::from(i))?;
        // Skip instructions belonging to a different program.
        if inner.get_program_id() != program_id {
            continue;
        }
        // If it can't recover the discriminator, it's fine: we expect that
        // instruction to fail, but this isn't something that matters here.
        // If the discriminator is valid, then it should not be the start
        // or end of a settlement.
        if let Ok((discriminator, _)) = recover_discriminator(inner.get_instruction_data()) {
            if [
                SettlementInstruction::BeginSettle,
                SettlementInstruction::FinalizeSettle,
            ]
            .contains(&discriminator)
            {
                return Err(SettlementError::BeginFinalizePairOverlap.into());
            }
        }
    }

    Ok(())
}

/// Validate and pull funds for each order, requiring:
/// - the legacy SPL Token program;
/// - the canonical state PDA, which signs each transfer as the user's delegate;
/// - orders strictly increasing by address, rejecting duplicates.
///
/// Further validation and the actual transfers are processed through
/// [`process_order`].
#[must_use = "ignoring the output may lead to an unintended on-chain state"]
fn pull_funds<'a>(
    program_id: &Address,
    token_program_account: &AccountView,
    state_pda_account: &AccountView,
    orders: impl IntoIterator<Item = SettledOrder<'a>>,
) -> ProgramResult {
    if token_program_account.address() != &SPL_TOKEN_PROGRAM_ID {
        return Err(ProgramError::IncorrectProgramId);
    }

    // Funds are pulled with the state PDA's delegation, so it must be the signer.
    let seeds = state_pda_seeds();
    let (state_pda, state_bump) = Address::find_program_address(&seeds, program_id);
    if state_pda_account.address() != &state_pda {
        return Err(SettlementError::StateAccountMismatch.into());
    }

    let [seed] = seeds;
    let state_bump = [state_bump];
    let signer_seeds = [seed, &state_bump].map(Seed::from);
    let state_pda_signer = Signer::from(&signer_seeds);

    // Orders must be passed strictly increasing by address; this rejects
    // duplicates (settling the same order twice) without a separate scan.
    let mut previous: Option<&Address> = None;

    let now = Clock::get()?.unix_timestamp;

    for order in orders {
        let order_pda = order.order_pda;
        if previous.is_some_and(|previous| order_pda.address() <= previous) {
            return Err(SettlementError::OrdersNotStrictlyIncreasing.into());
        }
        previous = Some(order_pda.address());

        process_order(program_id, order, now, state_pda_account, &state_pda_signer)?;
    }

    Ok(())
}

/// Validate a single order and process its pulls.
/// This checks that the order is valid and settleable. Once the order passes
/// those checks, its pulls are executed.
#[must_use = "ignoring the output may lead to an unintended on-chain state"]
fn process_order(
    program_id: &Address,
    order: SettledOrder<'_>,
    now: i64,
    state_account: &AccountView,
    state_pda_signer: &Signer,
) -> ProgramResult {
    let SettledOrder {
        order_pda,
        sell_token_account,
        bump,
        destinations,
        amounts,
    } = order;

    // Decode the order body. Reading is safe regardless of who owns the
    // account; the canonical-address check below is what proves provenance.
    // The borrow is released at the end of this block, before any other
    // account is touched.
    let (cancelled, intent, uid) = {
        let data = order_pda.try_borrow()?;
        let bytes: &[u8; EncodedOrderAccount::SIZE] = (&*data)
            .try_into()
            .map_err(|_| ProgramError::InvalidAccountData)?;
        let (account, uid) = EncodedOrderAccount::decode_and_hash(bytes)?;
        (account.cancelled, account.intent, uid)
    };

    // Only at this point we can validate that the PDA is indeed a valid
    // order PDA by seeing its address matches the computed one.
    let expected =
        Address::create_program_address(&order_pda_signer_seeds(&uid, &[bump]), program_id)
            .map_err(|_| SettlementError::OrderNotCanonical)?;
    if &expected != order_pda.address() {
        return Err(SettlementError::OrderNotCanonical.into());
    }

    if cancelled {
        return Err(SettlementError::OrderCancelled.into());
    }

    if now > i64::from(intent.valid_to) {
        return Err(SettlementError::OrderExpired.into());
    }

    // The sell token account must be the one named in the intent, owned by
    // the intent owner: an order can only sell funds its own owner controls.
    if !address_matches_pubkey(sell_token_account.address(), &intent.sell_token_account) {
        return Err(SettlementError::SellTokenAccountMismatch.into());
    }
    // Assert the order intent owner matches that of the sell token account.
    {
        // `from_account_view` confirms this is a real SPL token account
        // (right length, owned by the token program) before we read its
        // owner. The borrow it holds is released at the end of this block,
        // before the transfers below touch the same account.
        let token_account = TokenAccount::from_account_view(sell_token_account)
            .map_err(|_| SettlementError::SellTokenAccountInvalid)?;
        if !address_matches_pubkey(token_account.owner(), &intent.owner) {
            return Err(SettlementError::SellTokenOwnerMismatch.into());
        }
    }

    // Pull the configured amounts out of the sell token account. The state
    // PDA is the SPL delegate, so it signs each transfer via `signer`.
    for (destination, amount) in destinations.iter().zip(amounts) {
        Transfer::new(
            sell_token_account,
            destination,
            state_account,
            u64::from_le_bytes(*amount),
        )
        .invoke_signed(core::slice::from_ref(state_pda_signer))?;
    }

    Ok(())
}

fn address_matches_pubkey(address: &Address, pubkey: &Pubkey) -> bool {
    address.as_array() == &pubkey.to_bytes()
}
