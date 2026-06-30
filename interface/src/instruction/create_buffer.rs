//! `CreateBuffer` instruction builder.
//!
//! Allocates one or more per-token buffer PDAs (see [`crate::pda::buffer`]) as
//! SPL token accounts and initializes each with the settlement state PDA as its
//! token authority. Each token is identified by its `mint` account; the buffer
//! address must be the canonical PDA for that mint.

use solana_account_view::AccountView;
use solana_instruction::{AccountMeta, Instruction};
use solana_program_error::ProgramError;
use solana_pubkey::Pubkey;

pub use solana_system_interface::program::ID as SYSTEM_PROGRAM_ID;

use super::InstructionInputParsing;
use crate::SettlementInstruction;

/// The SPL Token program. Buffers are created as token accounts owned by this
/// program.
pub use spl_token_interface::ID as SPL_TOKEN_PROGRAM_ID;

/// Build a `CreateBuffer` instruction that creates one buffer per
/// `(buffer_pda, mint)` pair in `buffers`.
///
/// Each `buffer_pda` must be the canonical PDA returned by
/// [`crate::pda::buffer::find_buffer_pda`] for its `mint`; the program derives
/// the bump itself and rejects any other address. `payer` funds every new token
/// account's rent.
///
/// Wire format: `[discriminator]`, 1 byte. The tokens are implied by the
/// `mint` accounts, so no further data is needed.
/// Required accounts:
/// `[payer (W,S), system_program (R), token_program (R), (buffer_pda (W), mint (R))...]`.
/// The three shared accounts come first and are read positionally; the
/// per-buffer pairs follow. The system program only has to be present so the
/// `CreateAccount` CPI can dispatch; it isn't read by index.
pub fn create_buffers(
    program_id: &Pubkey,
    payer: &Pubkey,
    buffers: &[(Pubkey, Pubkey)],
) -> Instruction {
    let mut accounts = vec![
        AccountMeta::new(*payer, true),
        AccountMeta::new_readonly(SYSTEM_PROGRAM_ID, false),
        AccountMeta::new_readonly(SPL_TOKEN_PROGRAM_ID, false),
    ];
    for (buffer_pda, mint) in buffers {
        accounts.push(AccountMeta::new(*buffer_pda, false));
        accounts.push(AccountMeta::new_readonly(*mint, false));
    }
    Instruction {
        program_id: *program_id,
        accounts,
        data: vec![SettlementInstruction::CreateBuffer.discriminator()],
    }
}

/// Parsed inputs of a `CreateBuffer` instruction.
pub struct CreateBufferInput<'a> {
    pub payer: &'a AccountView,
    pub token_program: &'a AccountView,
    /// One `[buffer_pda, mint]` pair per buffer to create.
    pub buffers: &'a [[AccountView; 2]],
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
        // instruction. There must be at least one pair: an instruction that
        // creates no buffers is rejected as a likely encoding issue.
        let rest: &'a [AccountView] = rest;
        let (buffers, remainder) = rest.as_chunks::<2>();
        if !remainder.is_empty() || buffers.is_empty() {
            return Err(ProgramError::NotEnoughAccountKeys);
        }

        Ok(Self {
            payer,
            token_program,
            buffers,
        })
    }
}

/// Test scaffolding for `CreateBuffer` parsing and handling, shared by this
/// crate's tests and the settlement program's via the `test-fixtures` feature.
#[cfg(any(test, feature = "test-fixtures"))]
pub mod fixtures {
    use solana_address::Address;

    use super::create_buffers;

    /// Number of accounts that don't depend on the number of buffers created:
    /// payer, system program, and token program.
    pub const NUM_SHARED_ACCOUNTS: usize = 3;

    /// `CreateBuffer` instruction data with placeholder addresses, for failure
    /// cases where the input is irrelevant.
    pub fn create_buffer_data() -> Vec<u8> {
        let zero = Address::new_from_array([0; 32]);
        create_buffers(&zero, &zero, &[(zero, zero)]).data
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::{create_buffer_data, NUM_SHARED_ACCOUNTS};
    use super::*;
    use crate::instruction::fixtures::{
        fake_account, fake_account_from_array, fake_sequential_accounts,
    };
    use solana_address::Address;

    #[test]
    fn create_buffer_input_parses_valid_input() {
        let program_id: Address = Address::new_from_array([1; 32]);
        let payer: Address = Address::new_from_array([2; 32]);
        let system_program = fake_account_from_array([4; 32]);
        let token_program = Address::new_from_array([3; 32]);
        let buffer_pda = Address::new_from_array([5; 32]);
        let mint = Address::new_from_array([6; 32]);

        let data = create_buffers(&program_id, &payer, &[(buffer_pda, mint)]).data;
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
        assert_eq!(buffers.len(), 1, "one buffer is one (pda, mint) pair");
        assert_eq!(*buffers[0][0].address(), buffer_pda);
        assert_eq!(*buffers[0][1].address(), mint);
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

        let data = create_buffers(
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

        assert_eq!(
            buffers[0].each_ref().map(|a| *a.address()),
            [buffer_a, mint_a]
        );
        assert_eq!(
            buffers[1].each_ref().map(|a| *a.address()),
            [buffer_b, mint_b]
        );
    }

    #[test]
    fn create_buffer_input_rejects_zero_buffers() {
        let data = vec![SettlementInstruction::CreateBuffer.discriminator()];
        // Only the three shared accounts, no (pda, mint) pairs.
        let mut accounts = fake_sequential_accounts::<NUM_SHARED_ACCOUNTS>();
        assert_eq!(
            CreateBufferInput::parse(&data, &mut accounts).err(),
            Some(ProgramError::NotEnoughAccountKeys),
            "an instruction that creates no buffers is rejected",
        );
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

    #[test]
    fn instruction_data_has_expected_layout() {
        let program_id = Pubkey::new_from_array([1; 32]);
        let payer = Pubkey::new_from_array([2; 32]);
        let buffer_pda = Pubkey::new_from_array([3; 32]);
        let mint = Pubkey::new_from_array([4; 32]);
        let ix = create_buffers(&program_id, &payer, &[(buffer_pda, mint)]);
        assert_eq!(
            ix.data,
            vec![SettlementInstruction::CreateBuffer.discriminator()]
        );
    }

    #[test]
    fn single_buffer_has_expected_accounts() {
        let program_id = Pubkey::new_from_array([1; 32]);
        let payer = Pubkey::new_from_array([2; 32]);
        let buffer_pda = Pubkey::new_from_array([3; 32]);
        let mint = Pubkey::new_from_array([4; 32]);
        let ix = create_buffers(&program_id, &payer, &[(buffer_pda, mint)]);

        assert_eq!(ix.accounts.len(), 5);
        // payer: writable, signer
        assert_eq!(ix.accounts[0].pubkey, payer);
        assert!(ix.accounts[0].is_writable);
        assert!(ix.accounts[0].is_signer);
        // system program: read-only
        assert_eq!(ix.accounts[1].pubkey, SYSTEM_PROGRAM_ID);
        assert!(!ix.accounts[1].is_writable);
        assert!(!ix.accounts[1].is_signer);
        // token program: read-only
        assert_eq!(ix.accounts[2].pubkey, SPL_TOKEN_PROGRAM_ID);
        assert!(!ix.accounts[2].is_writable);
        assert!(!ix.accounts[2].is_signer);
        // buffer_pda: writable, not signer (the program signs via PDA seeds)
        assert_eq!(ix.accounts[3].pubkey, buffer_pda);
        assert!(ix.accounts[3].is_writable);
        assert!(!ix.accounts[3].is_signer);
        // mint: read-only
        assert_eq!(ix.accounts[4].pubkey, mint);
        assert!(!ix.accounts[4].is_writable);
        assert!(!ix.accounts[4].is_signer);
    }

    #[test]
    fn multiple_buffers_append_pairs_after_shared_accounts() {
        let program_id = Pubkey::new_from_array([1; 32]);
        let payer = Pubkey::new_from_array([2; 32]);
        let buffer_a = Pubkey::new_from_array([3; 32]);
        let mint_a = Pubkey::new_from_array([4; 32]);
        let buffer_b = Pubkey::new_from_array([5; 32]);
        let mint_b = Pubkey::new_from_array([6; 32]);
        let ix = create_buffers(
            &program_id,
            &payer,
            &[(buffer_a, mint_a), (buffer_b, mint_b)],
        );

        // Three shared accounts followed by two (buffer, mint) pairs.
        assert_eq!(ix.accounts.len(), 3 + 2 * 2);
        assert_eq!(ix.accounts[3].pubkey, buffer_a);
        assert!(ix.accounts[3].is_writable);
        assert_eq!(ix.accounts[4].pubkey, mint_a);
        assert!(!ix.accounts[4].is_writable);
        assert_eq!(ix.accounts[5].pubkey, buffer_b);
        assert!(ix.accounts[5].is_writable);
        assert_eq!(ix.accounts[6].pubkey, mint_b);
        assert!(!ix.accounts[6].is_writable);
    }

    #[test]
    fn empty_buffers_has_only_shared_accounts() {
        let program_id = Pubkey::new_from_array([1; 32]);
        let payer = Pubkey::new_from_array([2; 32]);
        let ix = create_buffers(&program_id, &payer, &[]);
        assert_eq!(ix.accounts.len(), 3);
    }
}
