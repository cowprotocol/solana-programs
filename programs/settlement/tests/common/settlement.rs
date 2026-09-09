//! Scaffolding for building `[BeginSettle, FinalizeSettle]` settlement pairs.

use cow_settlement_client::instruction::{
    BeginSettle, FinalizeSettle, FinalizedIntent, InitializedIntent, Pull,
};
use cow_settlement_interface::{data::intent::OrderIntent, Instruction};
use litesvm::LiteSVM;
use solana_sdk::{pubkey::Pubkey, signature::Keypair};

use super::{buffer, token, unique_pubkey};

/// Positions of the two instructions in the `[BeginSettle, FinalizeSettle]` pair
/// the settlement tests build: begin first, finalize right after it. Each
/// instruction points at the other through its `begin_ix_index`/`finalize_ix_index`.
pub const BEGIN_INDEX: u8 = 0;
pub const FINALIZE_INDEX: u8 = 1;

/// Build the `[begin, finalize]` instructions where `finalize` is a pre-built
/// `FinalizeSettle` at [`FINALIZE_INDEX`] and `begin` settles `orders` (with no
/// pulls) at [`BEGIN_INDEX`], the same orders the finalize is expected to push
/// to. Submit the result with [`send`](super::send).
///
/// Use this when the test needs to hand-build the finalize (to corrupt it, say);
/// [`build_staged_settlement`] builds both halves from staged orders instead.
pub fn build_settlement(
    program_id: &Pubkey,
    solver: &Pubkey,
    orders: &[FinalizedIntent],
    finalize: impl Into<Instruction>,
) -> Vec<Instruction> {
    let begin_orders: Vec<InitializedIntent> = orders
        .iter()
        .map(|order| InitializedIntent {
            intent: order.intent,
            pulls: &[],
        })
        .collect();
    let begin = BeginSettle {
        program_id: *program_id,
        solver: *solver,
        finalize_ix_index: FINALIZE_INDEX.into(),
        auction_id: 0,
        orders: &begin_orders,
    };
    vec![begin.into(), finalize.into()]
}

/// An order staged for settlement by [`stage_order`]: the intent, the [`Pull`]s
/// to draw from its sell token account, and the amount to push to its buy token
/// account. It owns its intent, so a helper that mints an order can stage it and
/// hand back the result in one piece.
#[derive(Clone)]
pub struct StagedOrder {
    pub intent: OrderIntent,
    pub pulls: Vec<Pull>,
    pub amount_out: u64,
}

/// Stage the token side of settling `intent`, so a settlement of the result can
/// actually move the funds: fund and delegate the sell token account for the
/// total of `pulls`, give each pull its own throwaway destination account of the
/// sell mint, and fund the buy mint's canonical buffer with `amount_out` for the
/// push to draw on. `payer` funds all of it.
///
/// Buffers are shared per mint, so staging several orders that buy the same mint
/// accumulates their `amount_out` in the one buffer.
pub fn stage_order(
    svm: &mut LiteSVM,
    program_id: &Pubkey,
    payer: &Keypair,
    intent: &OrderIntent,
    pulls: &[u64],
    amount_out: u64,
) -> StagedOrder {
    let amount_in: u64 = pulls.iter().sum();
    token::fund_and_delegate(
        svm,
        program_id,
        payer,
        &intent.sell_token_account,
        amount_in,
    );
    let pulls = pulls
        .iter()
        .map(|&amount| Pull {
            destination: token::create_token_account(
                svm,
                payer,
                &intent.sell_mint,
                &unique_pubkey(),
            ),
            amount,
        })
        .collect();
    buffer::ensure_funded(svm, program_id, payer, &intent.buy_mint, amount_out);

    StagedOrder {
        intent: intent.clone(),
        pulls,
        amount_out,
    }
}

/// Build the instructions settling `orders`: a `BeginSettle` at [`BEGIN_INDEX`]
/// carrying each order's pulls, `between` right after it, and the matching
/// `FinalizeSettle` last, pushing each order's `amount_out`. Submit the result
/// with [`send`](super::send).
pub fn build_staged_settlement(
    program_id: &Pubkey,
    solver: &Pubkey,
    orders: &[StagedOrder],
    between: Vec<Instruction>,
) -> Vec<Instruction> {
    let begin_orders: Vec<InitializedIntent> = orders
        .iter()
        .map(|order| InitializedIntent {
            intent: &order.intent,
            pulls: &order.pulls,
        })
        .collect();
    let finalize_orders: Vec<FinalizedIntent> = orders
        .iter()
        .map(|order| FinalizedIntent {
            intent: &order.intent,
            amount: order.amount_out,
        })
        .collect();

    let begin = BeginSettle {
        program_id: *program_id,
        solver: *solver,
        finalize_ix_index: finalize_index(between.len()),
        auction_id: 0,
        orders: &begin_orders,
    };
    let finalize = FinalizeSettle {
        program_id: *program_id,
        begin_ix_index: BEGIN_INDEX.into(),
        orders: &finalize_orders,
    };

    let mut instructions = vec![begin.into()];
    instructions.extend(between);
    instructions.push(finalize.into());
    instructions
}

/// Where the `FinalizeSettle` lands in a settlement with `interposed`
/// instructions sitting between
fn finalize_index(interposed: usize) -> u16 {
    u16::try_from(interposed)
        .ok()
        .and_then(|shift| shift.checked_add(BEGIN_INDEX.into()))
        .and_then(|shift| shift.checked_add(1))
        .expect("a test transaction holds far fewer than u16::MAX instructions")
}
