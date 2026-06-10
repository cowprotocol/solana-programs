//! `CreateBuffer` instruction handler.

use pinocchio::{error::ProgramError, AccountView, Address, ProgramResult};
use pinocchio_associated_token_account::instructions::Create;
use settlement_interface::{
    pda::state::state_pda_seeds, SettlementInstruction, ASSOCIATED_TOKEN_PROGRAM_ID,
    SPL_TOKEN_PROGRAM_ID,
};

use crate::processor::InstructionInputParsing;

/// Parsed inputs of a `CreateBuffer` instruction.
struct CreateBufferInput<'a> {
    payer: &'a AccountView,
    buffer_pda: &'a AccountView,
    state_pda: &'a AccountView,
    mint: &'a AccountView,
    system_program: &'a AccountView,
    token_program: &'a AccountView,
    associated_token_program: &'a AccountView,
}

impl<'a> InstructionInputParsing<'a> for CreateBufferInput<'a> {
    const DISCRIMINATOR: SettlementInstruction = SettlementInstruction::CreateBuffer;

    fn parse_body(
        instruction_data: &[u8],
        accounts: &'a mut [AccountView],
    ) -> Result<Self, ProgramError> {
        if !instruction_data.is_empty() {
            return Err(ProgramError::InvalidInstructionData);
        }
        // Accounts mirror the Associated Token Account program's `Create`
        // account order, with the associated token program appended so the CPI
        // can dispatch:
        // [payer (W,S), buffer_pda (W), state_pda (R), mint (R),
        //  system_program (R), token_program (R), associated_token_program (R)].
        let [payer, buffer_pda, state_pda, mint, system_program, token_program, associated_token_program, ..] =
            accounts
        else {
            return Err(ProgramError::NotEnoughAccountKeys);
        };

        Ok(Self {
            payer,
            buffer_pda,
            state_pda,
            mint,
            system_program,
            token_program,
            associated_token_program,
        })
    }
}

pub fn process_create_buffer(
    program_id: &Address,
    accounts: &mut [AccountView],
    instruction_data: &[u8],
) -> ProgramResult {
    let CreateBufferInput {
        payer,
        buffer_pda,
        state_pda,
        mint,
        system_program,
        token_program,
        associated_token_program,
    } = CreateBufferInput::parse(instruction_data, accounts)?;

    // Only the legacy SPL Token program is supported (buffers are allocated at
    // the base token-account size). The CPI hands this account to the ATA
    // program as the token program, so reject a mismatch up front for a clear
    // error.
    if token_program.address() != &SPL_TOKEN_PROGRAM_ID {
        return Err(ProgramError::IncorrectProgramId);
    }
    // The CPI targets the ATA program by id; require the caller to pass that
    // exact program account so it's loaded and the failure mode is obvious.
    if associated_token_program.address() != &ASSOCIATED_TOKEN_PROGRAM_ID {
        return Err(ProgramError::IncorrectProgramId);
    }

    // The buffer's owner/authority must be the settlement state PDA. Re-derive
    // it so the handler doesn't trust the caller for the wallet the ATA is
    // created under. (The ATA program independently checks that `buffer_pda` is
    // the canonical ATA of `state_pda` for `mint`.)
    let (expected_state_pda, _) = Address::find_program_address(&state_pda_seeds(), program_id);
    if state_pda.address() != &expected_state_pda {
        return Err(ProgramError::InvalidSeeds);
    }

    // Forward to the Associated Token Account program's `Create`, which
    // allocates the buffer, initializes it as a token account, and sets its
    // authority to the state PDA in one CPI. No PDA signing is needed: the payer
    // already signed and the ATA program derives the buffer address itself. We
    // don't validate `mint` — the inner `InitializeAccount3` requires a real,
    // token-program-owned mint (special-casing the native mint), so a check of
    // our own would be redundant.
    Create {
        funding_account: payer,
        account: buffer_pda,
        wallet: state_pda,
        mint,
        system_program,
        token_program,
    }
    .invoke()?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::{fake_account, fake_account_from_array, fake_sequential_accounts};

    /// Number of accounts `CreateBuffer` expects: payer, buffer PDA, state PDA,
    /// mint, system program, token program, and associated token program.
    const NUM_ACCOUNTS: usize = 7;

    // Only used for failing tests where the input is irrelevant.
    fn create_buffer_data() -> Vec<u8> {
        let zero = Address::new_from_array([0; 32]);
        settlement_interface::instruction::create_buffer::create_buffer(
            &zero, &zero, &zero, &zero, &zero,
        )
        .data
    }

    #[test]
    fn create_buffer_input_parses_valid_input() {
        let program_id: Address = Address::new_from_array([1; 32]);
        let payer: Address = Address::new_from_array([2; 32]);
        let buffer_pda = Address::new_from_array([3; 32]);
        let state_pda = Address::new_from_array([4; 32]);
        let mint = Address::new_from_array([5; 32]);

        let data = settlement_interface::instruction::create_buffer::create_buffer(
            &program_id,
            &payer,
            &buffer_pda,
            &state_pda,
            &mint,
        )
        .data;
        let mut accounts = [
            fake_account(payer),
            fake_account(buffer_pda),
            fake_account(state_pda),
            fake_account(mint),
            fake_account_from_array([6; 32]),
            fake_account_from_array([7; 32]),
            fake_account_from_array([8; 32]),
        ];

        let CreateBufferInput {
            payer: parsed_payer,
            buffer_pda: parsed_buffer_pda,
            state_pda: parsed_state_pda,
            mint: parsed_mint,
            ..
        } = CreateBufferInput::parse(&data, &mut accounts).expect("parse should succeed");

        assert_eq!(*parsed_payer.address(), payer);
        assert_eq!(*parsed_buffer_pda.address(), buffer_pda);
        assert_eq!(*parsed_state_pda.address(), state_pda);
        assert_eq!(*parsed_mint.address(), mint);
    }

    #[test]
    fn create_buffer_input_rejects_long_data() {
        let mut data = create_buffer_data();
        data.push(0); // trailing byte
        let mut accounts = fake_sequential_accounts::<NUM_ACCOUNTS>();
        assert_eq!(
            CreateBufferInput::parse(&data, &mut accounts).err(),
            Some(ProgramError::InvalidInstructionData),
        );
    }

    #[test]
    fn create_buffer_input_rejects_missing_accounts() {
        let data = create_buffer_data();
        let mut accounts: Vec<AccountView> = fake_sequential_accounts::<NUM_ACCOUNTS>().into();
        accounts.pop();
        assert_eq!(
            CreateBufferInput::parse(&data, &mut accounts).err(),
            Some(ProgramError::NotEnoughAccountKeys),
        );
    }

    /// Arbitrary placeholder program id. The failure path exercised below
    /// returns before the program id is used for any syscall.
    const PROGRAM_ID: Address = Address::new_from_array([1; 32]);

    #[test]
    fn process_create_buffer_propagates_error() {
        let mut data = create_buffer_data();
        data.push(0); // make the data too long to trigger a parse error
        let mut accounts = fake_sequential_accounts::<NUM_ACCOUNTS>();
        assert_eq!(
            process_create_buffer(&PROGRAM_ID, &mut accounts, &data),
            Err(ProgramError::InvalidInstructionData),
        );
    }

    #[test]
    fn process_create_buffer_rejects_wrong_token_program() {
        let data = create_buffer_data();
        // The token-program account (index 5) is a sequential placeholder, not
        // the SPL Token program, so the guard rejects it before any CPI.
        let mut accounts = fake_sequential_accounts::<NUM_ACCOUNTS>();
        assert_eq!(
            process_create_buffer(&PROGRAM_ID, &mut accounts, &data),
            Err(ProgramError::IncorrectProgramId),
        );
    }
}
