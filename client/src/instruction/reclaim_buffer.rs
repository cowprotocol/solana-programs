//! Builder for the `ReclaimBuffer` instruction.

use cow_settlement_interface::{
    pda::{buffer::find_buffer_pda, state::STATE_PDA},
    token_program::TokenProgram,
    Instruction, Pubkey,
};

/// Builder for a `ReclaimBuffer` instruction closing the buffer for each of
/// `mints` and sending their rent lamports to `reclaim_recipient`, which
/// `reclaim_authority` picks freely and may set to itself.
///
/// A buffer is cleared and closed only when its balance doesn't exceed the
/// paired `burn_limit`: a balance within the limit is burned to zero (when
/// non-empty) and the account closed, while a balance above it reverts the
/// whole instruction. A zero limit forbids burning, so a non-empty buffer is
/// closed only once the caller allows it with a non-zero limit: a guard
/// against accidentally burning real funds.
pub struct ReclaimBuffer<'a> {
    pub program_id: Pubkey,
    pub reclaim_authority: Pubkey,
    pub reclaim_recipient: Pubkey,
    pub token_program: TokenProgram,
    /// One `(mint, burn_limit)` entry per buffer to close.
    pub mints: &'a [(Pubkey, u64)],
}

impl From<ReclaimBuffer<'_>> for Instruction {
    fn from(builder: ReclaimBuffer<'_>) -> Self {
        let buffers: Vec<(Pubkey, Pubkey, u64)> = builder
            .mints
            .iter()
            .map(|(mint, burn_limit)| {
                let (buffer_pda, _bump) = find_buffer_pda(&builder.program_id, mint);
                (buffer_pda, *mint, *burn_limit)
            })
            .collect();
        cow_settlement_interface::instruction::reclaim_buffer::ReclaimBuffer {
            program_id: builder.program_id,
            state_pda: STATE_PDA,
            reclaim_authority: builder.reclaim_authority,
            reclaim_recipient: builder.reclaim_recipient,
            token_program: builder.token_program.address(),
            buffers: &buffers,
        }
        .into()
    }
}
