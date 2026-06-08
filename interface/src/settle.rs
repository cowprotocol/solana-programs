//! `BeginSettle`/`FinalizeSettle` instruction tools, the instructions-sysvar
//! account ID they all reference, and the off-chain instruction builders.

use std::vec;

use solana_instruction::{AccountMeta, Instruction};
use solana_program_error::ProgramError;
use solana_pubkey::Pubkey;

pub use solana_sdk_ids::sysvar::instructions::ID as INSTRUCTIONS_SYSVAR_ID;

use crate::SettlementInstruction;

/// Build a `BeginSettle` instruction settling the orders described by the three
/// parallel lists: `order_pdas[i]` is the canonical order PDA (see
/// [`crate::pda::order`]), `sell_token_accounts[i]` its sell token account, and
/// `bumps[i]` the canonical PDA bump. The three slices are expected to have the
/// same length; the builder zips them and stops at the shortest.
///
/// Wire format:
/// `[discriminator=0][finalize_ix_index: u16 BE][bump...]`, one `bump` byte per
/// order.
/// Accounts:
/// `[instructions_sysvar (R), (order_pda (R), sell_token_account (R))...]`, one
//  pair of accounts per order.
///
/// The program requires the order PDAs to be strictly increasing by address.
/// The builder assumes the input already satisfies this: it emits the bumps and
/// account metas in the order given, without reordering.
pub fn begin_settle(
    program_id: &Pubkey,
    finalize_ix_index: u16,
    order_pdas: &[Pubkey],
    sell_token_accounts: &[Pubkey],
    bumps: &[u8],
) -> Instruction {
    let orders = order_pdas.iter().zip(sell_token_accounts).zip(bumps);

    let mut data = [
        &[SettlementInstruction::BeginSettle.discriminator()],
        &finalize_ix_index.to_be_bytes()[..],
    ]
    .concat();

    let mut accounts = vec![AccountMeta::new_readonly(INSTRUCTIONS_SYSVAR_ID, false)];
    for ((order_pda, sell_token_account), bump) in orders {
        data.push(*bump);
        accounts.push(AccountMeta::new_readonly(*order_pda, false));
        accounts.push(AccountMeta::new_readonly(*sell_token_account, false));
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
            recover_counterpart(&[
                0x13, // counterpart index
                0x37, // counterpart index
                42,   // trailing
            ]),
            Ok((0x1337, [42].as_slice())),
        );
    }

    #[test]
    fn expected_encoding_begin_settle() {
        let program_id = Pubkey::new_unique();
        let ix = begin_settle(&program_id, 0x1337, &[], &[], &[]);
        assert_eq!(
            ix.data,
            [
                SettlementInstruction::BeginSettle.discriminator(),
                0x13,
                0x37
            ]
        );
        // No orders: only the instructions sysvar is referenced.
        assert_eq!(ix.accounts.len(), 1);
        assert_eq!(ix.accounts[0].pubkey, INSTRUCTIONS_SYSVAR_ID);
        assert!(!ix.accounts[0].is_writable);
        assert!(!ix.accounts[0].is_signer);
    }

    #[test]
    fn begin_settle_emits_unsorted_orders() {
        let program_id = Pubkey::new_unique();
        // Two orders, sorted in the wrong way. Bad sorting stresses that the
        // instruction builder doesn't validate the input.
        let low_order_pda = Pubkey::new_from_array([0xb; 32]);
        let low_sell_token_account = Pubkey::new_from_array([0xbb; 32]);
        let low_bump = 0xbb;
        let high_order_pda = Pubkey::new_from_array([0xa; 32]);
        let high_sell_token_account = Pubkey::new_from_array([0xaa; 32]);
        let high_bump = 0xaa;
        let ix = begin_settle(
            &program_id,
            0x1337,
            &[low_order_pda, high_order_pda],
            &[low_sell_token_account, high_sell_token_account],
            &[low_bump, high_bump],
        );

        assert_eq!(
            ix.data,
            [
                SettlementInstruction::BeginSettle.discriminator(),
                0x13,
                0x37,
                low_bump,
                high_bump,
            ],
        );

        let expected: Vec<Pubkey> = vec![
            INSTRUCTIONS_SYSVAR_ID,
            low_order_pda,
            low_sell_token_account,
            high_order_pda,
            high_sell_token_account,
        ];
        let actual: Vec<Pubkey> = ix.accounts.iter().map(|meta| meta.pubkey).collect();
        assert_eq!(actual, expected);
        assert!(ix
            .accounts
            .iter()
            .all(|meta| !meta.is_writable && !meta.is_signer));
    }

    #[test]
    fn expected_encoding_finalize_settle() {
        let program_id = Pubkey::new_unique();
        let ix = finalize_settle(&program_id, 0x1337);
        assert_eq!(
            ix.data,
            [
                SettlementInstruction::FinalizeSettle.discriminator(),
                0x13,
                0x37
            ]
        );

        // Only the instructions sysvar is referenced.
        assert_eq!(ix.accounts.len(), 1);
        assert_eq!(ix.accounts[0].pubkey, INSTRUCTIONS_SYSVAR_ID);
        assert!(!ix.accounts[0].is_writable);
        assert!(!ix.accounts[0].is_signer);
    }
}
