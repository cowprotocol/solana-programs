use anyhow::Context as _;
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
    let solvers: Vec<_> = state.solvers().collect();

    println!("statePda: {state_pda}");
    println!("solvers: {}", solvers.len());
    for solver in solvers {
        println!("  {solver}");
    }

    Ok(())
}
