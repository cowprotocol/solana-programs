//! `TransferAuthority` instruction handler.
//!
//! It transfers a role in the state PDA to another account in a single step.
//! The transfer must come from the manager or the role's current holder.

use cow_settlement_interface::{
    data::state::StateAccount,
    instruction::{transfer_authority::TransferAuthorityInput, InstructionInputParsing},
    Role, SettlementError,
};
use pinocchio::{error::ProgramError, AccountView, Address, ProgramResult};

use crate::processor::check_state_pda;

pub fn process_transfer_authority(
    program_id: &Address,
    accounts: &mut [AccountView],
    instruction_data: &[u8],
) -> ProgramResult {
    let TransferAuthorityInput {
        signer,
        state_pda,
        role,
        new_authority,
    } = TransferAuthorityInput::parse(instruction_data, accounts)?;

    check_state_pda(program_id, state_pda)?;

    if !signer.is_signer() {
        return Err(ProgramError::MissingRequiredSignature);
    }

    // A copied `AccountView` writes through to the same runtime account, so the
    // mutable view goes through this local copy.
    let mut state_pda = *state_pda;
    let mut state = StateAccount::attach(state_pda.try_borrow_mut()?)?;

    let signer_key = signer.address();
    if signer_key != &state.authority(Role::Manager) && signer_key != &state.authority(role) {
        return Err(SettlementError::UnauthorizedAuthorityTransfer.into());
    }

    state.set_authority(role, &new_authority);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use cow_settlement_interface::fixtures::PROGRAM_ID;
    use cow_settlement_interface::instruction::fixtures::fake_sequential_accounts;
    use cow_settlement_interface::instruction::transfer_authority::fixtures::{
        transfer_authority_data, NUM_ACCOUNTS,
    };

    #[test]
    fn process_transfer_authority_propagates_parse_error() {
        let mut data = transfer_authority_data();
        data.push(0); // trailing byte triggers a parse error
        let mut accounts = fake_sequential_accounts::<NUM_ACCOUNTS>();
        assert_eq!(
            process_transfer_authority(&PROGRAM_ID, &mut accounts, &data),
            Err(ProgramError::InvalidInstructionData),
        );
    }

    #[test]
    fn process_transfer_authority_rejects_non_canonical_state_pda() {
        // `fake_sequential_accounts` puts the state PDA at `[2; 32]`, which is
        // not the canonical state PDA for this program.
        let data = transfer_authority_data();
        let mut accounts = fake_sequential_accounts::<NUM_ACCOUNTS>();
        assert_eq!(
            process_transfer_authority(&PROGRAM_ID, &mut accounts, &data),
            Err(SettlementError::StateAccountMismatch.into()),
        );
    }
}
