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
pub use settlement_interface::instruction::settle::{finalize_settle, Pull};

/// An order to settle together with the funds to pull from it: `intent`
/// identifies the order and `pulls` lists the [`Pull`]s to make from its sell
/// token account.
pub struct SettledOrder<'a> {
    pub intent: &'a OrderIntent,
    pub pulls: &'a [Pull],
}

/// Build a `BeginSettle` instruction settling the given orders.
pub fn begin_settle(
    program_id: &Pubkey,
    finalize_ix_index: u16,
    orders: &[SettledOrder],
) -> Instruction {
    let mut order_pdas = Vec::with_capacity(orders.len());
    let mut sell_token_accounts = Vec::with_capacity(orders.len());
    let mut bumps = Vec::with_capacity(orders.len());
    let mut pull_lists: Vec<&[Pull]> = Vec::with_capacity(orders.len());
    for order in orders {
        let (order_pda, bump) = find_order_pda(program_id, &order.intent.uid());
        order_pdas.push(order_pda);
        sell_token_accounts.push(order.intent.sell_token_account);
        bumps.push(bump);
        pull_lists.push(order.pulls);
    }
    let (state_pda, _bump) = find_state_pda(program_id);
    settlement_interface::instruction::settle::begin_settle(
        program_id,
        &state_pda,
        finalize_ix_index,
        &order_pdas,
        &bumps,
        &sell_token_accounts,
        &pull_lists,
    )
}

pub fn create_order(
    program_id: &Pubkey,
    owner: &Pubkey,
    created_by: &Pubkey,
    intent: &OrderIntent,
) -> Instruction {
    let encoded = EncodedOrderIntent::from(intent);
    let (order_pda, _bump) = find_order_pda(program_id, &encoded.hash());
    let intent_bytes: [u8; EncodedOrderIntent::SIZE] = (&encoded).into();
    settlement_interface::instruction::create_order::create_order(
        program_id,
        owner,
        created_by,
        &order_pda,
        &intent_bytes,
    )
}

pub fn create_buffers(program_id: &Pubkey, payer: &Pubkey, mints: &[Pubkey]) -> Instruction {
    let buffers: Vec<(Pubkey, Pubkey)> = mints
        .iter()
        .map(|mint| (find_buffer_pda(program_id, mint).0, *mint))
        .collect();
    settlement_interface::instruction::create_buffer::create_buffers(program_id, payer, &buffers)
}

pub fn initialize(program_id: &Pubkey, payer: &Pubkey) -> Instruction {
    let (state_pda, _bump) = find_state_pda(program_id);
    settlement_interface::instruction::initialize::initialize(program_id, payer, &state_pda)
}
