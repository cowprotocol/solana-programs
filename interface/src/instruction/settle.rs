//! `BeginSettle`/`FinalizeSettle` instruction tools, the instructions-sysvar
//! account ID they all reference, and the off-chain instruction builders.

use std::vec;

use solana_instruction::{AccountMeta, Instruction};
use solana_program_error::ProgramError;
use solana_pubkey::Pubkey;

pub use solana_sdk_ids::sysvar::instructions::ID as INSTRUCTIONS_SYSVAR_ID;
pub use spl_token_interface::ID as SPL_TOKEN_PROGRAM_ID;

use crate::SettlementInstruction;

/// A single transfer made when settling an order: `amount` tokens sent from the
/// order's sell token account to `destination`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Pull {
    pub destination: Pubkey,
    pub amount: u64,
}

/// Build a `BeginSettle` instruction settling the orders described by the
/// parallel lists:
/// - `order_pdas[i]` is the canonical order PDA (see [`crate::pda::order`])
/// - `sell_token_accounts[i]` its sell token account of the order,
/// - `bumps[i]` is the canonical PDA bump
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
/// Accounts:
/// `[instructions_sysvar (R), state_pda (R), token_program (R)]` followed, per
/// order, by `[order_pda (R), sell_token_account (W), destination (W)...]`.
///
/// The program requires the order PDAs to be strictly increasing by address.
/// This builder establishes that ordering for the caller: it sorts the orders by
/// PDA address, carrying each order's sell token account, bump, transfer count,
/// amounts, and destination metas before emitting them.
pub fn begin_settle(
    program_id: &Pubkey,
    state_pda: &Pubkey,
    finalize_ix_index: u16,
    order_pdas: &[Pubkey],
    sell_token_accounts: &[Pubkey],
    bumps: &[u8],
    pulls: &[&[Pull]],
) -> Instruction {
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
        &order.iter().map(|&i| bumps[i]).collect::<Vec<u8>>()[..],
        &counts[..],
        &amounts[..],
    ]
    .concat();

    let mut accounts = vec![
        AccountMeta::new_readonly(INSTRUCTIONS_SYSVAR_ID, false),
        AccountMeta::new_readonly(*state_pda, false),
        AccountMeta::new_readonly(SPL_TOKEN_PROGRAM_ID, false),
    ];
    for &i in &order {
        accounts.push(AccountMeta::new_readonly(order_pdas[i], false));
        accounts.push(AccountMeta::new(sell_token_accounts[i], false));
        for pull in pulls[i] {
            accounts.push(AccountMeta::new(pull.destination, false));
        }
    }

    Instruction {
        program_id: *program_id,
        accounts,
        data,
    }
}

pub fn finalize_settle(program_id: &Pubkey, begin_ix_index: u16) -> Instruction {
    Instruction {
        program_id: *program_id,
        accounts: vec![AccountMeta::new_readonly(INSTRUCTIONS_SYSVAR_ID, false)],
        data: [
            &[SettlementInstruction::FinalizeSettle.discriminator()],
            &begin_ix_index.to_be_bytes()[..],
        ]
        .concat(),
    }
}

/// Reads the first two bytes of a byte slice (instruction data) and
/// interprets them as a big-endian u16, returning it together with the
/// remaining bytes to parse.
/// It's meant to be used for BeginSettle and FinalizeSettle to extract the
/// counterpart index, that is, the index linking that instruction to the
/// opposite instruction which is encoded as the first
/// 2 bytes of the instruction data: `[0x13, 0x37]` → `0x1337`.
/// Returns `InvalidInstructionData` if fewer than two bytes are provided.
pub fn recover_counterpart(instruction_data: &[u8]) -> Result<(u16, &[u8]), ProgramError> {
    match instruction_data {
        [b1, b2, rest @ ..] => Ok((u16::from_be_bytes([*b1, *b2]), rest)),
        _ => Err(ProgramError::InvalidInstructionData),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hex_literal::hex;

    #[test]
    fn rejects_empty_payload() {
        assert_eq!(
            recover_counterpart(&[]),
            Err(ProgramError::InvalidInstructionData),
        );
    }

    #[test]
    fn rejects_too_short_payload() {
        assert_eq!(
            recover_counterpart(&[42]),
            Err(ProgramError::InvalidInstructionData),
        );
    }

    #[test]
    fn returns_trailing_bytes() {
        assert_eq!(
            recover_counterpart(
                &[
                    &hex!("1337")[..], // counterpart index
                    &[42][..],         // trailing
                ]
                .concat()
            ),
            Ok((0x1337, [42].as_slice())),
        );
    }

    #[test]
    fn expected_encoding_begin_settle_no_orders() {
        let program_id = Pubkey::new_unique();
        let state_pda = Pubkey::new_unique();
        let Instruction {
            program_id: ix_program_id,
            accounts,
            data,
        } = begin_settle(&program_id, &state_pda, 0x1337, &[], &[], &[], &[]);
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
        let high_bump = 0xaa;
        let low_order_pda = Pubkey::new_from_array([0xaa; 32]);
        let low_sell_token_account = Pubkey::new_from_array([0xb0; 32]);
        let low_bump = 0xbb;
        let ix = begin_settle(
            &program_id,
            &state_pda,
            0x1337,
            &[high_order_pda, low_order_pda],
            &[high_sell_token_account, low_sell_token_account],
            &[high_bump, low_bump],
            &[&[], &[]],
        );

        // Bumps follow the sorted order: the low PDA's bump comes first.
        assert_eq!(
            ix.data,
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
            high_order_pda,
            high_sell_token_account,
        ];
        let actual: Vec<Pubkey> = ix.accounts.iter().map(|meta| meta.pubkey).collect();
        assert_eq!(actual, expected);
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
        let ix = begin_settle(
            &program_id,
            &state_pda,
            0x1337,
            &[order_a, order_b],
            &[sell_a, sell_b],
            &[0xa1, 0xb1],
            &[
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
        );

        assert_eq!(
            ix.data,
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

        let actual: Vec<Pubkey> = ix.accounts.iter().map(|meta| meta.pubkey).collect();
        assert_eq!(
            actual,
            vec![
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
            ],
        );
        // The fixed accounts and the order PDAs are read-only; sell and
        // destination accounts are writable for the transfer.
        let writable: Vec<Pubkey> = ix
            .accounts
            .iter()
            .filter(|meta| meta.is_writable)
            .map(|meta| meta.pubkey)
            .collect();
        assert_eq!(writable, vec![sell_a, dest_a0, dest_a1, sell_b, dest_b0]);
        assert!(ix.accounts.iter().all(|meta| !meta.is_signer));
    }

    #[test]
    fn expected_encoding_finalize_settle() {
        let program_id = Pubkey::new_unique();
        let ix = finalize_settle(&program_id, 0x1337);
        assert_eq!(
            ix.data,
            [
                &[SettlementInstruction::FinalizeSettle.discriminator()][..],
                &hex!("1337")[..], // counterpart index
            ]
            .concat(),
        );

        // Only the instructions sysvar is referenced.
        assert_eq!(ix.accounts.len(), 1);
        assert_eq!(ix.accounts[0].pubkey, INSTRUCTIONS_SYSVAR_ID);
        assert!(!ix.accounts[0].is_writable);
        assert!(!ix.accounts[0].is_signer);
    }
}
