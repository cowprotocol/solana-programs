//! Off-chain builder and input parsing for the `BeginSettle` instruction.

use std::vec;

use solana_account_view::AccountView;
use solana_instruction::{AccountMeta, Instruction};
use solana_program_error::ProgramError;
use solana_pubkey::Pubkey;

use crate::instruction::InstructionInputParsing;
use crate::{SettlementError, SettlementInstruction};

use super::{recover_counterpart, INSTRUCTIONS_SYSVAR_ID, SPL_TOKEN_PROGRAM_ID};

/// A single transfer made when settling an order: `amount` tokens sent from the
/// order's sell token account to `destination`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Pull {
    pub destination: Pubkey,
    pub amount: u64,
}

/// Builder for a `BeginSettle` instruction settling the orders described by the
/// parallel lists:
/// - `order_pdas[i]` is the canonical order PDA (see [`crate::pda::order`])
/// - `order_pda_bumps[i]` is the bump of the canonical order PDA
/// - `sell_token_accounts[i]` is the order's sell token account,
/// - `buy_token_accounts[i]` is the order's buy token account, used as the
///   destination for the sell token account's rent if it's closed once empty,
/// - `pulls[i]` the list of [`Pull`]s to perform from that order's sell token
///   account, each sending an amount from the `i`-th order sell token account
///   to a destination.
///
/// The slices are assumed to have the same length but this is not enforced in
/// the builder.
///
/// Wire format (grouped, with `n` orders and `T` total transfers):
/// `[discriminator=0][finalize_ix_index: u16 BE][n: u8][bump×n][transfer_count×n]
/// [amount: u64 BE ×T]`.
/// Required accounts: `[instructions_sysvar (R), state_pda (R), token_program
/// (R)]` followed, per order, by `[order_pda (R), sell_token_account (W),
/// buy_token_account (W), destination (W)...]`.
///
/// The program requires the order PDAs to be strictly increasing by address.
/// This builder establishes that ordering for the caller: it sorts the orders by
/// PDA address, carrying each order's sell token account, buy token account,
/// bump, transfer count, amounts, and destination metas before emitting them.
pub struct BeginSettle<'a> {
    pub program_id: Pubkey,
    pub state_pda: Pubkey,
    pub finalize_ix_index: u16,
    pub order_pdas: &'a [Pubkey],
    pub order_pda_bumps: &'a [u8],
    pub sell_token_accounts: &'a [Pubkey],
    pub buy_token_accounts: &'a [Pubkey],
    pub pulls: &'a [&'a [Pull]],
}

impl From<BeginSettle<'_>> for Instruction {
    fn from(builder: BeginSettle<'_>) -> Self {
        let BeginSettle {
            program_id,
            state_pda,
            finalize_ix_index,
            order_pdas,
            order_pda_bumps,
            sell_token_accounts,
            buy_token_accounts,
            pulls,
        } = builder;

        // Sort the parallel lists together by order PDA address via a shared
        // permutation, so each order keeps its own sell token account, bump, and
        // pulls (transfer count, amounts, and destination metas).
        let mut order: Vec<usize> = (0..order_pdas.len()).collect();
        order.sort_by_key(|&i| order_pdas[i]);

        let counts: Vec<u8> = order.iter().map(|&i| pulls[i].len() as u8).collect();
        let amounts: Vec<u8> = order
            .iter()
            .flat_map(|&i| pulls[i].iter())
            .flat_map(|pull| pull.amount.to_be_bytes())
            .collect();
        let data = [
            &[SettlementInstruction::BeginSettle.discriminator()][..],
            &finalize_ix_index.to_be_bytes()[..],
            &[order_pdas.len() as u8][..],
            &order
                .iter()
                .map(|&i| order_pda_bumps[i])
                .collect::<Vec<u8>>()[..],
            &counts[..],
            &amounts[..],
        ]
        .concat();

        // Read-only accounts for instruction introspection, settlement state, and
        // the SPL token program.
        let mut accounts = vec![
            AccountMeta::new_readonly(INSTRUCTIONS_SYSVAR_ID, false),
            AccountMeta::new_readonly(state_pda, false),
            AccountMeta::new_readonly(SPL_TOKEN_PROGRAM_ID, false),
        ];
        for &i in &order {
            // Read-only account for the order.
            accounts.push(AccountMeta::new_readonly(order_pdas[i], false));
            // Writable accounts settling the order: its sell token account, its
            // buy token account (the destination if the sell token account is
            // closed once empty), and the recipient of each transfer.
            accounts.push(AccountMeta::new(sell_token_accounts[i], false));
            accounts.push(AccountMeta::new(buy_token_accounts[i], false));
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
pub struct SettledOrder<'a> {
    pub order_pda: &'a AccountView,
    pub sell_token_account: &'a AccountView,
    pub buy_token_account: &'a AccountView,
    pub bump: u8,
    /// Destination accounts for this order's transfers.
    pub destinations: &'a [AccountView],
    /// Transfer amounts (big-endian `u64`), one per destination.
    pub amounts: &'a [[u8; 8]],
}

/// Struct storing accounts, bumps, transfer counts, and amounts from parsing the
/// input of BeginSettle. The parsing step that created this struct guarantees
/// that there aren't missing elements or that they are assigned incorrectly.
pub struct SettledOrders<'a> {
    /// Order accounts, laid out per order as
    /// [order_accounts_1,  order_accounts_2, ...] where
    /// - each order_accounts is a series of accounts:
    ///   `order_pda_N, sell_token_account_N, buy_token_account_N, destination_N_1, destination_N_2, ..., destination_N_M`
    /// - and M is `counts[N]`
    order_accounts: &'a [AccountView],
    bumps: &'a [u8],
    /// One transfer count per order, parallel to `bumps`.
    counts: &'a [u8],
    /// Transfer amounts (big-endian `u64`), shared across orders and
    /// handed out `count` at a time.
    amounts: &'a [[u8; 8]],
}

impl<'a> SettledOrders<'a> {
    /// Returns an iterator yielding one [`SettledOrder`] per step.
    #[allow(
        clippy::arithmetic_side_effects,
        reason = "offsets are bounded by tx limits"
    )]
    pub fn iter(&self) -> impl Iterator<Item = SettledOrder<'a>> + '_ {
        let order_count = self.bumps.len();
        let mut i = 0usize;
        let mut account_offset = 0usize;
        let mut amount_offset = 0usize;
        std::iter::from_fn(move || {
            if i >= order_count {
                return None;
            }
            let bump = self.bumps[i];
            let count = usize::from(self.counts[i]);
            i += 1;

            let order_pda = &self.order_accounts[account_offset];
            let sell_token_account = &self.order_accounts[account_offset + 1];
            let buy_token_account = &self.order_accounts[account_offset + 2];
            let dest_start = account_offset + 3;
            let dest_end = dest_start + count;
            let destinations = &self.order_accounts[dest_start..dest_end];
            account_offset = dest_end;

            let amount_end = amount_offset + count;
            let amounts = &self.amounts[amount_offset..amount_end];
            amount_offset = amount_end;

            Some(SettledOrder {
                order_pda,
                sell_token_account,
                buy_token_account,
                bump,
                destinations,
                amounts,
            })
        })
    }
}

/// Parsed inputs of a `BeginSettle` instruction.
///
/// Strictly the raw extracted form. Fields are read from `instruction_data` and
/// `accounts` but **not validated** against runtime context except confirming
/// that the discriminator matches the desired input and that the number of
/// accounts and bumps is consistent.
pub struct BeginSettleInput<'a> {
    pub finalize_ix_index: u16,
    pub instructions_sysvar_account: &'a AccountView,
    pub state_pda_account: &'a AccountView,
    pub token_program_account: &'a AccountView,
    pub orders: SettledOrders<'a>,
}

/// This implementation defines how instruction bytes and accounts are laid out
/// in the transaction. It's the source of truth for deciding where the data
/// is stored.
impl<'a> InstructionInputParsing<'a> for BeginSettleInput<'a> {
    const DISCRIMINATOR: SettlementInstruction = SettlementInstruction::BeginSettle;

    fn parse_body(
        instruction_data: &'a [u8],
        accounts: &'a mut [AccountView],
    ) -> Result<Self, ProgramError> {
        let (finalize_ix_index, body) = recover_counterpart(instruction_data)?;

        let [instructions_sysvar_account, state_pda_account, token_program_account, order_accounts @ ..] =
            accounts
        else {
            return Err(ProgramError::NotEnoughAccountKeys);
        };

        // The leading byte is the order count `n`; the bumps and counts each take
        // `n` bytes and the remaining bytes are the amounts. `T` (total transfers)
        // is the number of 8-byte amounts. Too few bytes for the order count, the
        // bumps, or the counts, or a trailing amount that isn't a whole `u64`,
        // means the data can't be parsed into the pull layout at all.
        let (&order_count, body) = body
            .split_first()
            .ok_or(ProgramError::InvalidInstructionData)?;
        let order_count = usize::from(order_count);
        let take = |s: &'a [u8]| {
            s.split_at_checked(order_count)
                .ok_or(ProgramError::InvalidInstructionData)
        };
        let (bumps, body) = take(body)?;
        let (counts, amount_bytes) = take(body)?;
        let (amounts, []) = amount_bytes.as_chunks::<8>() else {
            return Err(ProgramError::InvalidInstructionData);
        };
        let transfer_count = amounts.len();

        // Each order contributes its order PDA, sell token account, buy token
        // account, and one destination per transfer, so the order accounts
        // count is `3n + T`.
        let expected_accounts = order_count
            .checked_mul(3)
            .and_then(|three_n| three_n.checked_add(transfer_count))
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
            instructions_sysvar_account,
            state_pda_account,
            token_program_account,
            orders: SettledOrders {
                order_accounts,
                bumps,
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
    use hex_literal::hex;
    use solana_address::Address;

    /// The fixed accounts every `BeginSettle` carries before its order accounts:
    /// the instructions sysvar, the settlement state PDA, and the token program.
    const FIXED_ACCOUNTS: usize = 3;

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
            order_pdas: &[],
            order_pda_bumps: &[],
            sell_token_accounts: &[],
            buy_token_accounts: &[],
            pulls: &[],
        }
        .into();
        assert_eq!(ix_program_id, program_id);
        assert_eq!(
            data,
            [
                &[SettlementInstruction::BeginSettle.discriminator()][..],
                &hex!("1337")[..], // counterpart index
                &[0][..],          // order count
            ]
            .concat(),
        );
        // No orders: the three fixed accounts (sysvar, state PDA, token program).
        assert_eq!(accounts.len(), 3);
        assert_eq!(accounts[0].pubkey, INSTRUCTIONS_SYSVAR_ID);
        assert_eq!(accounts[1].pubkey, state_pda);
        assert_eq!(accounts[2].pubkey, SPL_TOKEN_PROGRAM_ID);
        // They are all generic accounts that don't play an active role in the
        // transaction.
        assert!(accounts
            .iter()
            .all(|account| !account.is_writable && !account.is_signer));
    }

    #[test]
    fn begin_settle_sorts_orders_by_pda() {
        let program_id = Pubkey::new_unique();
        let state_pda = Pubkey::new_unique();
        // Two orders supplied in descending PDA order. All the other parameters
        // are chosen to sort in the opposite order.
        let high_order_pda = Pubkey::new_from_array([0xbb; 32]);
        let high_sell_token_account = Pubkey::new_from_array([0xa0; 32]);
        let high_buy_token_account = Pubkey::new_from_array([0xa2; 32]);
        let high_bump = 0xaa;
        let low_order_pda = Pubkey::new_from_array([0xaa; 32]);
        let low_sell_token_account = Pubkey::new_from_array([0xb0; 32]);
        let low_buy_token_account = Pubkey::new_from_array([0xb2; 32]);
        let low_bump = 0xbb;
        let Instruction { data, accounts, .. } = BeginSettle {
            program_id,
            state_pda,
            finalize_ix_index: 0x1337,
            order_pdas: &[high_order_pda, low_order_pda],
            order_pda_bumps: &[high_bump, low_bump],
            sell_token_accounts: &[high_sell_token_account, low_sell_token_account],
            buy_token_accounts: &[high_buy_token_account, low_buy_token_account],
            pulls: &[&[], &[]],
        }
        .into();

        // Bumps follow the sorted order: the low PDA's bump comes first.
        assert_eq!(
            data,
            [
                &[SettlementInstruction::BeginSettle.discriminator()][..],
                &hex!("1337")[..],          // counterpart index
                &[2][..],                   // order count
                &[low_bump, high_bump][..], // bumps
                &[0, 0][..],                // transfer counts (both zero)
            ]
            .concat(),
        );

        let expected: Vec<Pubkey> = vec![
            INSTRUCTIONS_SYSVAR_ID,
            state_pda,
            SPL_TOKEN_PROGRAM_ID,
            low_order_pda,
            low_sell_token_account,
            low_buy_token_account,
            high_order_pda,
            high_sell_token_account,
            high_buy_token_account,
        ];
        let actual: Vec<Pubkey> = accounts.iter().map(|account| account.pubkey).collect();
        assert_eq!(actual, expected);
        // The fixed accounts and the order PDAs are read-only; the sell and buy
        // token accounts are writable, following the sorted order.
        let writable: Vec<Pubkey> = accounts
            .iter()
            .filter(|account| account.is_writable)
            .map(|account| account.pubkey)
            .collect();
        assert_eq!(
            writable,
            vec![
                low_sell_token_account,
                low_buy_token_account,
                high_sell_token_account,
                high_buy_token_account,
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
        let buy_a = Pubkey::new_from_array([0x08; 32]);
        let order_b = Pubkey::new_from_array([0x03; 32]);
        let sell_b = Pubkey::new_from_array([0x04; 32]);
        let buy_b = Pubkey::new_from_array([0x09; 32]);
        let dest_a0 = Pubkey::new_from_array([0x05; 32]);
        let dest_a1 = Pubkey::new_from_array([0x06; 32]);
        let dest_b0 = Pubkey::new_from_array([0x07; 32]);

        // Order A has two transfers, order B has one.
        let Instruction { data, accounts, .. } = BeginSettle {
            program_id,
            state_pda,
            finalize_ix_index: 0x1337,
            order_pdas: &[order_a, order_b],
            order_pda_bumps: &[0xa1, 0xb1],
            sell_token_accounts: &[sell_a, sell_b],
            buy_token_accounts: &[buy_a, buy_b],
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
            [
                &[SettlementInstruction::BeginSettle.discriminator()][..],
                &hex!("1337")[..], // counterpart index
                &[2][..],          // order count
                &[0xa1, 0xb1][..], // bumps
                &[2, 1][..],       // counts
                // amounts
                &hex!("0000000000000102")[..],
                &hex!("0000000000000304")[..],
                &hex!("0000000000000506")[..],
            ]
            .concat(),
        );

        let expected: Vec<Pubkey> = vec![
            INSTRUCTIONS_SYSVAR_ID,
            state_pda,
            SPL_TOKEN_PROGRAM_ID,
            order_a,
            sell_a,
            buy_a,
            dest_a0,
            dest_a1,
            order_b,
            sell_b,
            buy_b,
            dest_b0,
        ];
        let actual: Vec<Pubkey> = accounts.iter().map(|account| account.pubkey).collect();
        assert_eq!(actual, expected);
        // The fixed accounts and the order PDAs are read-only; sell, buy, and
        // destination accounts are writable for the transfer.
        let writable: Vec<Pubkey> = accounts
            .iter()
            .filter(|account| account.is_writable)
            .map(|account| account.pubkey)
            .collect();
        assert_eq!(
            writable,
            vec![sell_a, buy_a, dest_a0, dest_a1, sell_b, buy_b, dest_b0],
        );
        assert!(accounts.iter().all(|account| !account.is_signer));
    }

    #[test]
    fn begin_settle_input_parses_valid_input() {
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
            [SettlementInstruction::BeginSettle.discriminator()],
            [0x13, 0x37], // finalize index
            [0x00],       // order count
        ];
        let BeginSettleInput {
            finalize_ix_index,
            instructions_sysvar_account,
            orders,
            token_program_account,
            state_pda_account,
        } = BeginSettleInput::parse(&data, &mut accounts).expect("parse should succeed");
        assert_eq!(finalize_ix_index, 0x1337);
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
        let mut accounts: [AccountView; 0] = [];
        assert_eq!(
            BeginSettleInput::parse(&data, &mut accounts).err(),
            Some(ProgramError::InvalidInstructionData),
        );
    }

    #[test]
    fn begin_settle_input_rejects_empty_accounts() {
        let data = ix_data![
            [SettlementInstruction::BeginSettle.discriminator()],
            [0, 0], // finalize index
        ];
        let mut accounts: [AccountView; 0] = [];
        assert_eq!(
            BeginSettleInput::parse(&data, &mut accounts).err(),
            Some(ProgramError::NotEnoughAccountKeys),
        );
    }

    #[test]
    fn begin_settle_input_parses_order_bumps_and_pairs() {
        let sysvar = Address::new_from_array([1u8; 32]);
        let state = Address::new_from_array([0xa1u8; 32]);
        let token_program = Address::new_from_array([0xa2u8; 32]);
        let order_pda = Address::new_from_array([2u8; 32]);
        let sell_token = Address::new_from_array([3u8; 32]);
        let buy_token = Address::new_from_array([4u8; 32]);
        let mut accounts = [
            fake_account(sysvar),
            fake_account(state),
            fake_account(token_program),
            fake_account(order_pda),
            fake_account(sell_token),
            fake_account(buy_token),
        ];
        let data = ix_data![
            [SettlementInstruction::BeginSettle.discriminator()],
            [0x13, 0x37], // finalize index
            [0x01],       // order count
            [0xab],       // one order's bump
            [0x00],       // that order's transfer count
        ];
        let BeginSettleInput {
            finalize_ix_index,
            instructions_sysvar_account,
            orders,
            state_pda_account,
            token_program_account,
        } = BeginSettleInput::parse(&data, &mut accounts).expect("parse should succeed");
        assert_eq!(finalize_ix_index, 0x1337);
        assert_eq!(instructions_sysvar_account.address(), &sysvar);
        assert_eq!(token_program_account.address(), &token_program);
        assert_eq!(state_pda_account.address(), &state);

        let mut orders = orders.iter();
        let order = orders.next().expect("one settled order");
        assert_eq!(order.order_pda.address(), &order_pda);
        assert_eq!(order.sell_token_account.address(), &sell_token);
        assert_eq!(order.buy_token_account.address(), &buy_token);
        assert_eq!(order.bump, 0xab);
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
        let buy_token = Address::new_from_array([6u8; 32]);
        let dest0 = Address::new_from_array([4u8; 32]);
        let dest1 = Address::new_from_array([5u8; 32]);
        let mut accounts = [
            fake_account(sysvar),
            fake_account(state),
            fake_account(token_program),
            fake_account(order_pda),
            fake_account(sell_token),
            fake_account(buy_token),
            fake_account(dest0),
            fake_account(dest1),
        ];
        let data = ix_data![
            [SettlementInstruction::BeginSettle.discriminator()],
            [0x13, 0x37], // finalize index
            [0x01],       // order count
            [0xab],       // bump
            [0x02],       // transfer count
            0x1122u64.to_be_bytes(),
            0x3344u64.to_be_bytes(),
        ];

        let BeginSettleInput { orders, .. } =
            BeginSettleInput::parse(&data, &mut accounts).expect("parse should succeed");

        let mut orders = orders.iter();
        let order = orders.next().expect("one settled order");
        assert_eq!(order.order_pda.address(), &order_pda);
        assert_eq!(order.sell_token_account.address(), &sell_token);
        assert_eq!(order.buy_token_account.address(), &buy_token);
        assert_eq!(order.bump, 0xab);
        let transfers: Vec<(&Address, u64)> = order
            .destinations
            .iter()
            .zip(order.amounts)
            .map(|(destination, amount)| (destination.address(), u64::from_be_bytes(*amount)))
            .collect();
        assert_eq!(transfers, vec![(&dest0, 0x1122), (&dest1, 0x3344)]);
        assert!(orders.next().is_none());
    }

    #[test]
    fn begin_settle_input_pairs_every_order_with_its_bump() {
        const ORDER_COUNT: usize = 16;

        let mut expected: Vec<(Address, Address, Address, u8)> = Vec::new();
        for i in 0..ORDER_COUNT {
            let order_pda = Address::new_from_array([i as u8; 32]);
            let sell_token = Address::new_from_array([(i + ORDER_COUNT) as u8; 32]);
            let buy_token = Address::new_from_array([(i + 2 * ORDER_COUNT) as u8; 32]);
            let bump: u8 = (i + 3 * ORDER_COUNT) as u8;
            expected.push((order_pda, sell_token, buy_token, bump));
        }

        // The three fixed accounts (`[0xff..]`, `[0xfe..]`, `[0xfd..]`) differ
        // from every order/token address above.
        let mut accounts = vec![
            fake_account_from_array([0xff; 32]),
            fake_account_from_array([0xfe; 32]),
            fake_account_from_array([0xfd; 32]),
        ];
        let mut bumps = Vec::new();
        for &(order_pda, sell_token, buy_token, bump) in &expected {
            accounts.push(fake_account(order_pda));
            accounts.push(fake_account(sell_token));
            accounts.push(fake_account(buy_token));
            bumps.push(bump);
        }
        // Grouped data: discriminator, finalize index, order count, all bumps,
        // then all transfer counts (every order has zero transfers).
        let data = ix_data![
            [SettlementInstruction::BeginSettle.discriminator()],
            [0x13, 0x37],        // finalize index
            [ORDER_COUNT as u8], // order count
            bumps,
            [0u8; ORDER_COUNT],
        ];

        let parsed = BeginSettleInput::parse(&data, &mut accounts).expect("parse should succeed");
        let orders: Vec<_> = parsed.orders.iter().collect();

        assert_eq!(orders.len(), ORDER_COUNT);
        for (order, (order_pda, sell_token, buy_token, bump)) in orders.iter().zip(&expected) {
            assert_eq!(order.order_pda.address(), order_pda);
            assert_eq!(order.sell_token_account.address(), sell_token);
            assert_eq!(order.buy_token_account.address(), buy_token);
            assert_eq!(order.bump, *bump);
            assert_eq!(order.destinations.len(), 0);
        }
    }

    #[test]
    fn begin_settle_input_rejects_account_count_mismatch() {
        // The body declares one order with no transfers, which needs exactly
        // three order accounts (its order PDA, sell token account, and buy
        // token account). Only one order account is supplied after the fixed
        // accounts, so the number of accounts doesn't match the `3n + T` the
        // body implies.
        let mut accounts = fake_sequential_accounts::<{ FIXED_ACCOUNTS + 1 }>();
        let data = ix_data![
            [SettlementInstruction::BeginSettle.discriminator()],
            [0, 0], // finalize index
            [0x01], // order count
            [0xab], // the order's bump
            [0x00], // the order's transfer count
        ];
        assert_eq!(
            BeginSettleInput::parse(&data, &mut accounts).err(),
            Some(SettlementError::AccountCountNotMatchingOrderCount.into()),
        );
    }

    #[test]
    fn begin_settle_input_rejects_counts_not_summing_to_destinations() {
        // One order whose two destination accounts (plus its order PDA, sell
        // token account, and buy token account) make the lengths recover T = 2
        // transfers, but the transfer-count byte claims only one.
        let mut accounts = fake_sequential_accounts::<{ FIXED_ACCOUNTS + 5 }>();
        let data = ix_data![
            [SettlementInstruction::BeginSettle.discriminator()],
            [0, 0], // finalize index
            [0x01], // order count
            [0xab], // bump
            [0x01], // count says one, but two amounts/destinations exist
            0u64.to_be_bytes(),
            0u64.to_be_bytes(),
        ];
        assert_eq!(
            BeginSettleInput::parse(&data, &mut accounts).err(),
            Some(SettlementError::TransferCountMismatch.into()),
        );
    }

    #[test]
    fn begin_settle_input_rejects_missing_order_count() {
        // The body carries the finalize index but no order-count byte, so the
        // pull layout can't even begin to be parsed.
        let mut accounts = fake_sequential_accounts::<FIXED_ACCOUNTS>();
        let data = ix_data![
            [SettlementInstruction::BeginSettle.discriminator()],
            [0, 0], // finalize index
        ];
        assert_eq!(
            BeginSettleInput::parse(&data, &mut accounts).err(),
            Some(ProgramError::InvalidInstructionData),
        );
    }

    #[test]
    fn begin_settle_input_rejects_body_too_short_for_bumps() {
        // The order count claims two orders, but only one bump byte follows, so
        // the bumps can't be split off.
        let mut accounts = fake_sequential_accounts::<FIXED_ACCOUNTS>();
        let data = ix_data![
            [SettlementInstruction::BeginSettle.discriminator()],
            [0, 0], // finalize index
            [0x02], // order count: two orders...
            [0xab], // ...but only one bump byte
        ];
        assert_eq!(
            BeginSettleInput::parse(&data, &mut accounts).err(),
            Some(ProgramError::InvalidInstructionData),
        );
    }

    #[test]
    fn begin_settle_input_rejects_body_too_short_for_counts() {
        // One order with its bump, but no transfer-count byte after it, so the
        // counts can't be split off.
        let mut accounts = fake_sequential_accounts::<FIXED_ACCOUNTS>();
        let data = ix_data![
            [SettlementInstruction::BeginSettle.discriminator()],
            [0, 0], // finalize index
            [0x01], // order count
            [0xab], // the order's bump, with no transfer count after it
        ];
        assert_eq!(
            BeginSettleInput::parse(&data, &mut accounts).err(),
            Some(ProgramError::InvalidInstructionData),
        );
    }

    #[test]
    fn begin_settle_input_rejects_partial_amount() {
        // One order with no transfers, but four trailing bytes that don't form a
        // whole `u64` amount.
        let mut accounts = fake_sequential_accounts::<FIXED_ACCOUNTS>();
        let data = ix_data![
            [SettlementInstruction::BeginSettle.discriminator()],
            [0, 0],                   // finalize index
            [0x01],                   // order count
            [0xab],                   // bump
            [0x00],                   // transfer count
            [0x11, 0x22, 0x33, 0x44], // a partial (4-byte) amount
        ];
        assert_eq!(
            BeginSettleInput::parse(&data, &mut accounts).err(),
            Some(ProgramError::InvalidInstructionData),
        );
    }
}
