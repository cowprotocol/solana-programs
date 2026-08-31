//! Instruction handlers for buffer operations.

mod create;
mod reclaim;

pub use create::process_create_buffer;
pub use reclaim::process_reclaim_buffer;
