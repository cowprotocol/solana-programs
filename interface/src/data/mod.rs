//! Wire-format data structures shared by clients and the on-chain
//! program: each module pairs an idiomatic Rust struct with its
//! canonical byte representation.

pub mod intent;
pub mod order;
pub mod state;
