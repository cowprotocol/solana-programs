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
