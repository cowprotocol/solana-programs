//! Order PDA seed and address derivation.
//!
//! The body stored at this address is [`crate::data::order::EncodedOrderAccount`];
//! the UID feeding the seeds is [`crate::data::intent::EncodedOrderIntent::hash`].
//!
//! Any account derived from [`crate::pda::order::find_order_pda`] that has
//! already been created on-chain can be considered safe to use. Invalid
//! address are rejected at creation time. In particular, if the PDA exists,
//! the bump can be provided by the user without recomputing the canonical
//! one.
//!
//! For every valid [`crate::data::intent::OrderIntent`], there exists only
//! a single valid PDA representing that intent.

use solana_pubkey::Pubkey;

use crate::pda::SETTLEMENT_SEED;

/// Trailing seed identifying the order PDAs.
pub const ORDER_SEED: &[u8] = b"order";

/// Canonical seed components for the order PDA at `uid`.
pub fn order_pda_seeds(uid: &[u8; 32]) -> [&[u8]; 3] {
    [SETTLEMENT_SEED, uid, ORDER_SEED]
}

/// Derive the canonical order PDA address (and bump) for `uid`.
///
/// `uid` is the unique identifier of an intent. See
/// [`crate::data::intent::OrderIntent::uid`].
pub fn find_order_pda(program_id: &Pubkey, uid: &[u8; 32]) -> (Pubkey, u8) {
    Pubkey::find_program_address(&order_pda_seeds(uid), program_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_order_pda_uses_canonical_seeds() {
        let uid = *Pubkey::new_unique().as_array();

        crate::pda::tests::assert_canonical_bump(
            |program_id| find_order_pda(program_id, &uid),
            order_pda_seeds(&uid),
        );
    }

    mod proptest {
        use ::proptest::prelude::*;

        use super::*;

        proptest! {
            #[test]
            fn distinct_uids_yield_distinct_pdas(
                program_id in any::<[u8; 32]>(),
                uid1 in any::<[u8; 32]>(),
                uid2 in any::<[u8; 32]>(),
            ) {
                prop_assume!(uid1 != uid2);
                let program_id = Pubkey::new_from_array(program_id);
                let (pda1, _) = find_order_pda(&program_id, &uid1);
                let (pda2, _) = find_order_pda(&program_id, &uid2);
                prop_assert_ne!(pda1, pda2);
            }
        }
    }
}
