use std::io::IsTerminal as _;

use anyhow::Context as _;
use solana_hash::MAX_BASE58_LEN;

use cow_settlement_client::cow_settlement_interface::{
    data::state::StateAccount, pda::state::find_state_pda,
};

use crate::cmd::Context;

pub fn run(ctx: Context) -> anyhow::Result<()> {
    let (state_pda, _) = find_state_pda(&ctx.program_id);
    let data = ctx
        .rpc
        .get_account_data(&state_pda)
        .with_context(|| format!("failed to fetch state account {state_pda}"))?;
    let state = StateAccount::attach(data.as_slice())
        .map_err(|e| anyhow::anyhow!("failed to decode state account {state_pda}: {e:?}"))?;

    // Base58 pubkeys are MAX_BASE58_LEN chars, but the occasional short one
    // would be confusing for a human reader since it isn't sorted. We right-pad
    // if the output is a terminal, but not do any padding otherwise to be able
    // to feed the output of this command to another CLI tool.
    let interactive = std::io::stdout().is_terminal();
    for solver in state.solvers() {
        if interactive {
            println!("{:>MAX_BASE58_LEN$}", solver.to_string());
        } else {
            println!("{solver}");
        }
    }

    Ok(())
}
