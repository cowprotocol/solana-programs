//! Orca Whirlpool swap planning: turns per-mint surplus/deficit tallies from a
//! settlement into the swap instructions (and payer-wallet routing) needed to
//! cover the deficits.
//!
//! The Whirlpools SDK is async-only; [`OrcaClient`] bridges into it with a
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
    instruction::create_associated_token_account_idempotent,
};
use std::collections::{HashMap, HashSet};

use crate::cmd::Context;

/// Bridges this otherwise-synchronous CLI into the (async-only) Orca Whirlpools SDK.
pub struct OrcaClient {
    rpc: AsyncRpcClient,
    runtime: tokio::runtime::Runtime,
}

impl OrcaClient {
    pub fn new(rpc_url: &str, commitment: CommitmentConfig) -> anyhow::Result<Self> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .context("failed to start the Tokio runtime needed to drive the Orca SDK")?;

        // The solver funds swaps through its own regular ATAs, same as how the rest of
        // this CLI treats WSOL, instead of Orca's default ephemeral-keypair wrapping —
        // so a swap never needs an extra transaction signer beyond the payer.
        set_native_mint_wrapping_strategy(NativeMintWrappingStrategy::Ata).map_err(|e| {
            anyhow::anyhow!("failed to configure Orca's SOL-wrapping strategy: {e}")
        })?;

        Ok(Self {
            rpc: AsyncRpcClient::new_with_commitment(rpc_url.to_string(), commitment),
            runtime,
        })
    }
}

/// Orca swap instructions needed to cover every deficit, plus the payer-wallet
/// routing [`crate::cmd::settle`] needs to fold into its pull plan.
pub struct SwapPlan {
    /// Idempotent payer-ATA creation for every surplus mint a swap draws from.
    /// Must run before `BeginSettle`, since its `Pull` destinations have to
    /// already exist on-chain.
    pub setup_ixs: Vec<Instruction>,
    /// The swaps themselves, plus the transfers moving their output into the
    /// deficit mints' buffer PDAs. Must run between `BeginSettle` and
    /// `FinalizeSettle`.
    pub swap_ixs: Vec<Instruction>,
    /// How much of each surplus mint needs to be pulled to the payer's own
    /// wallet (instead of its buffer) to fund the swaps above.
    pub payer_pulls: HashMap<Pubkey, u64>,
}

/// Find Orca swaps that cover every mint in `deficits` (mints the settlement
/// is short of) by drawing down `surplus` (mints the settlement has extra of),
/// greedily pairing them off. Errors if a deficit can't be fully covered by
/// the available surplus and Orca liquidity.
pub fn plan_swaps(
    orca: &OrcaClient,
    ctx: &Context,
    surplus: &HashMap<Pubkey, u64>,
    deficits: &[(Pubkey, u64)],
) -> anyhow::Result<SwapPlan> {
    let mut surplus_remaining = surplus.clone();
    let mut setup_ixs = Vec::new();
    let mut swap_ixs = Vec::new();
    let mut payer_pulls: HashMap<Pubkey, u64> = HashMap::new();
    let mut payer_atas_created: HashSet<Pubkey> = HashSet::new();

    for &(deficit_mint, deficit_amount) in deficits {
        let mut remaining = deficit_amount;

        for (&surplus_mint, avail) in surplus_remaining.iter_mut() {
            if remaining == 0 {
                break;
            }
            if *avail == 0 {
                continue;
            }

            let Some(fill) = orca.runtime.block_on(plan_pair(
                orca,
                ctx.payer.pubkey(),
                surplus_mint,
                deficit_mint,
                remaining,
                *avail,
            ))?
            else {
                // No initialized Orca pool for this pair; try the next surplus mint.
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

            let (deficit_buffer, _) = find_buffer_pda(&ctx.program_id, &deficit_mint);
            let payer_output_ata = get_associated_token_address_with_program_id(
                &ctx.payer.pubkey(),
                &deficit_mint,
                &spl_token_interface::id(),
            );
            swap_ixs.push(
                spl_token_interface::instruction::transfer(
                    &spl_token_interface::id(),
                    &payer_output_ata,
                    &deficit_buffer,
                    &ctx.payer.pubkey(),
                    &[],
                    fill.output_covered,
                )
                .context("failed to build transfer of swap proceeds into the buffer")?,
            );

            payer_pulls
                .entry(surplus_mint)
                .and_modify(|v| *v = v.saturating_add(fill.input_used))
                .or_insert(fill.input_used);
            *avail = avail.saturating_sub(fill.input_used);
            remaining = remaining.saturating_sub(fill.output_covered);
        }

        anyhow::ensure!(
            remaining == 0,
            "no Orca liquidity/surplus available to cover the remaining {remaining} of mint \
             {deficit_mint}'s deficit",
        );
    }

    Ok(SwapPlan {
        setup_ixs,
        swap_ixs,
        payer_pulls,
    })
}

/// The result of covering (some or all of) one deficit from one surplus mint.
struct PairFill {
    instructions: Vec<Instruction>,
    /// How much of `surplus_mint` this swap consumes.
    input_used: u64,
    /// How much of `deficit_mint` this swap's output covers.
    output_covered: u64,
}

/// Plan a single surplus/deficit pair: try to buy the full remaining deficit
/// with an exact-output swap; if the surplus mint can't cover that swap's
/// worst-case input, fall back to spending all of `surplus_avail` via an
/// exact-input swap instead (a partial fill, leaving the rest of the deficit
/// for another surplus mint). Returns `None` if there's no initialized Orca
/// pool for this pair.
async fn plan_pair(
    orca: &OrcaClient,
    signer: Pubkey,
    surplus_mint: Pubkey,
    deficit_mint: Pubkey,
    deficit_remaining: u64,
    surplus_avail: u64,
) -> anyhow::Result<Option<PairFill>> {
    let Some(pool) = find_pool(orca, surplus_mint, deficit_mint).await? else {
        return Ok(None);
    };

    // Both of the swap's own token accounts need to survive it intact: the output ATA
    // still has to hold the proceeds for the transfer into the deficit buffer that
    // follows, and the input ATA shouldn't get swept out from under a later swap that
    // also draws on this surplus mint. `NativeMintWrappingStrategy::Ata` (set in
    // `OrcaClient::new`) auto-closes a native-mint ATA it had to create for the swap,
    // which would silently destroy either of those — see `strip_close_account`.
    let protected = [
        get_associated_token_address_with_program_id(
            &signer,
            &surplus_mint,
            &spl_token_interface::id(),
        ),
        get_associated_token_address_with_program_id(
            &signer,
            &deficit_mint,
            &spl_token_interface::id(),
        ),
    ];

    let config = SwapConfig {
        slippage_tolerance_bps: None,
        signer: Some(signer),
        whirlpool_deployment: Some(WhirlpoolDeployment::devnet()),
    };

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

    if quote.token_max_in <= surplus_avail {
        return Ok(Some(PairFill {
            instructions: strip_close_account(exact_out.instructions, &protected),
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
        instructions: strip_close_account(exact_in.instructions, &protected),
        input_used: surplus_avail,
        // `token_min_out` is what the swap instruction is guaranteed to deliver even at
        // the edge of its slippage tolerance; using the estimate instead could leave the
        // buffer short of what FinalizeSettle expects it to push out.
        output_covered: quote.token_min_out,
    }))
}

/// Drop any SPL Token `CloseAccount` instruction targeting one of `protected` accounts.
/// `CloseAccount`'s instruction data is always the single discriminant byte `9` with no
/// extra payload, and its first account is always the account being closed.
fn strip_close_account(instructions: Vec<Instruction>, protected: &[Pubkey]) -> Vec<Instruction> {
    instructions
        .into_iter()
        .filter(|ix| {
            let is_close_account = ix.program_id == spl_token_interface::id() && ix.data == [9];
            let targets_protected = ix
                .accounts
                .first()
                .is_some_and(|a| protected.contains(&a.pubkey));
            !(is_close_account && targets_protected)
        })
        .collect()
}

/// Find the Orca Whirlpool with the most liquidity for `mint_a`/`mint_b` on devnet,
/// across every fee tier plus the splash pool. Returns `None` if none are initialized.
async fn find_pool(
    orca: &OrcaClient,
    mint_a: Pubkey,
    mint_b: Pubkey,
) -> anyhow::Result<Option<Pubkey>> {
    let deployment = WhirlpoolDeployment::devnet();

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
