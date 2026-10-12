//! On-chain CoW Protocol settlement program.

pub mod pda;
mod processor;

pub use pda::ID;
use pinocchio::entrypoint;
pub use processor::process_instruction;

entrypoint!(process_instruction);
