//! `WithdrawNativeSol` instruction builder and parser.
//!
//! Moves lamports out of the native SOL buffer (see
//! [`crate::pda::buffer::NATIVE_SOL_BUFFER_PDA`]) to a `recipient` of the
//! caller's choosing. Only the settlement's configured
//! `settlement_owned_order_authority` (see
//! [`crate::data::state::StateAccount`]) may authorize this.
//!
//! The buffer's rent is never withdrawable: the buffer must stay alive, since
//! only `Initialize` can create it.
//!
//! Wire format: `[discriminator=12, amount (u64 LE)]`, 9 bytes.
//! Required accounts:
//! `[state_pda (R), authority (R,S), native_sol_buffer (W), recipient (W)]`.

use core::mem::size_of;

use solana_instruction::{AccountMeta, Instruction};
use solana_program_error::ProgramError;
use solana_pubkey::Pubkey;

use super::InstructionInputParsing;
use crate::SettlementInstruction;

/// Builder for a `WithdrawNativeSol` instruction.
///
/// `state_pda` must be [`crate::pda::state::STATE_PDA`]. `authority` must sign
/// and must match the `settlement_owned_order_authority` recorded in the state
/// PDA's data. `native_sol_buffer` must be
/// [`crate::pda::buffer::NATIVE_SOL_BUFFER_PDA`]. `recipient` receives
/// `amount` lamports.
pub struct WithdrawNativeSol {
    pub program_id: Pubkey,
    pub state_pda: Pubkey,
    pub authority: Pubkey,
    pub native_sol_buffer: Pubkey,
    pub recipient: Pubkey,
    pub amount: u64,
}

impl From<WithdrawNativeSol> for Instruction {
    fn from(builder: WithdrawNativeSol) -> Self {
        let mut data = vec![SettlementInstruction::WithdrawNativeSol.discriminator()];
        data.extend_from_slice(&builder.amount.to_le_bytes());
        Instruction {
            program_id: builder.program_id,
            accounts: vec![
                AccountMeta::new_readonly(builder.state_pda, false),
                AccountMeta::new_readonly(builder.authority, true),
                AccountMeta::new(builder.native_sol_buffer, false),
                AccountMeta::new(builder.recipient, false),
            ],
            data,
        }
    }
}

/// Parsed inputs of a `WithdrawNativeSol` instruction.
pub struct WithdrawNativeSolInput<'a, A> {
    pub state_pda: &'a A,
    pub authority: &'a A,
    pub native_sol_buffer: &'a A,
    pub recipient: &'a A,
    pub amount: u64,
}

impl<'a, A> InstructionInputParsing<'a, A> for WithdrawNativeSolInput<'a, A> {
    const DISCRIMINATOR: SettlementInstruction = SettlementInstruction::WithdrawNativeSol;

    fn parse_body(instruction_data: &[u8], accounts: &'a [A]) -> Result<Self, ProgramError> {
        let amount: &[u8; size_of::<u64>()] = instruction_data
            .try_into()
            .map_err(|_| ProgramError::InvalidInstructionData)?;
        let amount = u64::from_le_bytes(*amount);

        // Accounts: [state_pda (R), authority (R,S), native_sol_buffer (W),
        // recipient (W)].
        let [state_pda, authority, native_sol_buffer, recipient, ..] = accounts else {
            return Err(ProgramError::NotEnoughAccountKeys);
        };

        Ok(Self {
            state_pda,
            authority,
            native_sol_buffer,
            recipient,
            amount,
        })
    }
}

/// Test scaffolding for `WithdrawNativeSol` parsing and handling, shared by
/// this crate's tests and the settlement program's via the `test-fixtures`
/// feature.
#[cfg(any(test, feature = "test-fixtures"))]
pub mod fixtures {
    use solana_address::Address;

    use super::{Instruction, WithdrawNativeSol};

    /// Number of accounts `WithdrawNativeSol` expects: state PDA, authority,
    /// native SOL buffer, and recipient.
    pub const NUM_ACCOUNTS: usize = 4;

    /// `WithdrawNativeSol` instruction data withdrawing `amount`, with
    /// placeholder addresses since they don't end up in the data.
    pub fn withdraw_native_sol_data(amount: u64) -> Vec<u8> {
        let zero = Address::new_from_array([0; 32]);
        Instruction::from(WithdrawNativeSol {
            program_id: zero,
            state_pda: zero,
            authority: zero,
            native_sol_buffer: zero,
            recipient: zero,
            amount,
        })
        .data
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::{withdraw_native_sol_data, NUM_ACCOUNTS};
    use super::*;
    use crate::fixtures::pubkey_from_seed;
    use crate::instruction::fixtures::{fake_account, fake_sequential_accounts};
    use crate::instruction::tests::{
        assert_readonly_nonsigner, assert_readonly_signer, assert_writable_nonsigner,
    };

    fn sample() -> WithdrawNativeSol {
        WithdrawNativeSol {
            program_id: pubkey_from_seed("program id"),
            state_pda: pubkey_from_seed("state pda"),
            authority: pubkey_from_seed("authority"),
            native_sol_buffer: pubkey_from_seed("native sol buffer"),
            recipient: pubkey_from_seed("recipient"),
            amount: 0x0102_0304_0506_0708,
        }
    }

    #[test]
    fn withdraw_native_sol_input_parses_valid_input() {
        let builder = sample();
        let accounts = [
            fake_account(builder.state_pda),
            fake_account(builder.authority),
            fake_account(builder.native_sol_buffer),
            fake_account(builder.recipient),
        ];
        let (state_pda, authority, native_sol_buffer, recipient, amount) = (
            builder.state_pda,
            builder.authority,
            builder.native_sol_buffer,
            builder.recipient,
            builder.amount,
        );
        let data = Instruction::from(builder).data;

        let parsed = WithdrawNativeSolInput::parse(&data, &accounts).expect("parse should succeed");

        assert_eq!(*parsed.state_pda.address(), state_pda);
        assert_eq!(*parsed.authority.address(), authority);
        assert_eq!(*parsed.native_sol_buffer.address(), native_sol_buffer);
        assert_eq!(*parsed.recipient.address(), recipient);
        assert_eq!(parsed.amount, amount);
    }

    #[test]
    fn withdraw_native_sol_input_rejects_short_data() {
        let mut data = withdraw_native_sol_data(1);
        data.pop();
        let accounts = fake_sequential_accounts::<NUM_ACCOUNTS>();
        assert_eq!(
            WithdrawNativeSolInput::parse(&data, &accounts).err(),
            Some(ProgramError::InvalidInstructionData),
        );
    }

    #[test]
    fn withdraw_native_sol_input_rejects_long_data() {
        let mut data = withdraw_native_sol_data(1);
        data.push(0);
        let accounts = fake_sequential_accounts::<NUM_ACCOUNTS>();
        assert_eq!(
            WithdrawNativeSolInput::parse(&data, &accounts).err(),
            Some(ProgramError::InvalidInstructionData),
        );
    }

    #[test]
    fn withdraw_native_sol_input_rejects_missing_accounts() {
        let data = withdraw_native_sol_data(1);
        let accounts = fake_sequential_accounts::<{ NUM_ACCOUNTS - 1 }>();
        assert_eq!(
            WithdrawNativeSolInput::parse(&data, &accounts).err(),
            Some(ProgramError::NotEnoughAccountKeys),
        );
    }

    #[test]
    fn instruction_data_has_expected_layout() {
        let Instruction { data, .. } = sample().into();
        assert_eq!(
            data,
            [
                SettlementInstruction::WithdrawNativeSol.discriminator(),
                8,
                7,
                6,
                5,
                4,
                3,
                2,
                1,
            ]
        );
    }

    #[test]
    fn instruction_has_expected_accounts() {
        let builder = sample();
        let expected = [
            builder.state_pda,
            builder.authority,
            builder.native_sol_buffer,
            builder.recipient,
        ];
        let program_id = builder.program_id;
        let ix = Instruction::from(builder);

        assert_eq!(ix.program_id, program_id);
        assert_eq!(ix.accounts.len(), NUM_ACCOUNTS);
        assert_readonly_nonsigner(&ix.accounts[0], expected[0]);
        assert_readonly_signer(&ix.accounts[1], expected[1]);
        assert_writable_nonsigner(&ix.accounts[2], expected[2]);
        assert_writable_nonsigner(&ix.accounts[3], expected[3]);
    }
}
