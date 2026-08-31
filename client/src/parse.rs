//! A single entry point for parsing any settlement instruction, for off-chain
//! consumers (indexers) that receive instructions without knowing their kind
//! up front. The program itself dispatches in its entrypoint and never uses
//! this.

use cow_settlement_interface::{
    instruction::{
        add_solver::AddSolverInput,
        create_buffer::CreateBufferInput,
        create_order::CreateOrderInput,
        initialize::InitializeInput,
        reclaim_buffer::ReclaimBufferInput,
        reclaim_order::ReclaimOrderInput,
        remove_solver::RemoveSolverInput,
        settle::{BeginSettleInput, FinalizeSettleInput},
        transfer_authority::TransferAuthorityInput,
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
    ReclaimBuffer(ReclaimBufferInput<'a, A>),
    TransferAuthority(TransferAuthorityInput<'a, A>),
    AddSolver(AddSolverInput<'a, A>),
    RemoveSolver(RemoveSolverInput<'a, A>),
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
        SettlementInstruction::ReclaimBuffer => ParsedInstruction::ReclaimBuffer(
            ReclaimBufferInput::parse_body(remaining_data, accounts)?,
        ),
        SettlementInstruction::TransferAuthority => ParsedInstruction::TransferAuthority(
            TransferAuthorityInput::parse_body(remaining_data, accounts)?,
        ),
        SettlementInstruction::AddSolver => {
            ParsedInstruction::AddSolver(AddSolverInput::parse_body(remaining_data, accounts)?)
        }
        SettlementInstruction::RemoveSolver => ParsedInstruction::RemoveSolver(
            RemoveSolverInput::parse_body(remaining_data, accounts)?,
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instructions::{
        AddSolver, BeginSettle, CreateBuffers, CreateOrder, FinalizeSettle, Initialize,
        InitializedIntent, RemoveSolver,
    };
    use cow_settlement_interface::{
        data::intent::fixtures::sample_intent,
        fixtures::pubkey_from_seed,
        instruction::{
            fixtures::fake_account_from_array, reclaim_buffer::ReclaimBuffer,
            reclaim_order::ReclaimOrder, transfer_authority::TransferAuthority,
        },
        Instruction, Role,
    };

    /// One buildable instruction per discriminator. The exhaustive match makes
    /// a new instruction a compile error until this test covers it.
    fn build(instruction: SettlementInstruction) -> Instruction {
        let program_id = pubkey_from_seed("program id");
        let payer = pubkey_from_seed("payer");
        let intent = sample_intent(Default::default());
        match instruction {
            SettlementInstruction::Initialize => Initialize {
                program_id,
                payer,
                manager: payer,
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
                mints: &[pubkey_from_seed("mint")],
            }
            .into(),
            SettlementInstruction::BeginSettle => BeginSettle {
                program_id,
                solver: payer,
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
                order_pda: pubkey_from_seed("order pda"),
                reclaim_recipient: payer,
            }
            .instruction(),
            SettlementInstruction::ReclaimBuffer => ReclaimBuffer {
                program_id,
                state_pda: pubkey_from_seed("state pda"),
                reclaim_authority: payer,
                reclaim_recipient: payer,
                buffers: &[(pubkey_from_seed("buffer pda"), pubkey_from_seed("mint"))],
            }
            .into(),
            SettlementInstruction::TransferAuthority => TransferAuthority {
                program_id,
                signer: payer,
                state_pda: pubkey_from_seed("state pda"),
                role: Role::Manager,
                new_authority: payer,
            }
            .into(),
            SettlementInstruction::AddSolver => AddSolver {
                program_id,
                manager: payer,
                payer,
                solver: pubkey_from_seed("solver"),
            }
            .into(),
            SettlementInstruction::RemoveSolver => RemoveSolver {
                program_id,
                manager: payer,
                solver: pubkey_from_seed("solver"),
            }
            .into(),
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
            SettlementInstruction::ReclaimBuffer,
            SettlementInstruction::TransferAuthority,
            SettlementInstruction::AddSolver,
            SettlementInstruction::RemoveSolver,
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
                ParsedInstruction::ReclaimBuffer(_) => SettlementInstruction::ReclaimBuffer,
                ParsedInstruction::TransferAuthority(_) => SettlementInstruction::TransferAuthority,
                ParsedInstruction::AddSolver(_) => SettlementInstruction::AddSolver,
                ParsedInstruction::RemoveSolver(_) => SettlementInstruction::RemoveSolver,
            };
            assert_eq!(actual, expected);
        }
    }
}
