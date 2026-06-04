//! Guard against cross-program invocations of settlement instructions.

use pinocchio::{error::ProgramError, sysvars::instructions::Instructions, Address};

/// The transaction-level stack height used by [`is_cpi_call`].
const TRANSACTION_LEVEL_STACK_HEIGHT: u64 = 1;

/// Return `true` if the current invocation is a CPI rather than a top-level
/// transaction instruction.
///
/// The instructions sysvar stores only top-level instructions; CPI calls are
/// never recorded there. Consequently, `load_current_index` always returns the
/// index of the currently-executing *top-level* instruction — even when we are
/// reached via CPI. Two independent tests are combined:
///
/// 1. **Program-ID mismatch**: if the instruction at `current_index` belongs to
///    a different program, the top-level instruction is not ours, so we must
///    have been invoked via CPI.
/// 2. **Call-stack depth**: `sol_get_stack_height() > 1` is a belt-and-suspenders
///    check for the same condition.
///
/// Modelled on the audited Solend flash-loan guard:
/// <https://github.com/solendprotocol/solana-program-library/blob/mainnet/token-lending/program/src/processor.rs#L3447>
pub fn is_cpi_call<T: core::ops::Deref<Target = [u8]>>(
    program_id: &Address,
    current_index: u16,
    instructions: &Instructions<T>,
) -> Result<bool, ProgramError> {
    let current_ixn = instructions.load_instruction_at(usize::from(current_index))?;
    if current_ixn.get_program_id() != program_id {
        return Ok(true);
    }
    if get_stack_height() > TRANSACTION_LEVEL_STACK_HEIGHT {
        return Ok(true);
    }
    Ok(false)
}

#[cfg(any(target_os = "solana", target_arch = "bpf"))]
fn get_stack_height() -> u64 {
    unsafe { pinocchio::syscalls::sol_get_stack_height() }
}

#[cfg(not(any(target_os = "solana", target_arch = "bpf")))]
fn get_stack_height() -> u64 {
    TRANSACTION_LEVEL_STACK_HEIGHT
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build raw bytes for a one-instruction sysvar where the single instruction
    /// at index 0 has the given `program_id` and no accounts or data.
    fn single_instruction_sysvar(program_id: [u8; 32]) -> Vec<u8> {
        // Layout (see pinocchio `Instructions` source):
        //   [0..2]   num_instructions = 1  (LE u16)
        //   [2..4]   offset of ix[0]  = 4  (LE u16, points past this header)
        //   [4..6]   ix[0] num_accounts = 0
        //   [6..38]  ix[0] program_id
        //   [38..40] ix[0] data_len = 0
        //   [40..42] current_index = 0     (LE u16, always last 2 bytes)
        let mut data = Vec::with_capacity(42);
        data.extend_from_slice(&1u16.to_le_bytes());
        data.extend_from_slice(&4u16.to_le_bytes());
        data.extend_from_slice(&0u16.to_le_bytes());
        data.extend_from_slice(&program_id);
        data.extend_from_slice(&0u16.to_le_bytes());
        data.extend_from_slice(&0u16.to_le_bytes());
        data
    }

    #[test]
    fn detects_cpi_via_program_id_mismatch() {
        let our_program = Address::new_from_array([1u8; 32]);
        let other_program = [2u8; 32];
        let data = single_instruction_sysvar(other_program);
        let instructions = unsafe { Instructions::new_unchecked(data.as_slice()) };
        assert_eq!(is_cpi_call(&our_program, 0, &instructions).unwrap(), true);
    }

    #[test]
    fn allows_top_level_call() {
        let our_program = Address::new_from_array([1u8; 32]);
        let data = single_instruction_sysvar([1u8; 32]);
        let instructions = unsafe { Instructions::new_unchecked(data.as_slice()) };
        assert_eq!(is_cpi_call(&our_program, 0, &instructions).unwrap(), false);
    }
}
