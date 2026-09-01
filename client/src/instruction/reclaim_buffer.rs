//! Builder for the `ReclaimBuffer` instruction.

use cow_settlement_interface::{
    pda::{buffer::find_buffer_pda, state::find_state_pda},
    Instruction, Pubkey,
};

/// Builder for a `ReclaimBuffer` instruction closing the buffer for each of
/// `mints` and sending their rent lamports to `reclaim_recipient`, which
/// `reclaim_authority` picks freely and may set to itself.
///
/// A buffer that still holds a token balance is silently skipped rather than
/// closed, so a successful instruction is no guarantee that any buffer went
/// away. This is done to prevent accidental loss of funds.
pub struct ReclaimBuffer<'a> {
    pub program_id: Pubkey,
    pub reclaim_authority: Pubkey,
    pub reclaim_recipient: Pubkey,
    pub mints: &'a [Pubkey],
}

impl From<ReclaimBuffer<'_>> for Instruction {
    fn from(builder: ReclaimBuffer<'_>) -> Self {
        let (state_pda, _bump) = find_state_pda(&builder.program_id);
        let buffers: Vec<(Pubkey, Pubkey)> = builder
            .mints
            .iter()
            .map(|mint| {
                let (buffer_pda, _bump) = find_buffer_pda(&builder.program_id, mint);
                (buffer_pda, *mint)
            })
            .collect();
        cow_settlement_interface::instruction::reclaim_buffer::ReclaimBuffer {
            program_id: builder.program_id,
            state_pda,
            reclaim_authority: builder.reclaim_authority,
            reclaim_recipient: builder.reclaim_recipient,
            buffers: &buffers,
        }
        .into()
    }
}
