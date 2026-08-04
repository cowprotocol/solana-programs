//! Test-only program that exercises the on-chain hashing syscalls.
//!
//! Given an instruction of the form `[op, message @ ..]`, it hashes `message`
//! with the algorithm [`HashOp`] selects and hands the 32-byte digest back
//! through the return-data channel, where the caller can read it.
//!
//! On the Solana target `solana_keccak_hasher`/`solana_sha256_hasher` lower to
//! the `sol_keccak256`/`sol_sha256` syscalls, so a test invoking this program
//! runs the real syscalls rather than a host reimplementation.

use num_enum::TryFromPrimitive;
use pinocchio::{entrypoint, error::ProgramError, AccountView, Address, ProgramResult};

/// The hashing operation the program should perform, chosen by the first byte
/// of the instruction data. The discriminants are the wire encoding, so a
/// caller builds the leading byte with `op as u8` and the program recovers the
/// variant with `HashOp::try_from`.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, TryFromPrimitive)]
pub enum HashOp {
    /// Hash nothing and return. A baseline for isolating a hash's compute cost:
    /// its cost is the fixed per-invocation overhead, with no syscall.
    Noop = 0,
    /// keccak256, the variant Ethereum uses.
    Keccak256 = 1,
    /// sha256.
    Sha256 = 2,
}

entrypoint!(process_instruction);

pub fn process_instruction(
    _program_id: &Address,
    _accounts: &mut [AccountView],
    instruction_data: &[u8],
) -> ProgramResult {
    let (op, message) = instruction_data
        .split_first()
        .ok_or(ProgramError::InvalidInstructionData)?;

    let digest = match HashOp::try_from(*op).map_err(|_| ProgramError::InvalidInstructionData)? {
        HashOp::Noop => return Ok(()),
        HashOp::Keccak256 => solana_keccak_hasher::hash(message).to_bytes(),
        HashOp::Sha256 => solana_sha256_hasher::hash(message).to_bytes(),
    };

    set_return_data(&digest);

    Ok(())
}

/// Hand `data` back to the caller through the return-data channel.
///
/// `pinocchio::syscalls` only exists on the Solana target, so the host build
/// (compiled by `cargo test`/`clippy` for the workspace) gets a no-op stub.
/// This program is only ever executed on-chain, so the stub is never reached.
fn set_return_data(data: &[u8]) {
    #[cfg(target_os = "solana")]
    // SAFETY: `data` outlives the call, and its length is passed alongside it.
    unsafe {
        pinocchio::syscalls::sol_set_return_data(data.as_ptr(), data.len() as u64)
    };
    #[cfg(not(target_os = "solana"))]
    let _ = data;
}
