//! `BeginSettle`/`FinalizeSettle` instruction handlers.

use std::ops::Deref;

use pinocchio::{
    error::ProgramError, sysvars::instructions::Instructions, AccountView, Address, ProgramResult,
};
use pinocchio_token::state::Account as TokenAccount;
use settlement_interface::{
    data::order::EncodedOrderAccount, instruction::settle::recover_counterpart,
    pda::order::order_pda_signer_seeds, recover_discriminator, Pubkey, SettlementError,
    SettlementInstruction,
};

use crate::processor::InstructionInputParsing;

/// A single settled order, resulted from parsing `BeginSettle`.
struct SettledOrder<'a> {
    order_pda: &'a AccountView,
    sell_token_account: &'a AccountView,
    bump: u8,
}

/// Struct storing account and bumps from parsing the input of BeginSettle.
/// We want parsing to provide the data in an ergonomic format. This struct
/// implements `IntoIterator`, yielding all available information for each
/// order without further parsing steps. The parsing step that created this
/// struct guarantees that there aren't missing elements or that they are
/// assigned incorrectly.
struct SettledOrders<'a> {
    accounts: &'a [[AccountView; 2]],
    bumps: &'a [u8],
}

impl<'a> IntoIterator for SettledOrders<'a> {
    type Item = SettledOrder<'a>;
    type IntoIter = std::iter::Map<
        std::iter::Zip<std::slice::Iter<'a, [AccountView; 2]>, std::slice::Iter<'a, u8>>,
        fn((&'a [AccountView; 2], &'a u8)) -> SettledOrder<'a>,
    >;

    fn into_iter(self) -> Self::IntoIter {
        // A non-capturing closure coerced to a function pointer so the iterator
        // type stays nameable in `IntoIter` above.
        let pair_to_order: fn((&'a [AccountView; 2], &'a u8)) -> SettledOrder<'a> =
            |([order_pda, sell_token_account], &bump)| SettledOrder {
                order_pda,
                sell_token_account,
                bump,
            };
        self.accounts.iter().zip(self.bumps).map(pair_to_order)
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
    /// The settled orders, each paired with its order PDA's canonical bump.
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
        // The remaining bytes after recovering the counterpart are the bumps.
        let (finalize_ix_index, bumps) = recover_counterpart(instruction_data)?;

        // Accounts: the instructions sysvar followed by one
        // `(order_pda, sell_token_account)` pair per bump.
        let (instructions_sysvar_account, order_accounts) = accounts
            .split_first()
            .ok_or(ProgramError::NotEnoughAccountKeys)?;
        let (account_pairs, remainder) = order_accounts.as_chunks::<2>();
        if !remainder.is_empty() || account_pairs.len() != bumps.len() {
            return Err(SettlementError::AccountCountNotMatchingBumps.into());
        }

        Ok(Self {
            finalize_ix_index,
            instructions_sysvar_account,
            orders: SettledOrders {
                accounts: account_pairs,
                bumps,
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

    validate_settled_orders(program_id, input.orders)?;

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
/// program and that its sell token account is the one the order's owner
/// controls.
fn validate_settled_orders<'a>(
    program_id: &Address,
    orders: impl IntoIterator<Item = SettledOrder<'a>>,
) -> ProgramResult {
    // Orders must be passed strictly increasing by address; this rejects
    // duplicates (settling the same order twice) without a separate scan.
    let mut previous: Option<&Address> = None;

    for SettledOrder {
        order_pda,
        sell_token_account,
        bump,
    } in orders
    {
        // Decode the order body. Reading is safe regardless of who owns the
        // account; the canonical-address check below is what proves provenance.
        // The borrow is released at the end of this block, before any other
        // account is touched.
        let (intent, uid) = {
            let data = order_pda.try_borrow()?;
            let bytes: &[u8; EncodedOrderAccount::SIZE] = (&*data)
                .try_into()
                .map_err(|_| ProgramError::InvalidAccountData)?;
            let (account, uid) = EncodedOrderAccount::decode_and_hash(bytes)?;
            (account.intent, uid)
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
    let input = FinalizeSettleInput::parse(instruction_data, accounts)?;
    let instructions = Instructions::try_from(input.instructions_sysvar_account)?;

    // Reciprocity: the input index is a begin_settle instruction and that
    // instruction points to the current one.
    validate_counterpart(
        program_id,
        &instructions,
        instructions.load_current_index(),
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
    use crate::test_utils::{fake_account, fake_account_from_array};
    use ::proptest::{prelude::*, test_runner::TestCaseError};
    use settlement_interface::{
        data::intent::fixtures::arb_order_intent, instruction::settle::INSTRUCTIONS_SYSVAR_ID,
        pda::order::find_order_pda,
    };

    #[test]
    fn begin_settle_input_parses_valid_input() {
        let address = Address::new_from_array([0x42u8; 32]);
        let mut accounts = [fake_account(address)];
        let data = [
            SettlementInstruction::BeginSettle.discriminator(),
            0x13,
            0x37,
        ];
        let BeginSettleInput {
            finalize_ix_index,
            instructions_sysvar_account,
            orders,
        } = BeginSettleInput::parse(&data, &mut accounts).expect("parse should succeed");
        assert_eq!(finalize_ix_index, 0x1337);
        assert_eq!(instructions_sysvar_account.address(), &address);
        assert_eq!(orders.into_iter().count(), 0);
    }

    #[test]
    fn finalize_settle_input_parses_valid_input() {
        let address = Address::new_from_array([0x42u8; 32]);
        let mut accounts = [fake_account(address)];
        let data = [
            SettlementInstruction::FinalizeSettle.discriminator(),
            0x13,
            0x37,
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
        let data = [SettlementInstruction::FinalizeSettle.discriminator(), 0, 0];
        let mut accounts: [AccountView; 0] = [];
        assert_eq!(
            BeginSettleInput::parse(&data, &mut accounts).err(),
            Some(ProgramError::InvalidInstructionData),
        );
    }

    #[test]
    fn finalize_settle_input_rejects_different_discriminator() {
        let data = [SettlementInstruction::BeginSettle.discriminator(), 0, 0];
        let mut accounts: [AccountView; 0] = [];
        assert_eq!(
            FinalizeSettleInput::parse(&data, &mut accounts).err(),
            Some(ProgramError::InvalidInstructionData),
        );
    }

    #[test]
    fn begin_settle_input_rejects_empty_accounts() {
        let data = [SettlementInstruction::BeginSettle.discriminator(), 0, 0];
        let mut accounts: [AccountView; 0] = [];
        assert_eq!(
            BeginSettleInput::parse(&data, &mut accounts).err(),
            Some(ProgramError::NotEnoughAccountKeys),
        );
    }

    #[test]
    fn finalize_settle_input_rejects_empty_accounts() {
        let data = [SettlementInstruction::FinalizeSettle.discriminator(), 0, 0];
        let mut accounts: [AccountView; 0] = [];
        assert_eq!(
            FinalizeSettleInput::parse(&data, &mut accounts).err(),
            Some(ProgramError::NotEnoughAccountKeys),
        );
    }

    #[test]
    fn begin_settle_input_parses_order_bumps_and_pairs() {
        let sysvar = Address::new_from_array([1u8; 32]);
        let order_pda = Address::new_from_array([2u8; 32]);
        let sell_token = Address::new_from_array([3u8; 32]);
        let mut accounts = [
            fake_account(sysvar),
            fake_account(order_pda),
            fake_account(sell_token),
        ];
        let data = [
            SettlementInstruction::BeginSettle.discriminator(),
            0x13, // finalize index hi
            0x37, // finalize index lo
            0xab, // one order's bump
        ];
        let BeginSettleInput {
            finalize_ix_index,
            instructions_sysvar_account,
            orders,
        } = BeginSettleInput::parse(&data, &mut accounts).expect("parse should succeed");
        assert_eq!(finalize_ix_index, 0x1337);
        assert_eq!(instructions_sysvar_account.address(), &sysvar);

        let mut orders = orders.into_iter();
        let order = orders.next().expect("one settled order");
        assert_eq!(order.order_pda.address(), &order_pda);
        assert_eq!(order.sell_token_account.address(), &sell_token);
        assert_eq!(order.bump, 0xab);
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

        // The sysvar (`[0xff; 32]`) differs from every order/token address above.
        let mut accounts = vec![(fake_account_from_array([0xff; 32]))];
        let mut data = vec![
            SettlementInstruction::BeginSettle.discriminator(),
            0x13,
            0x37,
        ];
        for &(order_pda, sell_token, bump) in &expected {
            accounts.push(fake_account(order_pda));
            accounts.push(fake_account(sell_token));
            data.push(bump);
        }

        let parsed = BeginSettleInput::parse(&data, &mut accounts).expect("parse should succeed");
        let orders: Vec<_> = parsed.orders.into_iter().collect();

        assert_eq!(orders.len(), ORDER_COUNT);
        for (order, (order_pda, sell_token, bump)) in orders.iter().zip(&expected) {
            assert_eq!(order.order_pda.address(), order_pda);
            assert_eq!(order.sell_token_account.address(), sell_token);
            assert_eq!(order.bump, *bump);
        }
    }

    #[test]
    fn begin_settle_input_rejects_account_count_mismatch() {
        // One bump expects two order accounts; only one is supplied (together
        // with the sysvar account).
        let mut accounts = [
            fake_account_from_array([1u8; 32]),
            fake_account_from_array([2u8; 32]),
        ];
        let data = [
            SettlementInstruction::BeginSettle.discriminator(),
            0,
            0,
            0xab, // one bump
        ];
        assert_eq!(
            BeginSettleInput::parse(&data, &mut accounts).err(),
            Some(SettlementError::AccountCountNotMatchingBumps.into()),
        );
    }

    #[test]
    fn finalize_settle_input_ignores_extra_parameters() {
        let first_address = Address::new_from_array([1u8; 32]);
        let second_address = Address::new_from_array([2u8; 32]);
        let mut accounts = [fake_account(first_address), fake_account(second_address)];
        let data = [
            SettlementInstruction::FinalizeSettle.discriminator(),
            0x13, // used
            0x37, // used
            42,   // extra
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
            let ix = settlement_client::instructions::begin_settle(
                &program_id,
                finalize_ix_index,
                &intents,
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

            let parsed_orders: Vec<_> = parsed.orders.into_iter().collect();
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
