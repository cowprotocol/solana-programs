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

/// The program address this build targets.
///
/// Production for now; the staging ("barn") deployment will later be selected
/// here behind a build feature. Every compile-time PDA in this module derives
/// from this constant.
pub const ID: Pubkey = cow_settlement_interface::PROD_ID;
