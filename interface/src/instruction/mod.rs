//! Off-chain instruction builders for the settlement program.
//!
//! Each submodule builds the [`solana_instruction::Instruction`] for specific
//! settlement instructions, encoding their discriminator (see
//! [`crate::SettlementInstruction`]) and laying out the required accounts.

pub mod create_buffer;
pub mod create_order;
pub mod initialize;
pub mod settle;
