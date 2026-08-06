//! Guards the property the CU benchmarks rest on: every address these tests
//! use is a function of the test itself.
//!
//! An address that moves between runs moves the PDAs derived from it, and
//! because `find_program_address` walks bumps down from 255 at ~1500 CU per
//! rejected candidate, it moves the compute cost of deriving them too — by far
//! more than the deltas a benchmark is trying to read. `clippy.toml` bans the
//! two generators that draw on state outside the caller; these tests check that
//! nothing else does either, by running a construction twice in one process and
//! requiring both passes to land in the same place.

use settlement_client::settlement_interface::pda::{
    buffer::find_buffer_pda, order::find_order_pda,
};
use solana_sdk::signature::Signer;

use crate::common::{order::OrderBuilder, setup, token, unique_pubkey};

mod common;

#[test]
fn setup_hands_out_the_same_program_id_and_payer_every_time() {
    let (_, first_program_id, first_payer) = setup();
    let (_, second_program_id, second_payer) = setup();

    assert_eq!(first_program_id, second_program_id);
    assert_eq!(first_payer.pubkey(), second_payer.pubkey());
}

#[test]
fn generated_addresses_increase_in_allocation_order() {
    // `BeginSettle` requires its order PDAs sorted by address, and tests lean on
    // this to lay out accounts that must sort a particular way.
    let _ = setup();
    let first = unique_pubkey();
    let second = unique_pubkey();

    assert!(
        first < second,
        "{first} was allocated before {second} and must sort before it",
    );
}

#[test]
fn an_order_lands_on_the_same_pda_with_the_same_bump() {
    // Covers the whole chain behind an order PDA: the program ID, the owner, and
    // the sell/buy token accounts and mints that `OrderBuilder` creates, all of
    // which feed the intent's UID.
    let derive_order_pda = || {
        let (mut svm, program_id, payer) = setup();
        let intent = OrderBuilder::new(&mut svm, &program_id, &payer).build();
        find_order_pda(&program_id, &intent.uid())
    };

    assert_eq!(derive_order_pda(), derive_order_pda());
}

#[test]
fn a_buffer_lands_on_the_same_pda_with_the_same_bump() {
    // A buffer PDA's seeds are the program ID and the mint, so this covers the
    // mint address `token::create_mint` generates.
    let derive_buffer_pda = || {
        let (mut svm, program_id, payer) = setup();
        let mint = token::create_mint(&mut svm, &payer);
        find_buffer_pda(&program_id, &mint)
    };

    assert_eq!(derive_buffer_pda(), derive_buffer_pda());
}
