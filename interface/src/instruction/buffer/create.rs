//! `CreateBuffer` instruction builder.
//!
//! Allocates one or more per-token buffer PDAs (see [`crate::pda::buffer`]) as
//! SPL token accounts and initializes each with the settlement state PDA as its
//! token authority. Each token is identified by its `mint` account; the buffer
//! address must be the canonical PDA for that mint.

use solana_instruction::{AccountMeta, Instruction};
use solana_program_error::ProgramError;
use solana_pubkey::Pubkey;

pub use solana_system_interface::program::ID as SYSTEM_PROGRAM_ID;

use crate::instruction::InstructionInputParsing;
use crate::SettlementInstruction;

/// The SPL Token program. Buffers are created as token accounts owned by this
/// program.
pub use spl_token_interface::ID as SPL_TOKEN_PROGRAM_ID;

/// Builder for a `CreateBuffer` instruction that creates one buffer per
/// `(buffer_pda, mint)` pair in `buffers`.
///
/// Each `buffer_pda` must be the canonical PDA returned by
/// [`crate::pda::buffer::find_buffer_pda`] for its `mint`; the program derives
/// the bump itself and rejects any other address. `payer` funds every new token
/// account's rent.
///
/// Buffer creation is idempotent: an already-existing `buffer_pda` is left
/// unchanged and the instruction still succeeds, so two parties racing to
/// create the same buffer both succeed.
///
/// Wire format: `[discriminator=4]`, 1 byte. The tokens are implied by the
/// `mint` accounts, so no further data is needed.
/// Required accounts:
/// `[payer (W,S), system_program (R), token_program (R), (buffer_pda (W), mint (R))...]`.
/// The three shared accounts come first and are read positionally; the
/// per-buffer pairs follow. The system program only has to be present so the
/// `CreateAccount` CPI can dispatch; it isn't read by index.
pub struct CreateBuffers<'a> {
    pub program_id: Pubkey,
    pub payer: Pubkey,
    pub buffers: &'a [(Pubkey, Pubkey)],
}

impl From<CreateBuffers<'_>> for Instruction {
    fn from(builder: CreateBuffers<'_>) -> Self {
        let mut accounts = vec![
            AccountMeta::new(builder.payer, true),
            AccountMeta::new_readonly(SYSTEM_PROGRAM_ID, false),
            AccountMeta::new_readonly(SPL_TOKEN_PROGRAM_ID, false),
        ];
        for (buffer_pda, mint) in builder.buffers {
            accounts.push(AccountMeta::new(*buffer_pda, false));
            accounts.push(AccountMeta::new_readonly(*mint, false));
        }
        Instruction {
            program_id: builder.program_id,
            accounts,
            data: vec![SettlementInstruction::CreateBuffer.discriminator()],
        }
    }
}

/// The two accounts of one buffer to create.
pub struct BufferAccounts<'a, A> {
    pub buffer_pda: &'a A,
    pub mint: &'a A,
}

/// Parsed inputs of a `CreateBuffer` instruction.
pub struct CreateBufferInput<'a, A> {
    pub payer: &'a A,
    pub token_program: &'a A,
    buffer_pairs: &'a [[A; 2]],
}

impl<'a, A> CreateBufferInput<'a, A> {
    /// The buffers to create, one [`BufferAccounts`] per pair.
    pub fn buffers(&self) -> impl Iterator<Item = BufferAccounts<'a, A>> + '_ {
        self.buffer_pairs
            .iter()
            .map(|[buffer_pda, mint]| BufferAccounts { buffer_pda, mint })
    }
}

impl<'a, A> InstructionInputParsing<'a, A> for CreateBufferInput<'a, A> {
    const DISCRIMINATOR: SettlementInstruction = SettlementInstruction::CreateBuffer;

    fn parse_body(instruction_data: &[u8], accounts: &'a [A]) -> Result<Self, ProgramError> {
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
        let rest: &'a [A] = rest;
        let (buffers, remainder) = rest.as_chunks::<2>();
        if !remainder.is_empty() || buffers.is_empty() {
            return Err(ProgramError::NotEnoughAccountKeys);
        }

        Ok(Self {
            payer,
            token_program,
            buffer_pairs: buffers,
        })
    }
}

/// Test scaffolding for `CreateBuffer` parsing and handling, shared by this
/// crate's tests and the settlement program's via the `test-fixtures` feature.
#[cfg(any(test, feature = "test-fixtures"))]
pub mod fixtures {
    use solana_address::Address;

    use super::{CreateBuffers, Instruction};

    /// Number of accounts that don't depend on the number of buffers created:
    /// payer, system program, and token program.
    pub const NUM_SHARED_ACCOUNTS: usize = 3;

    /// `CreateBuffer` instruction data with placeholder addresses, for failure
    /// cases where the input is irrelevant.
    pub fn create_buffer_data() -> Vec<u8> {
        let zero = Address::new_from_array([0; 32]);
        Instruction::from(CreateBuffers {
            program_id: zero,
            payer: zero,
            buffers: &[(zero, zero)],
        })
        .data
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::{create_buffer_data, NUM_SHARED_ACCOUNTS};
    use super::*;
    use crate::instruction::fixtures::{
        fake_account, fake_account_from_array, fake_sequential_accounts,
    };
    use crate::instruction::tests::{
        assert_readonly_nonsigner, assert_writable_nonsigner, assert_writable_signer,
    };
    use solana_account_view::AccountView;
    use solana_address::Address;

    #[test]
    fn create_buffer_input_parses_valid_input() {
        let program_id: Address = Address::new_from_array([1; 32]);
        let payer: Address = Address::new_from_array([2; 32]);
        let system_program = fake_account_from_array([4; 32]);
        let token_program = Address::new_from_array([3; 32]);
        let buffer_pda = Address::new_from_array([5; 32]);
        let mint = Address::new_from_array([6; 32]);

        let data = Instruction::from(CreateBuffers {
            program_id,
            payer,
            buffers: &[(buffer_pda, mint)],
        })
        .data;
        let accounts = [
            fake_account(payer),
            system_program,
            fake_account(token_program),
            fake_account(buffer_pda),
            fake_account(mint),
        ];

        let input = CreateBufferInput::parse(&data, &accounts).expect("parse should succeed");

        assert_eq!(*input.payer.address(), payer);
        assert_eq!(*input.token_program.address(), token_program);
        let buffers: Vec<_> = input.buffers().collect();
        assert_eq!(buffers.len(), 1, "one buffer is one (pda, mint) pair");
        assert_eq!(*buffers[0].buffer_pda.address(), buffer_pda);
        assert_eq!(*buffers[0].mint.address(), mint);
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

        let data = Instruction::from(CreateBuffers {
            program_id,
            payer,
            buffers: &[(buffer_a, mint_a), (buffer_b, mint_b)],
        })
        .data;
        let accounts = [
            fake_account(payer),
            fake_account_from_array([4; 32]),
            fake_account(token_program),
            fake_account(buffer_a),
            fake_account(mint_a),
            fake_account(buffer_b),
            fake_account(mint_b),
        ];

        let input = CreateBufferInput::parse(&data, &accounts).expect("parse should succeed");

        let buffers: Vec<_> = input.buffers().collect();
        assert_eq!(
            [*buffers[0].buffer_pda.address(), *buffers[0].mint.address()],
            [buffer_a, mint_a]
        );
        assert_eq!(
            [*buffers[1].buffer_pda.address(), *buffers[1].mint.address()],
            [buffer_b, mint_b]
        );
    }

    #[test]
    fn create_buffer_input_rejects_zero_buffers() {
        let data = vec![SettlementInstruction::CreateBuffer.discriminator()];
        // Only the three shared accounts, no (pda, mint) pairs.
        let accounts = fake_sequential_accounts::<NUM_SHARED_ACCOUNTS>();
        assert_eq!(
            CreateBufferInput::parse(&data, &accounts).err(),
            Some(ProgramError::NotEnoughAccountKeys),
            "an instruction that creates no buffers is rejected",
        );
    }

    #[test]
    fn create_buffer_input_rejects_long_data() {
        let mut data = create_buffer_data();
        data.push(0); // trailing byte
        let accounts: [AccountView; 0] = [];
        assert_eq!(
            CreateBufferInput::parse(&data, &accounts).err(),
            Some(ProgramError::InvalidInstructionData),
        );
    }

    #[test]
    fn create_buffer_input_rejects_missing_accounts() {
        let data = create_buffer_data();
        // Fewer than the three shared accounts.
        let accounts = fake_sequential_accounts::<{ NUM_SHARED_ACCOUNTS - 1 }>();
        assert_eq!(
            CreateBufferInput::parse(&data, &accounts).err(),
            Some(ProgramError::NotEnoughAccountKeys),
        );
    }

    #[test]
    fn create_buffer_input_rejects_odd_pair_accounts() {
        let data = create_buffer_data();
        // Three shared accounts plus a dangling account that can't form a pair.
        let accounts = fake_sequential_accounts::<4>();
        assert_eq!(
            CreateBufferInput::parse(&data, &accounts).err(),
            Some(ProgramError::NotEnoughAccountKeys),
        );
    }

    #[test]
    fn instruction_data_has_expected_layout() {
        let program_id = Pubkey::new_from_array([1; 32]);
        let payer = Pubkey::new_from_array([2; 32]);
        let buffer_pda = Pubkey::new_from_array([3; 32]);
        let mint = Pubkey::new_from_array([4; 32]);
        let Instruction { data, .. } = CreateBuffers {
            program_id,
            payer,
            buffers: &[(buffer_pda, mint)],
        }
        .into();
        assert_eq!(
            data,
            vec![SettlementInstruction::CreateBuffer.discriminator()]
        );
    }

    #[test]
    fn single_buffer_has_expected_accounts() {
        let program_id = Pubkey::new_from_array([1; 32]);
        let payer = Pubkey::new_from_array([2; 32]);
        let buffer_pda = Pubkey::new_from_array([3; 32]);
        let mint = Pubkey::new_from_array([4; 32]);
        let Instruction { accounts, .. } = CreateBuffers {
            program_id,
            payer,
            buffers: &[(buffer_pda, mint)],
        }
        .into();

        assert_eq!(accounts.len(), 5);
        // payer funds rent; the system and token programs and the mint are only
        // referenced; buffer_pda is created (written) and the mint is
        // untouched.
        assert_writable_signer(&accounts[0], payer);
        assert_readonly_nonsigner(&accounts[1], SYSTEM_PROGRAM_ID);
        assert_readonly_nonsigner(&accounts[2], SPL_TOKEN_PROGRAM_ID);
        assert_writable_nonsigner(&accounts[3], buffer_pda);
        assert_readonly_nonsigner(&accounts[4], mint);
    }

    #[test]
    fn multiple_buffers_append_pairs_after_shared_accounts() {
        let program_id = Pubkey::new_from_array([1; 32]);
        let payer = Pubkey::new_from_array([2; 32]);
        let buffer_a = Pubkey::new_from_array([3; 32]);
        let mint_a = Pubkey::new_from_array([4; 32]);
        let buffer_b = Pubkey::new_from_array([5; 32]);
        let mint_b = Pubkey::new_from_array([6; 32]);
        let Instruction { accounts, .. } = CreateBuffers {
            program_id,
            payer,
            buffers: &[(buffer_a, mint_a), (buffer_b, mint_b)],
        }
        .into();

        // Three shared accounts followed by two (buffer, mint) pairs.
        assert_eq!(accounts.len(), 3 + 2 * 2);
        assert_writable_nonsigner(&accounts[3], buffer_a);
        assert_readonly_nonsigner(&accounts[4], mint_a);
        assert_writable_nonsigner(&accounts[5], buffer_b);
        assert_readonly_nonsigner(&accounts[6], mint_b);
    }

    #[test]
    fn empty_buffers_has_only_shared_accounts() {
        let program_id = Pubkey::new_from_array([1; 32]);
        let payer = Pubkey::new_from_array([2; 32]);
        let Instruction { accounts, .. } = CreateBuffers {
            program_id,
            payer,
            buffers: &[],
        }
        .into();
        assert_eq!(accounts.len(), 3);
    }
}
