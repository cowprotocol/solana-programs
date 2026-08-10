//! `ReclaimBuffer` instruction builder.
//!
//! Closes one or more buffer PDAs (see [`crate::pda::buffer`]) and sends each
//! closed buffer's rent lamports to a `reclaim_recipient` of the caller's
//! choosing. Only the settlement's configured `reclaim_authority` (see
//! [`crate::data::state::StateAccount`]) may authorize this; the recipient is
//! unconstrained, so the authority can direct the rent anywhere, including to
//! itself.
//!
//! # A buffer that still holds tokens is skipped, not closed
//!
//! An SPL token account can only be closed once its balance is zero, and this
//! instruction neither moves nor destroys a buffer's balance. A funded buffer
//! is therefore left standing and the remaining buffers are still processed —
//! silently, so a `ReclaimBuffer` that closed nothing at all is
//! indistinguishable from one that closed every buffer it was given. Callers
//! that need to know which buffers went away must check whether the accounts
//! still exist afterwards.
//!
//! Wire format: `[discriminator=6]`, 1 byte.
//! Required accounts:
//! `[state_pda (R), reclaim_authority (R,S), reclaim_recipient (W), token_program (R), (buffer_pda (W), mint (R))...]`.

use solana_instruction::{AccountMeta, Instruction};
use solana_program_error::ProgramError;
use solana_pubkey::Pubkey;

use super::InstructionInputParsing;
pub use crate::instruction::create_buffer::SPL_TOKEN_PROGRAM_ID;
use crate::SettlementInstruction;

/// Builder for a `ReclaimBuffer` instruction that closes one buffer per
/// `(buffer_pda, mint)` pair in `buffers`.
///
/// `state_pda` must be the canonical PDA returned by
/// [`crate::pda::state::find_state_pda`]. `reclaim_authority` must sign and
/// must match the `reclaim_authority` recorded in the state PDA's data.
/// `reclaim_recipient` receives every closed buffer's rent lamports and is
/// otherwise unconstrained: the authority picks it freely, and may name itself.
/// Each `buffer_pda` must be the canonical PDA returned by
/// [`crate::pda::buffer::find_buffer_pda`] for its paired `mint`, which is
/// passed only so that derivation can be checked on-chain.
///
/// Buffers that still hold a token balance are skipped without failing the
/// instruction; see the module docs.
pub struct ReclaimBuffer<'a> {
    pub program_id: Pubkey,
    pub state_pda: Pubkey,
    pub reclaim_authority: Pubkey,
    pub reclaim_recipient: Pubkey,
    pub buffers: &'a [(Pubkey, Pubkey)],
}

impl From<ReclaimBuffer<'_>> for Instruction {
    fn from(builder: ReclaimBuffer<'_>) -> Self {
        let mut accounts = vec![
            AccountMeta::new_readonly(builder.state_pda, false),
            // Read-only: the authority only authorizes the close. If it named
            // itself as the recipient, the runtime merges both metas and the
            // account ends up writable regardless.
            AccountMeta::new_readonly(builder.reclaim_authority, true),
            AccountMeta::new(builder.reclaim_recipient, false),
            AccountMeta::new_readonly(SPL_TOKEN_PROGRAM_ID, false),
        ];
        for (buffer_pda, mint) in builder.buffers {
            accounts.push(AccountMeta::new(*buffer_pda, false));
            accounts.push(AccountMeta::new_readonly(*mint, false));
        }
        Instruction {
            program_id: builder.program_id,
            accounts,
            data: vec![SettlementInstruction::ReclaimBuffer.discriminator()],
        }
    }
}

/// Parsed inputs of a `ReclaimBuffer` instruction.
pub struct ReclaimBufferInput<'a, A> {
    pub state_pda: &'a A,
    pub reclaim_authority: &'a A,
    pub reclaim_recipient: &'a A,
    pub token_program: &'a A,
    /// One `[buffer_pda, mint]` pair per buffer to close.
    pub buffers: &'a [[A; 2]],
}

impl<'a, A> InstructionInputParsing<'a, A> for ReclaimBufferInput<'a, A> {
    const DISCRIMINATOR: SettlementInstruction = SettlementInstruction::ReclaimBuffer;

    fn parse_body(instruction_data: &[u8], accounts: &'a mut [A]) -> Result<Self, ProgramError> {
        if !instruction_data.is_empty() {
            return Err(ProgramError::InvalidInstructionData);
        }
        // Accounts: [state_pda (R), reclaim_authority (R,S), reclaim_recipient
        // (W), token_program (R), (buffer_pda (W), mint (R))...]. The four
        // shared accounts come first; the per-buffer pairs follow, one pair per
        // buffer.
        let [state_pda, reclaim_authority, reclaim_recipient, token_program, rest @ ..] = accounts
        else {
            return Err(ProgramError::NotEnoughAccountKeys);
        };
        // Group the trailing accounts into `[buffer_pda, mint]` pairs. Each
        // buffer needs both, so a stray leftover account is a malformed
        // instruction. There must be at least one pair: an instruction that
        // reclaims no buffers is rejected as a likely encoding issue.
        let rest: &'a [A] = rest;
        let (buffers, remainder) = rest.as_chunks::<2>();
        if !remainder.is_empty() || buffers.is_empty() {
            return Err(ProgramError::NotEnoughAccountKeys);
        }

        Ok(Self {
            state_pda,
            reclaim_authority,
            reclaim_recipient,
            token_program,
            buffers,
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

    /// `ReclaimBuffer` instruction data with placeholder addresses, for
    /// failure cases where the input is irrelevant.
    pub fn reclaim_buffer_data() -> Vec<u8> {
        let zero = Address::new_from_array([0; 32]);
        Instruction::from(ReclaimBuffer {
            program_id: zero,
            state_pda: zero,
            reclaim_authority: zero,
            reclaim_recipient: zero,
            buffers: &[(zero, zero)],
        })
        .data
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::{reclaim_buffer_data, NUM_SHARED_ACCOUNTS};
    use super::*;
    use crate::instruction::fixtures::{
        fake_account, fake_account_from_array, fake_sequential_accounts,
    };
    use crate::instruction::tests::{
        assert_readonly_nonsigner, assert_readonly_signer, assert_writable_nonsigner,
    };
    use solana_address::Address;

    #[test]
    fn reclaim_buffer_input_parses_valid_input() {
        let program_id = Address::new_from_array([1; 32]);
        let state_pda = Address::new_from_array([2; 32]);
        let reclaim_authority = Address::new_from_array([3; 32]);
        let reclaim_recipient = Address::new_from_array([6; 32]);
        let buffer_pda = Address::new_from_array([4; 32]);
        let mint = Address::new_from_array([5; 32]);

        let data = Instruction::from(ReclaimBuffer {
            program_id,
            state_pda,
            reclaim_authority,
            reclaim_recipient,
            buffers: &[(buffer_pda, mint)],
        })
        .data;
        let token_program = fake_account_from_array([7; 32]);
        let mut accounts = [
            fake_account(state_pda),
            fake_account(reclaim_authority),
            fake_account(reclaim_recipient),
            token_program,
            fake_account(buffer_pda),
            fake_account(mint),
        ];

        let ReclaimBufferInput {
            state_pda: parsed_state_pda,
            reclaim_authority: parsed_reclaim_authority,
            reclaim_recipient: parsed_reclaim_recipient,
            token_program: parsed_token_program,
            buffers,
        } = ReclaimBufferInput::parse(&data, &mut accounts).expect("parse should succeed");

        assert_eq!(*parsed_state_pda.address(), state_pda);
        assert_eq!(*parsed_reclaim_authority.address(), reclaim_authority);
        assert_eq!(*parsed_reclaim_recipient.address(), reclaim_recipient);
        assert_eq!(
            *parsed_token_program.address(),
            Address::new_from_array([7; 32])
        );
        assert_eq!(buffers.len(), 1, "one buffer is one pair");
        assert_eq!(*buffers[0][0].address(), buffer_pda);
        assert_eq!(*buffers[0][1].address(), mint);
    }

    #[test]
    fn reclaim_buffer_input_parses_multiple_buffers() {
        let program_id = Address::new_from_array([1; 32]);
        let state_pda = Address::new_from_array([2; 32]);
        let reclaim_authority = Address::new_from_array([3; 32]);
        let reclaim_recipient = Address::new_from_array([9; 32]);
        let token_program = Address::new_from_array([4; 32]);
        let buffer_a = Address::new_from_array([5; 32]);
        let mint_a = Address::new_from_array([6; 32]);
        let buffer_b = Address::new_from_array([7; 32]);
        let mint_b = Address::new_from_array([8; 32]);

        let data = Instruction::from(ReclaimBuffer {
            program_id,
            state_pda,
            reclaim_authority,
            reclaim_recipient,
            buffers: &[(buffer_a, mint_a), (buffer_b, mint_b)],
        })
        .data;
        let mut accounts = [
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
            ReclaimBufferInput::parse(&data, &mut accounts).expect("parse should succeed");

        assert_eq!(buffers.len(), 2, "two buffers are two pairs");
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
    fn reclaim_buffer_input_rejects_zero_buffers() {
        let data = reclaim_buffer_data();
        // Only the three shared accounts, no buffer pairs.
        let mut accounts = fake_sequential_accounts::<NUM_SHARED_ACCOUNTS>();
        assert_eq!(
            ReclaimBufferInput::parse(&data, &mut accounts).err(),
            Some(ProgramError::NotEnoughAccountKeys),
            "an instruction that reclaims no buffers is rejected",
        );
    }

    #[test]
    fn reclaim_buffer_input_rejects_long_data() {
        let mut data = reclaim_buffer_data();
        data.push(0); // trailing byte
        assert_eq!(
            ReclaimBufferInput::parse(&data, &mut [0]).err(),
            Some(ProgramError::InvalidInstructionData),
        );
    }

    #[test]
    fn reclaim_buffer_input_rejects_missing_accounts() {
        let data = reclaim_buffer_data();
        // Fewer than the three shared accounts.
        let mut accounts = fake_sequential_accounts::<{ NUM_SHARED_ACCOUNTS - 1 }>();
        assert_eq!(
            ReclaimBufferInput::parse(&data, &mut accounts).err(),
            Some(ProgramError::NotEnoughAccountKeys),
        );
    }

    #[test]
    fn reclaim_buffer_input_rejects_incomplete_pair() {
        let data = reclaim_buffer_data();
        // The shared accounts plus one dangling account that can't form a
        // full pair.
        let mut accounts = fake_sequential_accounts::<{ NUM_SHARED_ACCOUNTS + 1 }>();
        assert_eq!(
            ReclaimBufferInput::parse(&data, &mut accounts).err(),
            Some(ProgramError::NotEnoughAccountKeys),
        );
    }

    #[test]
    fn instruction_data_has_expected_layout() {
        let program_id = Pubkey::new_from_array([1; 32]);
        let state_pda = Pubkey::new_from_array([2; 32]);
        let reclaim_authority = Pubkey::new_from_array([3; 32]);
        let buffer_pda = Pubkey::new_from_array([4; 32]);
        let mint = Pubkey::new_from_array([5; 32]);
        let Instruction { data, .. } = ReclaimBuffer {
            program_id,
            state_pda,
            reclaim_authority,
            reclaim_recipient: Pubkey::new_from_array([6; 32]),
            buffers: &[(buffer_pda, mint)],
        }
        .into();
        assert_eq!(
            data,
            vec![SettlementInstruction::ReclaimBuffer.discriminator()]
        );
    }

    #[test]
    fn single_buffer_has_expected_accounts() {
        let program_id = Pubkey::new_from_array([1; 32]);
        let state_pda = Pubkey::new_from_array([2; 32]);
        let reclaim_authority = Pubkey::new_from_array([3; 32]);
        let reclaim_recipient = Pubkey::new_from_array([6; 32]);
        let buffer_pda = Pubkey::new_from_array([4; 32]);
        let mint = Pubkey::new_from_array([5; 32]);
        let Instruction { accounts, .. } = ReclaimBuffer {
            program_id,
            state_pda,
            reclaim_authority,
            reclaim_recipient,
            buffers: &[(buffer_pda, mint)],
        }
        .into();

        assert_eq!(accounts.len(), 6);
        // The authority only authorizes the close; the recipient is credited
        // the rent, the buffer is closed, and the mint is only read to derive
        // the buffer PDA.
        assert_readonly_nonsigner(&accounts[0], state_pda);
        assert_readonly_signer(&accounts[1], reclaim_authority);
        assert_writable_nonsigner(&accounts[2], reclaim_recipient);
        assert_readonly_nonsigner(&accounts[3], SPL_TOKEN_PROGRAM_ID);
        assert_writable_nonsigner(&accounts[4], buffer_pda);
        assert_readonly_nonsigner(&accounts[5], mint);
    }

    #[test]
    fn recipient_may_be_the_reclaim_authority_itself() {
        let reclaim_authority = Pubkey::new_from_array([3; 32]);
        let Instruction { accounts, .. } = ReclaimBuffer {
            program_id: Pubkey::new_from_array([1; 32]),
            state_pda: Pubkey::new_from_array([2; 32]),
            reclaim_authority,
            reclaim_recipient: reclaim_authority,
            buffers: &[(
                Pubkey::new_from_array([4; 32]),
                Pubkey::new_from_array([5; 32]),
            )],
        }
        .into();

        // Both metas name the same account; the runtime unions their
        // privileges, so the authority ends up a writable signer.
        assert_readonly_signer(&accounts[1], reclaim_authority);
        assert_writable_nonsigner(&accounts[2], reclaim_authority);
    }

    #[test]
    fn multiple_buffers_append_pairs_after_shared_accounts() {
        let program_id = Pubkey::new_from_array([1; 32]);
        let state_pda = Pubkey::new_from_array([2; 32]);
        let reclaim_authority = Pubkey::new_from_array([3; 32]);
        let buffer_a = Pubkey::new_from_array([4; 32]);
        let mint_a = Pubkey::new_from_array([5; 32]);
        let buffer_b = Pubkey::new_from_array([6; 32]);
        let mint_b = Pubkey::new_from_array([7; 32]);
        let Instruction { accounts, .. } = ReclaimBuffer {
            program_id,
            state_pda,
            reclaim_authority,
            reclaim_recipient: Pubkey::new_from_array([8; 32]),
            buffers: &[(buffer_a, mint_a), (buffer_b, mint_b)],
        }
        .into();

        // Four shared accounts followed by two (buffer, mint) pairs.
        assert_eq!(accounts.len(), 4 + 2 * 2);
        assert_writable_nonsigner(&accounts[4], buffer_a);
        assert_readonly_nonsigner(&accounts[5], mint_a);
        assert_writable_nonsigner(&accounts[6], buffer_b);
        assert_readonly_nonsigner(&accounts[7], mint_b);
    }

    #[test]
    fn empty_buffers_has_only_shared_accounts() {
        let program_id = Pubkey::new_from_array([1; 32]);
        let state_pda = Pubkey::new_from_array([2; 32]);
        let reclaim_authority = Pubkey::new_from_array([3; 32]);
        let Instruction { accounts, .. } = ReclaimBuffer {
            program_id,
            state_pda,
            reclaim_authority,
            reclaim_recipient: Pubkey::new_from_array([4; 32]),
            buffers: &[],
        }
        .into();
        assert_eq!(accounts.len(), 4);
    }
}
