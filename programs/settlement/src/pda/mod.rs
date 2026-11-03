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
/// Defaults to the production deployment.
/// `--features barn` uses the staging ("barn") deployment.
/// Either way, setting `SETTLEMENT_PROGRAM_ID=<address>` at build time
/// overrides the deployment address for an ad-hoc deployment.
pub const ID: Pubkey = match option_env!("SETTLEMENT_PROGRAM_ID") {
    Some(address) => Pubkey::from_str_const(address),
    #[cfg(not(feature = "barn"))]
    None => cow_settlement_interface::PROD_ID,
    #[cfg(feature = "barn")]
    None => cow_settlement_interface::BARN_ID,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(feature = "barn"))]
    #[test]
    fn default_build_targets_prod() {
        if option_env!("SETTLEMENT_PROGRAM_ID").is_none() {
            assert_eq!(ID, cow_settlement_interface::PROD_ID);
        }
    }

    #[cfg(feature = "barn")]
    #[test]
    fn barn_build_targets_barn() {
        if option_env!("SETTLEMENT_PROGRAM_ID").is_none() {
            assert_eq!(ID, cow_settlement_interface::BARN_ID);
        }
    }

    #[test]
    fn env_override_applies() {
        let Some(address) = option_env!("SETTLEMENT_PROGRAM_ID") else {
            return;
        };
        assert_eq!(ID, Pubkey::from_str_const(address));
    }
}
