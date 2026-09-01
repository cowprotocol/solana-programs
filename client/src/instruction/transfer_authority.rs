//! Builder for the `TransferAuthority` instruction.

use cow_settlement_interface::{pda::state::find_state_pda, Instruction, Pubkey, Role};

/// Transfers `role` to `new_authority` in a single step. Signed by `signer`,
/// which must be the manager or the current holder of `role`.
pub struct TransferAuthority {
    pub program_id: Pubkey,
    pub signer: Pubkey,
    pub role: Role,
    pub new_authority: Pubkey,
}

impl From<TransferAuthority> for Instruction {
    fn from(builder: TransferAuthority) -> Self {
        let (state_pda, _bump) = find_state_pda(&builder.program_id);
        cow_settlement_interface::instruction::transfer_authority::TransferAuthority {
            program_id: builder.program_id,
            signer: builder.signer,
            state_pda,
            role: builder.role,
            new_authority: builder.new_authority,
        }
        .into()
    }
}
