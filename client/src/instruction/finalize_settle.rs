//! Builder for the `FinalizeSettle` instruction.

use cow_settlement_interface::{
    data::intent::{Asset, OrderIntent},
    pda::{
        buffer::{find_buffer_pda, NATIVE_SOL_BUFFER_PDA, NATIVE_SOL_BUFFER_PDA_AND_BUMP},
        order::find_order_pda,
        state::STATE_PDA,
    },
    AccountMeta, Instruction, Pubkey,
};

use super::begin_settle::TokenProgram;

/// A settled order whose proceeds are pushed to it: `intent` identifies the
/// order (its buy account is the push destination and its buy asset selects the
/// canonical source buffer) and `amount` is the quantity to push.
pub struct FinalizedIntent<'a> {
    pub intent: &'a OrderIntent,
    pub amount: u64,
    /// Use TransferChecked instead of Transfer to move the tokens. This is
    /// generally costs more CU and resources but some Token2022 token
    /// extensions require it (TransferFeeAmount, TransferHookAccount,
    /// PausableAccount).
    pub use_transfer_checked: bool,
}

/// Builder for a `FinalizeSettle` instruction pushing each order's proceeds to
/// its buy token account.
///
/// The destination is the order intent's buy account and the source is the
/// canonical buffer PDA for its buy mint (see [`find_buffer_pda`]), the only
/// buffer `BeginSettle` accepts as the source of that order's push. An order
/// buying [`Asset::Native`] SOL is paid out of the lamports of
/// [`NATIVE_SOL_BUFFER_PDA`] instead. The
/// orders are sorted by their canonical order PDA (the same key
/// [`BeginSettle`](super::begin_settle::BeginSettle) orders its settled-order
/// list by) so the two instructions present the orders
/// in the same order and their lists line up.
pub struct FinalizeSettle<'a> {
    pub program_id: Pubkey,
    pub begin_ix_index: u16,
    /// By default, a settlement support both token programs at the same time.
    /// If you know you only need a single token program, you can make the byte
    /// size of the settlement transaction a bit smaller and reduce the total
    /// accounts used in the transaction by specifying the
    /// only token program you need here.
    pub only_token_program: Option<TokenProgram>,
    pub orders: &'a [FinalizedIntent<'a>],
    /// Appended to every `TransferChecked` (for example, transfer hook
    /// accounts).
    pub extra_transfer_accounts: &'a [AccountMeta],
}

impl From<FinalizeSettle<'_>> for Instruction {
    fn from(builder: FinalizeSettle<'_>) -> Self {
        // Sort the orders by their canonical order PDA, the key `BeginSettle`
        // lays its settled orders out by, so the two instruction lists align.
        // For BeginSettle, sorting can take place in the interface. But the
        // order PDAs don't appear in the actual FinalizeSettle instruction, so
        // the sorting can only happen here.
        let mut orders: Vec<&FinalizedIntent> = builder.orders.iter().collect();
        orders.sort_by_key(|order| find_order_pda(&builder.program_id, &order.intent.uid()).0);
        let pushes: Vec<OrderPush> = orders
            .iter()
            .map(|order| order.push(&builder.program_id))
            .collect();
        instruction_from_pushes(
            builder.program_id,
            builder.begin_ix_index,
            builder.only_token_program,
            builder.extra_transfer_accounts,
            &pushes,
        )
    }
}

/// The push one order contributes to `FinalizeSettle`.
#[derive(Debug, PartialEq)]
struct OrderPush {
    source_buffer: Pubkey,
    destination: Pubkey,
    mint: Option<Pubkey>,
    bump: u8,
    amount: u64,
}

impl FinalizedIntent<'_> {
    /// An order buying native SOL is paid from the native SOL buffer's lamports and
    /// never carries a mint; a token order is paid from its buy mint's buffer.
    fn push(&self, program_id: &Pubkey) -> OrderPush {
        let (source_buffer, bump, destination, mint) = match &self.intent.buy {
            Asset::Native(account) => (
                NATIVE_SOL_BUFFER_PDA,
                NATIVE_SOL_BUFFER_PDA_AND_BUMP.1,
                *account,
                None,
            ),
            Asset::TokenProgram(token) => {
                let (buffer, bump) = find_buffer_pda(program_id, &token.mint);
                let mint = self.use_transfer_checked.then_some(token.mint);
                (buffer, bump, token.token_account, mint)
            }
        };
        OrderPush {
            source_buffer,
            destination,
            mint,
            bump,
            amount: self.amount,
        }
    }
}

/// Lays `pushes` out, in the given order, as the interface's `FinalizeSettle`.
fn instruction_from_pushes(
    program_id: Pubkey,
    begin_ix_index: u16,
    only_token_program: Option<TokenProgram>,
    extra_transfer_accounts: &[AccountMeta],
    pushes: &[OrderPush],
) -> Instruction {
    let source_buffers: Vec<Pubkey> = pushes.iter().map(|push| push.source_buffer).collect();
    let destinations: Vec<Pubkey> = pushes.iter().map(|push| push.destination).collect();
    let mints: Vec<Option<Pubkey>> = pushes.iter().map(|push| push.mint).collect();
    let bumps: Vec<u8> = pushes.iter().map(|push| push.bump).collect();
    let amounts: Vec<u64> = pushes.iter().map(|push| push.amount).collect();
    cow_settlement_interface::instruction::settle::FinalizeSettle {
        program_id,
        state_pda: STATE_PDA,
        begin_ix_index,
        only_token_program,
        source_buffers: &source_buffers,
        destinations: &destinations,
        mints: &mints,
        bumps: &bumps,
        amounts: &amounts,
        extra_transfer_accounts,
    }
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::proptest::prelude::*;
    use cow_settlement_interface::{
        data::intent::{fixtures::arb_order_intent, TokenAsset},
        fixtures::pubkey_from_seed,
        instruction::{
            fixtures::fake_account_from_array, settle::FinalizeSettleInput, InstructionInputParsing,
        },
    };

    #[test]
    fn native_sol_push_never_carries_a_mint() {
        let program_id = pubkey_from_seed("program id");
        let recipient = pubkey_from_seed("recipient wallet");
        let intent = OrderIntent {
            buy: Asset::Native(recipient),
            ..OrderIntent::default()
        };
        for use_transfer_checked in [false, true] {
            let order = FinalizedIntent {
                intent: &intent,
                amount: 42,
                use_transfer_checked,
            };
            assert_eq!(
                order.push(&program_id),
                OrderPush {
                    source_buffer: NATIVE_SOL_BUFFER_PDA,
                    destination: recipient,
                    mint: None,
                    bump: NATIVE_SOL_BUFFER_PDA_AND_BUMP.1,
                    amount: 42,
                },
            );
        }
    }

    #[test]
    fn token_push_comes_from_the_mint_buffer() {
        let program_id = pubkey_from_seed("program id");
        let mint = pubkey_from_seed("buy mint");
        let token_account = pubkey_from_seed("buy token account");
        let (buffer, bump) = find_buffer_pda(&program_id, &mint);
        let intent = OrderIntent {
            buy: Asset::TokenProgram(TokenAsset {
                mint,
                token_account,
            }),
            ..OrderIntent::default()
        };
        for (use_transfer_checked, expected_mint) in [(false, None), (true, Some(mint))] {
            let order = FinalizedIntent {
                intent: &intent,
                amount: 42,
                use_transfer_checked,
            };
            assert_eq!(
                order.push(&program_id),
                OrderPush {
                    source_buffer: buffer,
                    destination: token_account,
                    mint: expected_mint,
                    bump,
                    amount: 42,
                },
            );
        }
    }

    #[test]
    fn native_sol_order_pushes_from_the_native_sol_buffer() {
        let program_id = pubkey_from_seed("program id");
        let recipient = pubkey_from_seed("recipient wallet");
        let intent = OrderIntent {
            buy: Asset::Native(recipient),
            ..OrderIntent::default()
        };
        let ix = Instruction::from(FinalizeSettle {
            program_id,
            begin_ix_index: 0,
            only_token_program: None,
            extra_transfer_accounts: &[],
            orders: &[FinalizedIntent {
                intent: &intent,
                amount: 1_337,
                use_transfer_checked: false,
            }],
        });

        let accounts: Vec<_> = ix
            .accounts
            .iter()
            .map(|meta| fake_account_from_array(meta.pubkey.to_bytes()))
            .collect();
        let parsed = FinalizeSettleInput::parse(&ix.data, &accounts)
            .expect("the builder emits a parseable finalize");
        let pushes: Vec<_> = parsed.pushes.iter().collect();
        let [push] = pushes.as_slice() else {
            panic!("one order pushes once, got {} pushes", pushes.len());
        };

        assert_eq!(push.source_buffer.address(), &NATIVE_SOL_BUFFER_PDA);
        assert_eq!(push.bump, NATIVE_SOL_BUFFER_PDA_AND_BUMP.1);
        assert_eq!(push.destination.address(), &recipient);
        assert_eq!(push.amount, 1_337);
    }

    proptest! {
        // `FinalizeSettle` sorts its pushes by canonical order PDA, the order
        // `BeginSettle` lays its settled orders out in, so the two lists align.
        #[test]
        fn finalize_settle_sorts_pushes_by_order_pda(
            begin_ix_index in any::<u16>(),
            cases in prop::collection::vec(
                (arb_order_intent(), any::<u64>(), any::<bool>()),
                1..=5,
            ),
        ) {
            let program_id = pubkey_from_seed("program id");
            let orders: Vec<FinalizedIntent> = cases
                .iter()
                .map(|(intent, amount, use_transfer_checked)| FinalizedIntent {
                    intent,
                    amount: *amount,
                    use_transfer_checked: *use_transfer_checked,
                })
                .collect();
            let ix = Instruction::from(FinalizeSettle {
                program_id,
                begin_ix_index,
                only_token_program: None,
                orders: &orders,
                extra_transfer_accounts: &[],
            });

            let mut expected: Vec<(Pubkey, OrderPush)> = orders
                .iter()
                .map(|order| {
                    let (order_pda, _bump) = find_order_pda(&program_id, &order.intent.uid());
                    (order_pda, order.push(&program_id))
                })
                .collect();
            expected.sort_by_key(|(order_pda, _)| *order_pda);
            let expected: Vec<OrderPush> = expected.into_iter().map(|(_, push)| push).collect();
            prop_assert_eq!(
                ix,
                instruction_from_pushes(program_id, begin_ix_index, None, &[], &expected),
            );
        }
    }
}
