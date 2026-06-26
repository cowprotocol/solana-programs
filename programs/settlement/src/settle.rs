//! `BeginSettle`/`FinalizeSettle` instruction handlers.

use std::ops::Deref;

use pinocchio::{
    error::ProgramError,
    sysvars::{clock::Clock, instructions::Instructions, Sysvar},
    AccountView, Address, ProgramResult,
};
use pinocchio_token::state::Account as TokenAccount;
use settlement_interface::{
    data::order::EncodedOrderAccount, instruction::settle::recover_counterpart,
    pda::order::order_pda_signer_seeds, recover_discriminator, Pubkey, SettlementError,
    SettlementInstruction,
};

use crate::processor::{is_cpi_call, InstructionInputParsing};

/// A single settled order, resulted from parsing `BeginSettle`, together with
/// the funds to pull from its sell token account.
struct SettledOrder<'a> {
    order_pda: &'a AccountView,
    sell_token_account: &'a AccountView,
    bump: u8,
    /// Destination accounts for this order's transfers.
    #[allow(dead_code, reason = "consumed by the future pull-execution step")]
    destinations: &'a [AccountView],
    /// Transfer amounts (big-endian `u64`), one per destination.
    #[allow(dead_code, reason = "consumed by the future pull-execution step")]
    amounts: &'a [[u8; 8]],
}

/// Struct storing accounts, bumps, transfer counts, and amounts from parsing the
/// input of BeginSettle. The parsing step that created this struct guarantees
/// that there aren't missing elements or that they are assigned incorrectly.
struct SettledOrders<'a> {
    /// Order accounts, laid out per order as
    /// [order_accounts_1,  order_accounts_2, ...] where
    /// - each order_accounts is a series of accounts:
    ///   `order_pda_N, sell_token_account_N, destination_N_1, destination_N_2, ..., destination_N_M`
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
    fn iter(&self) -> impl Iterator<Item = SettledOrder<'a>> + '_ {
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
            let dest_start = account_offset + 2;
            let dest_end = dest_start + count;
            let destinations = &self.order_accounts[dest_start..dest_end];
            account_offset = dest_end;

            let amount_end = amount_offset + count;
            let amounts = &self.amounts[amount_offset..amount_end];
            amount_offset = amount_end;

            Some(SettledOrder {
                order_pda,
                sell_token_account,
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
struct BeginSettleInput<'a> {
    finalize_ix_index: u16,
    instructions_sysvar_account: &'a AccountView,
    orders: SettledOrders<'a>,
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

        // The state PDA and token-program slots are reserved for the future pull
        // step; they're consumed here to keep the wire format stable but not yet
        // used.
        let [instructions_sysvar_account, _state_pda, _token_program, order_accounts @ ..] =
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

        // Each order contributes its order PDA, sell token account, and one
        // destination per transfer, so the order accounts count is `2n + T`.
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
            instructions_sysvar_account,
            orders: SettledOrders {
                order_accounts,
                bumps,
                counts,
                amounts,
            },
        })
    }
}

/// Parsed inputs (instruction-data fields + relevant accounts) of a
/// `FinalizeSettle` instruction.
///
/// Strictly the raw extracted form. Fields are read from `instruction_data` and
/// `accounts` but **not validated** against runtime context except confirming
/// that the discriminator matches the desired input.
struct FinalizeSettleInput<'a> {
    begin_ix_index: u16,
    instructions_sysvar_account: &'a AccountView,
}

impl<'a> InstructionInputParsing<'a> for FinalizeSettleInput<'a> {
    const DISCRIMINATOR: SettlementInstruction = SettlementInstruction::FinalizeSettle;

    fn parse_body(
        instruction_data: &[u8],
        accounts: &'a mut [AccountView],
    ) -> Result<Self, ProgramError> {
        let (begin_ix_index, _) = recover_counterpart(instruction_data)?;
        let instructions_sysvar_account =
            accounts.first().ok_or(ProgramError::NotEnoughAccountKeys)?;
        Ok(Self {
            begin_ix_index,
            instructions_sysvar_account,
        })
    }
}

pub fn process_begin_settle(
    program_id: &Address,
    accounts: &mut [AccountView],
    instruction_data: &[u8],
) -> ProgramResult {
    if is_cpi_call() {
        return Err(SettlementError::CalledViaCpi.into());
    }

    let input = BeginSettleInput::parse(instruction_data, accounts)?;

    // We use `instructions_sysvar_account` from the input but this could be
    // any address since parsing doesn't validate the input. We rely on the
    // fact that the Pinocchio library already checks that the input account
    // is the expected one.
    let instructions = Instructions::try_from(input.instructions_sysvar_account)?;
    let current_index = instructions.load_current_index();

    // Reciprocity: the input index is a finalize_settle instruction and that
    // instruction points to the current one.
    validate_counterpart(
        program_id,
        &instructions,
        current_index,
        input.finalize_ix_index,
        SettlementInstruction::FinalizeSettle,
    )?;

    validate_no_nested_settlement(
        program_id,
        &instructions,
        current_index,
        input.finalize_ix_index,
    )?;

    validate_settled_orders(program_id, input.orders.iter())?;

    Ok(())
}

/// Reject a `BeginSettle` whose pair encloses another settlement: no
/// `BeginSettle`/`FinalizeSettle` of this program may appear strictly between
/// `current_index` and `finalize_ix_index`. The bounds themselves are excluded.
#[must_use = "skipping the nesting check silently accepts overlapping settle pairs"]
fn validate_no_nested_settlement<T: Deref<Target = [u8]>>(
    program_id: &Address,
    instructions: &Instructions<T>,
    current_index: u16,
    finalize_ix_index: u16,
) -> ProgramResult {
    if finalize_ix_index <= current_index {
        return Err(SettlementError::FinalizeBeforeInitialize.into());
    }

    let search_start = current_index
        .checked_add(1)
        .expect("the finalize index is tested to be larger, no overflow can happen");
    for i in search_start..finalize_ix_index {
        let inner = instructions.load_instruction_at(usize::from(i))?;
        // Skip instructions belonging to a different program.
        if inner.get_program_id() != program_id {
            continue;
        }
        // If it can't recover the discriminator, it's fine: we expect that
        // instruction to fail, but this isn't something that matters here.
        // If the discriminator is valid, then it should not be the start
        // or end of a settlement.
        if let Ok((discriminator, _)) = recover_discriminator(inner.get_instruction_data()) {
            if [
                SettlementInstruction::BeginSettle,
                SettlementInstruction::FinalizeSettle,
            ]
            .contains(&discriminator)
            {
                return Err(SettlementError::BeginFinalizePairOverlap.into());
            }
        }
    }

    Ok(())
}

/// For each order, this checks that the order account was created by this
/// program, that it is still settleable (neither cancelled nor expired), and
/// that its sell token account is the one the order's owner controls.
fn validate_settled_orders<'a>(
    program_id: &Address,
    orders: impl IntoIterator<Item = SettledOrder<'a>>,
) -> ProgramResult {
    // Orders must be passed strictly increasing by address; this rejects
    // duplicates (settling the same order twice) without a separate scan.
    let mut previous: Option<&Address> = None;

    let now = Clock::get()?.unix_timestamp;

    for SettledOrder {
        order_pda,
        sell_token_account,
        bump,
        ..
    } in orders
    {
        // Decode the order body. Reading is safe regardless of who owns the
        // account; the canonical-address check below is what proves provenance.
        // The borrow is released at the end of this block, before any other
        // account is touched.
        let (cancelled, intent, uid) = {
            let data = order_pda.try_borrow()?;
            let bytes: &[u8; EncodedOrderAccount::SIZE] = (&*data)
                .try_into()
                .map_err(|_| ProgramError::InvalidAccountData)?;
            let (account, uid) = EncodedOrderAccount::decode_and_hash(bytes)?;
            (account.cancelled, account.intent, uid)
        };

        // Only at this point we can validate that the PDA is indeed a valid
        // order PDA by seeing its address matches the computed one.
        let derived =
            Address::create_program_address(&order_pda_signer_seeds(&uid, &[bump]), program_id)
                .map_err(|_| SettlementError::OrderNotCanonical)?;
        if &derived != order_pda.address() {
            return Err(SettlementError::OrderNotCanonical.into());
        }

        if previous.is_some_and(|previous| order_pda.address() <= previous) {
            return Err(SettlementError::OrdersNotStrictlyIncreasing.into());
        }
        previous = Some(order_pda.address());

        if cancelled {
            return Err(SettlementError::OrderCancelled.into());
        }

        if now > i64::from(intent.valid_to) {
            return Err(SettlementError::OrderExpired.into());
        }

        // The sell token account must be the one named in the intent, owned by
        // the intent owner: an order can only sell funds its own owner controls.
        if !address_matches_pubkey(sell_token_account.address(), &intent.sell_token_account) {
            return Err(SettlementError::SellTokenAccountMismatch.into());
        }
        // `from_account_view` confirms this is a real SPL token account (right
        // length, owned by the token program) before we read its owner.
        let token_account = TokenAccount::from_account_view(sell_token_account)
            .map_err(|_| SettlementError::SellTokenAccountInvalid)?;
        if !address_matches_pubkey(token_account.owner(), &intent.owner) {
            return Err(SettlementError::SellTokenOwnerMismatch.into());
        }
    }

    Ok(())
}

fn address_matches_pubkey(address: &Address, pubkey: &Pubkey) -> bool {
    address.as_array() == &pubkey.to_bytes()
}

pub fn process_finalize_settle(
    program_id: &Address,
    accounts: &mut [AccountView],
    instruction_data: &[u8],
) -> ProgramResult {
    if is_cpi_call() {
        return Err(SettlementError::CalledViaCpi.into());
    }

    let input = FinalizeSettleInput::parse(instruction_data, accounts)?;
    let instructions = Instructions::try_from(input.instructions_sysvar_account)?;
    let current_index = instructions.load_current_index();

    // Reciprocity: the input index is a begin_settle instruction and that
    // instruction points to the current one.
    validate_counterpart(
        program_id,
        &instructions,
        current_index,
        input.begin_ix_index,
        SettlementInstruction::BeginSettle,
    )

    // Some checks are carried out by `BeginSettle` and we don't repeat them
    // under the assumption that the counterpart exists and, since it's a
    // `BeginSettle`, it performs the checks.
}

/// Load the counterpart instruction at `counterpart_index` and verify it
/// belongs to `program_id`, carries `expected_discriminator`, and points
/// back at the current instruction. Ordering (before/after) is the caller's
/// responsibility.
#[must_use = "skipping the counterpart check silently accepts an invalid settle pair"]
fn validate_counterpart<T: Deref<Target = [u8]>>(
    program_id: &Address,
    instructions: &Instructions<T>,
    current_index: u16,
    counterpart_index: u16,
    expected_discriminator: SettlementInstruction,
) -> ProgramResult {
    let counterpart_ix = instructions
        .load_instruction_at(usize::from(counterpart_index))
        .map_err(|_| SettlementError::MissingCounterpartInstruction)?;
    if counterpart_ix.get_program_id() != program_id {
        return Err(SettlementError::CounterpartIsExternal.into());
    }
    let counterpart_ix_data = counterpart_ix.get_instruction_data();
    let (their_discriminator, remaining_data) = recover_discriminator(counterpart_ix_data)
        .map_err(|_| SettlementError::InvalidCounterpartDiscriminator)?;
    let (their_counterpart_ix, _) = recover_counterpart(remaining_data)
        .map_err(|_| SettlementError::InvalidCounterpartCounterpart)?;
    if their_discriminator != expected_discriminator || their_counterpart_ix != current_index {
        return Err(SettlementError::MismatchedCounterpartDiscriminator.into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::{fake_account, fake_account_from_array, fake_sequential_accounts};
    use ::proptest::{prelude::*, test_runner::TestCaseError};
    use settlement_interface::{
        data::intent::fixtures::arb_order_intent, instruction::settle::INSTRUCTIONS_SYSVAR_ID,
        pda::order::find_order_pda,
    };

    /// The fixed accounts every `BeginSettle` carries before its order accounts:
    /// the instructions sysvar, the settlement state PDA, and the token program.
    const FIXED_ACCOUNTS: usize = 3;

    /// Builds an instruction-data byte vector from a list of field chunks, so a
    /// test can spell out the wire layout one field per line without repeating
    /// the `&[..][..]` slicing. Each chunk is anything sliceable to `[u8]` (a
    /// byte array, a `Vec<u8>`, the result of `to_be_bytes()`, ...).
    macro_rules! ix_data {
        ($($chunk:expr),* $(,)?) => {
            [$(&$chunk[..]),*].concat()
        };
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
        } = BeginSettleInput::parse(&data, &mut accounts).expect("parse should succeed");
        assert_eq!(finalize_ix_index, 0x1337);
        assert_eq!(instructions_sysvar_account.address(), &sysvar);
        assert_eq!(orders.iter().count(), 0);
    }

    #[test]
    fn finalize_settle_input_parses_valid_input() {
        let address = Address::new_from_array([0x42u8; 32]);
        let mut accounts = [fake_account(address)];
        let data = ix_data![
            [SettlementInstruction::FinalizeSettle.discriminator()],
            [0x13, 0x37], // begin index
        ];
        let FinalizeSettleInput {
            begin_ix_index,
            instructions_sysvar_account,
        } = FinalizeSettleInput::parse(&data, &mut accounts).expect("parse should succeed");
        assert_eq!(begin_ix_index, 0x1337);
        assert_eq!(instructions_sysvar_account.address(), &address);
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
    fn begin_settle_input_parses_order_bumps_and_pairs() {
        let sysvar = Address::new_from_array([1u8; 32]);
        let state = Address::new_from_array([0xa1u8; 32]);
        let token_program = Address::new_from_array([0xa2u8; 32]);
        let order_pda = Address::new_from_array([2u8; 32]);
        let sell_token = Address::new_from_array([3u8; 32]);
        let mut accounts = [
            fake_account(sysvar),
            fake_account(state),
            fake_account(token_program),
            fake_account(order_pda),
            fake_account(sell_token),
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
        } = BeginSettleInput::parse(&data, &mut accounts).expect("parse should succeed");
        assert_eq!(finalize_ix_index, 0x1337);
        assert_eq!(instructions_sysvar_account.address(), &sysvar);

        let mut orders = orders.iter();
        let order = orders.next().expect("one settled order");
        assert_eq!(order.order_pda.address(), &order_pda);
        assert_eq!(order.sell_token_account.address(), &sell_token);
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
        let dest0 = Address::new_from_array([4u8; 32]);
        let dest1 = Address::new_from_array([5u8; 32]);
        let mut accounts = [
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

        let mut expected: Vec<(Address, Address, u8)> = Vec::new();
        for i in 0..ORDER_COUNT {
            let order_pda = Address::new_from_array([i as u8; 32]);
            let sell_token = Address::new_from_array([(i + ORDER_COUNT) as u8; 32]);
            let bump: u8 = (i + 2 * ORDER_COUNT) as u8;
            expected.push((order_pda, sell_token, bump));
        }

        // The three fixed accounts (`[0xff..]`, `[0xfe..]`, `[0xfd..]`) differ
        // from every order/token address above.
        let mut accounts = vec![
            fake_account_from_array([0xff; 32]),
            fake_account_from_array([0xfe; 32]),
            fake_account_from_array([0xfd; 32]),
        ];
        let mut bumps = Vec::new();
        for &(order_pda, sell_token, bump) in &expected {
            accounts.push(fake_account(order_pda));
            accounts.push(fake_account(sell_token));
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
        for (order, (order_pda, sell_token, bump)) in orders.iter().zip(&expected) {
            assert_eq!(order.order_pda.address(), order_pda);
            assert_eq!(order.sell_token_account.address(), sell_token);
            assert_eq!(order.bump, *bump);
            assert_eq!(order.destinations.len(), 0);
        }
    }

    #[test]
    fn begin_settle_input_rejects_account_count_mismatch() {
        // The body declares one order with no transfers, which needs exactly two
        // order accounts (its order PDA and sell token account). Only one order
        // account is supplied after the fixed accounts, so the number of accounts
        // doesn't match the `2n + T` the body implies.
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
        // One order whose two destination accounts (plus its order PDA and sell
        // token account) make the lengths recover T = 2 transfers, but the
        // transfer-count byte claims only one.
        let mut accounts = fake_sequential_accounts::<{ FIXED_ACCOUNTS + 4 }>();
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

    #[test]
    fn finalize_settle_input_ignores_extra_parameters() {
        let first_address = Address::new_from_array([1u8; 32]);
        let second_address = Address::new_from_array([2u8; 32]);
        let mut accounts = [fake_account(first_address), fake_account(second_address)];
        let data = ix_data![
            [SettlementInstruction::FinalizeSettle.discriminator()],
            [0x13, 0x37], // begin index
            [42],         // extra
        ];
        let FinalizeSettleInput {
            begin_ix_index,
            instructions_sysvar_account,
        } = FinalizeSettleInput::parse(&data, &mut accounts).expect("parse should succeed");
        assert_eq!(begin_ix_index, 0x1337);
        assert_eq!(instructions_sysvar_account.address(), &first_address);
    }

    proptest! {
        // The client's `begin_settle` builder derives each order's PDA from its
        // intent and forwards to the interface builder so that the on-chain
        // parser recovers exactly those orders.
        #[test]
        fn client_begin_settle_derives_orders_from_intents(
            finalize_ix_index in any::<u16>(),
            intents in prop::collection::vec(arb_order_intent(), 1..=5),
        ) {
            let program_id = Pubkey::new_unique();
            // No pulls here: this test only checks that orders are derived and
            // laid out correctly.
            let orders: Vec<settlement_client::instructions::SettledOrder> = intents
                .iter()
                .map(|intent| settlement_client::instructions::SettledOrder { intent, pulls: &[] })
                .collect();
            let ix = settlement_client::instructions::begin_settle(
                &program_id,
                finalize_ix_index,
                &orders,
            );

            // Expected orders: each intent's canonical PDA paired with its sell
            // token account and bump, sorted by PDA address (the builder's order).
            let mut expected: Vec<(Pubkey, Pubkey, u8)> = intents
                .iter()
                .map(|intent| {
                    let (order_pda, bump) = find_order_pda(&program_id, &intent.uid());
                    (order_pda, intent.sell_token_account, bump)
                })
                .collect();
            expected.sort_by_key(|(order_pda, _, _)| *order_pda);


            let mut accounts: Vec<AccountView> = ix
                .accounts
                .iter()
                .map(|meta| fake_account_from_array(meta.pubkey.to_bytes()))
                .collect();
            let parsed = BeginSettleInput::parse(&ix.data, &mut accounts)
                .map_err(|e| TestCaseError::fail(format!("parse failed: {e:?}")))?;

            prop_assert_eq!(parsed.finalize_ix_index, finalize_ix_index);
            prop_assert!(address_matches_pubkey(
                parsed.instructions_sysvar_account.address(),
                &INSTRUCTIONS_SYSVAR_ID,
            ));

            let parsed_orders: Vec<_> = parsed.orders.iter().collect();
            prop_assert_eq!(parsed_orders.len(), expected.len());
            for (order, (order_pda, sell_token, bump)) in parsed_orders.iter().zip(&expected) {
                prop_assert!(address_matches_pubkey(order.order_pda.address(), order_pda));
                prop_assert!(address_matches_pubkey(
                    order.sell_token_account.address(),
                    sell_token,
                ));
                prop_assert_eq!(order.bump, *bump);
            }
        }
    }
}
