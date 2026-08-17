//! Instruction tools for transferring a [`Role`](crate::Role)
//! stored in the state PDA.
//!
//! A transfer is two steps: the manager or the role's current holder proposes a
//! new account (see [`ProposeAuthority`]), then that account accepts,
//! finalizing the transfer. Requiring the recipient to accept proves it
//! controls its key before it takes over, guarding against handing a role to an
//! address nobody can sign for.
//!
//! The instructions are role-agnostic: the [`Role`](crate::Role) is part of the
//! the instruction data, so the same instruction is used for all authorities.

pub mod propose;

pub use propose::{ProposeAuthority, ProposeAuthorityInput};
