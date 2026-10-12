//! Compile-time program-derived addresses for the deployment this program was
//! built for.
//!
//! The shared `cow-settlement-interface` crate bakes no program-derived
//! addresses: every PDA helper there takes the program id as an argument. This
//! module is where the on-chain program pins its own addresses at compile time
//! from [`ID`], so handlers compare against constants and sign with fixed seeds
//! instead of searching for PDAs on-chain. All the efficiency tricks that keep
//! the compute-unit cost down live here, independent of the interface.

pub mod buffer;
pub mod state;

use cow_settlement_interface::Pubkey;

/// The program address this build targets, chosen at compile time. Every
/// compile-time PDA in this module derives from it.
///
/// Exactly one target must be selected, and a build that selects none fails to
/// compile — so a `.so` is never produced without a deliberate address:
/// - `--features prod` bakes the production deployment,
/// - `--features barn` bakes the staging ("barn") deployment,
/// - otherwise `SETTLEMENT_PROGRAM_ID=<address>` bakes an ad-hoc deployment.
///
/// `prod` wins if both features are on (e.g. under `--all-features`), so the
/// lint and doc builds resolve cleanly.
#[cfg(feature = "prod")]
pub const ID: Pubkey = cow_settlement_interface::PROD_ID;

#[cfg(all(feature = "barn", not(feature = "prod")))]
pub const ID: Pubkey = cow_settlement_interface::BARN_ID;

#[cfg(not(any(feature = "prod", feature = "barn")))]
pub const ID: Pubkey = match option_env!("SETTLEMENT_PROGRAM_ID") {
    Some(address) => Pubkey::from_str_const(address),
    None => panic!(
        "no deployment selected: build with `--features prod`, `--features barn`, \
         or set SETTLEMENT_PROGRAM_ID=<address>"
    ),
};

#[cfg(test)]
mod tests {
    #[cfg(feature = "prod")]
    #[test]
    fn id_is_the_production_address() {
        assert_eq!(super::ID, cow_settlement_interface::PROD_ID);
    }

    #[cfg(all(feature = "barn", not(feature = "prod")))]
    #[test]
    fn id_is_the_barn_address() {
        assert_eq!(super::ID, cow_settlement_interface::BARN_ID);
    }
}
