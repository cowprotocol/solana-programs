//! Instruction builders for the settlement program.
//!
//! A single place for client callers to reach for instruction constructors.
//! The instruction builders are the same as those in the interface, but they
//! provide a simplified interface at the price of more computation done
//! by the function, making it more suitable for off-chain use.

pub mod add_solver;
pub mod begin_settle;
pub mod create_buffer;
pub mod create_order;
pub mod finalize_settle;
pub mod initialize;
pub mod reclaim_buffer;
pub mod transfer_authority;

pub use add_solver::AddSolver;
pub use begin_settle::{BeginSettle, InitializedIntent, Pull};
pub use create_buffer::CreateBuffers;
pub use create_order::CreateOrder;
pub use finalize_settle::{FinalizeSettle, FinalizedIntent};
pub use initialize::Initialize;
pub use reclaim_buffer::ReclaimBuffer;
pub use transfer_authority::TransferAuthority;
