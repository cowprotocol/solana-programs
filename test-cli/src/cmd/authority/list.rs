use anyhow::Context as _;
use cow_settlement_client::{
    cow_settlement_interface::pda::state::find_state_pda, pda::state::DecodedStateAccount,
};

use crate::cmd::Context;
use crate::utils::output::print_summary;

pub fn run(ctx: Context) -> anyhow::Result<()> {
    let (state_pda, _) = find_state_pda(&ctx.program_id);
    let data = ctx
        .rpc
        .get_account_data(&state_pda)
        .with_context(|| format!("failed to fetch state account {state_pda}"))?;
    let DecodedStateAccount {
        manager,
        reclaim_authority,
        self_order_authority,
    } = DecodedStateAccount::try_from(data.as_slice())
        .map_err(|e| anyhow::anyhow!("failed to decode state account {state_pda}: {e:?}"))?;

    print_summary(&[
        ("manager", &manager),
        ("reclaimAuthority", &reclaim_authority),
        ("selfOrderAuthority", &self_order_authority),
    ]);

    Ok(())
}
