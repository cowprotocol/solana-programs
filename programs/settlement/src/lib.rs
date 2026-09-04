//! On-chain CoW Protocol settlement program.

mod processor;

use pinocchio::entrypoint;
pub use processor::process_instruction;

entrypoint!(process_instruction);
