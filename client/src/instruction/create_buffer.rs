//! Builder for the `CreateBuffer` instruction.

use cow_settlement_interface::{
    pda::buffer::find_buffer_pda, token_program::TokenProgram, Instruction, Pubkey,
};

pub struct CreateBuffers<'a> {
    pub program_id: Pubkey,
    pub payer: Pubkey,
    pub token_program: TokenProgram,
    pub mints: &'a [Pubkey],
}

impl From<CreateBuffers<'_>> for Instruction {
    fn from(builder: CreateBuffers<'_>) -> Self {
        let buffers: Vec<(Pubkey, Pubkey)> = builder
            .mints
            .iter()
            .map(|mint| (find_buffer_pda(&builder.program_id, mint).0, *mint))
            .collect();
        cow_settlement_interface::instruction::create_buffer::CreateBuffers {
            program_id: builder.program_id,
            payer: builder.payer,
            token_program: builder.token_program.address(),
            buffers: &buffers,
        }
        .into()
    }
}
