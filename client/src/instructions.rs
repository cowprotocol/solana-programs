//! Instruction builders for the settlement program.
//!
//! A single place for client callers to reach for instruction constructors.
//! The instruction builders are the same as those in the interface, but they
//! provide a simplified interface at the price of more computation done
//! by the function, making it more suitable for off-chain use.

use settlement_interface::{
    data::intent::{EncodedOrderIntent, OrderIntent},
    pda::order::find_order_pda,
    Instruction, Pubkey,
};

// Reexport the instruction builders that don't change from the interface.
// We want the client to provide all instruction builders.
pub use settlement_interface::settle::finalize_settle;

/// Build a `BeginSettle` instruction settling the specified orders.
pub fn begin_settle(
    program_id: &Pubkey,
    finalize_ix_index: u16,
    intents: &[OrderIntent],
) -> Instruction {
    let mut orders: Vec<(Pubkey, Pubkey, u8)> = intents
        .iter()
        .map(|intent| {
            let (order_pda, bump) = find_order_pda(program_id, &intent.uid());
            (order_pda, intent.sell_token_account, bump)
        })
        .collect();
    orders.sort_by_key(|(order_pda, _, _)| *order_pda);

    let mut order_pdas = Vec::with_capacity(orders.len());
    let mut sell_token_accounts = Vec::with_capacity(orders.len());
    let mut bumps = Vec::with_capacity(orders.len());
    for (order_pda, sell_token_account, bump) in orders {
        order_pdas.push(order_pda);
        sell_token_accounts.push(sell_token_account);
        bumps.push(bump);
    }
    settlement_interface::settle::begin_settle(
        program_id,
        finalize_ix_index,
        &order_pdas,
        &sell_token_accounts,
        &bumps,
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
    settlement_interface::create_order::create_order(
        program_id,
        owner,
        created_by,
        &order_pda,
        &intent_bytes,
    )
}
