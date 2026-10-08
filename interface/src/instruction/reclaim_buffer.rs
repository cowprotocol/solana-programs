//! `ReclaimBuffer` instruction builder.
//!
//! Closes one or more buffer PDAs (see [`crate::pda::buffer`]) and sends each
//! closed buffer's rent lamports to a `reclaim_recipient` of the caller's
//! choosing. Only the settlement's configured `reclaim_authority` (see
//! [`crate::data::state::StateAccount`]) may authorize this.
//!
//! Each relaimed buffer takes a burn limit. Up to this amount of balance is
//! burned, so that rent reclamation remains possible when the tokens can't be
//! otherwise be disposed of. A zero limit prevents any burning and is the
//! expected standard way to reclaim buffers, since most tokens should rather
//! be sold in advance than burned.
//!
//! The token_program supplied to this instruction must be the owner of all mints
//! supplied. Only one token program can be supplied to this instruction at a time.
//! If mints from two separate token programs are required, the client needs to
//! divide it into separate instructions.
//!
//! Wire format (with `n` buffers): `[discriminator=6][burn_limit: u64 LE ×n]`.
//! Required accounts:
//! `[state_pda (R), reclaim_authority (R,S), reclaim_recipient (W),
//! token_program (R), (buffer_pda (W), mint (W))...]`.

use solana_instruction::{AccountMeta, Instruction};
use solana_program_error::ProgramError;
use solana_pubkey::Pubkey;

use super::InstructionInputParsing;
use crate::SettlementInstruction;

/// Number of accounts each buffer contributes to the instruction: its buffer
/// PDA and its mint.
pub const ACCOUNTS_PER_BUFFER: usize = 2;

/// Builder for a `ReclaimBuffer` instruction that closes one buffer per
/// `(buffer_pda, mint, burn_limit)` entry in `buffers`.
///
/// `state_pda` must be [`crate::pda::state::STATE_PDA`]. `reclaim_authority`
/// must sign and must match the `reclaim_authority` recorded in the state PDA's data.
/// `reclaim_recipient` is the account receiving the closed buffer's lamports.
/// Each `buffer_pda` must be the canonical PDA returned by
/// [`crate::pda::buffer::find_buffer_pda`] for its paired `mint`, which is
/// passed both so that derivation can be checked on-chain and so the balance can
/// be burned when clearing the buffer.
///
/// `burn_limit` caps how much of a buffer's balance may be burned to clear it:
/// a buffer whose balance doesn't exceed its limit is burned to zero (when
/// non-empty) and closed, while one that exceeds its limit reverts the whole
/// instruction. A zero limit prevents any burning.
pub struct ReclaimBuffer<'a> {
    pub program_id: Pubkey,
    pub state_pda: Pubkey,
    pub reclaim_authority: Pubkey,
    pub reclaim_recipient: Pubkey,
    /// The token program owning every buffer this instruction closes. Must be
    /// the address of a [`crate::token_program::TokenProgram`].
    pub token_program: Pubkey,
    /// One `(buffer_pda, mint, burn_limit)` entry per buffer to close.
    pub buffers: &'a [(Pubkey, Pubkey, u64)],
}

impl From<ReclaimBuffer<'_>> for Instruction {
    fn from(builder: ReclaimBuffer<'_>) -> Self {
        let mut accounts = vec![
            AccountMeta::new_readonly(builder.state_pda, false),
            AccountMeta::new_readonly(builder.reclaim_authority, true),
            AccountMeta::new(builder.reclaim_recipient, false),
            AccountMeta::new_readonly(builder.token_program, false),
        ];
        let mut data = vec![SettlementInstruction::ReclaimBuffer.discriminator()];
        for (buffer_pda, mint, burn_limit) in builder.buffers {
            accounts.push(AccountMeta::new(*buffer_pda, false));
            // The mint is writable so the handler can burn the buffer's
            // balance to clear it, which decrements the mint's supply.
            accounts.push(AccountMeta::new(*mint, false));
            data.extend_from_slice(&burn_limit.to_le_bytes());
        }
        Instruction {
            program_id: builder.program_id,
            accounts,
            data,
        }
    }
}

/// Decoded info for a single buffer to reclaim
pub struct Buffer<'a, A> {
    pub buffer_pda: &'a A,
    pub mint: &'a A,
    pub burn_limit: u64,
}

/// The `(buffer_pda, mint)` account pairs of a `ReclaimBuffer` and their
/// parallel per-buffer burn limits. Parsing guarantees the two have the same
/// length.
pub struct Buffers<'a, A> {
    accounts: &'a [[A; ACCOUNTS_PER_BUFFER]],
    burn_limits: &'a [[u8; core::mem::size_of::<u64>()]],
}

impl<'a, A> Buffers<'a, A> {
    pub fn iter(&self) -> impl Iterator<Item = Buffer<'a, A>> + '_ {
        self.accounts
            .iter()
            .zip(self.burn_limits)
            .map(|([buffer_pda, mint], burn_limit)| Buffer {
                buffer_pda,
                mint,
                burn_limit: u64::from_le_bytes(*burn_limit),
            })
    }
}

/// Parsed inputs of a `ReclaimBuffer` instruction.
pub struct ReclaimBufferInput<'a, A> {
    pub state_pda: &'a A,
    pub reclaim_authority: &'a A,
    pub reclaim_recipient: &'a A,
    pub buffers: Buffers<'a, A>,
}

impl<'a, A> InstructionInputParsing<'a, A> for ReclaimBufferInput<'a, A> {
    const DISCRIMINATOR: SettlementInstruction = SettlementInstruction::ReclaimBuffer;

    fn parse_body(instruction_data: &'a [u8], accounts: &'a [A]) -> Result<Self, ProgramError> {
        // Accounts: [state_pda (R), reclaim_authority (R,S), reclaim_recipient
        // (W), token_program (R), (buffer_pda (W), mint (W))...]. The four
        // shared accounts come first; the per-buffer pairs follow, one pair per
        // buffer. The token program is skipped rather than read: each buffer is
        // closed by the program that owns it, so the account is only there to
        // put that program in the transaction.
        let [state_pda, reclaim_authority, reclaim_recipient, _token_program, rest @ ..] = accounts
        else {
            return Err(ProgramError::NotEnoughAccountKeys);
        };
        // Group the trailing accounts into `[buffer_pda, mint]` pairs. Each
        // buffer needs both, so a stray leftover account is a malformed
        // instruction. There must be at least one pair: an instruction that
        // reclaims no buffers is rejected as a likely encoding issue.
        let (buffer_pairs, remainder) = rest.as_chunks::<ACCOUNTS_PER_BUFFER>();
        if !remainder.is_empty() || buffer_pairs.is_empty() {
            return Err(ProgramError::NotEnoughAccountKeys);
        }

        // The data carries one little-endian `u64` burn limit per buffer, in
        // buffer order. A trailing partial limit, or a limit count that doesn't
        // match the buffers, is a malformed instruction.
        let (burn_limits, remainder) =
            instruction_data.as_chunks::<{ core::mem::size_of::<u64>() }>();
        if !remainder.is_empty() || burn_limits.len() != buffer_pairs.len() {
            return Err(ProgramError::InvalidInstructionData);
        }

        Ok(Self {
            state_pda,
            reclaim_authority,
            reclaim_recipient,
            buffers: Buffers {
                accounts: buffer_pairs,
                burn_limits,
            },
        })
    }
}

/// Test scaffolding for `ReclaimBuffer` parsing and handling, shared by this
/// crate's tests and the settlement program's via the `test-fixtures` feature.
#[cfg(any(test, feature = "test-fixtures"))]
pub mod fixtures {
    use solana_address::Address;

    use super::{Instruction, ReclaimBuffer};

    /// Number of accounts that don't depend on the number of buffers
    /// reclaimed: state PDA, reclaim authority, reclaim recipient, and token
    /// program.
    pub const NUM_SHARED_ACCOUNTS: usize = 4;

    /// `ReclaimBuffer` instruction data with placeholder addresses and data,
    /// for failure cases where the input is irrelevant.
    pub fn reclaim_buffer_data() -> Vec<u8> {
        let zero = Address::new_from_array([0; 32]);
        Instruction::from(ReclaimBuffer {
            program_id: zero,
            state_pda: zero,
            reclaim_authority: zero,
            reclaim_recipient: zero,
            token_program: zero,
            buffers: &[(zero, zero, 0)],
        })
        .data
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::{reclaim_buffer_data, NUM_SHARED_ACCOUNTS};
    use super::*;
    use crate::fixtures::pubkey_from_seed;
    use crate::instruction::fixtures::{fake_account, fake_sequential_accounts};
    use crate::instruction::tests::{
        assert_readonly_nonsigner, assert_readonly_signer, assert_writable_nonsigner,
    };

    #[test]
    fn reclaim_buffer_input_parses_valid_input() {
        let program_id = pubkey_from_seed("program id");
        let state_pda = pubkey_from_seed("state pda");
        let reclaim_authority = pubkey_from_seed("reclaim authority");
        let reclaim_recipient = pubkey_from_seed("reclaim recipient");
        let token_program = pubkey_from_seed("token program");
        let buffer_pda = pubkey_from_seed("buffer pda");
        let mint = pubkey_from_seed("mint");

        let data = Instruction::from(ReclaimBuffer {
            program_id,
            state_pda,
            reclaim_authority,
            reclaim_recipient,
            token_program,
            buffers: &[(buffer_pda, mint, 1337)],
        })
        .data;
        let token_program = pubkey_from_seed("token program");
        let accounts = [
            fake_account(state_pda),
            fake_account(reclaim_authority),
            fake_account(reclaim_recipient),
            fake_account(token_program),
            fake_account(buffer_pda),
            fake_account(mint),
        ];

        let ReclaimBufferInput {
            state_pda: parsed_state_pda,
            reclaim_authority: parsed_reclaim_authority,
            reclaim_recipient: parsed_reclaim_recipient,
            buffers,
        } = ReclaimBufferInput::parse(&data, &accounts).expect("parse should succeed");

        assert_eq!(*parsed_state_pda.address(), state_pda);
        assert_eq!(*parsed_reclaim_authority.address(), reclaim_authority);
        assert_eq!(*parsed_reclaim_recipient.address(), reclaim_recipient);
        let parsed: Vec<_> = buffers.iter().collect();
        let [Buffer {
            buffer_pda: parsed_buffer_pda,
            mint: parsed_mint,
            burn_limit,
        }] = parsed.as_slice()
        else {
            panic!("expected exactly one parsed buffer");
        };
        assert_eq!(*parsed_buffer_pda.address(), buffer_pda);
        assert_eq!(*parsed_mint.address(), mint);
        assert_eq!(*burn_limit, 1337);
    }

    #[test]
    fn reclaim_buffer_input_parses_multiple_buffers() {
        let program_id = pubkey_from_seed("program id");
        let state_pda = pubkey_from_seed("state pda");
        let reclaim_authority = pubkey_from_seed("reclaim authority");
        let reclaim_recipient = pubkey_from_seed("reclaim recipient");
        let token_program = pubkey_from_seed("token program");
        let buffer_a = pubkey_from_seed("buffer a");
        let mint_a = pubkey_from_seed("mint a");
        let buffer_b = pubkey_from_seed("buffer b");
        let mint_b = pubkey_from_seed("mint b");

        let data = Instruction::from(ReclaimBuffer {
            program_id,
            state_pda,
            token_program,
            reclaim_authority,
            reclaim_recipient,
            buffers: &[(buffer_a, mint_a, 42), (buffer_b, mint_b, 1337)],
        })
        .data;
        let accounts = [
            fake_account(state_pda),
            fake_account(reclaim_authority),
            fake_account(reclaim_recipient),
            fake_account(token_program),
            fake_account(buffer_a),
            fake_account(mint_a),
            fake_account(buffer_b),
            fake_account(mint_b),
        ];

        let ReclaimBufferInput { buffers, .. } =
            ReclaimBufferInput::parse(&data, &accounts).expect("parse should succeed");

        let parsed: Vec<_> = buffers.iter().collect();
        let [Buffer {
            buffer_pda: parsed_buffer_a,
            mint: parsed_mint_a,
            burn_limit: limit_a,
        }, Buffer {
            buffer_pda: parsed_buffer_b,
            mint: parsed_mint_b,
            burn_limit: limit_b,
        }] = parsed.as_slice()
        else {
            panic!("expected exactly two parsed buffers");
        };
        assert_eq!(*parsed_buffer_a.address(), buffer_a);
        assert_eq!(*parsed_mint_a.address(), mint_a);
        assert_eq!(*limit_a, 42);
        assert_eq!(*parsed_buffer_b.address(), buffer_b);
        assert_eq!(*parsed_mint_b.address(), mint_b);
        assert_eq!(*limit_b, 1337);
    }

    #[test]
    fn reclaim_buffer_input_rejects_zero_buffers() {
        let data = reclaim_buffer_data();
        // Only the four shared accounts, no buffer pairs.
        let accounts = fake_sequential_accounts::<NUM_SHARED_ACCOUNTS>();
        assert_eq!(
            ReclaimBufferInput::parse(&data, &accounts).err(),
            Some(ProgramError::NotEnoughAccountKeys),
            "an instruction that reclaims no buffers is rejected",
        );
    }

    #[test]
    fn reclaim_buffer_input_rejects_a_partial_trailing_limit() {
        let mut data = reclaim_buffer_data();
        data.push(0); // trailing byte
        let accounts = fake_sequential_accounts::<{ NUM_SHARED_ACCOUNTS + ACCOUNTS_PER_BUFFER }>();
        assert_eq!(
            ReclaimBufferInput::parse(&data, &accounts).err(),
            Some(ProgramError::InvalidInstructionData),
        );
    }

    #[test]
    fn reclaim_buffer_input_rejects_a_limit_count_that_does_not_match_the_buffers() {
        // Data for one buffer, but two buffer pairs in the accounts.
        let data = reclaim_buffer_data();
        let accounts =
            fake_sequential_accounts::<{ NUM_SHARED_ACCOUNTS + 2 * ACCOUNTS_PER_BUFFER }>();
        assert_eq!(
            ReclaimBufferInput::parse(&data, &accounts).err(),
            Some(ProgramError::InvalidInstructionData),
        );
    }

    #[test]
    fn reclaim_buffer_input_rejects_missing_accounts() {
        let data = reclaim_buffer_data();
        // Fewer than the four shared accounts.
        let accounts = fake_sequential_accounts::<{ NUM_SHARED_ACCOUNTS - 1 }>();
        assert_eq!(
            ReclaimBufferInput::parse(&data, &accounts).err(),
            Some(ProgramError::NotEnoughAccountKeys),
        );
    }

    #[test]
    fn reclaim_buffer_input_rejects_incomplete_pair() {
        let data = reclaim_buffer_data();
        // The shared accounts plus one dangling account that can't form a
        // full pair.
        let accounts =
            fake_sequential_accounts::<{ NUM_SHARED_ACCOUNTS + ACCOUNTS_PER_BUFFER + 1 }>();
        assert_eq!(
            ReclaimBufferInput::parse(&data, &accounts).err(),
            Some(ProgramError::NotEnoughAccountKeys),
        );
    }

    #[test]
    fn instruction_data_has_expected_layout() {
        let program_id = pubkey_from_seed("program id");
        let state_pda = pubkey_from_seed("state pda");
        let reclaim_authority = pubkey_from_seed("reclaim authority");
        let reclaim_recipient = pubkey_from_seed("reclaim recipient");
        let token_program = pubkey_from_seed("token program");
        let buffer_pda = pubkey_from_seed("buffer pda");
        let mint = pubkey_from_seed("mint");
        let Instruction { data, .. } = ReclaimBuffer {
            program_id,
            state_pda,
            reclaim_authority,
            reclaim_recipient,
            token_program,
            buffers: &[(buffer_pda, mint, 0x1337)],
        }
        .into();
        let mut expected = vec![SettlementInstruction::ReclaimBuffer.discriminator()];
        expected.extend_from_slice(&[0x37, 0x13, 0, 0, 0, 0, 0, 0]);
        assert_eq!(data, expected);
    }

    #[test]
    fn single_buffer_has_expected_accounts() {
        let program_id = pubkey_from_seed("program id");
        let state_pda = pubkey_from_seed("state pda");
        let reclaim_authority = pubkey_from_seed("reclaim authority");
        let reclaim_recipient = pubkey_from_seed("reclaim recipient");
        let token_program = pubkey_from_seed("token program");
        let buffer_pda = pubkey_from_seed("buffer pda");
        let mint = pubkey_from_seed("mint");
        let Instruction { accounts, .. } = ReclaimBuffer {
            program_id,
            state_pda,
            reclaim_authority,
            reclaim_recipient,
            token_program,
            buffers: &[(buffer_pda, mint, 0)],
        }
        .into();

        assert_eq!(accounts.len(), 6);
        assert_readonly_nonsigner(&accounts[0], state_pda);
        assert_readonly_signer(&accounts[1], reclaim_authority);
        assert_writable_nonsigner(&accounts[2], reclaim_recipient);
        assert_readonly_nonsigner(&accounts[3], token_program);
        assert_writable_nonsigner(&accounts[4], buffer_pda);
        assert_writable_nonsigner(&accounts[5], mint);
    }

    #[test]
    fn recipient_may_be_the_reclaim_authority_itself() {
        let reclaim_authority = pubkey_from_seed("reclaim authority");
        let Instruction { accounts, .. } = ReclaimBuffer {
            program_id: pubkey_from_seed("program id"),
            state_pda: pubkey_from_seed("state pda"),
            reclaim_authority,
            reclaim_recipient: reclaim_authority,
            token_program: pubkey_from_seed("token program"),
            buffers: &[(pubkey_from_seed("buffer pda"), pubkey_from_seed("mint"), 0)],
        }
        .into();

        assert_readonly_signer(&accounts[1], reclaim_authority);
        assert_writable_nonsigner(&accounts[2], reclaim_authority);
    }

    #[test]
    fn multiple_buffers_append_pairs_after_shared_accounts() {
        let program_id = pubkey_from_seed("program id");
        let state_pda = pubkey_from_seed("state pda");
        let reclaim_authority = pubkey_from_seed("reclaim authority");
        let reclaim_recipient = pubkey_from_seed("reclaim recipient");
        let token_program = pubkey_from_seed("token program");
        let buffer_a = pubkey_from_seed("buffer a");
        let mint_a = pubkey_from_seed("mint a");
        let buffer_b = pubkey_from_seed("buffer b");
        let mint_b = pubkey_from_seed("mint b");
        let Instruction { accounts, .. } = ReclaimBuffer {
            program_id,
            state_pda,
            reclaim_authority,
            reclaim_recipient,
            token_program,
            buffers: &[(buffer_a, mint_a, 0), (buffer_b, mint_b, 0)],
        }
        .into();

        // The shared accounts followed by two (buffer, mint) pairs.
        assert_eq!(
            accounts.len(),
            NUM_SHARED_ACCOUNTS + 2 * ACCOUNTS_PER_BUFFER
        );
        assert_writable_nonsigner(&accounts[4], buffer_a);
        assert_writable_nonsigner(&accounts[5], mint_a);
        assert_writable_nonsigner(&accounts[6], buffer_b);
        assert_writable_nonsigner(&accounts[7], mint_b);
    }

    #[test]
    fn empty_buffers_has_only_shared_accounts() {
        let program_id = pubkey_from_seed("program id");
        let state_pda = pubkey_from_seed("state pda");
        let reclaim_authority = pubkey_from_seed("reclaim authority");
        let reclaim_recipient = pubkey_from_seed("reclaim recipient");
        let token_program = pubkey_from_seed("token program");
        let Instruction { accounts, .. } = ReclaimBuffer {
            program_id,
            state_pda,
            reclaim_authority,
            reclaim_recipient,
            token_program,
            buffers: &[],
        }
        .into();
        assert_eq!(accounts.len(), NUM_SHARED_ACCOUNTS);
    }
}
