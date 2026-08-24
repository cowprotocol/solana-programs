//! Off-chain builder and input parsing for the `FinalizeSettle` instruction.

use std::vec;

use borsh::{BorshDeserialize, BorshSerialize};
use solana_instruction::{AccountMeta, Instruction};
use solana_program_error::ProgramError;
use solana_pubkey::Pubkey;

use crate::instruction::InstructionInputParsing;
use crate::{recover_discriminator, SettlementError, SettlementInstruction};

use super::{INSTRUCTIONS_SYSVAR_ID, SPL_TOKEN_PROGRAM_ID};

/// The number of fixed accounts every `FinalizeSettle` carries before its push
/// accounts: the instructions sysvar, the settlement state PDA, and the token
/// program.
pub const FINALIZE_FIXED_ACCOUNTS: usize = 3;

/// Borsh-encoded body of a `FinalizeSettle` instruction (everything after the
/// one-byte discriminator).
///
/// `begin_ix_index` is serialized first so its two little-endian bytes sit at a
/// fixed offset right after the discriminator, where
/// [`recover_counterpart`](super::recover_counterpart) reads it out of a peer
/// instruction via introspection without a full decode.
#[derive(BorshSerialize, BorshDeserialize)]
struct FinalizeSettleData {
    begin_ix_index: u16,
    /// Canonical buffer bump per push.
    bumps: Vec<u8>,
    /// Amount pushed per push, parallel to `bumps`.
    amounts: Vec<u64>,
}

/// The push amounts, in push order, carried by a `FinalizeSettle` instruction,
/// recovered from its full instruction data alone (discriminator included, as
/// returned by instruction introspection).
///
/// This function doesn't otherwise check that the instruction is consistent
/// with a `FinalizeSettle` instruction. For example, it doesn't check that the
/// bump count matches the amount count.
pub fn finalize_push_amounts(instruction_data: &[u8]) -> Result<Vec<u64>, ProgramError> {
    let (_discriminator, body) = recover_discriminator(instruction_data)?;
    let data = FinalizeSettleData::try_from_slice(body)
        .map_err(|_| ProgramError::InvalidInstructionData)?;
    Ok(data.amounts)
}

/// Builder for a `FinalizeSettle` instruction pushing the funds described by the
/// parallel lists:
/// - `source_buffers[i]` is the buffer token account the funds come from,
/// - `destinations[i]` is the account the funds go to (an order's buy token
///   account),
/// - `bumps[i]` is the canonical bump of `source_buffers[i]`, so the program
///   re-derives the buffer PDA with one hash instead of searching,
/// - `amounts[i]` is the amount to push.
///
/// This instruction comes in pair with a `BeginSettle` instruction. The slices
/// are assumed to be built in parallel with the order information specified in
/// `BeginSettle`. Notably, (1) the source buffer is the one corresponding to
/// the order's buy token, and (2) the destinations must be the buy token
/// accounts specified in the corresponding order.
/// The slices are assumed to have the same length but this is not enforced by
/// the builder.
///
/// Wire format (Borsh, with `n` total pushes):
/// `[discriminator=1][begin_ix_index: u16 LE][bumps: Vec<u8>][amounts: Vec<u64>]`,
/// where each `Vec` is a `u32` LE length prefix followed by its elements.
/// Required accounts:
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

impl From<FinalizeSettle<'_>> for Instruction {
    fn from(builder: FinalizeSettle<'_>) -> Self {
        let FinalizeSettle {
            program_id,
            state_pda,
            begin_ix_index,
            source_buffers,
            destinations,
            bumps,
            amounts,
        } = builder;

        let mut data = vec![SettlementInstruction::FinalizeSettle.discriminator()];
        borsh::to_writer(
            &mut data,
            &FinalizeSettleData {
                begin_ix_index,
                bumps: bumps.to_vec(),
                amounts: amounts.to_vec(),
            },
        )
        .expect("Borsh serialization into a Vec is infallible");

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

/// A single fund push parsed from `FinalizeSettle`: move `amount` from
/// `source_buffer` to `destination`. `bump` is `source_buffer`'s claimed
/// canonical buffer bump, which the program re-derives against.
pub struct Push<'a, A> {
    pub source_buffer: &'a A,
    pub destination: &'a A,
    pub bump: u8,
    pub amount: u64,
}

/// Struct storing accounts, bumps, and amounts from parsing the input of
/// `FinalizeSettle`, laid out as a flat list of `[source_buffer, destination]`
/// account pairs parallel to `bumps` and `amounts`. The parsing step that created
/// this struct guarantees `push_accounts.len() == 2 * amounts.len()` and
/// `bumps.len() == amounts.len()`, so the offsets below never run short.
///
/// The bumps and amounts are owned (Borsh decodes them into `Vec`s); the
/// accounts are borrowed from the parsed instruction.
pub struct Pushes<'a, A> {
    /// `[source_buffer, destination]` per push, flattened.
    push_accounts: &'a [A],
    bumps: Vec<u8>,
    /// One push amount per push, parallel to `bumps`.
    amounts: Vec<u64>,
}

impl<A> Pushes<'_, A> {
    /// Returns an iterator yielding one [`Push`] per step.
    #[allow(
        clippy::arithmetic_side_effects,
        reason = "offsets are bounded by tx limits"
    )]
    pub fn iter(&self) -> impl Iterator<Item = Push<'_, A>> + '_ {
        let bumps = self.bumps.as_slice();
        let amounts = self.amounts.as_slice();
        let push_accounts = self.push_accounts;
        let push_count = bumps.len();
        let mut i = 0usize;
        let mut account_offset = 0usize;
        std::iter::from_fn(move || {
            if i >= push_count {
                return None;
            }
            let bump = bumps[i];
            let amount = amounts[i];
            i += 1;

            let source_buffer = &push_accounts[account_offset];
            let destination = &push_accounts[account_offset + 1];
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
/// accounts, bumps, and amounts is consistent.
pub struct FinalizeSettleInput<'a, A> {
    pub begin_ix_index: u16,
    pub instructions_sysvar_account: &'a A,
    pub state_pda_account: &'a A,
    pub token_program_account: &'a A,
    pub pushes: Pushes<'a, A>,
}

/// This implementation defines how instruction bytes and accounts are laid out
/// in the transaction. It's the source of truth for deciding where the data
/// is stored.
impl<'a, A> InstructionInputParsing<'a, A> for FinalizeSettleInput<'a, A> {
    const DISCRIMINATOR: SettlementInstruction = SettlementInstruction::FinalizeSettle;

    fn parse_body(instruction_data: &'a [u8], accounts: &'a [A]) -> Result<Self, ProgramError> {
        // Borsh decodes the whole body and rejects trailing or truncated bytes,
        // so a malformed layout surfaces as `InvalidInstructionData` here.
        let FinalizeSettleData {
            begin_ix_index,
            bumps,
            amounts,
        } = FinalizeSettleData::try_from_slice(instruction_data)
            .map_err(|_| ProgramError::InvalidInstructionData)?;

        // Each push has both a bump and an amount, so their counts must match.
        if bumps.len() != amounts.len() {
            return Err(ProgramError::InvalidInstructionData);
        }

        let [instructions_sysvar_account, state_pda_account, token_program_account, push_accounts @ ..] =
            accounts
        else {
            return Err(ProgramError::NotEnoughAccountKeys);
        };

        // Each push contributes a source buffer and a destination account, so
        // the push-account count is `2 * n`.
        let push_count = amounts.len();
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
    use crate::instruction::tests::assert_readonly_nonsigner;
    use hex_literal::hex;
    use proptest::prelude::*;
    use solana_account_view::AccountView;
    use solana_address::Address;

    #[test]
    fn finalize_fixed_accounts_matches_builder() {
        // A no-push finalize carries exactly the fixed accounts, so the constant
        // must equal the account count the builder emits with no pushes.
        let ix = Instruction::from(FinalizeSettle {
            program_id: Pubkey::new_unique(),
            state_pda: Pubkey::new_unique(),
            begin_ix_index: 0,
            source_buffers: &[],
            destinations: &[],
            bumps: &[],
            amounts: &[],
        });
        assert_eq!(ix.accounts.len(), FINALIZE_FIXED_ACCOUNTS);
    }

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
        .into();
        assert_eq!(ix_program_id, program_id);
        assert_eq!(
            data,
            ix_data![
                [SettlementInstruction::FinalizeSettle.discriminator()],
                hex!("3713"),       // begin ix index (little-endian)
                0u32.to_le_bytes(), // bumps: Borsh Vec length prefix (empty)
                0u32.to_le_bytes(), // amounts: Borsh Vec length prefix (empty)
            ],
        );
        // No orders: the three fixed accounts (sysvar, state PDA, token
        // program). They are all generic accounts that don't play an active
        // role in the base instruction (the state PDA CPI signature isn't
        // relevant here).
        assert_eq!(accounts.len(), 3);
        assert_readonly_nonsigner(&accounts[0], INSTRUCTIONS_SYSVAR_ID);
        assert_readonly_nonsigner(&accounts[1], state_pda);
        assert_readonly_nonsigner(&accounts[2], SPL_TOKEN_PROGRAM_ID);
    }

    #[test]
    fn finalize_settle_encodes_pushes() {
        let program_id = Pubkey::new_unique();
        let state_pda = Pubkey::new_unique();
        let source_a = Pubkey::new_from_array([0x01; 32]);
        let dest_a = Pubkey::new_from_array([0x02; 32]);
        let source_b = Pubkey::new_from_array([0x03; 32]);
        let dest_b = Pubkey::new_from_array([0x04; 32]);

        let ix = Instruction::from(FinalizeSettle {
            program_id,
            state_pda,
            begin_ix_index: 0x1337,
            source_buffers: &[source_a, source_b],
            destinations: &[dest_a, dest_b],
            bumps: &[0xa1, 0xb1],
            amounts: &[0x01020304, 0x05060708],
        });

        assert_eq!(
            ix.data,
            ix_data![
                [SettlementInstruction::FinalizeSettle.discriminator()],
                hex!("3713"),       // begin ix index (little-endian)
                2u32.to_le_bytes(), // bumps: Borsh Vec length prefix
                [0xa1, 0xb1],       // bumps
                2u32.to_le_bytes(), // amounts: Borsh Vec length prefix
                // amounts (little-endian)
                hex!("0403020100000000"),
                hex!("0807060500000000"),
            ],
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
        let accounts = [
            fake_account(sysvar),
            fake_account(state),
            fake_account(token_program),
        ];
        let data = ix_data![
            [SettlementInstruction::FinalizeSettle.discriminator()],
            hex!("3713"),       // begin index, little-endian
            0u32.to_le_bytes(), // bumps (empty)
            0u32.to_le_bytes(), // amounts (empty)
        ];
        let FinalizeSettleInput {
            begin_ix_index,
            instructions_sysvar_account,
            state_pda_account,
            token_program_account,
            pushes,
        } = FinalizeSettleInput::parse(&data, &accounts).expect("parse should succeed");
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
        let accounts = [
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
            hex!("1337"),       // begin index (0x3713), little-endian
            2u32.to_le_bytes(), // bumps length
            [0xfe, 0xfd],       // bumps
            2u32.to_le_bytes(), // amounts length
            0x1122u64.to_le_bytes(),
            0x3344u64.to_le_bytes(),
        ];

        let FinalizeSettleInput { pushes, .. } =
            FinalizeSettleInput::parse(&data, &accounts).expect("parse should succeed");

        let parsed: Vec<(&Address, &Address, u8, u64)> = pushes
            .iter()
            .map(|push| {
                (
                    push.source_buffer.address(),
                    push.destination.address(),
                    push.bump,
                    push.amount,
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

        struct ExpectedPush {
            source: Address,
            dest: Address,
            bump: u8,
            amount: u64,
        }
        let mut expected: Vec<ExpectedPush> = Vec::new();
        for i in 0..PUSH_COUNT {
            let source = Address::new_from_array([i as u8; 32]);
            let dest = Address::new_from_array([(i + PUSH_COUNT) as u8; 32]);
            let bump = (i + 2 * PUSH_COUNT) as u8;
            let amount = u64::from_le_bytes([(i + 3 * PUSH_COUNT) as u8; 8]);
            expected.push(ExpectedPush {
                source,
                dest,
                bump,
                amount,
            });
        }

        // The three fixed accounts (`[0xff..]`, `[0xfe..]`, `[0xfd..]`) differ
        // from every source/destination address above.
        let mut accounts = vec![
            fake_account_from_array([0xff; 32]),
            fake_account_from_array([0xfe; 32]),
            fake_account_from_array([0xfd; 32]),
        ];
        let mut bumps = Vec::new();
        let mut amount_bytes = Vec::new();
        for push in &expected {
            accounts.push(fake_account(push.source));
            accounts.push(fake_account(push.dest));
            bumps.push(push.bump);
            amount_bytes.extend_from_slice(&push.amount.to_le_bytes());
        }
        let data = ix_data![
            [SettlementInstruction::FinalizeSettle.discriminator()],
            hex!("1337"),                      // begin index (0x3713), little-endian
            (PUSH_COUNT as u32).to_le_bytes(), // bumps length
            bumps,
            (PUSH_COUNT as u32).to_le_bytes(), // amounts length
            amount_bytes,
        ];

        let parsed = FinalizeSettleInput::parse(&data, &accounts).expect("parse should succeed");
        let pushes: Vec<_> = parsed.pushes.iter().collect();

        assert_eq!(pushes.len(), PUSH_COUNT);
        for (push, expected) in pushes.iter().zip(&expected) {
            assert_eq!(push.source_buffer.address(), &expected.source);
            assert_eq!(push.destination.address(), &expected.dest);
            assert_eq!(push.bump, expected.bump);
            assert_eq!(push.amount, expected.amount);
        }
    }

    #[test]
    fn finalize_settle_input_rejects_different_discriminator() {
        let data = ix_data![
            [SettlementInstruction::BeginSettle.discriminator()],
            [0, 0], // begin index
        ];
        let accounts: [AccountView; 0] = [];
        assert_eq!(
            FinalizeSettleInput::parse(&data, &accounts).err(),
            Some(ProgramError::InvalidInstructionData),
        );
    }

    #[test]
    fn finalize_settle_input_rejects_empty_accounts() {
        // A well-formed body (so Borsh decoding succeeds) with no accounts, so
        // the fixed-account destructuring is what fails.
        let data = ix_data![
            [SettlementInstruction::FinalizeSettle.discriminator()],
            [0, 0],             // begin index
            0u32.to_le_bytes(), // bumps (empty)
            0u32.to_le_bytes(), // amounts (empty)
        ];
        let accounts: [AccountView; 0] = [];
        assert_eq!(
            FinalizeSettleInput::parse(&data, &accounts).err(),
            Some(ProgramError::NotEnoughAccountKeys),
        );
    }

    #[test]
    fn finalize_settle_input_rejects_account_count_mismatch() {
        // One push (a bump and an amount) needs exactly two push accounts: its
        // source buffer and destination.
        let data = ix_data![
            [SettlementInstruction::FinalizeSettle.discriminator()],
            [13, 37],           // begin index
            1u32.to_le_bytes(), // bumps length
            [0xff],             // the push's bump
            1u32.to_le_bytes(), // amounts length
            31337u64.to_le_bytes(),
        ];

        // Too few: only one push account follows the fixed accounts.
        let too_few = fake_sequential_accounts::<{ FINALIZE_FIXED_ACCOUNTS + 1 }>();
        assert_eq!(
            FinalizeSettleInput::parse(&data, &too_few).err(),
            Some(SettlementError::AccountCountNotMatchingPushCount.into()),
        );

        // Too many: three push accounts follow the fixed accounts.
        let too_many = fake_sequential_accounts::<{ FINALIZE_FIXED_ACCOUNTS + 3 }>();
        assert_eq!(
            FinalizeSettleInput::parse(&data, &too_many).err(),
            Some(SettlementError::AccountCountNotMatchingPushCount.into()),
        );
    }

    #[test]
    fn finalize_settle_input_rejects_mismatched_bumps_and_amounts() {
        // Two bumps but one amount: the pairing is inconsistent and can't be
        // parsed into the push layout.
        let accounts = fake_sequential_accounts::<{ FINALIZE_FIXED_ACCOUNTS + 4 }>();
        let data = ix_data![
            [SettlementInstruction::FinalizeSettle.discriminator()],
            [13, 37],           // begin index
            2u32.to_le_bytes(), // bumps length: two bumps...
            [0xff, 0xee],       // bumps
            1u32.to_le_bytes(), // ...but only one amount
            31337u64.to_le_bytes(),
        ];
        assert_eq!(
            FinalizeSettleInput::parse(&data, &accounts).err(),
            Some(ProgramError::InvalidInstructionData),
        );
    }

    #[test]
    fn finalize_push_amounts_extracts_amounts() {
        let amounts = [0x0102, 0x0304];
        let ix = Instruction::from(FinalizeSettle {
            program_id: Pubkey::new_unique(),
            state_pda: Pubkey::new_unique(),
            begin_ix_index: 0x1337,
            source_buffers: &[Pubkey::new_unique(), Pubkey::new_unique()],
            destinations: &[Pubkey::new_unique(), Pubkey::new_unique()],
            bumps: &[0xa1, 0xb1],
            amounts: &amounts,
        });

        let recovered = finalize_push_amounts(&ix.data).expect("valid finalize data");
        assert_eq!(recovered, amounts);
    }

    #[test]
    fn finalize_push_amounts_handles_no_pushes() {
        let ix = Instruction::from(FinalizeSettle {
            program_id: Pubkey::new_unique(),
            state_pda: Pubkey::new_unique(),
            begin_ix_index: 0,
            source_buffers: &[],
            destinations: &[],
            bumps: &[],
            amounts: &[],
        });
        let amounts = finalize_push_amounts(&ix.data).expect("valid empty finalize data");
        assert!(amounts.is_empty());
    }

    #[test]
    fn finalize_push_amounts_rejects_incorrect_bytes() {
        let mut ix = Instruction::from(FinalizeSettle {
            program_id: Pubkey::new_unique(),
            state_pda: Pubkey::new_unique(),
            begin_ix_index: 0,
            source_buffers: &[Pubkey::new_unique()],
            destinations: &[Pubkey::new_unique()],
            bumps: &[0xff],
            amounts: &[31337],
        });
        ix.data.pop();
        assert_eq!(
            finalize_push_amounts(&ix.data).err(),
            Some(ProgramError::InvalidInstructionData),
        );
    }

    /// An arbitrary well-formed `FinalizeSettle` instruction with `push_count`
    /// pushes.
    fn arb_finalize_instruction(
        push_count: impl Into<prop::collection::SizeRange>,
    ) -> impl Strategy<Value = Instruction> {
        (
            any::<[u8; 32]>().prop_map(Pubkey::new_from_array),
            any::<[u8; 32]>().prop_map(Pubkey::new_from_array),
            any::<u16>(),
            crate::instruction::settle::fixtures::arb_pushes(push_count),
        )
            .prop_map(
                |(
                    program_id,
                    state_pda,
                    begin_ix_index,
                    (source_buffers, destinations, bumps, amounts),
                )| {
                    Instruction::from(FinalizeSettle {
                        program_id,
                        state_pda,
                        begin_ix_index,
                        source_buffers: &source_buffers,
                        destinations: &destinations,
                        bumps: &bumps,
                        amounts: &amounts,
                    })
                },
            )
    }

    proptest! {
        /// For any well-formed `FinalizeSettle`, the amounts `finalize_push_amounts`
        /// recovers from the instruction data alone (the way `BeginSettle` sees it
        /// through introspection) must equal the amounts the full
        /// `FinalizeSettleInput` parser reads from the same data plus its accounts.
        #[test]
        fn finalize_push_amounts_matches_parser(ix in arb_finalize_instruction(0..=16usize)) {
            // Recovered from the instruction data alone.
            let recovered: Vec<u64> =
                finalize_push_amounts(&ix.data).expect("well-formed finalize data");

            // Read by the full parser from the same data plus its accounts.
            let accounts: Vec<AccountView> =
                ix.accounts.iter().map(|meta| fake_account(meta.pubkey)).collect();
            let parsed = FinalizeSettleInput::parse(&ix.data, &accounts)
                .expect("a well-formed finalize parses");
            let parsed_amounts: Vec<u64> =
                parsed.pushes.iter().map(|push| push.amount).collect();

            prop_assert_eq!(recovered, parsed_amounts);
        }
    }
}
