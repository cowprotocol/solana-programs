//! Builder for the `BeginSettle` instruction.

use cow_settlement_interface::{
    data::intent::OrderIntent,
    pda::{order::find_order_pda, state::find_state_pda},
    Instruction, Pubkey,
};

// Reexport the interface's `Pull` so the client provides all the types a caller
// needs to build a settlement.
pub use cow_settlement_interface::instruction::settle::Pull;

/// An order ready to be settled, together with the funds to pull from it:
/// `intent` identifies the order and `pulls` lists the [`Pull`]s to make from
/// its sell token account.
pub struct InitializedIntent<'a> {
    pub intent: &'a OrderIntent,
    pub pulls: &'a [Pull],
}

/// Builder for a `BeginSettle` instruction settling the given orders.
pub struct BeginSettle<'a> {
    pub program_id: Pubkey,
    pub solver: Pubkey,
    pub finalize_ix_index: u16,
    /// The off-chain auction this settlement executes, carried so it can be tied
    /// back to its auction off-chain.
    pub auction_id: i64,
    pub orders: &'a [InitializedIntent<'a>],
}

impl From<BeginSettle<'_>> for Instruction {
    fn from(builder: BeginSettle<'_>) -> Self {
        let mut order_pdas = Vec::with_capacity(builder.orders.len());
        let mut sell_token_accounts = Vec::with_capacity(builder.orders.len());
        let mut pull_lists: Vec<&[Pull]> = Vec::with_capacity(builder.orders.len());
        for order in builder.orders {
            let (order_pda, _bump) = find_order_pda(&builder.program_id, &order.intent.uid());
            order_pdas.push(order_pda);
            sell_token_accounts.push(order.intent.sell_token_account);
            pull_lists.push(order.pulls);
        }
        let (state_pda, _bump) = find_state_pda(&builder.program_id);
        cow_settlement_interface::instruction::settle::BeginSettle {
            program_id: builder.program_id,
            state_pda,
            solver: builder.solver,
            finalize_ix_index: builder.finalize_ix_index,
            auction_id: builder.auction_id,
            order_pdas: &order_pdas,
            sell_token_accounts: &sell_token_accounts,
            pulls: &pull_lists,
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
            settle::{BeginSettleInput, INSTRUCTIONS_SYSVAR_ID},
            InstructionInputParsing,
        },
    };

    proptest! {
        // `BeginSettle` derives each order's PDA from its intent and forwards to
        // the interface builder so that the on-chain parser recovers exactly
        // those orders.
        #[test]
        fn begin_settle_derives_orders_from_intents(
            finalize_ix_index in any::<u16>(),
            intents in prop::collection::vec(arb_order_intent(), 1..=5),
        ) {
            let program_id = pubkey_from_seed("program id");
            // No pulls here: this test only checks that orders are derived and
            // laid out correctly.
            let orders: Vec<InitializedIntent> = intents
                .iter()
                .map(|intent| InitializedIntent { intent, pulls: &[] })
                .collect();
            let ix = Instruction::from(BeginSettle {
                program_id,
                solver: pubkey_from_seed("solver"),
                finalize_ix_index,
                auction_id: 0,
                orders: &orders,
            });

            // Expected orders: each intent's canonical PDA paired with its sell
            // token account, sorted by PDA address (the builder's order).
            let mut expected: Vec<(Pubkey, Pubkey)> = intents
                .iter()
                .map(|intent| {
                    let (order_pda, _bump) = find_order_pda(&program_id, &intent.uid());
                    (order_pda, intent.sell_token_account)
                })
                .collect();
            expected.sort_by_key(|(order_pda, _)| *order_pda);

            let accounts: Vec<_> = ix
                .accounts
                .iter()
                .map(|meta| fake_account_from_array(meta.pubkey.to_bytes()))
                .collect();
            let parsed = BeginSettleInput::parse(&ix.data, &accounts)
                .map_err(|e| TestCaseError::fail(format!("parse failed: {e:?}")))?;

            prop_assert_eq!(parsed.finalize_ix_index, finalize_ix_index);
            prop_assert_eq!(
                parsed.instructions_sysvar_account.address(),
                &INSTRUCTIONS_SYSVAR_ID,
            );

            let actual: Vec<(Pubkey, Pubkey)> = parsed
                .orders
                .iter()
                .map(|order| {
                    (
                        *order.order_pda.address(),
                        *order.sell_token_account.address(),
                    )
                })
                .collect();
            prop_assert_eq!(actual, expected);
        }
    }
}
