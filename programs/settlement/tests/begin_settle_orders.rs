//! Integration tests for the settled-orders list carried by `BeginSettle`.
//!
//! Each settlement transaction here is the minimal `[BeginSettle, FinalizeSettle]`
//! pair (begin at index 0 pointing to finalize at index 1, and vice versa) so
//! that the begin/finalize pairing always validates and execution reaches the
//! order-list checks, which is what these tests exercise.

use crate::common::{
    assert_instruction_error, assert_settlement_error, create_account, set_unix_timestamp, setup,
    signed_tx, token,
};
use litesvm::{types::TransactionMetadata, LiteSVM};
use settlement_client::instructions::{
    BeginSettle, CreateOrder, FinalizeSettle, Pull, SettledOrder,
};
use settlement_client::settlement_interface::{
    data::{
        intent::{OrderIntent, OrderKind},
        order::{EncodedOrderAccount, OrderAccount},
    },
    instruction::settle::{
        BeginSettle as RawBeginSettle, INSTRUCTIONS_SYSVAR_ID, SPL_TOKEN_PROGRAM_ID,
    },
    pda::{order::find_order_pda, state::find_state_pda},
    Instruction, SettlementError, SettlementInstruction,
};
use solana_sdk::{
    account::Account,
    instruction::{AccountMeta, InstructionError},
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    transaction::{Transaction, TransactionError},
};

mod common;

/// A list of empty transfer lists, one per order. Used for settling `n` orders
/// without pulling any funds.
fn no_pulls(n: usize) -> Vec<&'static [Pull]> {
    vec![&[]; n]
}

fn sample_intent(owner: Pubkey, sell_token_account: Pubkey, salt: u8) -> OrderIntent {
    OrderIntent {
        owner,
        buy_token_account: Pubkey::new_from_array([0x22; 32]),
        sell_token_account,
        sell_amount: 1_000_000,
        buy_amount: 2_000_000,
        valid_to: 0xdead_beef,
        kind: OrderKind::Sell,
        partially_fillable: true,
        // `salt` is folded into `app_data` so callers can mint several orders that
        // hash to different UIDs (and therefore different order PDAs).
        app_data: [salt; 32],
    }
}

/// Create `intent`'s order PDA on-chain, signed and paid for by `owner`.
fn create_order_pda(svm: &mut LiteSVM, program_id: &Pubkey, owner: &Keypair, intent: &OrderIntent) {
    let ix = CreateOrder {
        program_id: *program_id,
        owner: owner.pubkey(),
        created_by: owner.pubkey(),
        intent,
    }
    .instruction();
    let tx = signed_tx(svm, owner, owner, ix);
    svm.send_transaction(tx)
        .expect("create_order should succeed");
}

/// Builder that mints a valid settleable order on-chain and returns its intent.
/// If nothing else is specified, It uses default parameters to build the order.
/// Individula parameters can be changed before building the order.
struct SettleableOrder<'a> {
    svm: &'a mut LiteSVM,
    program_id: &'a Pubkey,
    payer: &'a Keypair,
    intent: OrderIntent,
}

impl<'a> SettleableOrder<'a> {
    fn new(
        svm: &'a mut LiteSVM,
        program_id: &'a Pubkey,
        payer: &'a Keypair,
        mint: &'a Pubkey,
    ) -> Self {
        let sell_token = token::create_token_account(svm, payer, mint, &payer.pubkey());
        let intent = sample_intent(payer.pubkey(), sell_token, 0);
        Self {
            svm,
            program_id,
            payer,
            intent,
        }
    }

    /// Make this order distinct from its siblings: `salt` is folded into
    /// `app_data` so each value hashes to a different UID (and order PDA).
    fn salt(mut self, salt: u8) -> Self {
        self.intent.app_data = [salt; 32];
        self
    }

    fn valid_to(mut self, valid_to: u32) -> Self {
        self.intent.valid_to = valid_to;
        self
    }

    fn build(self) -> OrderIntent {
        create_order_pda(self.svm, self.program_id, self.payer, &self.intent);
        self.intent
    }
}

/// Send `[begin, finalize_settle(..)]` signed by `payer`, where `begin` is a
/// pre-built `BeginSettle` instruction.
fn send_settlement(
    svm: &mut LiteSVM,
    program_id: &Pubkey,
    payer: &Keypair,
    begin: Instruction,
) -> Result<TransactionMetadata, TransactionError> {
    let finalize = FinalizeSettle {
        program_id: *program_id,
        begin_ix_index: 0,
    }
    .instruction();
    let tx = Transaction::new_signed_with_payer(
        &[begin, finalize],
        Some(&payer.pubkey()),
        &[payer],
        svm.latest_blockhash(),
    );
    svm.send_transaction(tx).map_err(|e| e.err)
}

/// Settle `orders` in a minimal `[BeginSettle, FinalizeSettle]` transaction
/// (begin at index 0, finalize at index 1) signed by `payer`.
fn settle(
    svm: &mut LiteSVM,
    program_id: &Pubkey,
    payer: &Keypair,
    orders: &[SettledOrder],
) -> Result<TransactionMetadata, TransactionError> {
    send_settlement(
        svm,
        program_id,
        payer,
        BeginSettle {
            program_id: *program_id,
            finalize_ix_index: 1,
            orders,
        }
        .instruction(),
    )
}

/// Settle orders described by raw, parallel `(order_pda, sell_token, bump)`
/// lists, pulling nothing. Uses the canonical state PDA and SPL Token program so
/// execution reaches the order-validation checks; tests that need a
/// non-canonical state PDA or token program build the instruction directly.
fn settle_raw(
    svm: &mut LiteSVM,
    program_id: &Pubkey,
    payer: &Keypair,
    order_pdas: &[Pubkey],
    sell_token_accounts: &[Pubkey],
    bumps: &[u8],
) -> Result<TransactionMetadata, TransactionError> {
    let begin = RawBeginSettle {
        program_id: *program_id,
        state_pda: find_state_pda(program_id).0,
        finalize_ix_index: 1,
        order_pdas,
        order_pda_bumps: bumps,
        sell_token_accounts,
        pulls: &no_pulls(bumps.len()),
    }
    .instruction();
    send_settlement(svm, program_id, payer, begin)
}

#[test]
fn settles_a_single_order() {
    let (mut svm, program_id, payer) = setup();
    let mint = token::create_mint(&mut svm, &payer);

    let intent = SettleableOrder::new(&mut svm, &program_id, &payer, &mint).build();
    settle(
        &mut svm,
        &program_id,
        &payer,
        &[SettledOrder {
            intent: &intent,
            pulls: &[],
        }],
    )
    .expect("settlement should succeed");
}

#[test]
fn settles_multiple_orders() {
    let (mut svm, program_id, payer) = setup();
    let mint = token::create_mint(&mut svm, &payer);

    let mut intents = Vec::new();
    for salt in 0..3u8 {
        intents.push(
            SettleableOrder::new(&mut svm, &program_id, &payer, &mint)
                .salt(salt)
                .build(),
        );
    }

    let orders: Vec<SettledOrder> = intents
        .iter()
        .map(|intent| SettledOrder { intent, pulls: &[] })
        .collect();
    settle(&mut svm, &program_id, &payer, &orders).expect("multi-order settlement should succeed");
}

#[test]
fn rejects_wrong_bump() {
    let (mut svm, program_id, payer) = setup();
    let mint = token::create_mint(&mut svm, &payer);

    let intent = SettleableOrder::new(&mut svm, &program_id, &payer, &mint).build();
    let (order_pda, bump) = find_order_pda(&program_id, &intent.uid());
    assert_settlement_error(
        settle_raw(
            &mut svm,
            &program_id,
            &payer,
            &[order_pda],
            &[intent.sell_token_account],
            &[bump ^ 0x01],
        ),
        SettlementError::OrderNotCanonical,
    );
}

#[test]
fn rejects_fabricated_program_owned_account() {
    let (mut svm, program_id, payer) = setup();
    let mint = token::create_mint(&mut svm, &payer);

    let sell_token = token::create_token_account(&mut svm, &payer, &mint, &payer.pubkey());
    let body: [u8; EncodedOrderAccount::SIZE] = EncodedOrderAccount::from(OrderAccount {
        cancelled: false,
        amount_withdrawn: 0,
        amount_received: 0,
        created_by: payer.pubkey(),
        intent: sample_intent(payer.pubkey(), sell_token, 0),
    })
    .into();
    // A program-owned account holding a valid order body, but sitting at an
    // address that isn't the canonical order PDA.
    let fake_order = create_account(&mut svm, &program_id, &body);

    assert_settlement_error(
        settle_raw(
            &mut svm,
            &program_id,
            &payer,
            &[fake_order],
            &[sell_token],
            &[255],
        ),
        SettlementError::OrderNotCanonical,
    );
}

#[test]
fn rejects_non_order_account_in_order_slot() {
    let (mut svm, program_id, payer) = setup();
    let mint = token::create_mint(&mut svm, &payer);

    let sell_token = token::create_token_account(&mut svm, &payer, &mint, &payer.pubkey());

    // Put a token account in the order slot. Its 165-byte data can't decode as a
    // 199-byte order body, so it's rejected before the canonical-address check.
    assert_instruction_error(
        settle_raw(
            &mut svm,
            &program_id,
            &payer,
            &[sell_token],
            &[sell_token],
            &[255],
        ),
        InstructionError::InvalidAccountData,
    );
}

#[test]
fn rejects_sell_token_account_mismatch() {
    let (mut svm, program_id, payer) = setup();
    let mint = token::create_mint(&mut svm, &payer);

    // Supply a different token account than the one the order's intent names.
    let intent = SettleableOrder::new(&mut svm, &program_id, &payer, &mint).build();
    let (order_pda, bump) = find_order_pda(&program_id, &intent.uid());
    let wrong_sell_token = token::create_token_account(&mut svm, &payer, &mint, &payer.pubkey());
    assert_settlement_error(
        settle_raw(
            &mut svm,
            &program_id,
            &payer,
            &[order_pda],
            &[wrong_sell_token],
            &[bump],
        ),
        SettlementError::SellTokenAccountMismatch,
    );
}

#[test]
fn rejects_sell_token_owner_mismatch() {
    let (mut svm, program_id, payer) = setup();
    let mint = token::create_mint(&mut svm, &payer);

    let other_owner = Pubkey::new_unique();
    let sell_token = token::create_token_account(&mut svm, &payer, &mint, &other_owner);
    let intent = sample_intent(payer.pubkey(), sell_token, 1);
    create_order_pda(&mut svm, &program_id, &payer, &intent);

    assert_settlement_error(
        settle(
            &mut svm,
            &program_id,
            &payer,
            &[SettledOrder {
                intent: &intent,
                pulls: &[],
            }],
        ),
        SettlementError::SellTokenOwnerMismatch,
    );
}

#[test]
fn rejects_non_token_sell_account() {
    let (mut svm, program_id, payer) = setup();

    let non_token = Pubkey::new_unique();
    let intent = sample_intent(payer.pubkey(), non_token, 1);
    create_order_pda(&mut svm, &program_id, &payer, &intent);

    assert_settlement_error(
        settle(
            &mut svm,
            &program_id,
            &payer,
            &[SettledOrder {
                intent: &intent,
                pulls: &[],
            }],
        ),
        SettlementError::SellTokenAccountInvalid,
    );
}

#[test]
fn rejects_duplicate_orders() {
    let (mut svm, program_id, payer) = setup();
    let mint = token::create_mint(&mut svm, &payer);

    let intent = SettleableOrder::new(&mut svm, &program_id, &payer, &mint).build();
    assert_settlement_error(
        settle(
            &mut svm,
            &program_id,
            &payer,
            &[
                SettledOrder {
                    intent: &intent,
                    pulls: &[],
                },
                SettledOrder {
                    intent: &intent,
                    pulls: &[],
                },
            ],
        ),
        SettlementError::OrdersNotStrictlyIncreasing,
    );
}

#[test]
fn rejects_orders_in_wrong_address_order() {
    let (mut svm, program_id, payer) = setup();
    let mint = token::create_mint(&mut svm, &payer);

    let first = SettleableOrder::new(&mut svm, &program_id, &payer, &mint)
        .salt(0)
        .build();
    let second = SettleableOrder::new(&mut svm, &program_id, &payer, &mint)
        .salt(1)
        .build();

    let (first_pda, first_bump) = find_order_pda(&program_id, &first.uid());
    let (second_pda, second_bump) = find_order_pda(&program_id, &second.uid());

    // Lay out the two distinct orders strictly decreasing by PDA address, which
    // the program rejects. The interface builder would sort them, so build the
    // instruction by hand in the current wire format: data is
    // `[discriminator, finalize_ix_index (BE), order_count, bump×n, transfer_count×n]`
    // (no transfers here) and accounts are `[instructions_sysvar, state_pda,
    // token_program, (order_pda, sell_token_account)...]`.
    let mut orders = [
        (first_pda, first.sell_token_account, first_bump),
        (second_pda, second.sell_token_account, second_bump),
    ];
    orders.sort_by_key(|&(pda, _, _)| std::cmp::Reverse(pda));

    let mut data = vec![SettlementInstruction::BeginSettle.discriminator()];
    data.extend_from_slice(&1u16.to_be_bytes());
    data.push(orders.len() as u8);
    data.extend(orders.iter().map(|&(_, _, bump)| bump));
    // No transfers: one zero transfer-count byte per order.
    data.extend(orders.iter().map(|_| 0u8));

    let mut accounts = vec![
        AccountMeta::new_readonly(INSTRUCTIONS_SYSVAR_ID, false),
        AccountMeta::new_readonly(find_state_pda(&program_id).0, false),
        AccountMeta::new_readonly(SPL_TOKEN_PROGRAM_ID, false),
    ];
    for (order_pda, sell_token_account, _) in orders {
        accounts.push(AccountMeta::new_readonly(order_pda, false));
        accounts.push(AccountMeta::new(sell_token_account, false));
    }

    assert_settlement_error(
        send_settlement(
            &mut svm,
            &program_id,
            &payer,
            Instruction {
                program_id,
                accounts,
                data,
            },
        ),
        SettlementError::OrdersNotStrictlyIncreasing,
    );
}

#[test]
fn rejects_cancelled_order() {
    let (mut svm, program_id, payer) = setup();
    let mint = token::create_mint(&mut svm, &payer);

    // There's no cancel instruction yet, and `CreateOrder` always writes an
    // active order, so write the PDA directly with the `cancelled` flag set. The
    // account still sits at the canonical PDA holding a matching intent, so it
    // clears the provenance check and the cancelled flag is what trips the
    // rejection.
    let sell_token = token::create_token_account(&mut svm, &payer, &mint, &payer.pubkey());
    let intent = sample_intent(payer.pubkey(), sell_token, 0);
    let (order_pda, _bump) = find_order_pda(&program_id, &intent.uid());

    let data: [u8; EncodedOrderAccount::SIZE] = EncodedOrderAccount::from(OrderAccount {
        cancelled: true,
        amount_withdrawn: 0,
        amount_received: 0,
        created_by: payer.pubkey(),
        intent: intent.clone(),
    })
    .into();
    svm.set_account(
        order_pda,
        Account {
            lamports: svm.minimum_balance_for_rent_exemption(EncodedOrderAccount::SIZE),
            data: data.to_vec(),
            owner: program_id,
            executable: false,
            rent_epoch: 0,
        },
    )
    .expect("placing a cancelled order at its canonical PDA should succeed");

    assert_settlement_error(
        settle(
            &mut svm,
            &program_id,
            &payer,
            &[SettledOrder {
                intent: &intent,
                pulls: &[],
            }],
        ),
        SettlementError::OrderCancelled,
    );
}

#[test]
fn rejects_expired_order() {
    let (mut svm, program_id, payer) = setup();
    let mint = token::create_mint(&mut svm, &payer);

    let valid_to = 1_000_000;
    let intent = SettleableOrder::new(&mut svm, &program_id, &payer, &mint)
        .valid_to(valid_to)
        .build();
    let after_expiration = i64::from(valid_to) + 1;
    set_unix_timestamp(&mut svm, after_expiration);

    assert_settlement_error(
        settle(
            &mut svm,
            &program_id,
            &payer,
            &[SettledOrder {
                intent: &intent,
                pulls: &[],
            }],
        ),
        SettlementError::OrderExpired,
    );
}

#[test]
fn settles_order_at_exact_valid_to() {
    let (mut svm, program_id, payer) = setup();
    let mint = token::create_mint(&mut svm, &payer);

    let valid_to = 1_000_000;
    let intent = SettleableOrder::new(&mut svm, &program_id, &payer, &mint)
        .valid_to(valid_to)
        .build();
    set_unix_timestamp(&mut svm, i64::from(valid_to));

    settle(
        &mut svm,
        &program_id,
        &payer,
        &[SettledOrder {
            intent: &intent,
            pulls: &[],
        }],
    )
    .expect("an order is still settleable at exactly valid_to");
}

#[test]
fn pulls_funds_to_destination() {
    let (mut svm, program_id, payer) = setup();
    let mint = token::create_mint(&mut svm, &payer);

    let intent = SettleableOrder::new(&mut svm, &program_id, &payer, &mint).build();
    let sell_token = intent.sell_token_account;
    let initial_amount = 42_000_000;
    token::fund_and_delegate(&mut svm, &program_id, &payer, &sell_token, initial_amount);
    let destination = token::create_token_account(&mut svm, &payer, &mint, &Pubkey::new_unique());

    let amount = 2_000_000;
    settle(
        &mut svm,
        &program_id,
        &payer,
        &[SettledOrder {
            intent: &intent,
            pulls: &[Pull {
                destination,
                amount,
            }],
        }],
    )
    .expect("a pull within the approved delegation should succeed");

    assert_eq!(token::balance(&svm, &destination), amount);
    assert_eq!(token::balance(&svm, &sell_token), initial_amount - amount);
    assert_eq!(
        token::delegated_amount(&svm, &sell_token),
        initial_amount - amount
    );
}

#[test]
fn pulls_to_multiple_destinations() {
    let (mut svm, program_id, payer) = setup();
    let mint = token::create_mint(&mut svm, &payer);

    let intent = SettleableOrder::new(&mut svm, &program_id, &payer, &mint).build();
    let sell_token = intent.sell_token_account;
    let initial_amount: u64 = 1_000_000;
    token::fund_and_delegate(&mut svm, &program_id, &payer, &sell_token, initial_amount);
    let dest0 = token::create_token_account(&mut svm, &payer, &mint, &Pubkey::new_unique());
    let dest1 = token::create_token_account(&mut svm, &payer, &mint, &Pubkey::new_unique());

    let pulled0 = 300_000;
    let pulled1 = 100_000;
    settle(
        &mut svm,
        &program_id,
        &payer,
        &[SettledOrder {
            intent: &intent,
            pulls: &[
                Pull {
                    destination: dest0,
                    amount: pulled0,
                },
                Pull {
                    destination: dest1,
                    amount: pulled1,
                },
            ],
        }],
    )
    .expect("multiple pulls from one order should succeed");

    assert_eq!(token::balance(&svm, &dest0), pulled0);
    assert_eq!(token::balance(&svm, &dest1), pulled1);
    assert_eq!(
        token::balance(&svm, &sell_token),
        initial_amount - pulled0 - pulled1
    );
    assert_eq!(
        token::delegated_amount(&svm, &sell_token),
        initial_amount - pulled0 - pulled1
    );
}

#[test]
fn pulls_from_multiple_orders() {
    let (mut svm, program_id, payer) = setup();
    let mint = token::create_mint(&mut svm, &payer);

    // Two distinct orders, each selling from its own token account.
    let first = SettleableOrder::new(&mut svm, &program_id, &payer, &mint)
        .salt(0)
        .build();
    let second = SettleableOrder::new(&mut svm, &program_id, &payer, &mint)
        .salt(1)
        .build();
    let initial_amount_first = 1_337_000;
    let initial_amount_second = 31_337_000;
    token::fund_and_delegate(
        &mut svm,
        &program_id,
        &payer,
        &first.sell_token_account,
        initial_amount_first,
    );
    token::fund_and_delegate(
        &mut svm,
        &program_id,
        &payer,
        &second.sell_token_account,
        initial_amount_second,
    );
    let dest_first = token::create_token_account(&mut svm, &payer, &mint, &Pubkey::new_unique());
    let dest_second = token::create_token_account(&mut svm, &payer, &mint, &Pubkey::new_unique());

    let pulled_first = 42_000;
    let pulled_second = 67_000;
    settle(
        &mut svm,
        &program_id,
        &payer,
        &[
            SettledOrder {
                intent: &first,
                pulls: &[Pull {
                    destination: dest_first,
                    amount: pulled_first,
                }],
            },
            SettledOrder {
                intent: &second,
                pulls: &[Pull {
                    destination: dest_second,
                    amount: pulled_second,
                }],
            },
        ],
    )
    .expect("pulls from several orders should succeed");

    assert_eq!(token::balance(&svm, &dest_first), pulled_first);
    assert_eq!(token::balance(&svm, &dest_second), pulled_second);
    assert_eq!(
        token::balance(&svm, &first.sell_token_account),
        initial_amount_first - pulled_first
    );
    assert_eq!(
        token::balance(&svm, &second.sell_token_account),
        initial_amount_second - pulled_second
    );
}

#[test]
fn zero_pulls_moves_nothing() {
    let (mut svm, program_id, payer) = setup();
    let mint = token::create_mint(&mut svm, &payer);

    let intent = SettleableOrder::new(&mut svm, &program_id, &payer, &mint).build();
    let sell_token = intent.sell_token_account;
    let initial_amount = 42_000_000;
    token::mint_to(&mut svm, &payer, &mint, &sell_token, initial_amount);

    let transaction = settle(
        &mut svm,
        &program_id,
        &payer,
        &[SettledOrder {
            intent: &intent,
            pulls: &[],
        }],
    )
    .expect("settling without pulling should succeed");

    assert_eq!(token::balance(&svm, &sell_token), initial_amount);
    // Confirm that there are no transfers because there are no token
    // invocations in general.
    token::assert_no_spl_token_invocation(&transaction);
}

#[test]
fn rejects_wrong_state_pda() {
    let (mut svm, program_id, payer) = setup();
    let mint = token::create_mint(&mut svm, &payer);

    let intent = SettleableOrder::new(&mut svm, &program_id, &payer, &mint).build();
    let (order_pda, bump) = find_order_pda(&program_id, &intent.uid());
    let not_the_state_pda = Pubkey::new_unique();

    assert_settlement_error(
        send_settlement(
            &mut svm,
            &program_id,
            &payer,
            RawBeginSettle {
                program_id,
                state_pda: not_the_state_pda,
                finalize_ix_index: 1,
                order_pdas: &[order_pda],
                order_pda_bumps: &[bump],
                sell_token_accounts: &[intent.sell_token_account],
                pulls: &no_pulls(1),
            }
            .instruction(),
        ),
        SettlementError::StateAccountMismatch,
    );
}

#[test]
fn rejects_wrong_token_program() {
    let (mut svm, program_id, payer) = setup();
    let mint = token::create_mint(&mut svm, &payer);

    let intent = SettleableOrder::new(&mut svm, &program_id, &payer, &mint).build();

    // The builder always fills in the SPL Token program, so we swap the
    // token-program account out afterwards.
    let mut begin = BeginSettle {
        program_id,
        finalize_ix_index: 1,
        orders: &[SettledOrder {
            intent: &intent,
            pulls: &[],
        }],
    }
    .instruction();
    let token_account_index = 2;
    begin.accounts[token_account_index] = AccountMeta::new_readonly(Pubkey::new_unique(), false);

    assert_instruction_error(
        send_settlement(&mut svm, &program_id, &payer, begin),
        InstructionError::IncorrectProgramId,
    );
}

#[test]
fn rejects_pull_delegated_to_incorrect_address() {
    let (mut svm, program_id, payer) = setup();
    let mint = token::create_mint(&mut svm, &payer);

    let intent = SettleableOrder::new(&mut svm, &program_id, &payer, &mint).build();
    let amount = 100_000;
    let sell_token = intent.sell_token_account;
    // Funds are present but some account other than the state PDA was
    // approved as a delegate.
    token::mint_to(&mut svm, &payer, &mint, &sell_token, 1_000_000);
    token::delegate(&mut svm, &payer, &sell_token, &Pubkey::new_unique(), amount);
    let destination = token::create_token_account(&mut svm, &payer, &mint, &Pubkey::new_unique());

    let result = settle(
        &mut svm,
        &program_id,
        &payer,
        &[SettledOrder {
            intent: &intent,
            pulls: &[Pull {
                destination,
                amount,
            }],
        }],
    );
    assert!(
        result.is_err(),
        "pulling without an approved delegation must fail"
    );
}

#[test]
fn rejects_pull_exceeding_delegation() {
    let (mut svm, program_id, payer) = setup();
    let mint = token::create_mint(&mut svm, &payer);

    let intent = SettleableOrder::new(&mut svm, &program_id, &payer, &mint).build();
    let sell_token = intent.sell_token_account;
    // Funded generously, but the state PDA is delegated only 100_000.
    let initial_amount = 42_000_000;
    let delegated = 100_000;
    token::mint_to(&mut svm, &payer, &mint, &sell_token, initial_amount);
    token::delegate(
        &mut svm,
        &payer,
        &sell_token,
        &find_state_pda(&program_id).0,
        delegated,
    );
    let destination = token::create_token_account(&mut svm, &payer, &mint, &Pubkey::new_unique());

    let result = settle(
        &mut svm,
        &program_id,
        &payer,
        &[SettledOrder {
            intent: &intent,
            pulls: &[Pull {
                destination,
                amount: 200_000,
            }],
        }],
    );
    assert!(
        result.is_err(),
        "a pull exceeding the approved delegation must fail"
    );
    assert_eq!(token::balance(&svm, &sell_token), initial_amount);
    assert_eq!(token::balance(&svm, &destination), 0);
    // The rejected pull must not have consumed any of the delegation.
    assert_eq!(token::delegated_amount(&svm, &sell_token), delegated);
}

#[test]
fn rejects_extra_account() {
    let (mut svm, program_id, payer) = setup();
    let mint = token::create_mint(&mut svm, &payer);

    let intent = SettleableOrder::new(&mut svm, &program_id, &payer, &mint).build();
    // A well-formed single-order, no-transfer settlement...
    let mut begin = BeginSettle {
        program_id,
        finalize_ix_index: 1,
        orders: &[SettledOrder {
            intent: &intent,
            pulls: &[],
        }],
    }
    .instruction();
    // ...with one extra account appended, so the account count no longer matches
    // the `2n + T` the instruction data implies.
    begin
        .accounts
        .push(AccountMeta::new_readonly(Pubkey::new_unique(), false));

    assert_settlement_error(
        send_settlement(&mut svm, &program_id, &payer, begin),
        SettlementError::AccountCountNotMatchingOrderCount,
    );
}
