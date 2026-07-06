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
pub use settlement_interface::instruction::settle::{FinalizeSettle, Pull};

/// An order to settle together with the funds to pull from it: `intent`
/// identifies the order and `pulls` lists the [`Pull`]s to make from its sell
/// token account.
pub struct SettledOrder<'a> {
    pub intent: &'a OrderIntent,
    pub pulls: &'a [Pull],
}

/// Builder for a `BeginSettle` instruction settling the given orders.
pub struct BeginSettle<'a> {
    pub program_id: Pubkey,
    pub finalize_ix_index: u16,
    pub orders: &'a [SettledOrder<'a>],
}

impl From<BeginSettle<'_>> for Instruction {
    fn from(builder: BeginSettle<'_>) -> Self {
        let mut order_pdas = Vec::with_capacity(builder.orders.len());
        let mut sell_token_accounts = Vec::with_capacity(builder.orders.len());
        let mut bumps = Vec::with_capacity(builder.orders.len());
        let mut pull_lists: Vec<&[Pull]> = Vec::with_capacity(builder.orders.len());
        for order in builder.orders {
            let (order_pda, bump) = find_order_pda(&builder.program_id, &order.intent.uid());
            order_pdas.push(order_pda);
            sell_token_accounts.push(order.intent.sell_token_account);
            bumps.push(bump);
            pull_lists.push(order.pulls);
        }
        let (state_pda, _bump) = find_state_pda(&builder.program_id);
        settlement_interface::instruction::settle::BeginSettle {
            program_id: builder.program_id,
            state_pda,
            finalize_ix_index: builder.finalize_ix_index,
            order_pdas: &order_pdas,
            order_pda_bumps: &bumps,
            sell_token_accounts: &sell_token_accounts,
            pulls: &pull_lists,
        }
        .into()
    }
}

pub struct CreateOrder<'a> {
    pub program_id: Pubkey,
    pub owner: Pubkey,
    pub created_by: Pubkey,
    pub intent: &'a OrderIntent,
}

impl From<CreateOrder<'_>> for Instruction {
    fn from(builder: CreateOrder<'_>) -> Self {
        let encoded = EncodedOrderIntent::from(builder.intent);
        let (order_pda, _bump) = find_order_pda(&builder.program_id, &encoded.hash());
        let intent_bytes: [u8; EncodedOrderIntent::SIZE] = (&encoded).into();
        settlement_interface::instruction::create_order::CreateOrder {
            program_id: builder.program_id,
            owner: builder.owner,
            created_by: builder.created_by,
            order_pda,
            intent_bytes,
        }
        .into()
    }
}

pub struct CreateBuffers<'a> {
    pub program_id: Pubkey,
    pub payer: Pubkey,
    pub mints: &'a [Pubkey],
}

impl From<CreateBuffers<'_>> for Instruction {
    fn from(builder: CreateBuffers<'_>) -> Self {
        let buffers: Vec<(Pubkey, Pubkey)> = builder
            .mints
            .iter()
            .map(|mint| (find_buffer_pda(&builder.program_id, mint).0, *mint))
            .collect();
        settlement_interface::instruction::create_buffer::CreateBuffers {
            program_id: builder.program_id,
            payer: builder.payer,
            buffers: &buffers,
        }
        .into()
    }
}

pub struct Initialize {
    pub program_id: Pubkey,
    pub payer: Pubkey,
    pub receiver: Pubkey,
}

impl From<Initialize> for Instruction {
    fn from(builder: Initialize) -> Self {
        let (state_pda, _bump) = find_state_pda(&builder.program_id);
        settlement_interface::instruction::initialize::Initialize {
            program_id: builder.program_id,
            payer: builder.payer,
            state_pda,
            receiver: builder.receiver,
        }
        .into()
    }
}

/// Builder for a `ReclaimBuffer` instruction closing the buffer for each of
/// `mints`.
///
/// **Warning:** any token balance still held by a buffer is burned, not
/// recovered, before the buffer is closed. Only reclaim buffers expected to
/// be empty, or to write off dust/dead balances — never one that might still
/// hold funds of useful value.
pub struct ReclaimBuffer<'a> {
    pub program_id: Pubkey,
    pub receiver: Pubkey,
    pub mints: &'a [Pubkey],
}

impl From<ReclaimBuffer<'_>> for Instruction {
    fn from(builder: ReclaimBuffer<'_>) -> Self {
        let (state_pda, _bump) = find_state_pda(&builder.program_id);
        let buffers: Vec<(Pubkey, Pubkey)> = builder
            .mints
            .iter()
            .map(|mint| {
                let (buffer_pda, _bump) = find_buffer_pda(&builder.program_id, mint);
                (buffer_pda, *mint)
            })
            .collect();
        settlement_interface::instruction::reclaim_buffer::ReclaimBuffer {
            program_id: builder.program_id,
            state_pda,
            receiver: builder.receiver,
            buffers: &buffers,
        }
        .into()
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
            settle::{BeginSettleInput, INSTRUCTIONS_SYSVAR_ID},
            InstructionInputParsing,
        },
        pda::order::find_order_pda,
    };

    proptest! {
        // `begin_settle` derives each order's PDA from its intent and forwards to
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
            let orders: Vec<SettledOrder> = intents
                .iter()
                .map(|intent| SettledOrder { intent, pulls: &[] })
                .collect();
            let ix = Instruction::from(BeginSettle {
                program_id,
                finalize_ix_index,
                orders: &orders,
            });

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
    }
}
