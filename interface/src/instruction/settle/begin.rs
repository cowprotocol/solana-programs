//! Off-chain builder and input parsing for the `BeginSettle` instruction.

use std::vec;

use borsh::{BorshDeserialize, BorshSerialize};
use solana_instruction::{AccountMeta, Instruction};
use solana_program_error::ProgramError;
use solana_pubkey::Pubkey;

use crate::instruction::InstructionInputParsing;
use crate::{SettlementError, SettlementInstruction};

use super::{INSTRUCTIONS_SYSVAR_ID, SPL_TOKEN_PROGRAM_ID};

/// A single transfer made when settling an order: `amount` tokens sent from the
/// order's sell token account to `destination`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Pull {
    pub destination: Pubkey,
    pub amount: u64,
}

/// Borsh-encoded body of a `BeginSettle` instruction (everything after the
/// one-byte discriminator).
///
/// `finalize_ix_index` is serialized first so its two little-endian bytes sit at
/// a fixed offset right after the discriminator, where
/// [`recover_counterpart`](super::recover_counterpart) reads it out of a peer
/// instruction via introspection without a full decode.
#[derive(BorshSerialize, BorshDeserialize)]
struct BeginSettleData {
    finalize_ix_index: u16,
    auction_id: i64,
    /// Transfer count per order, one entry per settled order.
    counts: Vec<u8>,
    /// Transfer amounts, flattened across orders and handed out `count` at a time.
    amounts: Vec<u64>,
}

/// Builder for a `BeginSettle` instruction settling the orders described by the
/// parallel lists:
/// - `order_pdas[i]` is the canonical order PDA (see [`crate::pda::order`])
/// - `sell_token_accounts[i]` is the order's sell token account,
/// - `pulls[i]` the list of [`Pull`]s to perform from that order's sell token
///   account, each sending an amount from the `i`-th order sell token account
///   to a destination.
///
/// The slices are assumed to have the same length but this is not enforced in
/// the builder.
///
/// Wire format (Borsh, with `n` orders and `T` total transfers):
/// `[discriminator=0][finalize_ix_index: u16 LE][auction_id: i64 LE]
/// [counts: Vec<u8>][amounts: Vec<u64>]`, where each `Vec` is a `u32` LE length
/// prefix followed by its elements.
/// Required accounts: `[instructions_sysvar (R), state_pda (R), token_program
/// (R)]` followed, per order, by `[order_pda (W), sell_token_account (W),
/// destination (W)...]`.
///
/// The program requires the order PDAs to be strictly increasing by address.
/// This builder establishes that ordering for the caller: it sorts the orders by
/// PDA address, carrying each order's sell token account, transfer count,
/// amounts, and destination metas before emitting them.
pub struct BeginSettle<'a> {
    pub program_id: Pubkey,
    pub state_pda: Pubkey,
    pub finalize_ix_index: u16,
    /// The off-chain auction this settlement executes. Carried in the
    /// instruction data so the settlement can be tied back to its auction
    /// off-chain, unused on-chain.
    pub auction_id: i64,
    pub order_pdas: &'a [Pubkey],
    pub sell_token_accounts: &'a [Pubkey],
    pub pulls: &'a [&'a [Pull]],
}

impl From<BeginSettle<'_>> for Instruction {
    fn from(builder: BeginSettle<'_>) -> Self {
        let BeginSettle {
            program_id,
            state_pda,
            finalize_ix_index,
            auction_id,
            order_pdas,
            sell_token_accounts,
            pulls,
        } = builder;

        // Sort the parallel lists together by order PDA address via a shared
        // permutation, so each order keeps its own sell token account and pulls
        // (transfer count, amounts, and destination metas).
        let mut order: Vec<usize> = (0..order_pdas.len()).collect();
        order.sort_by_key(|&i| order_pdas[i]);

        let counts: Vec<u8> = order.iter().map(|&i| pulls[i].len() as u8).collect();
        let amounts: Vec<u64> = order
            .iter()
            .flat_map(|&i| pulls[i].iter())
            .map(|pull| pull.amount)
            .collect();

        let mut data = vec![SettlementInstruction::BeginSettle.discriminator()];
        borsh::to_writer(
            &mut data,
            &BeginSettleData {
                finalize_ix_index,
                auction_id,
                counts,
                amounts,
            },
        )
        .expect("Borsh serialization into a Vec is infallible");

        // Read-only accounts for instruction introspection, settlement state, and
        // the SPL token program.
        let mut accounts = vec![
            AccountMeta::new_readonly(INSTRUCTIONS_SYSVAR_ID, false),
            AccountMeta::new_readonly(state_pda, false),
            AccountMeta::new_readonly(SPL_TOKEN_PROGRAM_ID, false),
        ];
        for &i in &order {
            // Writable account for the order: `BeginSettle` updates its filled
            // amounts (`amount_withdrawn`/`amount_received`).
            accounts.push(AccountMeta::new(order_pdas[i], false));
            // Writable accounts settling the order: its sell token account and the
            // recipient of each transfer.
            accounts.push(AccountMeta::new(sell_token_accounts[i], false));
            for pull in pulls[i] {
                accounts.push(AccountMeta::new(pull.destination, false));
            }
        }

        Instruction {
            program_id,
            accounts,
            data,
        }
    }
}

/// A single settled order, resulted from parsing `BeginSettle`, together with
/// the funds to pull from its sell token account.
pub struct SettledOrder<'a, A> {
    pub order_pda: &'a A,
    pub sell_token_account: &'a A,
    /// Destination accounts for this order's transfers.
    pub destinations: &'a [A],
    /// Transfer amounts, one per destination.
    pub amounts: &'a [u64],
}

/// Struct storing accounts, transfer counts, and amounts from parsing the
/// input of BeginSettle. The parsing step that created this struct guarantees
/// that there aren't missing elements or that they are assigned incorrectly.
///
/// The counts and amounts are owned (Borsh decodes them into `Vec`s); the
/// accounts are borrowed from the parsed instruction.
pub struct SettledOrders<'a, A> {
    /// Order accounts, laid out per order as
    /// [order_accounts_1,  order_accounts_2, ...] where
    /// - each order_accounts is a series of accounts:
    ///   `order_pda_N, sell_token_account_N, destination_N_1, destination_N_2, ..., destination_N_M`
    /// - and M is `counts[N]`
    order_accounts: &'a [A],
    /// One transfer count per order.
    counts: Vec<u8>,
    /// Transfer amounts, shared across orders and handed out `count` at a time.
    amounts: Vec<u64>,
}

impl<A> SettledOrders<'_, A> {
    /// Returns an iterator yielding one [`SettledOrder`] per step.
    #[allow(
        clippy::arithmetic_side_effects,
        reason = "offsets are bounded by tx limits"
    )]
    pub fn iter(&self) -> impl Iterator<Item = SettledOrder<'_, A>> + '_ {
        let counts = self.counts.as_slice();
        let amounts = self.amounts.as_slice();
        // Cursor over the remaining order accounts; each step splits one order's
        // `[order_pda, sell_token_account, destinations..count]` off the front.
        let mut rest: &[A] = self.order_accounts;
        let mut i = 0usize;
        let mut amount_offset = 0usize;
        std::iter::from_fn(move || {
            if i >= counts.len() {
                return None;
            }
            let count = usize::from(counts[i]);
            i += 1;

            let (order_pda, tail) = rest.split_first()?;
            let (sell_token_account, tail) = tail.split_first()?;
            let (destinations, remainder) = tail.split_at(count);
            rest = remainder;

            let amount_end = amount_offset + count;
            let order_amounts = &amounts[amount_offset..amount_end];
            amount_offset = amount_end;

            Some(SettledOrder {
                order_pda,
                sell_token_account,
                destinations,
                amounts: order_amounts,
            })
        })
    }
}

/// Parsed inputs of a `BeginSettle` instruction.
///
/// Strictly the raw extracted form. Fields are read from `instruction_data` and
/// `accounts` but **not validated** against runtime context except confirming
/// that the discriminator matches the desired input and that the number of
/// accounts and transfer counts is consistent.
pub struct BeginSettleInput<'a, A> {
    pub finalize_ix_index: u16,
    /// The off-chain auction this settlement executes, read from the instruction
    /// data. Not validated on-chain: it's carried only so the settlement can be
    /// tied back to its auction off-chain.
    pub auction_id: i64,
    pub instructions_sysvar_account: &'a A,
    pub state_pda_account: &'a A,
    pub token_program_account: &'a A,
    pub orders: SettledOrders<'a, A>,
}

/// This implementation defines how instruction bytes and accounts are laid out
/// in the transaction. It's the source of truth for deciding where the data
/// is stored.
impl<'a, A> InstructionInputParsing<'a, A> for BeginSettleInput<'a, A> {
    const DISCRIMINATOR: SettlementInstruction = SettlementInstruction::BeginSettle;

    fn parse_body(instruction_data: &'a [u8], accounts: &'a [A]) -> Result<Self, ProgramError> {
        // Borsh decodes the whole body and rejects trailing or truncated bytes,
        // so a malformed layout surfaces as `InvalidInstructionData` here.
        let BeginSettleData {
            finalize_ix_index,
            auction_id,
            counts,
            amounts,
        } = BeginSettleData::try_from_slice(instruction_data)
            .map_err(|_| ProgramError::InvalidInstructionData)?;

        let [instructions_sysvar_account, state_pda_account, token_program_account, order_accounts @ ..] =
            accounts
        else {
            return Err(ProgramError::NotEnoughAccountKeys);
        };

        // Each order contributes its order PDA, sell token account, and one
        // destination per transfer, so the order accounts count is `2n + T`.
        let order_count = counts.len();
        let transfer_count = amounts.len();
        let expected_accounts = order_count
            .checked_mul(2)
            .and_then(|two_n| two_n.checked_add(transfer_count))
            .ok_or(ProgramError::InvalidInstructionData)?;
        if order_accounts.len() != expected_accounts {
            return Err(SettlementError::AccountCountNotMatchingOrderCount.into());
        }

        // The transfer counts must sum to `T` so that every destination account
        // is matched to exactly one amount and the order accounts are consumed
        // exactly by the iterator.
        let counts_sum: usize = counts.iter().map(|&c| usize::from(c)).sum();
        if counts_sum != transfer_count {
            return Err(SettlementError::TransferCountMismatch.into());
        }

        Ok(Self {
            finalize_ix_index,
            auction_id,
            instructions_sysvar_account,
            state_pda_account,
            token_program_account,
            orders: SettledOrders {
                order_accounts,
                counts,
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
    use solana_account_view::AccountView;
    use solana_address::Address;

    /// The fixed accounts every `BeginSettle` carries before its order accounts:
    /// the instructions sysvar, the settlement state PDA, and the token program.
    const FIXED_ACCOUNTS: usize = 3;

    /// A placeholder auction id for the tests where its specific value is
    /// incidental. The wire-layout tests spell out the literal bytes instead.
    const AUCTION_ID: i64 = 0x0102_0304_0506_0708;

    #[test]
    fn expected_encoding_begin_settle_no_orders() {
        let program_id = Pubkey::new_unique();
        let state_pda = Pubkey::new_unique();
        let Instruction {
            program_id: ix_program_id,
            accounts,
            data,
        } = BeginSettle {
            program_id,
            state_pda,
            finalize_ix_index: 0x1337,
            auction_id: 0x0102_0304_0506_0708,
            order_pdas: &[],
            sell_token_accounts: &[],
            pulls: &[],
        }
        .into();
        assert_eq!(ix_program_id, program_id);
        assert_eq!(
            data,
            ix_data![
                [SettlementInstruction::BeginSettle.discriminator()],
                hex!("3713"),             // finalize ix index, little endian
                hex!("0807060504030201"), // auction id, little endian
                0u32.to_le_bytes(),       // counts: Borsh Vec length prefix (empty)
                0u32.to_le_bytes(),       // amounts: Borsh Vec length prefix (empty)
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
    fn begin_settle_sorts_orders_by_pda() {
        let program_id = Pubkey::new_unique();
        let state_pda = Pubkey::new_unique();
        // Two orders supplied in descending PDA order. All the other parameters
        // are chosen to sort in the opposite order.
        let high_order_pda = Pubkey::new_from_array([0xbb; 32]);
        let high_sell_token_account = Pubkey::new_from_array([0xa0; 32]);
        let low_order_pda = Pubkey::new_from_array([0xaa; 32]);
        let low_sell_token_account = Pubkey::new_from_array([0xb0; 32]);
        let Instruction { data, accounts, .. } = BeginSettle {
            program_id,
            state_pda,
            finalize_ix_index: 0x1337,
            auction_id: AUCTION_ID,
            order_pdas: &[high_order_pda, low_order_pda],
            sell_token_accounts: &[high_sell_token_account, low_sell_token_account],
            pulls: &[&[], &[]],
        }
        .into();

        assert_eq!(
            data,
            ix_data![
                [SettlementInstruction::BeginSettle.discriminator()],
                hex!("3713"),             // finalize ix index, little endian
                AUCTION_ID.to_le_bytes(), // auction id, little endian
                2u32.to_le_bytes(),       // counts: Borsh Vec length prefix
                [0, 0],                   // transfer counts (both zero)
                0u32.to_le_bytes(),       // amounts: Borsh Vec length prefix (empty)
            ],
        );

        let expected: Vec<Pubkey> = vec![
            INSTRUCTIONS_SYSVAR_ID,
            state_pda,
            SPL_TOKEN_PROGRAM_ID,
            low_order_pda,
            low_sell_token_account,
            high_order_pda,
            high_sell_token_account,
        ];
        let actual: Vec<Pubkey> = accounts.iter().map(|account| account.pubkey).collect();
        assert_eq!(actual, expected);
        // The fixed accounts are read-only; the rest are writable and should be sorted
        let writable: Vec<Pubkey> = accounts
            .iter()
            .filter(|account| account.is_writable)
            .map(|account| account.pubkey)
            .collect();
        assert_eq!(
            writable,
            vec![
                low_order_pda,
                low_sell_token_account,
                high_order_pda,
                high_sell_token_account,
            ],
        );
        assert!(accounts.iter().all(|account| !account.is_signer));
    }

    #[test]
    fn begin_settle_encodes_grouped_transfers() {
        let program_id = Pubkey::new_unique();
        let state_pda = Pubkey::new_unique();
        let order_a = Pubkey::new_from_array([0x01; 32]);
        let sell_a = Pubkey::new_from_array([0x02; 32]);
        let order_b = Pubkey::new_from_array([0x03; 32]);
        let sell_b = Pubkey::new_from_array([0x04; 32]);
        let dest_a0 = Pubkey::new_from_array([0x05; 32]);
        let dest_a1 = Pubkey::new_from_array([0x06; 32]);
        let dest_b0 = Pubkey::new_from_array([0x07; 32]);

        // Order A has two transfers, order B has one.
        let Instruction { data, accounts, .. } = BeginSettle {
            program_id,
            state_pda,
            finalize_ix_index: 0x1337,
            auction_id: AUCTION_ID,
            order_pdas: &[order_a, order_b],
            sell_token_accounts: &[sell_a, sell_b],
            pulls: &[
                &[
                    Pull {
                        destination: dest_a0,
                        amount: 0x0102,
                    },
                    Pull {
                        destination: dest_a1,
                        amount: 0x0304,
                    },
                ],
                &[Pull {
                    destination: dest_b0,
                    amount: 0x0506,
                }],
            ],
        }
        .into();

        assert_eq!(
            data,
            ix_data![
                [SettlementInstruction::BeginSettle.discriminator()],
                hex!("3713"),             // finalize ix index, little endian
                AUCTION_ID.to_le_bytes(), // auction id, little endian
                2u32.to_le_bytes(),       // counts: Borsh Vec length prefix
                [2, 1],                   // counts
                3u32.to_le_bytes(),       // amounts: Borsh Vec length prefix
                // amounts, little endian
                0x0102u64.to_le_bytes(),
                0x0304u64.to_le_bytes(),
                0x0506u64.to_le_bytes(),
            ],
        );

        let expected: Vec<Pubkey> = vec![
            INSTRUCTIONS_SYSVAR_ID,
            state_pda,
            SPL_TOKEN_PROGRAM_ID,
            order_a,
            sell_a,
            dest_a0,
            dest_a1,
            order_b,
            sell_b,
            dest_b0,
        ];
        let actual: Vec<Pubkey> = accounts.iter().map(|account| account.pubkey).collect();
        assert_eq!(actual, expected);
        // The fixed accounts are read-only; the rest are writable
        let writable: Vec<Pubkey> = accounts
            .iter()
            .filter(|account| account.is_writable)
            .map(|account| account.pubkey)
            .collect();
        assert_eq!(
            writable,
            vec![order_a, sell_a, dest_a0, dest_a1, order_b, sell_b, dest_b0],
        );
        assert!(accounts.iter().all(|account| !account.is_signer));
    }

    #[test]
    fn begin_settle_input_parses_valid_input() {
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
            [SettlementInstruction::BeginSettle.discriminator()],
            hex!("3713"),             // finalize index, little-endian
            hex!("0807060504030201"), // auction id, little-endian
            0u32.to_le_bytes(),       // counts: Borsh Vec length prefix (empty)
            0u32.to_le_bytes(),       // amounts: Borsh Vec length prefix (empty)
        ];
        let BeginSettleInput {
            finalize_ix_index,
            auction_id,
            instructions_sysvar_account,
            orders,
            token_program_account,
            state_pda_account,
        } = BeginSettleInput::parse(&data, &accounts).expect("parse should succeed");
        assert_eq!(finalize_ix_index, 0x1337);
        assert_eq!(auction_id, 0x0102_0304_0506_0708);
        assert_eq!(instructions_sysvar_account.address(), &sysvar);
        assert_eq!(orders.iter().count(), 0);
        assert_eq!(token_program_account.address(), &token_program);
        assert_eq!(state_pda_account.address(), &state);
    }

    #[test]
    fn begin_settle_input_rejects_different_discriminator() {
        let data = ix_data![
            [SettlementInstruction::FinalizeSettle.discriminator()],
            [0, 0], // finalize index
        ];
        let accounts: [AccountView; 0] = [];
        assert_eq!(
            BeginSettleInput::parse(&data, &accounts).err(),
            Some(ProgramError::InvalidInstructionData),
        );
    }

    #[test]
    fn begin_settle_input_rejects_empty_accounts() {
        // A well-formed body (so Borsh decoding succeeds) with no accounts, so
        // the fixed-account destructuring is what fails.
        let data = ix_data![
            [SettlementInstruction::BeginSettle.discriminator()],
            hex!("3713"),             // finalize index, little-endian
            AUCTION_ID.to_le_bytes(), // auction id
            0u32.to_le_bytes(),       // counts (empty)
            0u32.to_le_bytes(),       // amounts (empty)
        ];
        let accounts: [AccountView; 0] = [];
        assert_eq!(
            BeginSettleInput::parse(&data, &accounts).err(),
            Some(ProgramError::NotEnoughAccountKeys),
        );
    }

    #[test]
    fn begin_settle_input_rejects_missing_auction_id() {
        // The body carries the finalize index but fewer than eight bytes for the
        // auction id, so Borsh can't decode it.
        let accounts = fake_sequential_accounts::<FIXED_ACCOUNTS>();
        let data = ix_data![
            [SettlementInstruction::BeginSettle.discriminator()],
            [0, 0],             // finalize index
            [0x01, 0x02, 0x03], // a partial (3-byte) auction id
        ];
        assert_eq!(
            BeginSettleInput::parse(&data, &accounts).err(),
            Some(ProgramError::InvalidInstructionData),
        );
    }

    #[test]
    fn begin_settle_input_pairs_orders_with_their_accounts() {
        let sysvar = Address::new_from_array([1u8; 32]);
        let state = Address::new_from_array([0xa1u8; 32]);
        let token_program = Address::new_from_array([0xa2u8; 32]);
        let order_pda = Address::new_from_array([2u8; 32]);
        let sell_token = Address::new_from_array([3u8; 32]);
        let accounts = [
            fake_account(sysvar),
            fake_account(state),
            fake_account(token_program),
            fake_account(order_pda),
            fake_account(sell_token),
        ];
        let data = ix_data![
            [SettlementInstruction::BeginSettle.discriminator()],
            hex!("3713"),             // finalize index, little-endian
            AUCTION_ID.to_le_bytes(), // auction id, little-endian
            1u32.to_le_bytes(),       // counts: one order
            [0x00],                   // that order's transfer count
            0u32.to_le_bytes(),       // amounts (empty)
        ];
        let BeginSettleInput {
            finalize_ix_index,
            auction_id,
            instructions_sysvar_account,
            orders,
            state_pda_account,
            token_program_account,
        } = BeginSettleInput::parse(&data, &accounts).expect("parse should succeed");
        assert_eq!(finalize_ix_index, 0x1337);
        assert_eq!(auction_id, AUCTION_ID);
        assert_eq!(instructions_sysvar_account.address(), &sysvar);
        assert_eq!(token_program_account.address(), &token_program);
        assert_eq!(state_pda_account.address(), &state);

        let mut orders = orders.iter();
        let order = orders.next().expect("one settled order");
        assert_eq!(order.order_pda.address(), &order_pda);
        assert_eq!(order.sell_token_account.address(), &sell_token);
        assert_eq!(order.destinations.len(), 0);
        assert!(orders.next().is_none());
    }

    #[test]
    fn begin_settle_input_parses_transfers() {
        let sysvar = Address::new_from_array([1u8; 32]);
        let state = Address::new_from_array([0xa1u8; 32]);
        let token_program = Address::new_from_array([0xa2u8; 32]);
        let order_pda = Address::new_from_array([2u8; 32]);
        let sell_token = Address::new_from_array([3u8; 32]);
        let dest0 = Address::new_from_array([4u8; 32]);
        let dest1 = Address::new_from_array([5u8; 32]);
        let accounts = [
            fake_account(sysvar),
            fake_account(state),
            fake_account(token_program),
            fake_account(order_pda),
            fake_account(sell_token),
            fake_account(dest0),
            fake_account(dest1),
        ];
        let data = ix_data![
            [SettlementInstruction::BeginSettle.discriminator()],
            hex!("3713"),             // finalize index, little-endian
            AUCTION_ID.to_le_bytes(), // auction id, little-endian
            1u32.to_le_bytes(),       // counts: one order
            [0x02],                   // that order's transfer count
            2u32.to_le_bytes(),       // amounts: two transfers
            0x1122u64.to_le_bytes(),
            0x3344u64.to_le_bytes(),
        ];

        let BeginSettleInput { orders, .. } =
            BeginSettleInput::parse(&data, &accounts).expect("parse should succeed");

        let mut orders = orders.iter();
        let order = orders.next().expect("one settled order");
        assert_eq!(order.order_pda.address(), &order_pda);
        assert_eq!(order.sell_token_account.address(), &sell_token);
        let transfers: Vec<(&Address, u64)> = order
            .destinations
            .iter()
            .zip(order.amounts)
            .map(|(destination, amount)| (destination.address(), *amount))
            .collect();
        assert_eq!(transfers, vec![(&dest0, 0x1122), (&dest1, 0x3344)]);
        assert!(orders.next().is_none());
    }

    #[test]
    fn begin_settle_input_pairs_every_order_with_its_sell_token_account() {
        const ORDER_COUNT: usize = 16;

        let mut expected: Vec<(Address, Address)> = Vec::new();
        for i in 0..ORDER_COUNT {
            let order_pda = Address::new_from_array([i as u8; 32]);
            let sell_token = Address::new_from_array([(i + ORDER_COUNT) as u8; 32]);
            expected.push((order_pda, sell_token));
        }

        // The three fixed accounts (`[0xff..]`, `[0xfe..]`, `[0xfd..]`) differ
        // from every order/token address above.
        let mut accounts = vec![
            fake_account_from_array([0xff; 32]),
            fake_account_from_array([0xfe; 32]),
            fake_account_from_array([0xfd; 32]),
        ];
        for &(order_pda, sell_token) in &expected {
            accounts.push(fake_account(order_pda));
            accounts.push(fake_account(sell_token));
        }
        // Every order has zero transfers.
        let data = ix_data![
            [SettlementInstruction::BeginSettle.discriminator()],
            hex!("3713"),                       // finalize index, little-endian
            AUCTION_ID.to_le_bytes(),           // auction id, little-endian
            (ORDER_COUNT as u32).to_le_bytes(), // counts length
            [0u8; ORDER_COUNT],                 // per-order transfer counts
            0u32.to_le_bytes(),                 // amounts (empty)
        ];

        let parsed = BeginSettleInput::parse(&data, &accounts).expect("parse should succeed");

        let actual: Vec<(Address, Address)> = parsed
            .orders
            .iter()
            .map(|order| {
                assert_eq!(order.destinations.len(), 0);
                (
                    *order.order_pda.address(),
                    *order.sell_token_account.address(),
                )
            })
            .collect();
        assert_eq!(actual, expected);
    }

    #[test]
    fn begin_settle_input_rejects_account_count_mismatch() {
        // The body declares one order with no transfers, which needs exactly two
        // order accounts (its order PDA and sell token account). Only one order
        // account is supplied after the fixed accounts, so the number of accounts
        // doesn't match the `2n + T` the body implies.
        let accounts = fake_sequential_accounts::<{ FIXED_ACCOUNTS + 1 }>();
        let data = ix_data![
            [SettlementInstruction::BeginSettle.discriminator()],
            [0, 0],                   // finalize index
            AUCTION_ID.to_le_bytes(), // auction id
            1u32.to_le_bytes(),       // counts: one order
            [0x00],                   // the order's transfer count
            0u32.to_le_bytes(),       // amounts (empty)
        ];
        assert_eq!(
            BeginSettleInput::parse(&data, &accounts).err(),
            Some(SettlementError::AccountCountNotMatchingOrderCount.into()),
        );
    }

    #[test]
    fn begin_settle_input_rejects_counts_not_summing_to_destinations() {
        // One order whose two destination accounts (plus its order PDA and sell
        // token account) make the lengths recover T = 2 transfers, but the
        // transfer-count byte claims only one.
        let accounts = fake_sequential_accounts::<{ FIXED_ACCOUNTS + 4 }>();
        let data = ix_data![
            [SettlementInstruction::BeginSettle.discriminator()],
            [0, 0],                   // finalize index
            AUCTION_ID.to_le_bytes(), // auction id
            1u32.to_le_bytes(),       // counts: one order
            [0x01],                   // count says one, but two amounts exist
            2u32.to_le_bytes(),       // amounts: two transfers
            0u64.to_le_bytes(),
            0u64.to_le_bytes(),
        ];
        assert_eq!(
            BeginSettleInput::parse(&data, &accounts).err(),
            Some(SettlementError::TransferCountMismatch.into()),
        );
    }
}
