//! `CreateBuffer` instruction handler.

use pinocchio::{error::ProgramError, AccountView, Address, ProgramResult};
use pinocchio_token::{instructions::InitializeAccount3, state::Account as TokenAccount};
use settlement_interface::{
    instruction::create_buffer::SPL_TOKEN_PROGRAM_ID,
    pda::{buffer::buffer_pda_seeds, state::state_pda_seeds},
    SettlementInstruction,
};

use crate::processor::{create_canonical_pda, InstructionInputParsing};

/// Parsed inputs of a `CreateBuffer` instruction.
struct CreateBufferInput<'a> {
    payer: &'a AccountView,
    buffer_pda: &'a mut AccountView,
    mint: &'a AccountView,
    token_program: &'a AccountView,
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
        // Accounts: [payer (W,S), buffer_pda (W), mint (R), token_program (R),
        // system_program (R)]. The system program needs to be present for the
        // `CreateAccount` CPI but isn't dereferenced here.
        let [payer, buffer_pda, mint, token_program, _system, ..] = accounts else {
            return Err(ProgramError::NotEnoughAccountKeys);
        };

        Ok(Self {
            payer,
            buffer_pda,
            mint,
            token_program,
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
        mint,
        token_program,
    } = CreateBufferInput::parse(instruction_data, accounts)?;

    // Only the legacy SPL Token program is supported. The InitializeAccount3
    // CPI targets that program unconditionally; reject a mismatching account
    // up front so the caller gets a clear error.
    if token_program.address() != &SPL_TOKEN_PROGRAM_ID {
        return Err(ProgramError::IncorrectProgramId);
    }

    // One buffer per token. `create_canonical_pda` derives the canonical bump
    // and, by signing the allocation with the buffer seeds, rejects any
    // `buffer_pda` that isn't the canonical address. The buffer is a token
    // account, so it's assigned to the SPL Token program rather than to us.
    //
    // We don't validate `mint` here. `InitializeAccount3` requires a real,
    // token-program-owned mint (and special-cases the native mint), so a check
    // of our own would be redundant.
    let mint_key = mint.address().as_array();
    create_canonical_pda(
        program_id,
        payer,
        buffer_pda,
        TokenAccount::LEN as u64,
        &SPL_TOKEN_PROGRAM_ID,
        buffer_pda_seeds(mint_key),
    )?;

    // The buffer's token authority is the settlement state PDA, the single
    // authority over every buffer.
    let (state_pda, _) = Address::find_program_address(&state_pda_seeds(), program_id);
    InitializeAccount3::new(buffer_pda, mint, &state_pda).invoke()?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::{fake_account, fake_account_from_array};

    // Only used for failing tests where the input is irrelevant.
    fn create_buffer_data() -> Vec<u8> {
        let zero = Address::new_from_array([0; 32]);
        settlement_interface::instruction::create_buffer::create_buffer(&zero, &zero, &zero, &zero)
            .data
    }

    fn five_accounts() -> [AccountView; 5] {
        [
            fake_account_from_array([1; 32]),
            fake_account_from_array([2; 32]),
            fake_account_from_array([3; 32]),
            fake_account_from_array([4; 32]),
            fake_account_from_array([5; 32]),
        ]
    }

    #[test]
    fn create_buffer_input_parses_valid_input() {
        let program_id: Address = Address::new_from_array([1; 32]);
        let payer: Address = Address::new_from_array([2; 32]);
        let buffer_pda = Address::new_from_array([3; 32]);
        let mint = Address::new_from_array([4; 32]);
        let token_program = Address::new_from_array([5; 32]);
        let system_program = fake_account_from_array([6; 32]);

        let data = settlement_interface::instruction::create_buffer::create_buffer(
            &program_id,
            &payer,
            &buffer_pda,
            &mint,
        )
        .data;
        let mut accounts = [
            fake_account(payer),
            fake_account(buffer_pda),
            fake_account(mint),
            fake_account(token_program),
            system_program,
        ];

        let CreateBufferInput {
            payer: parsed_payer,
            buffer_pda: parsed_buffer_pda,
            mint: parsed_mint,
            token_program: parsed_token_program,
        } = CreateBufferInput::parse(&data, &mut accounts).expect("parse should succeed");

        assert_eq!(*parsed_payer.address(), payer);
        assert_eq!(*parsed_buffer_pda.address(), buffer_pda);
        assert_eq!(*parsed_mint.address(), mint);
        assert_eq!(*parsed_token_program.address(), token_program);
    }

    #[test]
    fn create_buffer_input_rejects_long_data() {
        let mut data = create_buffer_data();
        data.push(0); // trailing byte
        let mut accounts = five_accounts();
        assert_eq!(
            CreateBufferInput::parse(&data, &mut accounts).err(),
            Some(ProgramError::InvalidInstructionData),
        );
    }

    #[test]
    fn create_buffer_input_rejects_missing_accounts() {
        let data = create_buffer_data();
        let mut accounts: Vec<AccountView> = five_accounts().into();
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
        let mut accounts = five_accounts();
        assert_eq!(
            process_create_buffer(&PROGRAM_ID, &mut accounts, &data),
            Err(ProgramError::InvalidInstructionData),
        );
    }

    #[test]
    fn process_create_buffer_rejects_wrong_token_program() {
        let data = create_buffer_data();
        // The fourth account (token program) is not the SPL Token program.
        let mut accounts = five_accounts();
        assert_eq!(
            process_create_buffer(&PROGRAM_ID, &mut accounts, &data),
            Err(ProgramError::IncorrectProgramId),
        );
    }
}
