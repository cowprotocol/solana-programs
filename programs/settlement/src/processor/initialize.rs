//! `Initialize` instruction handler.

use cow_settlement_interface::{
    data::state::{StateAccount, StateInitArgs, WIDTH_HEADER},
    instruction::{
        initialize::{InitializeInput, DEPLOYER},
        InstructionInputParsing,
    },
    pda::{
        buffer::NATIVE_SOL_BUFFER_PDA_SEEDS,
        state::{validate_is_state_pda, STATE_PDA_SEEDS},
    },
};
use pinocchio::{error::ProgramError, AccountView, Address, ProgramResult};

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
        manager,
        solver_authority,
        reclaim_authority,
        settlement_owned_order_authority,
    } = InitializeInput::parse(instruction_data, accounts)?;

    // Prevent initialize from being called by an unrelated entity.
    if !payer.is_signer() || payer.address() != &DEPLOYER {
        return Err(ProgramError::MissingRequiredSignature);
    }

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

#[cfg(test)]
mod tests {
    use super::*;
    use cow_settlement_interface::fixtures::pubkey_from_seed;
    use cow_settlement_interface::instruction::fixtures::{
        fake_account, fake_sequential_accounts, fake_signer,
    };
    use cow_settlement_interface::instruction::initialize::fixtures::{
        initialize_data, NUM_ACCOUNTS,
    };

    /// Arbitrary accounts behind a `payer` slot holding `payer`.
    fn accounts_paid_by(payer: AccountView) -> [AccountView; NUM_ACCOUNTS] {
        let mut accounts = fake_sequential_accounts::<NUM_ACCOUNTS>();
        accounts[0] = payer;
        accounts
    }

    #[test]
    fn process_initialize_propagates_parse_error() {
        let mut data = initialize_data();
        data.push(0); // make the data too long to trigger a parse error
        let mut accounts = fake_sequential_accounts::<NUM_ACCOUNTS>();
        assert_eq!(
            process_initialize(&pubkey_from_seed("program id"), &mut accounts, &data),
            Err(ProgramError::InvalidInstructionData),
        );
    }

    #[test]
    fn process_initialize_rejects_payer_other_than_deployer() {
        let mut accounts = accounts_paid_by(fake_signer(pubkey_from_seed("not the deployer")));
        assert_eq!(
            process_initialize(
                &pubkey_from_seed("program id"),
                &mut accounts,
                &initialize_data()
            ),
            Err(ProgramError::MissingRequiredSignature),
        );
    }

    #[test]
    fn process_initialize_rejects_nonsigner_deployer() {
        // `fake_account`, unlike `fake_signer`, leaves the signer flag clear.
        let mut accounts = accounts_paid_by(fake_account(DEPLOYER));
        assert_eq!(
            process_initialize(
                &pubkey_from_seed("program id"),
                &mut accounts,
                &initialize_data()
            ),
            Err(ProgramError::MissingRequiredSignature),
        );
    }

    #[test]
    fn process_initialize_rejects_mismatching_state_account() {
        // The sequential state PDA is wrong, so the deployer passing the gate
        // shows up as the next check failing.
        let mut accounts = accounts_paid_by(fake_signer(DEPLOYER));
        assert_eq!(
            process_initialize(
                &pubkey_from_seed("program id"),
                &mut accounts,
                &initialize_data()
            ),
            Err(cow_settlement_interface::SettlementError::StateAccountMismatch.into()),
        );
    }
}
