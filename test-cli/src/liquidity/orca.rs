//! Orca Whirlpool swap planning
//!
//! The Whirlpools SDK is async-only, so [`OrcaClient`] bridges into it with a
//! small Tokio runtime scoped to this module so the rest of the CLI can stay
//! synchronous.

use anyhow::Context as _;
use orca_whirlpools::{
    fetch_splash_pool, fetch_whirlpools_by_token_pair, set_native_mint_wrapping_strategy,
    swap_instructions, NativeMintWrappingStrategy, PoolInfo, SwapConfig, SwapQuote, SwapType,
    WhirlpoolDeployment,
};
use settlement_client::settlement_interface::{pda::buffer::find_buffer_pda, Pubkey};
use solana_client::nonblocking::rpc_client::RpcClient as AsyncRpcClient;
use solana_commitment_config::CommitmentConfig;
use solana_instruction::Instruction;
use solana_sdk::signature::Signer;
use spl_associated_token_account_interface::{
    address::get_associated_token_address_with_program_id,
    instruction::create_associated_token_account_idempotent, program::ID as ATA_PROGRAM_ID,
};
use std::collections::{HashMap, HashSet};

use super::SwapPlan;
use crate::cmd::Context;
use crate::network::{DEVNET_GENESIS_HASH, MAINNET_GENESIS_HASH};

/// Bridges this otherwise-synchronous CLI into the (async-only) Orca Whirlpools SDK.
pub struct OrcaClient {
    rpc: AsyncRpcClient,
    runtime: tokio::runtime::Runtime,
    deployment: WhirlpoolDeployment,
}

impl OrcaClient {
    pub fn new(rpc_url: &str, commitment: CommitmentConfig) -> anyhow::Result<Self> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .context("failed to start the Tokio runtime needed to drive the Orca SDK")?;

        // Compatibility hack: The SDK's default wraps SOL through an ephemeral keypair account.
        // But we are sophisticated and want do our own wrapping. To prevent this from happening,
        // set `NativeMintWrappingStrategy::None`.
        set_native_mint_wrapping_strategy(NativeMintWrappingStrategy::None).map_err(|e| {
            anyhow::anyhow!("failed to configure Orca's SOL-wrapping strategy: {e}")
        })?;

        let rpc = AsyncRpcClient::new_with_commitment(rpc_url.to_string(), commitment);
        let deployment = runtime.block_on(resolve_deployment(&rpc))?;

        Ok(Self {
            rpc,
            runtime,
            deployment,
        })
    }
}

/// Resolve the Orca deployment configuration corresponding to the configured RPC URL
async fn resolve_deployment(rpc: &AsyncRpcClient) -> anyhow::Result<WhirlpoolDeployment> {
    let genesis_hash = rpc
        .get_genesis_hash()
        .await
        .context("failed to fetch genesis hash (is the RPC URL correct?)")?
        .to_string();

    match genesis_hash.as_str() {
        DEVNET_GENESIS_HASH => Ok(WhirlpoolDeployment::devnet()),
        MAINNET_GENESIS_HASH => Ok(WhirlpoolDeployment::mainnet()),
        other => anyhow::bail!(
            "no Orca Whirlpools deployment is known for the network with genesis hash {other}.",
        ),
    }
}

/// Find Orca swaps that cover every mint in `deficits` (mints the settlement
/// is short of) by drawing down `surplus` (mints the settlement has extra of),
/// greedily pairing them off. Errors if a deficit can't be fully covered by
/// the available surplus and Orca liquidity.
pub fn plan_swaps(
    orca: &OrcaClient,
    ctx: &Context,
    surplus: &HashMap<Pubkey, u64>,
    deficits: &HashMap<Pubkey, u64>,
) -> anyhow::Result<SwapPlan> {
    let mut surplus_remaining = surplus.clone();
    let mut setup_ixs = Vec::new();
    let mut swap_ixs = Vec::new();
    let mut sinks: HashMap<Pubkey, u64> = HashMap::new();
    let mut payer_atas_created: HashSet<Pubkey> = HashSet::new();

    for (deficit_mint, deficit_amount) in deficits {
        let mut deficit_remaining = *deficit_amount;
        let (deficit_buffer, _) = find_buffer_pda(&ctx.program_id, deficit_mint);

        for (&surplus_mint, avail) in surplus_remaining.iter_mut() {
            if deficit_remaining == 0 {
                break;
            }
            if *avail == 0 {
                continue;
            }

            // See if there is a swap route from surplus to deficit
            let Some(fill) = orca.runtime.block_on(plan_pair(
                orca,
                ctx.payer.pubkey(),
                surplus_mint,
                *deficit_mint,
                deficit_buffer,
                deficit_remaining,
                *avail,
            ))?
            else {
                // No route for this pair; try the next surplus mint.
                continue;
            };

            if payer_atas_created.insert(surplus_mint) {
                setup_ixs.push(create_associated_token_account_idempotent(
                    &ctx.payer.pubkey(),
                    &ctx.payer.pubkey(),
                    &surplus_mint,
                    &spl_token_interface::id(),
                ));
            }

            swap_ixs.extend(fill.instructions);

            // update the amount of funds needed to complete the settlement
            let sink = sinks.entry(surplus_mint).or_insert(0);
            *sink = sink
                .checked_add(fill.input_used)
                .with_context(|| format!("swap input tally overflow for mint {surplus_mint}"))?;

            // `plan_pair` never spends more than the `*avail` it was handed, so an
            // underflow here would mean it ignored the budget.
            *avail = avail.checked_sub(fill.input_used).with_context(|| {
                format!("swap consumed more of mint {surplus_mint} than was available")
            })?;

            // Unlike the input side, the output can legitimately overshoot, so saturating is fine here
            deficit_remaining = deficit_remaining.saturating_sub(fill.output_covered);
        }

        anyhow::ensure!(
            deficit_remaining == 0,
            "no Orca liquidity/surplus available to cover the remaining {deficit_remaining} of mint \
             {deficit_mint}'s deficit",
        );
    }

    Ok(SwapPlan {
        setup_ixs,
        swap_ixs,
        teardown_ixs: vec![],
        sinks,
    })
}

/// The result of covering (some or all of) one deficit mint from one surplus mint.
struct PairFill {
    /// Instructions to execute to actually complete the swap
    instructions: Vec<Instruction>,
    /// How much of `surplus_mint` this swap consumes.
    input_used: u64,
    /// How much of `deficit_mint` this swap's output covers.
    output_covered: u64,
}

/// Create a route for single surplus/deficit pair.
/// Try to buy the full remaining deficit with an exact-output swap.
/// If the surplus mint can't cover that swap's worst-case input, fall back
/// to spending all of `surplus_avail` via an exact-input swap instead
/// (a partial fill, leaving the rest of the deficit for another surplus mint).
/// The proceeds are paid into `deficit_buffer` rather than the signer's wallet.
/// Returns `None` if there's no suitable Orca pool for this pair.
async fn plan_pair(
    orca: &OrcaClient,
    signer: Pubkey,
    surplus_mint: Pubkey,
    deficit_mint: Pubkey,
    deficit_buffer: Pubkey,
    deficit_remaining: u64,
    surplus_avail: u64,
) -> anyhow::Result<Option<PairFill>> {
    let Some(pool) = find_pool(orca, surplus_mint, deficit_mint).await? else {
        return Ok(None);
    };

    // HACK: orca's rust library only lets us put output tokens into our wallet's ATA.
    // however, we actually want to put it into the buffers for the settlement program.
    // So we need to search for where the output_ata appears in the transaciton, and replace
    // with the actual wallet for the buffer.
    let output_ata = get_associated_token_address_with_program_id(
        &signer,
        &deficit_mint,
        &spl_token_interface::id(),
    );

    let swap_instruction_rewrites = |instructions| {
        rewrite_swap_output_to_buffer(
            instructions,
            orca.deployment.id(),
            output_ata,
            deficit_buffer,
        )
    };

    let config = SwapConfig {
        slippage_tolerance_bps: None,
        signer: Some(signer),
        whirlpool_deployment: Some(orca.deployment),
    };

    // First we try an exact_out transaction. If input funds are insufficient, this will fail
    // and we can exact_in the funds we actually have instead.
    let exact_out = swap_instructions(
        &orca.rpc,
        pool,
        deficit_remaining,
        deficit_mint,
        SwapType::ExactOut,
        config.clone(),
    )
    .await
    .map_err(|e| anyhow::anyhow!("failed to quote {surplus_mint} -> {deficit_mint} swap: {e}"))?;

    let SwapQuote::ExactOut(quote) = exact_out.quote else {
        anyhow::bail!("Orca returned the wrong quote type for an exact-output swap");
    };
    anyhow::ensure!(
        exact_out.additional_signers.is_empty(),
        "swap unexpectedly requires extra signers beyond the payer",
    );

    // Exit early if we got the necessary funds
    if quote.token_max_in <= surplus_avail {
        return Ok(Some(PairFill {
            instructions: swap_instruction_rewrites(exact_out.instructions)?,
            input_used: quote.token_max_in,
            output_covered: deficit_remaining,
        }));
    }

    // The full deficit would need more than the surplus mint has on offer; spend all of
    // it via an exact-input swap instead, and leave the rest of the deficit uncovered.
    let exact_in = swap_instructions(
        &orca.rpc,
        pool,
        surplus_avail,
        surplus_mint,
        SwapType::ExactIn,
        config,
    )
    .await
    .map_err(|e| anyhow::anyhow!("failed to quote {surplus_mint} -> {deficit_mint} swap: {e}"))?;

    let SwapQuote::ExactIn(quote) = exact_in.quote else {
        anyhow::bail!("Orca returned the wrong quote type for an exact-input swap");
    };
    anyhow::ensure!(
        exact_in.additional_signers.is_empty(),
        "swap unexpectedly requires extra signers beyond the payer",
    );

    Ok(Some(PairFill {
        instructions: swap_instruction_rewrites(exact_in.instructions)?,
        input_used: surplus_avail,
        // `token_min_out` is what the swap instruction is guaranteed to deliver even at
        // the edge of its slippage tolerance; using the estimate instead could leave the
        // buffer short of what FinalizeSettle expects it to push out.
        output_covered: quote.token_min_out,
    }))
}

/// Pay the swap's proceeds into `buffer` instead of the signer's own `output_ata`.
/// It also removes any instructions that create the `output_ata`.
///
/// This function is needed since orca's own library doesn't provide a way to specify swap accounts
/// outside the ATA (and using the lower level libs would be way too much more work)
fn rewrite_swap_output_to_buffer(
    instructions: Vec<Instruction>,
    whirlpool_program: Pubkey,
    output_ata: Pubkey,
    buffer: Pubkey,
) -> anyhow::Result<Vec<Instruction>> {
    let mut redirected = false;

    let routed = instructions
        .into_iter()
        .filter_map(|mut ix| {
            if ix.program_id == whirlpool_program {
                for account in &mut ix.accounts {
                    if account.pubkey == output_ata {
                        account.pubkey = buffer;
                        redirected = true;
                    }
                }
                return Some(ix);
            }

            // Orca also tries to add account creation transaction for the ATA,
            // but we don't need it.
            (!creates_token_account(&ix, output_ata)).then_some(ix)
        })
        .collect();

    anyhow::ensure!(
        redirected,
        "no swap instruction references the payer's output ATA {output_ata}; Orca's account \
         layout may have changed",
    );

    Ok(routed)
}

/// Whether `ix` is an Associated Token Account program instruction creating `account`,
/// which every one of its variants passes as its second account.
fn creates_token_account(ix: &Instruction, account: Pubkey) -> bool {
    ix.program_id == ATA_PROGRAM_ID && ix.accounts.get(1).is_some_and(|a| a.pubkey == account)
}

/// Find the Orca Whirlpool with the most liquidity for `mint_a`/`mint_b` on the
/// cluster's deployment, across every fee tier plus the splash pool. Returns `None`
/// if none are initialized.
async fn find_pool(
    orca: &OrcaClient,
    mint_a: Pubkey,
    mint_b: Pubkey,
) -> anyhow::Result<Option<Pubkey>> {
    let deployment = orca.deployment;

    let mut pools = fetch_whirlpools_by_token_pair(&orca.rpc, mint_a, mint_b, Some(deployment))
        .await
        .map_err(|e| anyhow::anyhow!("failed to look up Orca pools for {mint_a}/{mint_b}: {e}"))?;

    // The splash pool uses a fee tier outside the ones enumerated above; a failure here
    // (e.g. it was never created for this pair) just means one fewer candidate to check.
    if let Ok(splash) = fetch_splash_pool(&orca.rpc, mint_a, mint_b, Some(deployment)).await {
        pools.push(splash);
    }

    Ok(pools
        .into_iter()
        .filter_map(|pool| match pool {
            PoolInfo::Initialized(pool) => Some(pool),
            PoolInfo::Uninitialized(_) => None,
        })
        .max_by_key(|pool| pool.data.liquidity)
        .map(|pool| pool.address))
}
