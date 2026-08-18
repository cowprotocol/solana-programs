//! `ProposeAuthority` instruction handler.
//!
//! It records a proposed new holder for a role in the state PDA. The proposal
//! must come from the manager or the role's current holder and takes effect
//! only once the proposed account accepts in a separate acceptance step. See
//! the [module docs](super) for the full transfer flow.

use cow_settlement_interface::{
    data::state::{write_account, EncodedStateAccount, StateAccount},
    instruction::{authority::ProposeAuthorityInput, InstructionInputParsing},
    SettlementError,
};
use pinocchio::{error::ProgramError, AccountView, Address, ProgramResult};

pub fn process_propose_authority(
    program_id: &Address,
    accounts: &mut [AccountView],
    instruction_data: &[u8],
) -> ProgramResult {
    let ProposeAuthorityInput {
        signer,
        state_pda,
        role,
        new_authority,
    } = ProposeAuthorityInput::parse(instruction_data, accounts)?;

    let mut state = StateAccount::load_from_pda(state_pda, program_id)?;

    // Only the manager or the role's current holder may propose a transfer, and
    // they must sign it.
    if !signer.is_signer() {
        return Err(ProgramError::MissingRequiredSignature);
    }
    let signer_key = signer.address();
    if signer_key != &state.manager && signer_key != &state.authority(role) {
        return Err(SettlementError::UnauthorizedAuthorityProposal.into());
    }

    *state.pending_mut(role) = new_authority;

    let mut buffer = state_pda.try_borrow_mut()?;
    let buffer: &mut [u8; EncodedStateAccount::SIZE] = (&mut *buffer)
        .try_into()
        .map_err(|_| ProgramError::AccountDataTooSmall)?;
    write_account(buffer, &state);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use cow_settlement_interface::instruction::authority::propose::fixtures::{
        propose_authority_data, NUM_ACCOUNTS,
    };
    use cow_settlement_interface::instruction::fixtures::fake_sequential_accounts;

    const PROGRAM_ID: Address = Address::new_from_array([0xc0; 32]);

    #[test]
    fn process_propose_authority_propagates_parse_error() {
        let mut data = propose_authority_data();
        data.push(0); // trailing byte triggers a parse error
        let mut accounts = fake_sequential_accounts::<NUM_ACCOUNTS>();
        assert_eq!(
            process_propose_authority(&PROGRAM_ID, &mut accounts, &data),
            Err(ProgramError::InvalidInstructionData),
        );
    }

    #[test]
    fn process_propose_authority_rejects_non_canonical_state_pda() {
        // `fake_sequential_accounts` puts the state PDA at `[2; 32]`, which is
        // not the canonical state PDA for this program.
        let data = propose_authority_data();
        let mut accounts = fake_sequential_accounts::<NUM_ACCOUNTS>();
        assert_eq!(
            process_propose_authority(&PROGRAM_ID, &mut accounts, &data),
            Err(SettlementError::StateAccountMismatch.into()),
        );
    }
}
