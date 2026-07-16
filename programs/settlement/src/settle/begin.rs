//! `BeginSettle` instruction handler.

use std::ops::Deref;

use pinocchio::{
    cpi::{Seed, Signer},
    error::ProgramError,
    sysvars::{
        clock::Clock,
        instructions::{Instructions, IntrospectedInstruction},
        Sysvar,
    },
    AccountView, Address, ProgramResult,
};
use pinocchio_token::{instructions::Transfer, state::Account as TokenAccount};
use settlement_interface::{
    data::order::OrderAccount,
    instruction::{
        create_buffer::SPL_TOKEN_PROGRAM_ID,
        settle::{BeginSettleInput, SettledOrder, FINALIZE_FIXED_ACCOUNTS},
        InstructionInputParsing,
    },
    pda::state::state_pda_seeds,
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

    let finalize_ix = instructions.load_instruction_at(usize::from(input.finalize_ix_index))?;

    settle_orders(
        program_id,
        input.token_program_account,
        input.state_pda_account,
        input.orders.iter(),
        &finalize_ix,
    )?;

    Ok(())
}

/// The destination address of each push carried by the paired `FinalizeSettle`,
/// seen through instruction introspection, in order.
///
/// The push structure isn't validated here: the paired `FinalizeSettle` re-parses
/// the same instruction from its own data and rejects a dangling source buffer or
/// a push count that disagrees with its accounts. The caller pairs these
/// destinations with the settled orders one-to-one, which is what catches a count
/// mismatch.
fn push_destinations<'a>(
    instruction: &'a IntrospectedInstruction<'a>,
) -> impl Iterator<Item = &'a Address> {
    // Each push occupies a `[source_buffer, destination]` meta pair after the
    // fixed accounts, so the destinations are every second meta beginning at the
    // first push's destination.
    (FINALIZE_FIXED_ACCOUNTS + 1..instruction.num_account_metas())
        .step_by(2)
        .map(|destination_index| {
            // The index stays below `num_account_metas`, so the lookup, whose only
            // error is an out-of-bounds index, always succeeds.
            &instruction
                .get_instruction_account_at(destination_index)
                .expect("index within num_account_metas")
                .key
        })
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

/// Validate each order against its push, and pull user funds. This requires:
/// - the legacy SPL Token program;
/// - the canonical state PDA, which signs each transfer as the user's delegate;
/// - orders strictly increasing by address, rejecting duplicates.
///
/// Each order is paid by exactly one push. The orders and the finalize's pushes
/// are both laid out sorted by order PDA, so order `i` is paid by push `i`, and
/// that push's destination must be order `i`'s buy token account. Pairing them in
/// a single pass (one push consumed per order, with none left over) rejects any
/// count mismatch without counting the orders up front.
///
/// Further validation and the actual pulls are processed through
/// [`process_order`].
#[must_use = "ignoring the output may lead to an unintended on-chain state"]
fn settle_orders<'a>(
    program_id: &Address,
    token_program_account: &AccountView,
    state_pda_account: &AccountView,
    orders: impl IntoIterator<Item = SettledOrder<'a>>,
    finalize_ix: &IntrospectedInstruction,
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

    // Pull one push destination per order; running out mid-loop means fewer pushes
    // than orders. A leftover push (more pushes than orders) is caught after.
    let mut destinations = push_destinations(finalize_ix);

    for order in orders {
        let order_pda = order.order_pda;
        if previous.is_some_and(|previous| order_pda.address() <= previous) {
            return Err(SettlementError::OrdersNotStrictlyIncreasing.into());
        }
        previous = Some(order_pda.address());

        let push_destination = destinations
            .next()
            .ok_or(SettlementError::SettledOrderPushCountMismatch)?;

        process_order(
            program_id,
            order,
            push_destination,
            now,
            state_pda_account,
            &state_pda_signer,
        )?;
    }

    if destinations.next().is_some() {
        return Err(SettlementError::SettledOrderPushCountMismatch.into());
    }

    Ok(())
}

/// Validate a single order, process its pulls, and confirm its push pays it.
/// This checks that the order is valid, settleable, and that `push_destination`
/// matches the buy token account. Once the order passes those checks, its pulls
/// are executed.
#[must_use = "ignoring the output may lead to an unintended on-chain state"]
fn process_order(
    program_id: &Address,
    order: SettledOrder<'_>,
    push_destination: &Address,
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

    // Decode the order body and prove its provenance: `load_from_pda` checks
    // that `order_pda` is the canonical order PDA for the intent it stores.
    let OrderAccount {
        cancelled, intent, ..
    } = OrderAccount::load_from_pda(order_pda, program_id, bump)?;

    if cancelled {
        return Err(SettlementError::OrderCancelled.into());
    }

    if now > i64::from(intent.valid_to) {
        return Err(SettlementError::OrderExpired.into());
    }

    // The push paying this order must send to the order's buy token account.
    if !address_matches_pubkey(push_destination, &intent.buy_token_account) {
        return Err(SettlementError::PushDestinationMismatch.into());
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
            u64::from_be_bytes(*amount),
        )
        .invoke_signed(core::slice::from_ref(state_pda_signer))?;
    }

    Ok(())
}

fn address_matches_pubkey(address: &Address, pubkey: &Pubkey) -> bool {
    address.as_array() == &pubkey.to_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use settlement_interface::instruction::fixtures::fake_account;
    use settlement_interface::instruction::settle::{FinalizeSettle, FinalizeSettleInput};
    use settlement_interface::instruction::InstructionInputParsing;
    use solana_instruction::{BorrowedAccountMeta, BorrowedInstruction, Instruction};

    /// Strategy producing `count` random pushes as the parallel
    /// `(source_buffers, destinations, bumps, amounts)` lists the `FinalizeSettle`
    /// builder takes.
    fn arb_pushes(
        count: impl Into<prop::collection::SizeRange>,
    ) -> impl Strategy<Value = (Vec<Pubkey>, Vec<Pubkey>, Vec<u8>, Vec<u64>)> {
        prop::collection::vec(
            (
                any::<[u8; 32]>().prop_map(Pubkey::new_from_array),
                any::<[u8; 32]>().prop_map(Pubkey::new_from_array),
                any::<u8>(),
                any::<u64>(),
            ),
            count,
        )
        .prop_map(|pushes| {
            let source_buffers = pushes.iter().map(|&(source, ..)| source).collect();
            let destinations = pushes.iter().map(|&(_, dest, ..)| dest).collect();
            let bumps = pushes.iter().map(|&(.., bump, _)| bump).collect();
            let amounts = pushes.iter().map(|&(.., amount)| amount).collect();
            (source_buffers, destinations, bumps, amounts)
        })
    }

    /// Encode `ix` as the introspected instruction from the instructions
    /// sysvar.
    fn introspected_instruction(ix: &Instruction) -> IntrospectedInstruction<'static> {
        // From the Solana docs for `BorrowedInstruction`: "This struct is
        // used by the runtime when constructing the instructions sysvar."
        let borrowed = BorrowedInstruction {
            program_id: &ix.program_id,
            accounts: ix
                .accounts
                .iter()
                .map(|meta| BorrowedAccountMeta {
                    pubkey: &meta.pubkey,
                    is_signer: meta.is_signer,
                    is_writable: meta.is_writable,
                })
                .collect(),
            data: &ix.data,
        };
        // From the Solana docs for this function: "construct the account data
        // for the instructions sysvar."
        let instructions_sysvar_data =
            solana_instructions_sysvar::construct_instructions_data(&[borrowed]);
        // SAFETY: from Pinocchio's docs for `new_unchecked`: "this function is
        // unsafe because it does not check if the provided data is from the
        // Sysvar Account."
        // We built the data using `construct_instructions_data`, so we know the
        // data is correctly built.
        // https://docs.rs/pinocchio/0.11.1/pinocchio/sysvars/instructions/struct.Instructions.html#method.new_unchecked
        // https://docs.rs/solana-instructions-sysvar/3.0.0/src/solana_instructions_sysvar/lib.rs.html#85-141
        let instructions = unsafe { Instructions::new_unchecked(instructions_sysvar_data) };
        // Leak the buffer so the returned view can borrow it for the rest of the
        // test process.
        let instructions: &'static Instructions<Vec<u8>> = Box::leak(Box::new(instructions));
        instructions
            .load_instruction_at(0)
            .expect("the finalize is the only instruction, at index 0")
    }

    proptest! {
        /// `BeginSettle` reads a paired `FinalizeSettle`'s push destinations by
        /// introspection through `push_destinations`, while `FinalizeSettle`
        /// reads its own pushes via `FinalizeSettleInput` (off the instruction
        /// data + accounts).
        /// For any well-formed finalize the two must recover the same push
        /// count and the same destination for every push.
        #[test]
        fn push_destinations_output_matches_finalize_parser(
            program_id in any::<[u8; 32]>(),
            state_pda in any::<[u8; 32]>(),
            begin_ix_index in any::<u16>(),
            (source_buffers, destinations, bumps, amounts) in arb_pushes(0..=16usize),
        ) {
            let ix = Instruction::from(FinalizeSettle {
                program_id: Pubkey::new_from_array(program_id),
                state_pda: Pubkey::new_from_array(state_pda),
                begin_ix_index,
                source_buffers: &source_buffers,
                destinations: &destinations,
                bumps: &bumps,
                amounts: &amounts,
            });

            let introspected = introspected_instruction(&ix);
            let introspected_destinations: Vec<Address> =
                push_destinations(&introspected).copied().collect();

            let mut accounts: Vec<AccountView> =
                ix.accounts.iter().map(|account| fake_account(account.pubkey)).collect();
            let parsed = FinalizeSettleInput::parse(&ix.data, &mut accounts)
                .expect("a well-formed finalize parses");
            let parsed_destinations: Vec<Address> =
                parsed.pushes.iter().map(|push| *push.destination.address()).collect();

            prop_assert_eq!(&introspected_destinations, &destinations);
            prop_assert_eq!(&parsed_destinations, &destinations);
        }
    }
}
