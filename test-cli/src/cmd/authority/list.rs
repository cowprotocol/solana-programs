use anyhow::Context as _;
use clap::ValueEnum;
use cow_settlement_client::cow_settlement_interface::{
    data::state::StateAccount, pda::state::find_state_pda, Role,
};

use crate::cmd::Context;
use crate::utils::output::print_summary;

pub fn run(ctx: Context) -> anyhow::Result<()> {
    let (state_pda, _) = find_state_pda(&ctx.program_id);
    let data = ctx
        .rpc
        .get_account_data(&state_pda)
        .with_context(|| format!("failed to fetch state account {state_pda}"))?;
    let state = StateAccount::attach(data.as_slice())
        .map_err(|e| anyhow::anyhow!("failed to decode state account {state_pda}: {e:?}"))?;

    let authorities: Vec<_> = Role::value_variants()
        .iter()
        .map(|role| {
            (
                role.to_possible_value().expect("no role is skipped"),
                state.authority(*role),
            )
        })
        .collect();

    print_summary(
        &authorities
            .iter()
            .map(|(role, authority)| (role.get_name(), authority as &dyn ToString))
            .collect::<Vec<_>>(),
    );

    Ok(())
}
