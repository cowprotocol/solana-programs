//! `ReclaimBuffer` instruction builder.
//!
//! Closes one or more buffer PDAs (see [`crate::pda::buffer`]) and forwards
//! their proceeds to the settlement's configured `receiver` (see
//! [`crate::data::state::StateAccount`]): each buffer's rent lamports go
//! directly to `receiver`, and any leftover token balance is burned (a
//! non-native SPL token account can only be closed once its balance is
//! zero).
//!
//! # Warning: any tokens left in a buffer are destroyed
//!
//! This instruction **burns** whatever balance remains in each buffer before
//! closing it — those tokens are gone permanently, they are not routed to
//! `receiver` or anyone else. Only reclaim a buffer once you expect its
//! balance to be zero or dust that is intentionally being written off (e.g.
//! unroutable remainders left behind by settlement). Reclaiming a buffer that
//! still holds a meaningful balance destroys those funds.
//!
//! Wire format: `[discriminator=6]`, 1 byte.
//! Required accounts:
//! `[state_pda (R), receiver (W,S), token_program (R), (buffer_pda (W), mint (W))...]`.

use solana_account_view::AccountView;
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
/// [`crate::pda::state::find_state_pda`]. `receiver` must sign and must match
/// the `receiver` recorded in the state PDA's data; it receives every closed
/// buffer's rent lamports. Each `buffer_pda` must be the canonical PDA
/// returned by [`crate::pda::buffer::find_buffer_pda`] for its paired `mint`.
/// `mint` must be writable: any leftover balance in the buffer is burned,
/// which updates the mint's supply.
///
/// **Any token balance still held by a buffer at reclaim time is burned, not
/// recovered.** Only use this for buffers expected to be empty, or to write
/// off dust/dead balances — never on a buffer that might still hold funds of
/// useful value.
pub struct ReclaimBuffer<'a> {
    pub program_id: Pubkey,
    pub state_pda: Pubkey,
    pub receiver: Pubkey,
    pub buffers: &'a [(Pubkey, Pubkey)],
}

impl From<ReclaimBuffer<'_>> for Instruction {
    fn from(builder: ReclaimBuffer<'_>) -> Self {
        let mut accounts = vec![
            AccountMeta::new_readonly(builder.state_pda, false),
            AccountMeta::new(builder.receiver, true),
            AccountMeta::new_readonly(SPL_TOKEN_PROGRAM_ID, false),
        ];
        for (buffer_pda, mint) in builder.buffers {
            accounts.push(AccountMeta::new(*buffer_pda, false));
            accounts.push(AccountMeta::new(*mint, false));
        }
        Instruction {
            program_id: builder.program_id,
            accounts,
            data: vec![SettlementInstruction::ReclaimBuffer.discriminator()],
        }
    }
}

/// Parsed inputs of a `ReclaimBuffer` instruction.
pub struct ReclaimBufferInput<'a> {
    pub state_pda: &'a AccountView,
    pub receiver: &'a AccountView,
    pub token_program: &'a AccountView,
    /// One `[buffer_pda, mint]` pair per buffer to
    /// close.
    pub buffers: &'a [[AccountView; 2]],
}

impl<'a> InstructionInputParsing<'a> for ReclaimBufferInput<'a> {
    const DISCRIMINATOR: SettlementInstruction = SettlementInstruction::ReclaimBuffer;

    fn parse_body(
        instruction_data: &[u8],
        accounts: &'a mut [AccountView],
    ) -> Result<Self, ProgramError> {
        if !instruction_data.is_empty() {
            return Err(ProgramError::InvalidInstructionData);
        }
        // Accounts: [state_pda (R), receiver (W,S), token_program (R),
        // (buffer_pda (W), mint (W))...]. The three shared accounts come
        // first; the per-buffer pairs follow, one pair per buffer.
        let [state_pda, receiver, token_program, rest @ ..] = accounts else {
            return Err(ProgramError::NotEnoughAccountKeys);
        };
        // Group the trailing accounts into `[buffer_pda, mint]` pairs. Each
        // buffer needs both, so a stray leftover account is a malformed
        // instruction. There must be at least one pair: an instruction that
        // reclaims no buffers is rejected as a likely encoding issue.
        let rest: &'a [AccountView] = rest;
        let (buffers, remainder) = rest.as_chunks::<2>();
        if !remainder.is_empty() || buffers.is_empty() {
            return Err(ProgramError::NotEnoughAccountKeys);
        }

        Ok(Self {
            state_pda,
            receiver,
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
    /// reclaimed: state PDA, receiver, and token program.
    pub const NUM_SHARED_ACCOUNTS: usize = 3;

    /// `ReclaimBuffer` instruction data with placeholder addresses, for
    /// failure cases where the input is irrelevant.
    pub fn reclaim_buffer_data() -> Vec<u8> {
        let zero = Address::new_from_array([0; 32]);
        Instruction::from(ReclaimBuffer {
            program_id: zero,
            state_pda: zero,
            receiver: zero,
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
    use solana_address::Address;

    #[test]
    fn reclaim_buffer_input_parses_valid_input() {
        let program_id = Address::new_from_array([1; 32]);
        let state_pda = Address::new_from_array([2; 32]);
        let receiver = Address::new_from_array([3; 32]);
        let buffer_pda = Address::new_from_array([4; 32]);
        let mint = Address::new_from_array([5; 32]);

        let data = Instruction::from(ReclaimBuffer {
            program_id,
            state_pda,
            receiver,
            buffers: &[(buffer_pda, mint)],
        })
        .data;
        let token_program = fake_account_from_array([7; 32]);
        let mut accounts = [
            fake_account(state_pda),
            fake_account(receiver),
            token_program,
            fake_account(buffer_pda),
            fake_account(mint),
        ];

        let ReclaimBufferInput {
            state_pda: parsed_state_pda,
            receiver: parsed_receiver,
            token_program: parsed_token_program,
            buffers,
        } = ReclaimBufferInput::parse(&data, &mut accounts).expect("parse should succeed");

        assert_eq!(*parsed_state_pda.address(), state_pda);
        assert_eq!(*parsed_receiver.address(), receiver);
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
        let receiver = Address::new_from_array([3; 32]);
        let token_program = Address::new_from_array([4; 32]);
        let buffer_a = Address::new_from_array([5; 32]);
        let mint_a = Address::new_from_array([6; 32]);
        let buffer_b = Address::new_from_array([7; 32]);
        let mint_b = Address::new_from_array([8; 32]);

        let data = Instruction::from(ReclaimBuffer {
            program_id,
            state_pda,
            receiver,
            buffers: &[(buffer_a, mint_a), (buffer_b, mint_b)],
        })
        .data;
        let mut accounts = [
            fake_account(state_pda),
            fake_account(receiver),
            fake_account(token_program),
            fake_account(buffer_a),
            fake_account(mint_a),
            fake_account(buffer_b),
            fake_account(mint_b),
        ];

        let ReclaimBufferInput { buffers, .. } =
            ReclaimBufferInput::parse(&data, &mut accounts).expect("parse should succeed");

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
        let data = vec![SettlementInstruction::ReclaimBuffer.discriminator()];
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
            ReclaimBufferInput::parse(&data, &mut []).err(),
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
        // Three shared accounts plus one dangling account that can't form a
        // full pair.
        let mut accounts = fake_sequential_accounts::<4>();
        assert_eq!(
            ReclaimBufferInput::parse(&data, &mut accounts).err(),
            Some(ProgramError::NotEnoughAccountKeys),
        );
    }

    #[test]
    fn instruction_data_has_expected_layout() {
        let program_id = Pubkey::new_from_array([1; 32]);
        let state_pda = Pubkey::new_from_array([2; 32]);
        let receiver = Pubkey::new_from_array([3; 32]);
        let buffer_pda = Pubkey::new_from_array([4; 32]);
        let mint = Pubkey::new_from_array([5; 32]);
        let Instruction { data, .. } = ReclaimBuffer {
            program_id,
            state_pda,
            receiver,
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
        let receiver = Pubkey::new_from_array([3; 32]);
        let buffer_pda = Pubkey::new_from_array([4; 32]);
        let mint = Pubkey::new_from_array([5; 32]);
        let Instruction { accounts, .. } = ReclaimBuffer {
            program_id,
            state_pda,
            receiver,
            buffers: &[(buffer_pda, mint)],
        }
        .into();

        assert_eq!(accounts.len(), 5);
        // state_pda: read-only, not signer
        assert_eq!(accounts[0].pubkey, state_pda);
        assert!(!accounts[0].is_writable);
        assert!(!accounts[0].is_signer);
        // receiver: writable, signer
        assert_eq!(accounts[1].pubkey, receiver);
        assert!(accounts[1].is_writable);
        assert!(accounts[1].is_signer);
        // token program: read-only
        assert_eq!(accounts[2].pubkey, SPL_TOKEN_PROGRAM_ID);
        assert!(!accounts[2].is_writable);
        assert!(!accounts[2].is_signer);
        // buffer_pda: writable, not signer
        assert_eq!(accounts[3].pubkey, buffer_pda);
        assert!(accounts[3].is_writable);
        assert!(!accounts[3].is_signer);
        // mint: writable, not signer (burning updates the mint's supply)
        assert_eq!(accounts[4].pubkey, mint);
        assert!(accounts[4].is_writable);
        assert!(!accounts[4].is_signer);
    }

    #[test]
    fn multiple_buffers_append_pairs_after_shared_accounts() {
        let program_id = Pubkey::new_from_array([1; 32]);
        let state_pda = Pubkey::new_from_array([2; 32]);
        let receiver = Pubkey::new_from_array([3; 32]);
        let buffer_a = Pubkey::new_from_array([4; 32]);
        let mint_a = Pubkey::new_from_array([5; 32]);
        let buffer_b = Pubkey::new_from_array([6; 32]);
        let mint_b = Pubkey::new_from_array([7; 32]);
        let Instruction { accounts, .. } = ReclaimBuffer {
            program_id,
            state_pda,
            receiver,
            buffers: &[(buffer_a, mint_a), (buffer_b, mint_b)],
        }
        .into();

        // Three shared accounts followed by two (buffer, mint) pairs.
        assert_eq!(accounts.len(), 3 + 2 * 2);
        assert_eq!(accounts[3].pubkey, buffer_a);
        assert_eq!(accounts[4].pubkey, mint_a);
        assert_eq!(accounts[5].pubkey, buffer_b);
        assert_eq!(accounts[6].pubkey, mint_b);
    }

    #[test]
    fn empty_buffers_has_only_shared_accounts() {
        let program_id = Pubkey::new_from_array([1; 32]);
        let state_pda = Pubkey::new_from_array([2; 32]);
        let receiver = Pubkey::new_from_array([3; 32]);
        let Instruction { accounts, .. } = ReclaimBuffer {
            program_id,
            state_pda,
            receiver,
            buffers: &[],
        }
        .into();
        assert_eq!(accounts.len(), 3);
    }
}
