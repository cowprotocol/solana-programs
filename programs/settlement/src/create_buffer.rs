//! `CreateBuffer` instruction handler.

use cow_settlement_interface::{
    instruction::{
        create_buffer::{BufferAccounts, CreateBufferInput},
        InstructionInputParsing,
    },
    pda::{buffer::buffer_pda_seeds, state::state_pda_seeds},
};
use pinocchio::{AccountView, Address, ProgramResult};
use pinocchio_token::instructions::InitializeAccount3;

use crate::{
    processor::CanonicalPda,
    token::{token_account_len, validate_token_program},
};

pub fn process_create_buffer(
    program_id: &Address,
    accounts: &mut [AccountView],
    instruction_data: &[u8],
) -> ProgramResult {
    let input = CreateBufferInput::parse(instruction_data, accounts)?;

    // Every buffer this instruction creates belongs to the one token program
    // it was handed, so reject an unsupported one up front rather than at the
    // first CPI.
    let token_program = validate_token_program(input.token_program)?;

    // The buffers' token authority is the settlement state PDA, the single
    // authority over every buffer. Derive it once for all buffers.
    let (state_pda, _) = Address::find_program_address(&state_pda_seeds(), program_id);

    for BufferAccounts { buffer_pda, mint } in input.buffers() {
        // One buffer per token. `CanonicalPda::create_idempotent` derives the
        // canonical bump and, by signing the allocation with the buffer seeds,
        // rejects any `buffer_pda` that isn't the canonical address. The buffer
        // is a token account, so it's assigned to the token program rather than
        // to the settlement program.
        //
        // We don't validate `mint` here. `InitializeAccount3` requires a real,
        // token-program-owned mint (and special-cases the native mint), so a
        // check of our own would be redundant.
        let mint_key = mint.address().as_array();
        let (created, _) = CanonicalPda {
            program_id,
            payer: input.payer,
            pda: buffer_pda,
            size: token_account_len(token_program, mint)?,
            owner: token_program,
            seeds: buffer_pda_seeds(mint_key),
        }
        .create_idempotent()?;

        // An existing buffer is already an initialized token account, so only
        // initialize a freshly created one.
        if created {
            InitializeAccount3::new(buffer_pda, mint, &state_pda)
                .invoke_with_unverified_program(token_program)?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use cow_settlement_interface::instruction::create_buffer::fixtures::{
        create_buffer_data, NUM_SHARED_ACCOUNTS,
    };
    use cow_settlement_interface::instruction::fixtures::fake_sequential_accounts;
    use pinocchio::error::ProgramError;

    /// Arbitrary placeholder program id. The failure path exercised below
    /// returns before the program id is used for any syscall.
    const PROGRAM_ID: Address = Address::new_from_array([0xc0; 32]);

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
