//! `CreateBuffer` instruction handler.

use pinocchio::{error::ProgramError, AccountView, Address, ProgramResult};
use pinocchio_token::{instructions::InitializeAccount3, state::Account as TokenAccount};
use settlement_interface::{
    instruction::create_buffer::SPL_TOKEN_PROGRAM_ID,
    pda::{buffer::buffer_pda_seeds, state::state_pda_seeds},
    SettlementInstruction,
};

use crate::processor::{CanonicalPda, InstructionInputParsing};

/// Parsed inputs of a `CreateBuffer` instruction.
struct CreateBufferInput<'a> {
    payer: &'a AccountView,
    token_program: &'a AccountView,
    /// Trailing accounts grouped into `[buffer_pda, mint]` pairs, one per
    /// buffer to create.
    buffers: &'a [[AccountView; 2]],
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
        // Accounts: [payer (W,S), system_program (R), token_program (R),
        // (buffer_pda (W), mint (R))...]. The three shared accounts come first;
        // the per-buffer pairs follow, one pair per buffer. The system program
        // needs to be present for the `CreateAccount` CPI but isn't dereferenced
        // here.
        let [payer, _system, token_program, rest @ ..] = accounts else {
            return Err(ProgramError::NotEnoughAccountKeys);
        };
        // Group the trailing accounts into `[buffer_pda, mint]` pairs. Each
        // buffer needs both, so a stray odd account left over is a malformed
        // instruction. Zero pairs is a valid no-op.
        let rest: &'a [AccountView] = rest;
        let (buffers, remainder) = rest.as_chunks::<2>();
        if !remainder.is_empty() {
            return Err(ProgramError::NotEnoughAccountKeys);
        }

        Ok(Self {
            payer,
            token_program,
            buffers,
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

    for [buffer_pda, mint] in buffers {
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
            pda: buffer_pda,
            size: TokenAccount::LEN as u64,
            owner: &SPL_TOKEN_PROGRAM_ID,
            seeds: buffer_pda_seeds(mint_key),
        }
        .create()?;

        InitializeAccount3::new(buffer_pda, mint, &state_pda).invoke()?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::{fake_account, fake_account_from_array, fake_sequential_accounts};

    /// Number of accounts that don't depend on the number of buffers created.
    const NUM_SHARED_ACCOUNTS: usize = 3;

    // Only used for failing tests where the input is irrelevant.
    fn create_buffer_data() -> Vec<u8> {
        let zero = Address::new_from_array([0; 32]);
        settlement_interface::instruction::create_buffer::create_buffers(
            &zero,
            &zero,
            &[(zero, zero)],
        )
        .data
    }

    #[test]
    fn create_buffer_input_parses_valid_input() {
        let program_id: Address = Address::new_from_array([1; 32]);
        let payer: Address = Address::new_from_array([2; 32]);
        let system_program = fake_account_from_array([4; 32]);
        let token_program = Address::new_from_array([3; 32]);
        let buffer_pda = Address::new_from_array([5; 32]);
        let mint = Address::new_from_array([6; 32]);

        let data = settlement_interface::instruction::create_buffer::create_buffers(
            &program_id,
            &payer,
            &[(buffer_pda, mint)],
        )
        .data;
        let mut accounts = [
            fake_account(payer),
            system_program,
            fake_account(token_program),
            fake_account(buffer_pda),
            fake_account(mint),
        ];

        let CreateBufferInput {
            payer: parsed_payer,
            token_program: parsed_token_program,
            buffers,
        } = CreateBufferInput::parse(&data, &mut accounts).expect("parse should succeed");

        assert_eq!(*parsed_payer.address(), payer);
        assert_eq!(*parsed_token_program.address(), token_program);
        let [[parsed_buffer_pda, parsed_mint]] = buffers else {
            panic!("expected exactly one (pda, mint) pair");
        };
        assert_eq!(*parsed_buffer_pda.address(), buffer_pda);
        assert_eq!(*parsed_mint.address(), mint);
    }

    #[test]
    fn create_buffer_input_parses_multiple_buffers() {
        let program_id = Address::new_from_array([1; 32]);
        let payer = Address::new_from_array([2; 32]);
        let token_program = Address::new_from_array([3; 32]);
        let buffer_a = Address::new_from_array([5; 32]);
        let mint_a = Address::new_from_array([6; 32]);
        let buffer_b = Address::new_from_array([7; 32]);
        let mint_b = Address::new_from_array([8; 32]);

        let data = settlement_interface::instruction::create_buffer::create_buffers(
            &program_id,
            &payer,
            &[(buffer_a, mint_a), (buffer_b, mint_b)],
        )
        .data;
        let mut accounts = [
            fake_account(payer),
            fake_account_from_array([4; 32]),
            fake_account(token_program),
            fake_account(buffer_a),
            fake_account(mint_a),
            fake_account(buffer_b),
            fake_account(mint_b),
        ];

        let CreateBufferInput { buffers, .. } =
            CreateBufferInput::parse(&data, &mut accounts).expect("parse should succeed");

        let [[buffer_pda_a, mint_pda_a], [buffer_pda_b, mint_pda_b]] = buffers else {
            panic!("expected exactly two (pda, mint) pairs");
        };
        assert_eq!(*buffer_pda_a.address(), buffer_a);
        assert_eq!(*mint_pda_a.address(), mint_a);
        assert_eq!(*buffer_pda_b.address(), buffer_b);
        assert_eq!(*mint_pda_b.address(), mint_b);
    }

    #[test]
    fn create_buffer_input_parses_zero_buffers_as_no_op() {
        let data = vec![SettlementInstruction::CreateBuffer.discriminator()];
        // Only the three shared accounts, no (pda, mint) pairs.
        let mut accounts = fake_sequential_accounts::<NUM_SHARED_ACCOUNTS>();
        let CreateBufferInput { buffers, .. } =
            CreateBufferInput::parse(&data, &mut accounts).expect("parse should succeed");
        assert!(buffers.is_empty(), "zero pairs is an empty buffer list");
    }

    #[test]
    fn create_buffer_input_rejects_long_data() {
        let mut data = create_buffer_data();
        data.push(0); // trailing byte
        assert_eq!(
            CreateBufferInput::parse(&data, &mut []).err(),
            Some(ProgramError::InvalidInstructionData),
        );
    }

    #[test]
    fn create_buffer_input_rejects_missing_accounts() {
        let data = create_buffer_data();
        // Fewer than the three shared accounts.
        let mut accounts = fake_sequential_accounts::<{ NUM_SHARED_ACCOUNTS - 1 }>();
        assert_eq!(
            CreateBufferInput::parse(&data, &mut accounts).err(),
            Some(ProgramError::NotEnoughAccountKeys),
        );
    }

    #[test]
    fn create_buffer_input_rejects_odd_pair_accounts() {
        let data = create_buffer_data();
        // Three shared accounts plus a dangling account that can't form a pair.
        let mut accounts = fake_sequential_accounts::<4>();
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
        assert_eq!(
            process_create_buffer(&PROGRAM_ID, &mut [], &data),
            Err(ProgramError::InvalidInstructionData),
        );
    }

    #[test]
    fn process_create_buffer_rejects_wrong_token_program() {
        let data = create_buffer_data();
        // The third account (token program) is not the SPL Token program.
        let mut accounts = fake_sequential_accounts::<NUM_SHARED_ACCOUNTS>();
        assert_eq!(
            process_create_buffer(&PROGRAM_ID, &mut accounts, &data),
            Err(ProgramError::IncorrectProgramId),
        );
    }
}
