//! Builder for the `BeginSettle` instruction.

use cow_settlement_interface::{
    data::intent::OrderIntent,
    pda::{order::find_order_pda, state::find_state_pda},
    Instruction, Pubkey,
};

// Reexport the interface's `Pull` and `TokenProgram` so the client provides
// all the types a caller needs to build a settlement.
pub use cow_settlement_interface::instruction::settle::{Pull, TokenProgram};

/// An order ready to be settled, together with the funds to pull from it:
/// `intent` identifies the order and `pulls` lists the [`Pull`]s to make from
/// its sell account.
pub struct InitializedIntent<'a> {
    pub intent: &'a OrderIntent,
    pub pulls: &'a [Pull],
    /// Use TransferChecked instead of Transfer to move the tokens. This is
    /// generally costs more CU and resources but some Token2022 token
    /// extensions require it (TransferFeeAmount, TransferHookAccount,
    /// PausableAccount).
    pub use_transfer_checked: bool,
}

impl InitializedIntent<'_> {
    /// The sell mint to name for `TransferChecked`, or `None` for a plain
    /// `Transfer`.
    fn sell_mint(&self) -> Option<Pubkey> {
        self.use_transfer_checked.then_some(self.intent.sell.mint)
    }
}

/// Builder for a `BeginSettle` instruction settling the given orders.
pub struct BeginSettle<'a> {
    pub program_id: Pubkey,
    pub solver: Pubkey,
    pub finalize_ix_index: u16,
    /// The off-chain auction this settlement executes, carried so it can be tied
    /// back to its auction off-chain.
    pub auction_id: i64,
    /// By default, a settlement support both token programs at the same time.
    /// If you know you only need a single token program, you can make the byte
    /// size of the settlement transaction a bit smaller and reduce the total
    /// accounts used in the transaction by specifying the
    /// only token program you need here.
    pub only_token_program: Option<TokenProgram>,
    pub orders: &'a [InitializedIntent<'a>],
}

impl From<BeginSettle<'_>> for Instruction {
    fn from(builder: BeginSettle<'_>) -> Self {
        let mut order_pdas = Vec::with_capacity(builder.orders.len());
        let mut sell_token_accounts = Vec::with_capacity(builder.orders.len());
        let mut sell_mints = Vec::with_capacity(builder.orders.len());
        let mut pull_lists: Vec<&[Pull]> = Vec::with_capacity(builder.orders.len());
        for order in builder.orders {
            let (order_pda, _bump) = find_order_pda(&builder.program_id, &order.intent.uid());
            order_pdas.push(order_pda);
            sell_token_accounts.push(order.intent.sell.token_account);
            sell_mints.push(order.sell_mint());
            pull_lists.push(order.pulls);
        }
        cow_settlement_interface::instruction::settle::BeginSettle {
            program_id: builder.program_id,
            state_pda: find_state_pda(&builder.program_id).0,
            solver: builder.solver,
            finalize_ix_index: builder.finalize_ix_index,
            auction_id: builder.auction_id,
            only_token_program: builder.only_token_program,
            order_pdas: &order_pdas,
            sell_token_accounts: &sell_token_accounts,
            sell_mints: &sell_mints,
            pulls: &pull_lists,
        }
        .into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::proptest::prelude::*;
    use cow_settlement_interface::{
        data::intent::fixtures::arb_order_intent, fixtures::pubkey_from_seed, instruction::settle,
    };

    #[test]
    fn sell_mint_is_named_only_for_transfer_checked() {
        let mint = pubkey_from_seed("sell mint");
        let mut intent = OrderIntent::default();
        intent.sell.mint = mint;
        for (use_transfer_checked, expected) in [(false, None), (true, Some(mint))] {
            let order = InitializedIntent {
                intent: &intent,
                pulls: &[],
                use_transfer_checked,
            };
            assert_eq!(order.sell_mint(), expected);
        }
    }

    proptest! {
        // `BeginSettle` derives each order's PDA from its intent and its sell
        // mint from `use_transfer_checked`, and forwards the rest unchanged to
        // the interface builder.
        #[test]
        fn begin_settle_derives_orders_from_intents(
            finalize_ix_index in any::<u16>(),
            cases in prop::collection::vec((arb_order_intent(), any::<bool>()), 1..=5),
        ) {
            let program_id = pubkey_from_seed("program id");
            let solver = pubkey_from_seed("solver");
            // No pulls here: this test only checks that orders are derived and
            // laid out correctly.
            let orders: Vec<InitializedIntent> = cases
                .iter()
                .map(|(intent, use_transfer_checked)| InitializedIntent {
                    intent,
                    pulls: &[],
                    use_transfer_checked: *use_transfer_checked,
                })
                .collect();
            let ix = Instruction::from(BeginSettle {
                program_id,
                solver,
                finalize_ix_index,
                auction_id: 0,
                only_token_program: None,
                orders: &orders,
            });

            let order_pdas: Vec<Pubkey> = cases
                .iter()
                .map(|(intent, _)| find_order_pda(&program_id, &intent.uid()).0)
                .collect();
            let sell_token_accounts: Vec<Pubkey> = cases
                .iter()
                .map(|(intent, _)| intent.sell.token_account)
                .collect();
            let sell_mints: Vec<Option<Pubkey>> =
                orders.iter().map(InitializedIntent::sell_mint).collect();
            let pulls: Vec<&[Pull]> = vec![&[]; cases.len()];
            let expected = Instruction::from(settle::BeginSettle {
                program_id,
                state_pda: find_state_pda(&program_id).0,
                solver,
                finalize_ix_index,
                auction_id: 0,
                only_token_program: None,
                order_pdas: &order_pdas,
                sell_token_accounts: &sell_token_accounts,
                sell_mints: &sell_mints,
                pulls: &pulls,
            });
            prop_assert_eq!(ix, expected);
        }
    }
}
