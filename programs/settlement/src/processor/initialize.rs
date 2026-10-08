//! `Initialize` instruction handler.

use cow_settlement_interface::{
    data::state::{StateAccount, StateInitArgs, WIDTH_HEADER},
    instruction::{initialize::InitializeInput, InstructionInputParsing},
    pda::{
        buffer::NATIVE_SOL_BUFFER_PDA_SEEDS,
        state::{validate_is_state_pda, STATE_PDA_SEEDS},
    },
    SettlementError,
};
use pinocchio::{AccountView, Address, ProgramResult};
use solana_loader_v3_interface::{get_program_data_address, state::UpgradeableLoaderState};

use crate::processor::utils::pda::CanonicalPda;

pub fn process_initialize(
    program_id: &Address,
    accounts: &mut [AccountView],
    instruction_data: &[u8],
) -> ProgramResult {
    let InitializeInput {
        payer,
        state_pda,
        native_sol_buffer,
        program_data,
        manager,
        solver_authority,
        reclaim_authority,
        settlement_owned_order_authority,
    } = InitializeInput::parse(instruction_data, accounts)?;

    require_upgrade_authority(program_id, payer, program_data)?;

    validate_is_state_pda(state_pda.address().as_array())?;

    // We derive the actual expected state pda address on-chain during initialization for sanity.
    CanonicalPda {
        program_id,
        payer,
        pda: state_pda,
        size: WIDTH_HEADER as u64,
        owner: program_id,
        seeds: STATE_PDA_SEEDS,
    }
    .create_new()?;

    // The native SOL buffer holds plain lamports, which `FinalizeSettle` pays
    // out by editing balances directly, so it's an empty account owned by the
    // settlement program.
    CanonicalPda {
        program_id,
        payer,
        pda: native_sol_buffer,
        size: 0,
        owner: program_id,
        seeds: NATIVE_SOL_BUFFER_PDA_SEEDS,
    }
    .create_new()?;

    // A copied `AccountView` handle writes through to the same runtime account.
    let mut state_pda = *state_pda;
    StateAccount::initialize(
        state_pda.try_borrow_mut()?,
        &StateInitArgs {
            manager,
            solver_authority,
            reclaim_authority,
            settlement_owned_order_authority,
        },
    )?;

    Ok(())
}

/// Confirm that `payer` signed and is the upgrade authority recorded in
/// `program_data`, which must be `program_id`'s `ProgramData` account.
fn require_upgrade_authority(
    program_id: &Address,
    payer: &AccountView,
    program_data: &AccountView,
) -> ProgramResult {
    if !payer.is_signer() || program_data.address() != &get_program_data_address(program_id) {
        return Err(SettlementError::UnauthorizedInitialize.into());
    }
    // The program bytes trail the loader state, which `deserialize` ignores.
    match wincode::deserialize(&program_data.try_borrow()?) {
        Ok(UpgradeableLoaderState::ProgramData {
            upgrade_authority_address: Some(authority),
            ..
        }) if authority == *payer.address() => Ok(()),
        _ => Err(SettlementError::UnauthorizedInitialize.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cow_settlement_interface::instruction::fixtures::fake_sequential_accounts;
    use cow_settlement_interface::instruction::initialize::fixtures::{
        initialize_data, NUM_ACCOUNTS,
    };
    use pinocchio::error::ProgramError;

    #[test]
    fn process_initialize_propagates_parse_error() {
        let mut data = initialize_data();
        data.push(0); // make the data too long to trigger a parse error
        let mut accounts = fake_sequential_accounts::<NUM_ACCOUNTS>();
        assert_eq!(
            process_initialize(&Address::new_from_array([100; 32]), &mut accounts, &data),
            Err(ProgramError::InvalidInstructionData),
        );
    }
}
