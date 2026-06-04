//! `Initialize` instruction handler.

use pinocchio::{error::ProgramError, AccountView, Address, ProgramResult};
use settlement_interface::{pda::state::state_pda_seeds, SettlementInstruction};

use crate::processor::{create_canonical_pda, InstructionInputParsing};

/// Parsed inputs of an `Initialize` instruction.
struct InitializeInput<'a> {
    payer: &'a AccountView,
    state_pda: &'a AccountView,
}

impl<'a> InstructionInputParsing<'a> for InitializeInput<'a> {
    const DISCRIMINATOR: SettlementInstruction = SettlementInstruction::Initialize;

    fn parse_body(
        instruction_data: &[u8],
        accounts: &'a mut [AccountView],
    ) -> Result<Self, ProgramError> {
        if !instruction_data.is_empty() {
            return Err(ProgramError::InvalidInstructionData);
        }
        // Accounts: [payer (W,S), state_pda (W), system_program (R)]. The system
        // program needs to be present for the `CreateAccount` CPI but doesn't
        // need to be referenced directly and can be at any later position.
        let [payer, state_pda, _system, ..] = accounts else {
            return Err(ProgramError::NotEnoughAccountKeys);
        };

        Ok(Self { payer, state_pda })
    }
}

pub fn process_initialize(
    program_id: &Address,
    accounts: &mut [AccountView],
    instruction_data: &[u8],
) -> ProgramResult {
    let InitializeInput { payer, state_pda } = InitializeInput::parse(instruction_data, accounts)?;

    create_canonical_pda(program_id, payer, state_pda, 0, state_pda_seeds())?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::fake_account_from_array;

    // Only used in failing tests, where actual data doesn't matter
    fn initialize_data() -> Vec<u8> {
        let zero = Address::new_from_array([0; 32]);
        settlement_interface::instruction::initialize::initialize(&zero, &zero, &zero).data
    }

    fn three_accounts() -> [AccountView; 3] {
        [
            fake_account_from_array([1; 32]),
            fake_account_from_array([2; 32]),
            fake_account_from_array([3; 32]),
        ]
    }

    #[test]
    fn initialize_input_parses_valid_input() {
        let program_id = Address::new_unique();
        let payer = fake_account_from_array([1; 32]);
        let state_pda = fake_account_from_array([2; 32]);
        let data = settlement_interface::instruction::initialize::initialize(
            &program_id,
            payer.address(),
            state_pda.address(),
        )
        .data;

        let system_program = fake_account_from_array([3; 32]);
        let mut accounts = [payer, state_pda, system_program];

        let InitializeInput {
            payer: parsed_payer,
            state_pda: parsed_state_pda,
        } = InitializeInput::parse(&data, &mut accounts).expect("parse should succeed");

        assert_eq!(parsed_payer.address(), payer.address());
        assert_eq!(parsed_state_pda.address(), state_pda.address());
    }

    #[test]
    fn initialize_input_rejects_long_data() {
        let mut data = initialize_data();
        data.push(0); // trailing byte
        let mut accounts = three_accounts();
        assert_eq!(
            InitializeInput::parse(&data, &mut accounts).err(),
            Some(ProgramError::InvalidInstructionData),
        );
    }

    #[test]
    fn initialize_input_rejects_missing_accounts() {
        let data = initialize_data();
        let mut accounts: Vec<AccountView> = three_accounts().into();
        accounts.pop();
        assert_eq!(
            InitializeInput::parse(&data, &mut accounts).err(),
            Some(ProgramError::NotEnoughAccountKeys),
        );
    }

    #[test]
    fn process_initialize_propagates_parse_error() {
        let mut data = initialize_data();
        data.push(0); // make the data too long to trigger a parse error
        let mut accounts = three_accounts();
        assert_eq!(
            process_initialize(&Address::new_unique(), &mut accounts, &data),
            Err(ProgramError::InvalidInstructionData),
        );
    }
}
