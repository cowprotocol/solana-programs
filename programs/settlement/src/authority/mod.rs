//! Instruction handlers for transferring a
//! [`Role`](cow_settlement_interface::Role) stored in the state PDA.
//!
//! A transfer is two steps: a proposal by the manager or the role's current
//! holder (see [`process_propose_authority`]), then an acceptance by the
//! proposed account. The acceptance signature proves the recipient controls its
//! key before it takes over the role.

mod propose;

pub use propose::process_propose_authority;
