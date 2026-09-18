//! Builder for the `FinalizeSettle` instruction.

use cow_settlement_interface::{
    data::intent::{BuyAsset, OrderIntent},
    pda::{buffer::find_buffer_pda, order::find_order_pda, state::find_state_pda},
    Instruction, Pubkey,
};

use super::begin_settle::TokenProgram;

/// A settled order whose proceeds are pushed to it: `intent` identifies the
/// order (its `buy_token_account` is the push destination and its `buy_mint`
/// selects the canonical source buffer) and `amount` is the quantity to push.
pub struct FinalizedIntent<'a> {
    pub intent: &'a OrderIntent<BuyAsset>,
    pub amount: u64,
}

/// Builder for a `FinalizeSettle` instruction pushing each order's proceeds to
/// its buy token account.
///
/// The destination is the order intent's `buy_token_account` and the source is
/// the canonical buffer PDA for its `buy_mint` (see [`find_buffer_pda`]), the
/// only buffer `BeginSettle` accepts as the source of that order's push. An
/// order buying native SOL ([`BuyAsset::NativeSol`]) has no buffer, so its
/// source is the settlement state PDA, whose lamports pay it. The
/// orders are sorted by their canonical order PDA (the same key
/// [`BeginSettle`](super::begin_settle::BeginSettle) orders its settled-order
/// list by) so the two instructions present the orders
/// in the same order and their lists line up.
pub struct FinalizeSettle<'a> {
    pub program_id: Pubkey,
    pub begin_ix_index: u16,
    /// By default, a settlement support both token programs at the same time.
    /// If you know you only need a single token program, you can make the byte
    /// size of the settlement transaction a bit smaller and reduce the total
    /// accounts used in the transaction by specifying the
    /// only token program you need here.
    pub only_token_program: Option<TokenProgram>,
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
        let (state_pda, state_bump) = find_state_pda(&builder.program_id);
        for &i in &orders {
            let intent = builder.orders[i].intent;
            let (source, bump) = match intent.buy_mint {
                BuyAsset::NativeSol => (state_pda, state_bump),
                BuyAsset::Token(mint) => find_buffer_pda(&builder.program_id, &mint),
            };
            source_buffers.push(source);
            destinations.push(intent.buy_token_account);
            bumps.push(bump);
            amounts.push(builder.orders[i].amount);
        }
        cow_settlement_interface::instruction::settle::FinalizeSettle {
            program_id: builder.program_id,
            state_pda,
            begin_ix_index: builder.begin_ix_index,
            only_token_program: builder.only_token_program,
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
        data::intent::fixtures::arb_explicit_order_intent,
        fixtures::pubkey_from_seed,
        instruction::{
            fixtures::fake_account_from_array,
            settle::{FinalizeSettleInput, INSTRUCTIONS_SYSVAR_ID},
            InstructionInputParsing,
        },
    };

    #[test]
    fn native_sol_order_pushes_from_the_state_pda() {
        let program_id = pubkey_from_seed("program id");
        let intent = OrderIntent {
            buy_mint: BuyAsset::NativeSol,
            buy_token_account: pubkey_from_seed("recipient wallet"),
            ..OrderIntent::default()
        };
        let ix = Instruction::from(FinalizeSettle {
            program_id,
            begin_ix_index: 0,
            orders: &[FinalizedIntent {
                intent: &intent,
                amount: 1_337,
            }],
            only_token_program: None,
        });

        let accounts: Vec<_> = ix
            .accounts
            .iter()
            .map(|meta| fake_account_from_array(meta.pubkey.to_bytes()))
            .collect();
        let parsed = FinalizeSettleInput::parse(&ix.data, &accounts)
            .expect("the builder emits a parseable finalize");
        let pushes: Vec<_> = parsed.pushes.iter().collect();
        let [push] = pushes.as_slice() else {
            panic!("one order pushes once, got {} pushes", pushes.len());
        };

        let (state_pda, state_bump) = find_state_pda(&program_id);
        assert_eq!(push.source_buffer.address(), &state_pda);
        assert_eq!(push.bump, state_bump);
        assert_eq!(push.destination.address(), &intent.buy_token_account);
        assert_eq!(push.amount, 1_337);
    }

    proptest! {
        // `FinalizeSettle` derives each order's source buffer from its buy mint
        // and destination from the intent, sorting by canonical order PDA like
        // `BeginSettle` so the on-chain parser recovers exactly those pushes in
        // that order.
        #[test]
        fn finalize_settle_derives_buffers_from_mints(
            begin_ix_index in any::<u16>(),
            cases in prop::collection::vec(
                (arb_explicit_order_intent(), any::<u64>()),
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
                only_token_program: None,
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
                    let (buffer, bump) = match order.intent.buy_mint {
                        BuyAsset::NativeSol => find_state_pda(&program_id),
                        BuyAsset::Token(mint) => find_buffer_pda(&program_id, &mint),
                    };
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
