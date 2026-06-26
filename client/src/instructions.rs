//! Instruction builders for the settlement program.
//!
//! A single place for client callers to reach for instruction constructors.
//! The instruction builders are the same as those in the interface, but they
//! provide a simplified interface at the price of more computation done
//! by the function, making it more suitable for off-chain use.

use settlement_interface::{
    data::intent::{EncodedOrderIntent, OrderIntent},
    pda::{buffer::find_buffer_pda, order::find_order_pda, state::find_state_pda},
    Instruction, Pubkey,
};

// Reexport the instruction builders that don't change from the interface.
// We want the client to provide all instruction builders.
pub use settlement_interface::instruction::settle::Pull;

/// An order ready to be settled, together with the funds to pull from it:
/// `intent` identifies the order and `pulls` lists the [`Pull`]s to make from
/// its sell token account.
pub struct SettleableOrder<'a> {
    pub intent: &'a OrderIntent,
    pub pulls: &'a [Pull],
}

/// Builder for a `BeginSettle` instruction settling the given orders.
pub struct BeginSettle<'a> {
    pub program_id: Pubkey,
    pub finalize_ix_index: u16,
    pub orders: &'a [SettleableOrder<'a>],
}

impl BeginSettle<'_> {
    pub fn instruction(self) -> Instruction {
        let mut order_pdas = Vec::with_capacity(self.orders.len());
        let mut sell_token_accounts = Vec::with_capacity(self.orders.len());
        let mut bumps = Vec::with_capacity(self.orders.len());
        let mut pull_lists: Vec<&[Pull]> = Vec::with_capacity(self.orders.len());
        for order in self.orders {
            let (order_pda, bump) = find_order_pda(&self.program_id, &order.intent.uid());
            order_pdas.push(order_pda);
            sell_token_accounts.push(order.intent.sell_token_account);
            bumps.push(bump);
            pull_lists.push(order.pulls);
        }
        let (state_pda, _bump) = find_state_pda(&self.program_id);
        settlement_interface::instruction::settle::BeginSettle {
            program_id: self.program_id,
            state_pda,
            finalize_ix_index: self.finalize_ix_index,
            order_pdas: &order_pdas,
            order_pda_bumps: &bumps,
            sell_token_accounts: &sell_token_accounts,
            pulls: &pull_lists,
        }
        .instruction()
    }
}

/// A settled order whose proceeds are pushed to it: `intent` identifies the
/// order (its `buy_token_account` is the push destination), `mint` selects the
/// canonical source buffer, and `amount` is the quantity to push.
pub struct SettledOrder<'a> {
    pub intent: &'a OrderIntent,
    pub mint: Pubkey,
    pub amount: u64,
}

/// Builder for a `FinalizeSettle` instruction pushing each order's proceeds to
/// its buy token account.
///
/// The destination is the order intent's `buy_token_account` and the source is
/// the canonical buffer PDA for `mint` (see [`find_buffer_pda`]). The orders are
/// sorted by their canonical order PDA (the same key [`BeginSettle`] orders its
/// settled-order list by) so the two instructions present the orders in the
/// same order and their lists line up.
pub struct FinalizeSettle<'a> {
    pub program_id: Pubkey,
    pub begin_ix_index: u16,
    pub orders: &'a [SettledOrder<'a>],
}

impl FinalizeSettle<'_> {
    pub fn instruction(self) -> Instruction {
        // Sort the orders by their canonical order PDA, the key `BeginSettle`
        // lays its settled orders out by, so the two instructions' lists align.
        let mut order: Vec<usize> = (0..self.orders.len()).collect();
        order.sort_by_key(|&i| find_order_pda(&self.program_id, &self.orders[i].intent.uid()).0);

        let mut source_buffers = Vec::with_capacity(self.orders.len());
        let mut destinations = Vec::with_capacity(self.orders.len());
        let mut bumps = Vec::with_capacity(self.orders.len());
        let mut amounts = Vec::with_capacity(self.orders.len());
        for &i in &order {
            let (buffer_pda, bump) = find_buffer_pda(&self.program_id, &self.orders[i].mint);
            source_buffers.push(buffer_pda);
            destinations.push(self.orders[i].intent.buy_token_account);
            bumps.push(bump);
            amounts.push(self.orders[i].amount);
        }
        let (state_pda, _bump) = find_state_pda(&self.program_id);
        settlement_interface::instruction::settle::FinalizeSettle {
            program_id: self.program_id,
            state_pda,
            begin_ix_index: self.begin_ix_index,
            source_buffers: &source_buffers,
            destinations: &destinations,
            bumps: &bumps,
            amounts: &amounts,
        }
        .instruction()
    }
}

pub struct CreateOrder<'a> {
    pub program_id: Pubkey,
    pub owner: Pubkey,
    pub created_by: Pubkey,
    pub intent: &'a OrderIntent,
}

impl CreateOrder<'_> {
    pub fn instruction(self) -> Instruction {
        let encoded = EncodedOrderIntent::from(self.intent);
        let (order_pda, _bump) = find_order_pda(&self.program_id, &encoded.hash());
        let intent_bytes: [u8; EncodedOrderIntent::SIZE] = (&encoded).into();
        settlement_interface::instruction::create_order::CreateOrder {
            program_id: self.program_id,
            owner: self.owner,
            created_by: self.created_by,
            order_pda,
            intent_bytes,
        }
        .instruction()
    }
}

pub struct CreateBuffers<'a> {
    pub program_id: Pubkey,
    pub payer: Pubkey,
    pub mints: &'a [Pubkey],
}

impl CreateBuffers<'_> {
    pub fn instruction(self) -> Instruction {
        let buffers: Vec<(Pubkey, Pubkey)> = self
            .mints
            .iter()
            .map(|mint| (find_buffer_pda(&self.program_id, mint).0, *mint))
            .collect();
        settlement_interface::instruction::create_buffer::CreateBuffers {
            program_id: self.program_id,
            payer: self.payer,
            buffers: &buffers,
        }
        .instruction()
    }
}

pub struct Initialize {
    pub program_id: Pubkey,
    pub payer: Pubkey,
}

impl Initialize {
    pub fn instruction(self) -> Instruction {
        let (state_pda, _bump) = find_state_pda(&self.program_id);
        settlement_interface::instruction::initialize::Initialize {
            program_id: self.program_id,
            payer: self.payer,
            state_pda,
        }
        .instruction()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::proptest::{prelude::*, test_runner::TestCaseError};
    use settlement_interface::{
        data::intent::fixtures::arb_order_intent,
        instruction::{
            fixtures::fake_account_from_array,
            settle::{
                BeginSettleInput, FinalizeSettleInput, INSTRUCTIONS_SYSVAR_ID, SPL_TOKEN_PROGRAM_ID,
            },
            InstructionInputParsing,
        },
        pda::order::find_order_pda,
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
            let program_id = Pubkey::new_unique();
            // No pulls here: this test only checks that orders are derived and
            // laid out correctly.
            let orders: Vec<SettleableOrder> = intents
                .iter()
                .map(|intent| SettleableOrder { intent, pulls: &[] })
                .collect();
            let ix = BeginSettle {
                program_id,
                finalize_ix_index,
                orders: &orders,
            }
            .instruction();

            // Expected orders: each intent's canonical PDA paired with its sell
            // token account and bump, sorted by PDA address (the builder's order).
            let mut expected: Vec<(Pubkey, Pubkey, u8)> = intents
                .iter()
                .map(|intent| {
                    let (order_pda, bump) = find_order_pda(&program_id, &intent.uid());
                    (order_pda, intent.sell_token_account, bump)
                })
                .collect();
            expected.sort_by_key(|(order_pda, _, _)| *order_pda);

            let mut accounts: Vec<_> = ix
                .accounts
                .iter()
                .map(|meta| fake_account_from_array(meta.pubkey.to_bytes()))
                .collect();
            let parsed = BeginSettleInput::parse(&ix.data, &mut accounts)
                .map_err(|e| TestCaseError::fail(format!("parse failed: {e:?}")))?;

            prop_assert_eq!(parsed.finalize_ix_index, finalize_ix_index);
            prop_assert_eq!(
                parsed.instructions_sysvar_account.address(),
                &INSTRUCTIONS_SYSVAR_ID,
            );

            let parsed_orders: Vec<_> = parsed.orders.iter().collect();
            prop_assert_eq!(parsed_orders.len(), expected.len());
            for (order, (order_pda, sell_token, bump)) in parsed_orders.iter().zip(&expected) {
                prop_assert_eq!(order.order_pda.address(), order_pda);
                prop_assert_eq!(order.sell_token_account.address(), sell_token);
                prop_assert_eq!(order.bump, *bump);
            }
        }

        // `FinalizeSettle` derives each order's source buffer from its mint and
        // destination from the intent, sorting by canonical order PDA like
        // `BeginSettle` so the on-chain parser recovers exactly those pushes in
        // that order.
        #[test]
        fn finalize_settle_derives_buffers_from_mints(
            begin_ix_index in any::<u16>(),
            cases in prop::collection::vec(
                (arb_order_intent(), any::<[u8; 32]>(), any::<u64>()),
                1..=5,
            ),
        ) {
            let program_id = Pubkey::new_unique();
            let orders: Vec<SettledOrder> = cases
                .iter()
                .map(|(intent, mint, amount)| SettledOrder {
                    intent,
                    mint: Pubkey::new_from_array(*mint),
                    amount: *amount,
                })
                .collect();
            let ix = FinalizeSettle {
                program_id,
                begin_ix_index,
                orders: &orders,
            }
            .instruction();

            // Expected pushes: each order's buffer PDA (and its canonical bump),
            // buy token account, and amount, sorted by the order's canonical PDA
            // (the builder's order).
            let mut expected: Vec<(Pubkey, Pubkey, u8, Pubkey, u64)> = orders
                .iter()
                .map(|order| {
                    let (order_pda, _bump) = find_order_pda(&program_id, &order.intent.uid());
                    let (buffer_pda, buffer_bump) = find_buffer_pda(&program_id, &order.mint);
                    (order_pda, buffer_pda, buffer_bump, order.intent.buy_token_account, order.amount)
                })
                .collect();
            expected.sort_by_key(|(order_pda, ..)| *order_pda);

            let mut accounts: Vec<_> = ix
                .accounts
                .iter()
                .map(|meta| fake_account_from_array(meta.pubkey.to_bytes()))
                .collect();
            let parsed = FinalizeSettleInput::parse(&ix.data, &mut accounts)
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
            for (push, (_order_pda, buffer, buffer_bump, destination, amount)) in
                parsed_pushes.iter().zip(&expected)
            {
                prop_assert_eq!(push.source_buffer.address(), buffer);
                prop_assert_eq!(push.destination.address(), destination);
                prop_assert_eq!(push.bump, *buffer_bump);
                prop_assert_eq!(u64::from_be_bytes(*push.amount), *amount);
            }
        }
    }
}
