//! Off-chain builder and input parsing for the `FinalizeSettle` instruction.

use std::vec;

use solana_account_view::AccountView;
use solana_instruction::{AccountMeta, Instruction};
use solana_program_error::ProgramError;
use solana_pubkey::Pubkey;

use crate::instruction::InstructionInputParsing;
use crate::{SettlementError, SettlementInstruction};

use super::{recover_counterpart, INSTRUCTIONS_SYSVAR_ID, SPL_TOKEN_PROGRAM_ID};

/// Builder for a `FinalizeSettle` instruction pushing the funds described by the
/// parallel lists:
/// - `source_buffers[i]` the buffer token account the funds come from,
/// - `destinations[i]` the account the funds go to (an order's buy token
///   account),
/// - `bumps[i]` the canonical bump of `source_buffers[i]`, so the program
///   re-derives the buffer PDA with one hash instead of searching,
/// - `amounts[i]` the amount to push.
///
/// The slices are assumed to have the same length but this is not enforced in
/// the builder.
///
/// Wire format (with `T` total pushes):
/// `[discriminator=1][begin_ix_index: u16 BE][bump: u8 ×T][amount: u64 BE ×T]`.
/// Accounts:
/// `[instructions_sysvar (R), state_pda (R), token_program (R)]` followed, per
/// push, by `[source_buffer (W), destination (W)]`.
///
/// `FinalizeSettle` validates that each source is the canonical buffer for its
/// destination's mint and executes the transfers; the order correspondence and
/// that each destination is an order's buy token account are `BeginSettle`'s
/// checks. So a push isn't aware of what orders are being paid, just the accounts
/// to move funds between, the source's bump, and the amount. The same buffer may
/// legitimately fund several pushes.
pub struct FinalizeSettle<'a> {
    pub program_id: Pubkey,
    pub state_pda: Pubkey,
    pub begin_ix_index: u16,
    pub source_buffers: &'a [Pubkey],
    pub destinations: &'a [Pubkey],
    pub bumps: &'a [u8],
    pub amounts: &'a [u64],
}

impl FinalizeSettle<'_> {
    pub fn instruction(self) -> Instruction {
        let FinalizeSettle {
            program_id,
            state_pda,
            begin_ix_index,
            source_buffers,
            destinations,
            bumps,
            amounts,
        } = self;

        let data: Vec<u8> = core::iter::once(SettlementInstruction::FinalizeSettle.discriminator())
            .chain(begin_ix_index.to_be_bytes())
            .chain(bumps.iter().copied())
            .chain(amounts.iter().flat_map(|amount| amount.to_be_bytes()))
            .collect();

        let mut accounts = vec![
            AccountMeta::new_readonly(INSTRUCTIONS_SYSVAR_ID, false),
            AccountMeta::new_readonly(state_pda, false),
            AccountMeta::new_readonly(SPL_TOKEN_PROGRAM_ID, false),
        ];
        for (source, destination) in source_buffers.iter().zip(destinations) {
            accounts.push(AccountMeta::new(*source, false));
            accounts.push(AccountMeta::new(*destination, false));
        }

        Instruction {
            program_id,
            accounts,
            data,
        }
    }
}

/// A single fund push parsed from `FinalizeSettle`: move `amount` (big-endian
/// `u64`) from `source_buffer` to `destination`. `bump` is `source_buffer`'s
/// claimed canonical buffer bump, which the program re-derives against.
pub struct Push<'a> {
    pub source_buffer: &'a AccountView,
    pub destination: &'a AccountView,
    pub bump: u8,
    pub amount: &'a [u8; 8],
}

/// Struct storing accounts, bumps, and amounts from parsing the input of
/// `FinalizeSettle`, laid out as a flat list of `[source_buffer, destination]`
/// account pairs parallel to `bumps` and `amounts`. The parsing step that created
/// this struct guarantees `push_accounts.len() == 2 * amounts.len()` and
/// `bumps.len() == amounts.len()`, so the offsets below never run short.
pub struct Pushes<'a> {
    /// `[source_buffer, destination]` per push, flattened.
    push_accounts: &'a [AccountView],
    bumps: &'a [u8],
    /// One push amount (big-endian `u64`) per push, parallel to `bumps`.
    amounts: &'a [[u8; 8]],
}

impl<'a> Pushes<'a> {
    /// Returns an iterator yielding one [`Push`] per step.
    #[allow(
        clippy::arithmetic_side_effects,
        reason = "offsets are bounded by tx limits"
    )]
    pub fn iter(&self) -> impl Iterator<Item = Push<'a>> + '_ {
        let push_count = self.bumps.len();
        let mut i = 0usize;
        let mut account_offset = 0usize;
        std::iter::from_fn(move || {
            if i >= push_count {
                return None;
            }
            let bump = self.bumps[i];
            let amount = &self.amounts[i];
            i += 1;

            let source_buffer = &self.push_accounts[account_offset];
            let destination = &self.push_accounts[account_offset + 1];
            account_offset += 2;

            Some(Push {
                source_buffer,
                destination,
                bump,
                amount,
            })
        })
    }
}

/// Parsed inputs (instruction-data fields + relevant accounts) of a
/// `FinalizeSettle` instruction.
///
/// Strictly the raw extracted form. Fields are read from `instruction_data` and
/// `accounts` but **not validated** against runtime context except confirming
/// that the discriminator matches the desired input and that the number of
/// accounts and amounts is consistent.
pub struct FinalizeSettleInput<'a> {
    pub begin_ix_index: u16,
    pub instructions_sysvar_account: &'a AccountView,
    pub state_pda_account: &'a AccountView,
    pub token_program_account: &'a AccountView,
    pub pushes: Pushes<'a>,
}

/// This implementation defines how instruction bytes and accounts are laid out
/// in the transaction. It's the source of truth for deciding where the data
/// is stored.
impl<'a> InstructionInputParsing<'a> for FinalizeSettleInput<'a> {
    const DISCRIMINATOR: SettlementInstruction = SettlementInstruction::FinalizeSettle;

    fn parse_body(
        instruction_data: &'a [u8],
        accounts: &'a mut [AccountView],
    ) -> Result<Self, ProgramError> {
        let (begin_ix_index, body) = recover_counterpart(instruction_data)?;

        let [instructions_sysvar_account, state_pda_account, token_program_account, push_accounts @ ..] =
            accounts
        else {
            return Err(ProgramError::NotEnoughAccountKeys);
        };

        // The body after the begin index is, per push, a bump byte (all `T`
        // first) then a big-endian `u64` amount: `9 * T` bytes. Unlike
        // `BeginSettle`, there's no explicit count byte, so `T` is recovered as
        // `body.len() / 9`; a body that isn't a whole number of these 9-byte
        // pushes can't be parsed into the push layout at all (and would otherwise
        // leave `bumps` and `amounts` with mismatched lengths).
        if body.len() % 9 != 0 {
            return Err(ProgramError::InvalidInstructionData);
        }
        let push_count = body.len() / 9;
        let (bumps, amount_bytes) = body
            .split_at_checked(push_count)
            .ok_or(ProgramError::InvalidInstructionData)?;
        // `amount_bytes` is `8 * push_count` long, so this splits cleanly into
        // `push_count` whole `u64`s with no remainder.
        let (amounts, _remainder) = amount_bytes.as_chunks::<8>();

        // Each push contributes a source buffer and a destination account, so
        // the push-account count is `2 * T`.
        let expected_accounts = push_count
            .checked_mul(2)
            .ok_or(ProgramError::InvalidInstructionData)?;
        if push_accounts.len() != expected_accounts {
            return Err(SettlementError::AccountCountNotMatchingPushCount.into());
        }

        Ok(Self {
            begin_ix_index,
            instructions_sysvar_account,
            state_pda_account,
            token_program_account,
            pushes: Pushes {
                push_accounts,
                bumps,
                amounts,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instruction::fixtures::{
        fake_account, fake_account_from_array, fake_sequential_accounts,
    };
    use crate::instruction::settle::tests::ix_data;
    use hex_literal::hex;
    use solana_address::Address;

    /// The fixed accounts every `FinalizeSettle` carries before its push
    /// accounts: the instructions sysvar, the settlement state PDA, and the
    /// token program.
    const FIXED_ACCOUNTS: usize = 3;

    #[test]
    fn expected_encoding_finalize_settle_no_pushes() {
        let program_id = Pubkey::new_unique();
        let state_pda = Pubkey::new_unique();
        let Instruction {
            program_id: ix_program_id,
            accounts,
            data,
        } = FinalizeSettle {
            program_id,
            state_pda,
            begin_ix_index: 0x1337,
            source_buffers: &[],
            destinations: &[],
            bumps: &[],
            amounts: &[],
        }
        .instruction();
        assert_eq!(ix_program_id, program_id);
        assert_eq!(
            data,
            [
                &[SettlementInstruction::FinalizeSettle.discriminator()][..],
                &hex!("1337")[..], // counterpart index
            ]
            .concat(),
        );
        // No pushes: the three fixed accounts (sysvar, state PDA, token program).
        assert_eq!(accounts.len(), 3);
        assert_eq!(accounts[0].pubkey, INSTRUCTIONS_SYSVAR_ID);
        assert_eq!(accounts[1].pubkey, state_pda);
        assert_eq!(accounts[2].pubkey, SPL_TOKEN_PROGRAM_ID);
        assert!(accounts
            .iter()
            .all(|meta| !meta.is_writable && !meta.is_signer));
    }

    #[test]
    fn finalize_settle_encodes_pushes() {
        let program_id = Pubkey::new_unique();
        let state_pda = Pubkey::new_unique();
        let source_a = Pubkey::new_from_array([0x01; 32]);
        let dest_a = Pubkey::new_from_array([0x02; 32]);
        let source_b = Pubkey::new_from_array([0x03; 32]);
        let dest_b = Pubkey::new_from_array([0x04; 32]);

        let ix = FinalizeSettle {
            program_id,
            state_pda,
            begin_ix_index: 0x1337,
            source_buffers: &[source_a, source_b],
            destinations: &[dest_a, dest_b],
            bumps: &[0xa1, 0xb1],
            amounts: &[0x0102, 0x0506],
        }
        .instruction();

        assert_eq!(
            ix.data,
            [
                &[SettlementInstruction::FinalizeSettle.discriminator()][..],
                &hex!("1337")[..], // counterpart index
                &[0xa1, 0xb1][..], // bumps
                // amounts
                &hex!("0000000000000102")[..],
                &hex!("0000000000000506")[..],
            ]
            .concat(),
        );

        let actual: Vec<Pubkey> = ix.accounts.iter().map(|meta| meta.pubkey).collect();
        assert_eq!(
            actual,
            vec![
                INSTRUCTIONS_SYSVAR_ID,
                state_pda,
                SPL_TOKEN_PROGRAM_ID,
                source_a,
                dest_a,
                source_b,
                dest_b,
            ],
        );
        // The fixed accounts are read-only; the source buffers and destinations
        // are writable for the transfers.
        let writable: Vec<Pubkey> = ix
            .accounts
            .iter()
            .filter(|meta| meta.is_writable)
            .map(|meta| meta.pubkey)
            .collect();
        assert_eq!(writable, vec![source_a, dest_a, source_b, dest_b]);
        assert!(ix.accounts.iter().all(|meta| !meta.is_signer));
    }

    #[test]
    fn finalize_settle_input_parses_no_pushes() {
        let sysvar = Address::new_from_array([0x42u8; 32]);
        // The state-PDA and token-program slots are reserved but not surfaced.
        let state = Address::new_from_array([0x43u8; 32]);
        let token_program = Address::new_from_array([0x44u8; 32]);
        let mut accounts = [
            fake_account(sysvar),
            fake_account(state),
            fake_account(token_program),
        ];
        let data = ix_data![
            [SettlementInstruction::FinalizeSettle.discriminator()],
            [0x13, 0x37], // begin index
        ];
        let FinalizeSettleInput {
            begin_ix_index,
            instructions_sysvar_account,
            state_pda_account,
            token_program_account,
            pushes,
        } = FinalizeSettleInput::parse(&data, &mut accounts).expect("parse should succeed");
        assert_eq!(begin_ix_index, 0x1337);
        assert_eq!(instructions_sysvar_account.address(), &sysvar);
        assert_eq!(state_pda_account.address(), &state);
        assert_eq!(token_program_account.address(), &token_program);
        assert_eq!(pushes.iter().count(), 0);
    }

    #[test]
    fn finalize_settle_input_parses_pushes() {
        let sysvar = Address::new_from_array([1u8; 32]);
        let state = Address::new_from_array([0xa1u8; 32]);
        let token_program = Address::new_from_array([0xa2u8; 32]);
        // The same source buffer funds both pushes: parsing makes no uniqueness
        // assumption about source buffers.
        let source = Address::new_from_array([3u8; 32]);
        let dest0 = Address::new_from_array([4u8; 32]);
        let dest1 = Address::new_from_array([5u8; 32]);
        let mut accounts = [
            fake_account(sysvar),
            fake_account(state),
            fake_account(token_program),
            fake_account(source),
            fake_account(dest0),
            fake_account(source),
            fake_account(dest1),
        ];
        let data = ix_data![
            [SettlementInstruction::FinalizeSettle.discriminator()],
            [0x13, 0x37], // begin index
            [0xfe, 0xfd], // bumps
            0x1122u64.to_be_bytes(),
            0x3344u64.to_be_bytes(),
        ];

        let FinalizeSettleInput { pushes, .. } =
            FinalizeSettleInput::parse(&data, &mut accounts).expect("parse should succeed");

        let parsed: Vec<(&Address, &Address, u8, u64)> = pushes
            .iter()
            .map(|push| {
                (
                    push.source_buffer.address(),
                    push.destination.address(),
                    push.bump,
                    u64::from_be_bytes(*push.amount),
                )
            })
            .collect();
        assert_eq!(
            parsed,
            vec![
                (&source, &dest0, 0xfe, 0x1122),
                (&source, &dest1, 0xfd, 0x3344),
            ],
        );
    }

    #[test]
    fn finalize_settle_input_parses_many_pushes() {
        const PUSH_COUNT: usize = 16;

        let mut expected: Vec<(Address, Address, u8, u64)> = Vec::new();
        for i in 0..PUSH_COUNT {
            let source = Address::new_from_array([i as u8; 32]);
            let dest = Address::new_from_array([(i + PUSH_COUNT) as u8; 32]);
            let bump = (i + 2 * PUSH_COUNT) as u8;
            let amount = (i as u64) << 8 | 0x07;
            expected.push((source, dest, bump, amount));
        }

        // The three fixed accounts (`[0xff..]`, `[0xfe..]`, `[0xfd..]`) differ
        // from every source/destination address above.
        let mut accounts = vec![
            fake_account_from_array([0xff; 32]),
            fake_account_from_array([0xfe; 32]),
            fake_account_from_array([0xfd; 32]),
        ];
        let mut bump_bytes = Vec::new();
        let mut amount_bytes = Vec::new();
        for &(source, dest, bump, amount) in &expected {
            accounts.push(fake_account(source));
            accounts.push(fake_account(dest));
            bump_bytes.push(bump);
            amount_bytes.extend_from_slice(&amount.to_be_bytes());
        }
        let data = ix_data![
            [SettlementInstruction::FinalizeSettle.discriminator()],
            [0x13, 0x37], // begin index
            bump_bytes,
            amount_bytes,
        ];

        let parsed =
            FinalizeSettleInput::parse(&data, &mut accounts).expect("parse should succeed");
        let pushes: Vec<_> = parsed.pushes.iter().collect();

        assert_eq!(pushes.len(), PUSH_COUNT);
        for (push, (source, dest, bump, amount)) in pushes.iter().zip(&expected) {
            assert_eq!(push.source_buffer.address(), source);
            assert_eq!(push.destination.address(), dest);
            assert_eq!(push.bump, *bump);
            assert_eq!(u64::from_be_bytes(*push.amount), *amount);
        }
    }

    #[test]
    fn finalize_settle_input_rejects_different_discriminator() {
        let data = ix_data![
            [SettlementInstruction::BeginSettle.discriminator()],
            [0, 0], // begin index
        ];
        let mut accounts: [AccountView; 0] = [];
        assert_eq!(
            FinalizeSettleInput::parse(&data, &mut accounts).err(),
            Some(ProgramError::InvalidInstructionData),
        );
    }

    #[test]
    fn finalize_settle_input_rejects_empty_accounts() {
        let data = ix_data![
            [SettlementInstruction::FinalizeSettle.discriminator()],
            [0, 0], // begin index
        ];
        let mut accounts: [AccountView; 0] = [];
        assert_eq!(
            FinalizeSettleInput::parse(&data, &mut accounts).err(),
            Some(ProgramError::NotEnoughAccountKeys),
        );
    }

    #[test]
    fn finalize_settle_input_rejects_account_count_mismatch() {
        // One push (a bump byte then a `u64` amount) needs exactly two push
        // accounts: its source buffer and destination.
        let data = ix_data![
            [SettlementInstruction::FinalizeSettle.discriminator()],
            [0, 0], // begin index
            [0xff], // the push's bump
            0u64.to_be_bytes(),
        ];

        // Too few: only one push account follows the fixed accounts.
        let mut too_few = fake_sequential_accounts::<{ FIXED_ACCOUNTS + 1 }>();
        assert_eq!(
            FinalizeSettleInput::parse(&data, &mut too_few).err(),
            Some(SettlementError::AccountCountNotMatchingPushCount.into()),
        );

        // Too many: three push accounts follow the fixed accounts.
        let mut too_many = fake_sequential_accounts::<{ FIXED_ACCOUNTS + 3 }>();
        assert_eq!(
            FinalizeSettleInput::parse(&data, &mut too_many).err(),
            Some(SettlementError::AccountCountNotMatchingPushCount.into()),
        );
    }

    #[test]
    fn finalize_settle_input_rejects_partial_push() {
        // Four trailing bytes: not a whole number of 9-byte pushes (a bump plus a
        // `u64` amount), so the body can't be parsed into the push layout.
        let mut accounts = fake_sequential_accounts::<FIXED_ACCOUNTS>();
        let data = ix_data![
            [SettlementInstruction::FinalizeSettle.discriminator()],
            [0, 0],                   // begin index
            [0x11, 0x22, 0x33, 0x44], // a partial push (4 bytes)
        ];
        assert_eq!(
            FinalizeSettleInput::parse(&data, &mut accounts).err(),
            Some(ProgramError::InvalidInstructionData),
        );
    }
}
