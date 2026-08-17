//! A single entry point for parsing any settlement instruction, for off-chain
//! consumers (indexers) that receive instructions without knowing their kind
//! up front. The program itself dispatches in its entrypoint and never uses
//! this.

use settlement_interface::{
    instruction::{
        create_buffer::CreateBufferInput,
        create_order::CreateOrderInput,
        initialize::InitializeInput,
        reclaim_order::ReclaimOrderInput,
        settle::{BeginSettleInput, FinalizeSettleInput},
        InstructionInputParsing,
    },
    recover_discriminator, SettlementInstruction,
};
use solana_program_error::ProgramError;

/// A settlement instruction parsed by [`parse_instruction`].
pub enum ParsedInstruction<'a, A> {
    Initialize(InitializeInput<'a, A>),
    CreateOrder(CreateOrderInput<'a, A>),
    CreateBuffer(CreateBufferInput<'a, A>),
    BeginSettle(BeginSettleInput<'a, A>),
    FinalizeSettle(FinalizeSettleInput<'a, A>),
    ReclaimOrder(ReclaimOrderInput<'a, A>),
}

/// Parses any settlement instruction by its discriminator.
pub fn parse_instruction<'a, A>(
    instruction_data: &'a [u8],
    accounts: &'a [A],
) -> Result<ParsedInstruction<'a, A>, ProgramError> {
    let (discriminator, remaining_data) = recover_discriminator(instruction_data)?;
    Ok(match discriminator {
        SettlementInstruction::Initialize => {
            ParsedInstruction::Initialize(InitializeInput::parse_body(remaining_data, accounts)?)
        }
        SettlementInstruction::CreateOrder => {
            ParsedInstruction::CreateOrder(CreateOrderInput::parse_body(remaining_data, accounts)?)
        }
        SettlementInstruction::CreateBuffer => ParsedInstruction::CreateBuffer(
            CreateBufferInput::parse_body(remaining_data, accounts)?,
        ),
        SettlementInstruction::BeginSettle => {
            ParsedInstruction::BeginSettle(BeginSettleInput::parse_body(remaining_data, accounts)?)
        }
        SettlementInstruction::FinalizeSettle => ParsedInstruction::FinalizeSettle(
            FinalizeSettleInput::parse_body(remaining_data, accounts)?,
        ),
        SettlementInstruction::ReclaimOrder => ParsedInstruction::ReclaimOrder(
            ReclaimOrderInput::parse_body(remaining_data, accounts)?,
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instructions::{
        BeginSettle, CreateBuffers, CreateOrder, FinalizeSettle, Initialize, InitializedIntent,
    };
    use settlement_interface::{
        data::intent::{OrderIntent, OrderKind},
        instruction::{fixtures::fake_account_from_array, reclaim_order::ReclaimOrder},
        Instruction, Pubkey,
    };

    fn intent() -> OrderIntent {
        OrderIntent {
            owner: Pubkey::new_from_array([0x11; 32]),
            buy_token_account: Pubkey::new_from_array([0x22; 32]),
            sell_token_account: Pubkey::new_from_array([0x33; 32]),
            sell_amount: 1_000,
            buy_amount: 2_000,
            valid_to: 42,
            kind: OrderKind::Sell,
            partially_fillable: false,
            app_data: [0x44; 32],
        }
    }

    /// One buildable instruction per discriminator. The exhaustive match makes
    /// a new instruction a compile error until this test covers it.
    fn build(instruction: SettlementInstruction) -> Instruction {
        let program_id = Pubkey::new_from_array([9; 32]);
        let payer = Pubkey::new_from_array([8; 32]);
        let intent = intent();
        match instruction {
            SettlementInstruction::Initialize => Initialize {
                program_id,
                payer,
                reclaim_authority: payer,
            }
            .into(),
            SettlementInstruction::CreateOrder => CreateOrder {
                program_id,
                owner: intent.owner,
                created_by: payer,
                intent: &intent,
            }
            .into(),
            SettlementInstruction::CreateBuffer => CreateBuffers {
                program_id,
                payer,
                mints: &[Pubkey::new_from_array([7; 32])],
            }
            .into(),
            SettlementInstruction::BeginSettle => BeginSettle {
                program_id,
                finalize_ix_index: 1,
                auction_id: 42,
                orders: &[InitializedIntent {
                    intent: &intent,
                    pulls: &[],
                }],
            }
            .into(),
            SettlementInstruction::FinalizeSettle => FinalizeSettle {
                program_id,
                begin_ix_index: 0,
                orders: &[],
            }
            .into(),
            SettlementInstruction::ReclaimOrder => ReclaimOrder {
                program_id,
                order_pda: Pubkey::new_from_array([6; 32]),
                reclaim_recipient: payer,
            }
            .instruction(),
        }
    }

    /// Every instruction parses to its `ParsedInstruction` variant. Both
    /// matches are wildcard-free, so adding an instruction fails to compile
    /// until it is handled here.
    #[test]
    fn parses_every_instruction_to_its_variant() {
        for expected in [
            SettlementInstruction::Initialize,
            SettlementInstruction::CreateOrder,
            SettlementInstruction::CreateBuffer,
            SettlementInstruction::BeginSettle,
            SettlementInstruction::FinalizeSettle,
            SettlementInstruction::ReclaimOrder,
        ] {
            let ix = build(expected);
            let accounts: Vec<_> = ix
                .accounts
                .iter()
                .map(|meta| fake_account_from_array(meta.pubkey.to_bytes()))
                .collect();
            let parsed = parse_instruction(&ix.data, &accounts)
                .unwrap_or_else(|err| panic!("{expected:?} failed to parse: {err:?}"));
            let actual = match parsed {
                ParsedInstruction::Initialize(_) => SettlementInstruction::Initialize,
                ParsedInstruction::CreateOrder(_) => SettlementInstruction::CreateOrder,
                ParsedInstruction::CreateBuffer(_) => SettlementInstruction::CreateBuffer,
                ParsedInstruction::BeginSettle(_) => SettlementInstruction::BeginSettle,
                ParsedInstruction::FinalizeSettle(_) => SettlementInstruction::FinalizeSettle,
                ParsedInstruction::ReclaimOrder(_) => SettlementInstruction::ReclaimOrder,
            };
            assert_eq!(actual, expected);
        }
    }
}
