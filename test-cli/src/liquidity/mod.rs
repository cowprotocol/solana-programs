use std::collections::HashMap;

use solana_instruction::Instruction;
use solana_pubkey::Pubkey;

pub mod orca;

/// Defines the instructions needed to cover every deficit, and the amount of sink funds needed to do so
pub struct SwapPlan {
    /// Txns to execute before pulling user funds
    pub setup_ixs: Vec<Instruction>,
    /// The swap transactions which fund the deficits to the buffer
    pub swap_ixs: Vec<Instruction>,
    /// Any transactions that should be run following the pushing of user funds
    pub teardown_ixs: Vec<Instruction>,

    /// How much of each input token needs to be pulled to the keyed token account
    /// to fund the swaps
    pub sinks: HashMap<Pubkey, u64>,
}
