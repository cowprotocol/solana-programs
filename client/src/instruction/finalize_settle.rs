//! Builder for the `FinalizeSettle` instruction.

use cow_settlement_interface::{
    data::intent::OrderIntent,
    pda::{buffer::find_buffer_pda, order::find_order_pda, state::find_state_pda},
    Instruction, Pubkey,
};

/// A settled order whose proceeds are pushed to it: `intent` identifies the
/// order (its `buy_token_account` is the push destination and its `buy_mint`
/// selects the canonical source buffer) and `amount` is the quantity to push.
pub struct FinalizedIntent<'a> {
    pub intent: &'a OrderIntent,
    pub amount: u64,
}

/// Builder for a `FinalizeSettle` instruction pushing each order's proceeds to
/// its buy token account.
///
/// The destination is the order intent's `buy_token_account` and the source is
/// the canonical buffer PDA for its `buy_mint` (see [`find_buffer_pda`]), the
/// only buffer `BeginSettle` accepts as the source of that order's push. The
/// orders are sorted by their canonical order PDA (the same key
/// [`BeginSettle`](super::begin_settle::BeginSettle) orders its settled-order
/// list by) so the two instructions present the orders
/// in the same order and their lists line up.
pub struct FinalizeSettle<'a> {
    pub program_id: Pubkey,
    pub begin_ix_index: u16,
    pub orders: &'a [FinalizedIntent<'a>],
}

impl From<FinalizeSettle<'_>> for Instruction {
    fn from(builder: FinalizeSettle<'_>) -> Self {
        // Sort the orders by their canonical order PDA, the key `BeginSettle`
        // lays its settled orders out by, so the two instruction lists align.
        // For BeginSettle, sorting can take place in the interface. But the
        // order PDAs don't appear in the actual FinalizeSettle instruction, so
        // the sorting can only happen here.
        let num_orders = builder.orders.len();
        let mut orders: Vec<usize> = (0..num_orders).collect();
        orders.sort_by_key(|&i| {
            find_order_pda(&builder.program_id, &builder.orders[i].intent.uid()).0
        });

        let mut source_buffers: Vec<Pubkey> = Vec::with_capacity(num_orders);
        let mut destinations = Vec::with_capacity(num_orders);
        let mut bumps = Vec::with_capacity(num_orders);
        let mut amounts = Vec::with_capacity(num_orders);
        for &i in &orders {
            let (buffer_pda, bump) =
                find_buffer_pda(&builder.program_id, &builder.orders[i].intent.buy_mint);
            source_buffers.push(buffer_pda);
            destinations.push(builder.orders[i].intent.buy_token_account);
            bumps.push(bump);
            amounts.push(builder.orders[i].amount);
        }
        let (state_pda, _bump) = find_state_pda(&builder.program_id);
        cow_settlement_interface::instruction::settle::FinalizeSettle {
            program_id: builder.program_id,
            state_pda,
            begin_ix_index: builder.begin_ix_index,
            source_buffers: &source_buffers,
            destinations: &destinations,
            bumps: &bumps,
            amounts: &amounts,
        }
        .into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::proptest::{prelude::*, test_runner::TestCaseError};
    use cow_settlement_interface::{
        data::intent::fixtures::arb_order_intent,
        fixtures::pubkey_from_seed,
        instruction::{
            fixtures::fake_account_from_array,
            settle::{FinalizeSettleInput, INSTRUCTIONS_SYSVAR_ID, SPL_TOKEN_PROGRAM_ID},
            InstructionInputParsing,
        },
    };

    proptest! {
        // `FinalizeSettle` derives each order's source buffer from its buy mint
        // and destination from the intent, sorting by canonical order PDA like
        // `BeginSettle` so the on-chain parser recovers exactly those pushes in
        // that order.
        #[test]
        fn finalize_settle_derives_buffers_from_mints(
            begin_ix_index in any::<u16>(),
            cases in prop::collection::vec(
                (arb_order_intent(), any::<u64>()),
                1..=5,
            ),
        ) {
            let program_id = pubkey_from_seed("program id");
            let orders: Vec<FinalizedIntent> = cases
                .iter()
                .map(|(intent, amount)| FinalizedIntent {
                    intent,
                    amount: *amount,
                })
                .collect();
            let ix = Instruction::from(FinalizeSettle {
                program_id,
                begin_ix_index,
                orders: &orders,
            });

            // Expected pushes: each order's buffer PDA (and its canonical bump),
            // buy token account, and amount, sorted by the order's canonical PDA
            // (the builder's order).
            struct ExpectedPush {
                order_pda: Pubkey,
                buffer: Pubkey,
                bump: u8,
                destination: Pubkey,
                amount: u64,
            }
            let mut expected: Vec<ExpectedPush> = orders
                .iter()
                .map(|order| {
                    let (order_pda, _bump) = find_order_pda(&program_id, &order.intent.uid());
                    let (buffer, bump) = find_buffer_pda(&program_id, &order.intent.buy_mint);
                    ExpectedPush {
                        order_pda,
                        buffer,
                        bump,
                        destination: order.intent.buy_token_account,
                        amount: order.amount,
                    }
                })
                .collect();
            expected.sort_by_key(|push| push.order_pda);

            let accounts: Vec<_> = ix
                .accounts
                .iter()
                .map(|meta| fake_account_from_array(meta.pubkey.to_bytes()))
                .collect();
            let parsed = FinalizeSettleInput::parse(&ix.data, &accounts)
                .map_err(|e| TestCaseError::fail(format!("parse failed: {e:?}")))?;

            prop_assert_eq!(parsed.begin_ix_index, begin_ix_index);
            prop_assert_eq!(
                parsed.instructions_sysvar_account.address(),
                &INSTRUCTIONS_SYSVAR_ID,
            );
            let (state_pda, _bump) = find_state_pda(&program_id);
            prop_assert_eq!(parsed.state_pda_account.address(), &state_pda);
            prop_assert_eq!(
                parsed.token_program_account.address(),
                &SPL_TOKEN_PROGRAM_ID,
            );

            let parsed_pushes: Vec<_> = parsed.pushes.iter().collect();
            prop_assert_eq!(parsed_pushes.len(), expected.len());
            for (push, expected) in parsed_pushes.iter().zip(&expected) {
                prop_assert_eq!(push.source_buffer.address(), &expected.buffer);
                prop_assert_eq!(push.destination.address(), &expected.destination);
                prop_assert_eq!(push.bump, expected.bump);
                prop_assert_eq!(push.amount, expected.amount);
            }
        }
    }
}
