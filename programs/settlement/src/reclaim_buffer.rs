//! `ReclaimBuffer` instruction handler.

use pinocchio::{
    cpi::{Seed, Signer},
    error::ProgramError,
    AccountView, Address, ProgramResult,
};
use pinocchio_token::{
    instructions::{CloseAccount, Transfer},
    state::Account as TokenAccount,
};
use settlement_interface::{
    instruction::{
        create_buffer::SPL_TOKEN_PROGRAM_ID, reclaim_buffer::ReclaimBufferInput,
        InstructionInputParsing,
    },
    pda::{buffer::buffer_pda_seeds, state::state_pda_seeds},
    Pubkey, SettlementError,
};
use spl_associated_token_account_interface::address::get_associated_token_address_with_program_id;

struct ReclaimBufferEntry {
    buffer_pda: AccountView,
    mint: AccountView,
    receiver_token_account: AccountView,
}

/// Read one slice element into a [`ReclaimBufferEntry`].
fn read_buffer_entry(
    &[buffer_pda, mint, receiver_token_account]: &[AccountView; 3],
) -> ReclaimBufferEntry {
    ReclaimBufferEntry {
        buffer_pda,
        mint,
        receiver_token_account,
    }
}

pub fn process_reclaim_buffer(
    program_id: &Address,
    accounts: &mut [AccountView],
    instruction_data: &[u8],
) -> ProgramResult {
    let ReclaimBufferInput {
        state_pda,
        receiver,
        token_program,
        buffers,
    } = ReclaimBufferInput::parse(instruction_data, accounts)?;

    if token_program.address() != &SPL_TOKEN_PROGRAM_ID {
        return Err(ProgramError::IncorrectProgramId);
    }

    // The buffers' token authority is the settlement state PDA; it signs both
    // the sweep transfer and the close of each buffer below.
    let seeds = state_pda_seeds();
    let (expected_state_pda, state_bump) = Address::find_program_address(&seeds, program_id);
    if state_pda.address() != &expected_state_pda {
        return Err(SettlementError::StateAccountMismatch.into());
    }
    let [seed] = seeds;
    let state_bump = [state_bump];
    let signer_seeds = [seed, &state_bump].map(Seed::from);
    let state_signer = Signer::from(&signer_seeds);

    // Only the `receiver` recorded in the state PDA at `Initialize` time may
    // trigger a reclaim. The funds always go to it (or its ATAs) regardless,
    // but requiring its signature keeps the operation operator-gated rather
    // than permissionless.
    let stored_receiver: [u8; 32] = {
        let data = state_pda.try_borrow()?;
        (&*data)
            .try_into()
            .map_err(|_| ProgramError::InvalidAccountData)?
    };
    if !receiver.is_signer() || receiver.address().as_array() != &stored_receiver {
        return Err(SettlementError::ReceiverMismatch.into());
    }
    let receiver_pubkey = Pubkey::new_from_array(stored_receiver);

    for ReclaimBufferEntry {
        buffer_pda,
        mint,
        receiver_token_account,
    } in buffers.iter().map(read_buffer_entry)
    {
        let expected_buffer_pda =
            Address::find_program_address(&buffer_pda_seeds(mint.address().as_array()), program_id)
                .0;
        if buffer_pda.address() != &expected_buffer_pda {
            return Err(SettlementError::BufferNotCanonical.into());
        }

        let mint_pubkey = Pubkey::new_from_array(*mint.address().as_array());
        let expected_receiver_ata = get_associated_token_address_with_program_id(
            &receiver_pubkey,
            &mint_pubkey,
            &SPL_TOKEN_PROGRAM_ID,
        );
        if receiver_token_account.address().as_array() != &expected_receiver_ata.to_bytes() {
            return Err(SettlementError::ReceiverTokenAccountMismatch.into());
        }

        let amount = TokenAccount::from_account_view(&buffer_pda)
            .map_err(|_| ProgramError::InvalidAccountData)?
            .amount();

        if amount > 0 {
            Transfer::new(&buffer_pda, &receiver_token_account, state_pda, amount)
                .invoke_signed(core::slice::from_ref(&state_signer))?;
        }

        CloseAccount::new(&buffer_pda, receiver, state_pda)
            .invoke_signed(core::slice::from_ref(&state_signer))?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use settlement_interface::instruction::fixtures::fake_sequential_accounts;
    use settlement_interface::instruction::reclaim_buffer::fixtures::{
        reclaim_buffer_data, NUM_SHARED_ACCOUNTS,
    };

    const PROGRAM_ID: Address = Address::new_from_array([1; 32]);

    #[test]
    fn process_reclaim_buffer_propagates_parse_error() {
        let mut data = reclaim_buffer_data();
        data.push(0); // make the data too long to trigger a parse error
        let mut accounts = fake_sequential_accounts::<NUM_SHARED_ACCOUNTS>();
        assert_eq!(
            process_reclaim_buffer(&PROGRAM_ID, &mut accounts, &data),
            Err(ProgramError::InvalidInstructionData),
        );
    }

    #[test]
    fn process_reclaim_buffer_rejects_wrong_token_program() {
        let data = reclaim_buffer_data();
        // The three shared accounts plus one buffer triple so parsing
        // succeeds and reaches the token-program check. None of the accounts
        // is the SPL Token program.
        let mut accounts = fake_sequential_accounts::<{ NUM_SHARED_ACCOUNTS + 3 }>();
        assert_eq!(
            process_reclaim_buffer(&PROGRAM_ID, &mut accounts, &data),
            Err(ProgramError::IncorrectProgramId),
        );
    }
}
