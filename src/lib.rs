use pinocchio::{entrypoint, AccountView, Address, ProgramResult};

entrypoint!(process_instruction);

pub fn process_instruction(
    program_id: &Address,
    accounts: &mut [AccountView],
    instruction_data: &[u8],
) -> ProgramResult {
    Ok(())
}
