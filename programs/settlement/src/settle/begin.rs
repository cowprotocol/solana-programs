//! `BeginSettle` instruction handler.

use std::ops::Deref;

use pinocchio::{
    cpi::Signer,
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
    data::{
        intent::{OrderIntent, OrderKind},
        order::{EncodedOrderAccount, OrderAccount},
    },
    instruction::{
        settle::{
            finalize_push_amounts, BeginSettleInput, SettledOrder, SettledOrders,
            FINALIZE_FIXED_ACCOUNTS,
        },
        InstructionInputParsing,
    },
    recover_discriminator, Pubkey, SettlementError, SettlementInstruction,
};

use crate::processor::is_cpi_call;

use super::{validate_counterpart, validate_token_program_account, with_state_pda_signer};

pub fn process_begin_settle(
    program_id: &Address,
    accounts: &mut [AccountView],
    instruction_data: &[u8],
) -> ProgramResult {
    if is_cpi_call() {
        return Err(SettlementError::CalledViaCpi.into());
    }

    let mut input = BeginSettleInput::parse(instruction_data, accounts)?;

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

    validate_token_program_account(input.token_program_account)?;

    with_state_pda_signer(program_id, input.state_pda_account, |state_pda_signer| {
        settle_orders(
            program_id,
            input.state_pda_account,
            state_pda_signer,
            &mut input.orders,
            &finalize_ix,
        )
    })
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

/// The paired pushes `BeginSettle` settles against: each push's destination
/// (read from the finalize's account metas) with the amount it pays in (read
/// from the finalize's instruction data), in push order.
fn finalize_pushes<'a>(
    finalize_ix: &'a IntrospectedInstruction<'a>,
) -> Result<impl Iterator<Item = (&'a Address, u64)>, ProgramError> {
    let amounts = finalize_push_amounts(finalize_ix.get_instruction_data())?;
    Ok(
        push_destinations(finalize_ix)
            .zip(amounts.iter().map(|amount| u64::from_le_bytes(*amount))),
    )
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

/// Validate each order against its push, and pull user funds, signing the pulls
/// as the canonical state PDA (the user's delegate). Orders must be strictly
/// increasing by address, which rejects duplicates.
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
fn settle_orders(
    program_id: &Address,
    state_pda_account: &AccountView,
    state_pda_signer: &Signer,
    orders: &mut SettledOrders,
    finalize_ix: &IntrospectedInstruction,
) -> ProgramResult {
    // Orders must be passed strictly increasing by address; this rejects
    // duplicates (settling the same order twice) without a separate scan.
    let mut previous: Option<Address> = None;

    let now = Clock::get()?.unix_timestamp;

    // Pull one push (destination and amount) per order; running out mid-loop
    // means fewer pushes than orders. A leftover push (more pushes than orders)
    // is caught after.
    let mut pushes = finalize_pushes(finalize_ix)?;

    for order in orders.iter_mut() {
        let order_pda_address = *order.order_pda.address();
        if previous.is_some_and(|previous| order_pda_address <= previous) {
            return Err(SettlementError::OrdersNotStrictlyIncreasing.into());
        }
        previous = Some(order_pda_address);

        let push = pushes
            .next()
            .ok_or(SettlementError::SettledOrderPushCountMismatch)?;

        process_order(
            program_id,
            order,
            push,
            now,
            state_pda_account,
            state_pda_signer,
        )?;
    }

    if pushes.next().is_some() {
        return Err(SettlementError::SettledOrderPushCountMismatch.into());
    }

    Ok(())
}

/// Validate a single order, process its pulls, and confirm its push pays it.
/// This checks that the order is valid, settleable, and that `push_destination`
/// matches the buy token account. Once the order passes those checks, its pulls
/// are executed and its settlement limit price is validated against the intent.
#[must_use = "ignoring the output may lead to an unintended on-chain state"]
fn process_order(
    program_id: &Address,
    order: SettledOrder<'_>,
    (push_destination, push_amount): (&Address, u64),
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
    let account = OrderAccount::load_from_pda(order_pda, program_id, bump)?;
    let intent = &account.intent;

    if account.cancelled {
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

    // Pull the configured amounts out of the sell token account, summing them
    // into `amount_in` as we go. The state PDA is the SPL delegate, so it signs
    // each transfer via `signer`.
    let mut amount_in: u64 = 0;
    for (destination, amount) in destinations.iter().zip(amounts) {
        let amount = u64::from_le_bytes(*amount);
        amount_in = amount_in
            .checked_add(amount)
            .ok_or(SettlementError::PullAmountOverflow)?;
        Transfer::new(sell_token_account, destination, state_account, amount)
            .invoke_signed(core::slice::from_ref(state_pda_signer))?;
    }

    validate_limit_price(intent, amount_in, push_amount)?;
    let (amount_withdrawn, amount_received) = validated_final_amounts(
        intent,
        account.amount_withdrawn,
        account.amount_received,
        amount_in,
        push_amount,
    )?;

    let updated: [u8; EncodedOrderAccount::SIZE] = EncodedOrderAccount::from(OrderAccount {
        amount_withdrawn,
        amount_received,
        ..account
    })
    .into();
    order_pda.try_borrow_mut()?.copy_from_slice(&updated);

    Ok(())
}

/// Check a settlement's executed price for one order against its limit, using
/// the total pulled from the sell account (`amount_in`) and what the paired push
/// pays in (`amount_out`). The check is local, so a favorable price in another
/// settlement never excuses a bad one here.
#[must_use = "ignoring the output may lead to an unintended on-chain state"]
fn validate_limit_price(
    intent: &OrderIntent,
    amount_in: u64,
    amount_out: u64,
) -> Result<(), SettlementError> {
    // Limit price: the executed price must be at least the order's limit,
    //   amount_out / amount_in >= buy_amount / sell_amount,
    // rearranged division-free to avoid rounding.
    // Every factor is a `u64`, so each product is at most `u64::MAX^2 < u128::MAX`;
    // the `expect`s document that the widening multiplication can never overflow.
    let lhs = u128::from(amount_out)
        .checked_mul(u128::from(intent.sell_amount))
        .expect("u64 * u64 always fits in u128");
    let rhs = u128::from(intent.buy_amount)
        .checked_mul(u128::from(amount_in))
        .expect("u64 * u64 always fits in u128");
    if lhs < rhs {
        return Err(SettlementError::LimitPriceViolated);
    }

    Ok(())
}

/// Fold this settlement's `amount_in`/`amount_out` into the order's stored
/// cumulative totals `amount_withdrawn`/`amount_received` and check the result,
/// returning the updated totals to persist. A `u64` overflow of either total is
/// rejected, despite this in exceptional circumstances being reasonbable.
/// The order never fills beyond its (`Sell`: sold; `Buy`: bought) amount; and a
/// non-`partially_fillable` order must be filled completely. The other side is
/// bounded by the limit price.
fn validated_final_amounts(
    intent: &OrderIntent,
    amount_withdrawn: u64,
    amount_received: u64,
    amount_in: u64,
    amount_out: u64,
) -> Result<(u64, u64), SettlementError> {
    let amount_withdrawn = amount_withdrawn
        .checked_add(amount_in)
        .ok_or(SettlementError::AmountWithdrawnOverflow)?;
    let amount_received = amount_received
        .checked_add(amount_out)
        .ok_or(SettlementError::AmountReceivedOverflow)?;

    let (filled, order_amount) = match intent.kind {
        OrderKind::Sell => (amount_withdrawn, intent.sell_amount),
        OrderKind::Buy => (amount_received, intent.buy_amount),
    };
    if filled > order_amount {
        return Err(SettlementError::FillExceedsOrderAmount);
    }
    if !intent.partially_fillable && filled != order_amount {
        return Err(SettlementError::OrderNotFullyFilled);
    }

    Ok((amount_withdrawn, amount_received))
}

fn address_matches_pubkey(address: &Address, pubkey: &Pubkey) -> bool {
    address.as_array() == &pubkey.to_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use settlement_interface::data::intent::fixtures::{arb_order_intent, sample_intent};
    use settlement_interface::instruction::fixtures::fake_account;
    use settlement_interface::instruction::settle::fixtures::arb_pushes;
    use settlement_interface::instruction::settle::{FinalizeSettle, FinalizeSettleInput};
    use settlement_interface::instruction::InstructionInputParsing;
    use solana_instruction::{BorrowedAccountMeta, BorrowedInstruction, Instruction};

    /// The largest value any amount can take on-chain (an SPL amount is a `u64`).
    const MAX: u64 = u64::MAX;

    fn intent_with(
        kind: OrderKind,
        partially_fillable: bool,
        sell_amount: u64,
        buy_amount: u64,
    ) -> OrderIntent {
        OrderIntent {
            sell_amount,
            buy_amount,
            ..sample_intent(kind, partially_fillable)
        }
    }

    /// A `validate_limit_price` scenario: the amount pulled from the sell
    /// account (`a_in`) and paid in by the push (`a_out`), plus the order's
    /// limit (`sell`, `buy`).
    struct Case {
        a_in: u64,
        a_out: u64,
        sell: u64,
        buy: u64,
    }

    /// Concrete settlements whose executed price meets or beats the order's
    /// limit, so the check accepts them.
    #[test]
    fn accepts_prices_at_or_above_limit() {
        #[rustfmt::skip]
        let cases = [
            // Full fill exactly at the limit price.
            Case { a_in: 1_000, a_out: 2_000, sell: 1_000, buy: 2_000 },
            // Full fill paying one token above the required amount.
            Case { a_in: 1_000, a_out: 2_001, sell: 1_000, buy: 2_000 },
            // Partial fill exactly at the limit price.
            Case { a_in: 500, a_out: 1_000, sell: 1_000, buy: 2_000 },
            // Taking in one token less.
            Case { a_in: 999, a_out: 2_000, sell: 1_000, buy: 2_000 },
            // Nothing pulled, nothing paid.
            Case { a_in: 0, a_out: 0, sell: 1_000, buy: 2_000 },
            // User gets free money!
            Case { a_in: 0, a_out: 1_337, sell: 1_000, buy: 2_000 },
            // Donation by the user.
            Case { a_in: 0, a_out: 1_337, sell: 1_337, buy: 0 },
            // The largest products the check can form, `u64::MAX * u64::MAX`.
            Case { a_in: MAX, a_out: MAX, sell: MAX, buy: MAX },
            // Pulling one token less at the maximal price beats the limit.
            Case { a_in: MAX - 1, a_out: MAX, sell: MAX, buy: MAX },
        ];

        for Case {
            a_in,
            a_out,
            sell,
            buy,
        } in cases
        {
            let intent = intent_with(OrderKind::Sell, true, sell, buy);
            assert_eq!(
                validate_limit_price(&intent, a_in, a_out),
                Ok(()),
                "in={a_in} out={a_out} sell={sell} buy={buy}",
            );
        }
    }

    /// Concrete settlements whose executed price falls below the order's limit,
    /// so the check rejects them.
    #[test]
    fn rejects_prices_below_limit() {
        #[rustfmt::skip]
        let cases = [
            // Full fill paying one token less than required.
            Case { a_in: 1_000, a_out: 1_999, sell: 1_000, buy: 2_000 },
            // Partial fill paying one token less than required.
            Case { a_in: 500, a_out: 999, sell: 1_000, buy: 2_000 },
            // Tokens pulled but nothing paid in return.
            Case { a_in: 1, a_out: 0, sell: 1_000, buy: 2_000 },
            // Price far below the limit.
            Case { a_in: 1, a_out: 1, sell: 1, buy: 1_000_000 },
            // User wants free money, can't take funds from order.
            Case { a_in: 1, a_out: 1_337, sell: 0, buy: MAX },
            // Straight-out stealing.
            Case { a_in: 42, a_out: 0, sell: 31_337, buy: 31_337 },
            // Paying one token less than the maximal amount must be caught,
            // not wrap around to acceptance.
            Case { a_in: MAX, a_out: MAX - 1, sell: MAX, buy: MAX },
        ];

        for Case {
            a_in,
            a_out,
            sell,
            buy,
        } in cases
        {
            let intent = intent_with(OrderKind::Sell, true, sell, buy);
            assert_eq!(
                validate_limit_price(&intent, a_in, a_out),
                Err(SettlementError::LimitPriceViolated),
                "in={a_in} out={a_out} sell={sell} buy={buy}",
            );
        }
    }

    /// A `validated_final_amounts` scenario: the order's prior stored fill (`withdrawn`
    /// sold, `received` bought), this settlement's `amount_in`/`amount_out`, and
    /// the order's `sell`/`buy` amounts, `kind`, and fillability.
    struct FillCase {
        withdrawn: u64,
        received: u64,
        amount_in: u64,
        amount_out: u64,
        sell: u64,
        buy: u64,
        kind: OrderKind,
        partially_fillable: bool,
    }

    /// Settlements the check accepts, returning the folded totals: within the
    /// exact side's amount, and a fill-or-kill order filled completely.
    #[test]
    fn accepts_fills_within_amounts() {
        let cases = [
            // Fresh partial sell fill, within the sell amount.
            FillCase {
                withdrawn: 0,
                received: 0,
                amount_in: 500,
                amount_out: 1_000,
                sell: 1_000,
                buy: 2_000,
                kind: OrderKind::Sell,
                partially_fillable: true,
            },
            // A second fill completing the sell order.
            FillCase {
                withdrawn: 500,
                received: 1_000,
                amount_in: 500,
                amount_out: 1_000,
                sell: 1_000,
                buy: 2_000,
                kind: OrderKind::Sell,
                partially_fillable: true,
            },
            // A `Sell` order tracks buy proceeds but isn't capped by them.
            FillCase {
                withdrawn: 0,
                received: 0,
                amount_in: 500,
                amount_out: 9_000,
                sell: 1_000,
                buy: 2_000,
                kind: OrderKind::Sell,
                partially_fillable: true,
            },
            // Fill-or-kill sell, filled exactly in one settlement.
            FillCase {
                withdrawn: 0,
                received: 0,
                amount_in: 1_000,
                amount_out: 2_000,
                sell: 1_000,
                buy: 2_000,
                kind: OrderKind::Sell,
                partially_fillable: false,
            },
            // Partial buy fill, within the buy amount.
            FillCase {
                withdrawn: 0,
                received: 0,
                amount_in: 1_000,
                amount_out: 500,
                sell: 2_000,
                buy: 1_000,
                kind: OrderKind::Buy,
                partially_fillable: true,
            },
            // Fill-or-kill buy, filled exactly.
            FillCase {
                withdrawn: 0,
                received: 0,
                amount_in: 2_000,
                amount_out: 1_000,
                sell: 2_000,
                buy: 1_000,
                kind: OrderKind::Buy,
                partially_fillable: false,
            },
            // A no-op settlement on a partially fillable order.
            FillCase {
                withdrawn: 0,
                received: 0,
                amount_in: 0,
                amount_out: 0,
                sell: 1_000,
                buy: 2_000,
                kind: OrderKind::Sell,
                partially_fillable: true,
            },
            // Folding up to the maximal total without overflowing.
            FillCase {
                withdrawn: MAX - 1,
                received: 0,
                amount_in: 1,
                amount_out: 0,
                sell: MAX,
                buy: 1,
                kind: OrderKind::Sell,
                partially_fillable: false,
            },
            // A degenerate zero-amount fill-or-kill order is trivially filled by
            // a zero settlement (`filled == order_amount == 0`).
            FillCase {
                withdrawn: 0,
                received: 0,
                amount_in: 0,
                amount_out: 0,
                sell: 0,
                buy: 0,
                kind: OrderKind::Sell,
                partially_fillable: false,
            },
            // Sanity checks: this function only bounds the order's exact side, so
            // the other side isn't capped and could reach `u64::MAX`: a `Sell`
            // order never checks the buy side, a `Buy` order never checks the sell
            // side. A real settlement's limit price rules these out, so they
            // aren't expected to ever be relevant in an actual settlement.
            FillCase {
                withdrawn: 0,
                received: 0,
                amount_in: 1_000,
                amount_out: MAX,
                sell: 1_000,
                buy: 2_000,
                kind: OrderKind::Sell,
                partially_fillable: true,
            },
            FillCase {
                withdrawn: 0,
                received: 0,
                amount_in: MAX,
                amount_out: 1_000,
                sell: 2_000,
                buy: 1_000,
                kind: OrderKind::Buy,
                partially_fillable: true,
            },
        ];

        for FillCase {
            withdrawn,
            received,
            amount_in,
            amount_out,
            sell,
            buy,
            kind,
            partially_fillable,
        } in cases
        {
            let intent = intent_with(kind, partially_fillable, sell, buy);
            let expected = (
                withdrawn.checked_add(amount_in).expect("no overflow"),
                received.checked_add(amount_out).expect("no overflow"),
            );
            assert_eq!(
                validated_final_amounts(&intent, withdrawn, received, amount_in, amount_out),
                Ok(expected),
                "withdrawn={withdrawn} received={received} in={amount_in} out={amount_out} sell={sell} buy={buy} kind={kind:?} pf={partially_fillable}",
            );
        }
    }

    /// Settlements the check rejects: the exact side over its amount, a fill-or-
    /// kill order left partially filled, and a cumulative total that overflows.
    #[test]
    fn rejects_overfill_incomplete_fill_or_kill_or_overflow() {
        use SettlementError::{
            AmountReceivedOverflow, AmountWithdrawnOverflow, FillExceedsOrderAmount,
            OrderNotFullyFilled,
        };
        let cases = [
            // Sell one token over the sell amount.
            (
                FillCase {
                    withdrawn: 0,
                    received: 0,
                    amount_in: 1_001,
                    amount_out: 0,
                    sell: 1_000,
                    buy: 2_000,
                    kind: OrderKind::Sell,
                    partially_fillable: true,
                },
                FillExceedsOrderAmount,
            ),
            // Buy one token over the buy amount.
            (
                FillCase {
                    withdrawn: 0,
                    received: 0,
                    amount_in: 0,
                    amount_out: 1_001,
                    sell: 2_000,
                    buy: 1_000,
                    kind: OrderKind::Buy,
                    partially_fillable: true,
                },
                FillExceedsOrderAmount,
            ),
            // A second fill pushing the cumulative sell past the sell amount.
            (
                FillCase {
                    withdrawn: 601,
                    received: 0,
                    amount_in: 400,
                    amount_out: 0,
                    sell: 1_000,
                    buy: 2_000,
                    kind: OrderKind::Sell,
                    partially_fillable: true,
                },
                FillExceedsOrderAmount,
            ),
            // A second fill pushing the cumulative buy past the buy amount.
            (
                FillCase {
                    withdrawn: 0,
                    received: 601,
                    amount_in: 0,
                    amount_out: 400,
                    sell: 2_000,
                    buy: 1_000,
                    kind: OrderKind::Buy,
                    partially_fillable: true,
                },
                FillExceedsOrderAmount,
            ),
            // Fill-or-kill sell left partially filled.
            (
                FillCase {
                    withdrawn: 0,
                    received: 0,
                    amount_in: 999,
                    amount_out: 2_000,
                    sell: 1_000,
                    buy: 2_000,
                    kind: OrderKind::Sell,
                    partially_fillable: false,
                },
                OrderNotFullyFilled,
            ),
            // Fill-or-kill buy left partially filled.
            (
                FillCase {
                    withdrawn: 0,
                    received: 0,
                    amount_in: 2_000,
                    amount_out: 999,
                    sell: 2_000,
                    buy: 1_000,
                    kind: OrderKind::Buy,
                    partially_fillable: false,
                },
                OrderNotFullyFilled,
            ),
            // Fill-or-kill order not filled at all.
            (
                FillCase {
                    withdrawn: 0,
                    received: 0,
                    amount_in: 0,
                    amount_out: 0,
                    sell: 1_000,
                    buy: 2_000,
                    kind: OrderKind::Sell,
                    partially_fillable: false,
                },
                OrderNotFullyFilled,
            ),
            // A degenerate zero sell-amount order can't have anything pulled.
            (
                FillCase {
                    withdrawn: 0,
                    received: 0,
                    amount_in: 1,
                    amount_out: 0,
                    sell: 0,
                    buy: 1_000,
                    kind: OrderKind::Sell,
                    partially_fillable: true,
                },
                FillExceedsOrderAmount,
            ),
            // A degenerate zero buy-amount order can't have anything delivered.
            (
                FillCase {
                    withdrawn: 0,
                    received: 0,
                    amount_in: 0,
                    amount_out: 1,
                    sell: 1_000,
                    buy: 0,
                    kind: OrderKind::Buy,
                    partially_fillable: true,
                },
                FillExceedsOrderAmount,
            ),
            // The cumulative withdrawn total overflows a `u64`.
            (
                FillCase {
                    withdrawn: MAX,
                    received: 0,
                    amount_in: 1,
                    amount_out: 0,
                    sell: 42,
                    buy: 1337,
                    kind: OrderKind::Sell,
                    partially_fillable: true,
                },
                AmountWithdrawnOverflow,
            ),
            // The cumulative received total overflows a `u64`.
            (
                FillCase {
                    withdrawn: 0,
                    received: MAX,
                    amount_in: 0,
                    amount_out: 1,
                    sell: 42,
                    buy: 1337,
                    kind: OrderKind::Sell,
                    partially_fillable: true,
                },
                AmountReceivedOverflow,
            ),
        ];

        for (
            FillCase {
                withdrawn,
                received,
                amount_in,
                amount_out,
                sell,
                buy,
                kind,
                partially_fillable,
            },
            error,
        ) in cases
        {
            let intent = intent_with(kind, partially_fillable, sell, buy);
            assert_eq!(
                validated_final_amounts(&intent, withdrawn, received, amount_in, amount_out),
                Err(error),
                "withdrawn={withdrawn} received={received} in={amount_in} out={amount_out} sell={sell} buy={buy} kind={kind:?} pf={partially_fillable}",
            );
        }
    }

    proptest! {
        /// The limit-price check never panics for any intent and any `u64`
        /// amounts: each product is `u64 * u64`, always within `u128`.
        #[test]
        fn validate_limit_price_never_panics(
            intent in arb_order_intent(),
            amount_in in any::<u64>(),
            push_amount in any::<u64>(),
        ) {
            let _ = validate_limit_price(&intent, amount_in, push_amount);
        }
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
        /// `BeginSettle` settles against a paired `FinalizeSettle`'s pushes via
        /// `finalize_pushes`: each destination (from the account metas) paired
        /// with its amount (from the instruction data). For any well-formed
        /// finalize those pairs must match both the builder's inputs and what
        /// `FinalizeSettleInput` parses from the same instruction.
        #[test]
        fn finalize_pushes_matches_parser(
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

            let introspected_instruction = introspected_instruction(&ix);
            let introspected: Vec<(Address, u64)> = finalize_pushes(&introspected_instruction)
                .expect("well-formed finalize data")
                .map(|(&destination, amount)| (destination, amount))
                .collect();

            let mut accounts: Vec<AccountView> =
                ix.accounts.iter().map(|account| fake_account(account.pubkey)).collect();
            let parsed_raw = FinalizeSettleInput::parse(&ix.data, &mut accounts)
                .expect("a well-formed finalize parses");
            let parsed: Vec<(Address, u64)> = parsed_raw
                .pushes
                .iter()
                .map(|push| (*push.destination.address(), u64::from_le_bytes(*push.amount)))
                .collect();

            // The builder's inputs, the ground truth both views should recover.
            let expected: Vec<(Address, u64)> = destinations
                .iter()
                .map(|destination| Address::new_from_array(destination.to_bytes()))
                .zip(amounts.iter().copied())
                .collect();

            prop_assert_eq!(&introspected, &expected);
            prop_assert_eq!(&parsed, &expected);
        }
    }
}
