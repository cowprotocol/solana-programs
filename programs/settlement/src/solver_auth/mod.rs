//! Instruction handlers for solver authentication.

mod add;
mod remove;

pub use add::process_add_solver;
pub use remove::process_remove_solver;
