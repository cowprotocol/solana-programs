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
pub use settlement_interface::instruction::settle::{begin_settle, finalize_settle};

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

pub fn create_buffer(program_id: &Pubkey, payer: &Pubkey, mint: &Pubkey) -> Instruction {
    let (buffer_pda, _bump) = find_buffer_pda(program_id, mint);
    settlement_interface::instruction::create_buffer::create_buffer(
        program_id,
        payer,
        &buffer_pda,
        mint,
    )
}

pub fn initialize(program_id: &Pubkey, payer: &Pubkey) -> Instruction {
    let (state_pda, _bump) = find_state_pda(program_id);
    settlement_interface::instruction::initialize::initialize(program_id, payer, &state_pda)
}
