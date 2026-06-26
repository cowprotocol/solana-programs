//! `CreateBuffer` instruction handler.

use pinocchio::{error::ProgramError, AccountView, Address, ProgramResult};
use pinocchio_token::{instructions::InitializeAccount3, state::Account as TokenAccount};
use settlement_interface::{
    instruction::{
        create_buffer::{CreateBufferInput, SPL_TOKEN_PROGRAM_ID},
        InstructionInputParsing,
    },
    pda::{buffer::buffer_pda_seeds, state::state_pda_seeds},
};

use crate::processor::CanonicalPda;

struct CreateBufferEntry {
    buffer_pda: AccountView,
    mint: AccountView,
}

/// Read one slice element into a [`CreateBufferEntry`].
fn read_buffer_entry(&[buffer_pda, mint]: &[AccountView; 2]) -> CreateBufferEntry {
    CreateBufferEntry { buffer_pda, mint }
}

pub fn process_create_buffer(
    program_id: &Address,
    accounts: &mut [AccountView],
    instruction_data: &[u8],
) -> ProgramResult {
    let CreateBufferInput {
        payer,
        token_program,
        buffers,
    } = CreateBufferInput::parse(instruction_data, accounts)?;

    // Only the legacy SPL Token program is supported. The InitializeAccount3
    // CPI targets that program unconditionally; reject a mismatching account
    // up front so the caller gets a clear error.
    if token_program.address() != &SPL_TOKEN_PROGRAM_ID {
        return Err(ProgramError::IncorrectProgramId);
    }

    // The buffers' token authority is the settlement state PDA, the single
    // authority over every buffer. Derive it once for all buffers.
    let (state_pda, _) = Address::find_program_address(&state_pda_seeds(), program_id);

    for CreateBufferEntry { buffer_pda, mint } in buffers.iter().map(read_buffer_entry) {
        // One buffer per token. `CanonicalPda::create` derives the canonical
        // bump and, by signing the allocation with the buffer seeds, rejects
        // any `buffer_pda` that isn't the canonical address. The buffer is a
        // token account, so it's assigned to the SPL Token program rather than
        // to us.
        //
        // We don't validate `mint` here. `InitializeAccount3` requires a real,
        // token-program-owned mint (and special-cases the native mint), so a
        // check of our own would be redundant.
        let mint_key = mint.address().as_array();
        CanonicalPda {
            program_id,
            payer,
            pda: &buffer_pda,
            size: TokenAccount::LEN as u64,
            owner: &SPL_TOKEN_PROGRAM_ID,
            seeds: buffer_pda_seeds(mint_key),
        }
        .create()?;

        InitializeAccount3::new(&buffer_pda, &mint, &state_pda).invoke()?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use settlement_interface::instruction::create_buffer::fixtures::{
        create_buffer_data, NUM_SHARED_ACCOUNTS,
    };
    use settlement_interface::instruction::fixtures::fake_sequential_accounts;

    /// Arbitrary placeholder program id. The failure path exercised below
    /// returns before the program id is used for any syscall.
    const PROGRAM_ID: Address = Address::new_from_array([1; 32]);

    #[test]
    fn process_create_buffer_propagates_error() {
        let mut data = create_buffer_data();
        data.push(0); // make the data too long to trigger a parse error
        assert_eq!(
            process_create_buffer(&PROGRAM_ID, &mut [], &data),
            Err(ProgramError::InvalidInstructionData),
        );
    }

    #[test]
    fn process_create_buffer_rejects_wrong_token_program() {
        let data = create_buffer_data();
        // The three shared accounts plus one (buffer_pda, mint) pair so parsing
        // succeeds and reaches the token-program check. The third account (token
        // program) is not the SPL Token program.
        let mut accounts = fake_sequential_accounts::<{ NUM_SHARED_ACCOUNTS + 2 }>();
        assert_eq!(
            process_create_buffer(&PROGRAM_ID, &mut accounts, &data),
            Err(ProgramError::IncorrectProgramId),
        );
    }
}
