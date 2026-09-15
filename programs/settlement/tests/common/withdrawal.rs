//! Withdrawal-order construction and helpers shared by the withdrawal
//! integration tests (placement and settlement).

use cow_settlement_client::cow_settlement_interface::data::intent::{
    Flags, OrderIntent, OrderKind,
};
use litesvm::LiteSVM;
use solana_sdk::{pubkey::Pubkey, signer::Signer};

use super::{buffer, token, InitializedParams};

/// The token accounts a withdrawal trades between: a funded buffer to sell out
/// of, and a treasury to receive the proceeds.
pub struct FeeWithdrawalAccounts {
    pub fee_mint: Pubkey,
    pub fee_buffer: Pubkey,
    pub buy_mint: Pubkey,
    pub treasury: Pubkey,
}

/// Create a fee mint whose canonical buffer holds `fees`, and a treasury of a
/// fresh buy mint for the proceeds to land in.
pub fn prepare_fee_withdrawal_accounts(
    svm: &mut LiteSVM,
    params: &InitializedParams,
    fees: u64,
) -> FeeWithdrawalAccounts {
    let fee_mint = token::create_mint(svm, &params.payer);
    let fee_buffer = buffer::ensure_funded(svm, &params.program_id, &params.payer, &fee_mint, fees);
    let buy_mint = token::create_mint(svm, &params.payer);
    let treasury =
        token::create_token_account(svm, &params.payer, &buy_mint, &params.payer.pubkey());
    FeeWithdrawalAccounts {
        fee_mint,
        fee_buffer,
        buy_mint,
        treasury,
    }
}

/// A fill-or-kill withdrawal order owned by `owner`, selling `sell_amount` out
/// of `fee_withdrawal_accounts`'s fee buffer for at least `buy_amount` delivered to its treasury.
pub fn sample_fee_order(
    owner: Pubkey,
    fee_withdrawal_accounts: &FeeWithdrawalAccounts,
    sell_amount: u64,
    buy_amount: u64,
) -> OrderIntent {
    OrderIntent {
        owner,
        sell_token_account: fee_withdrawal_accounts.fee_buffer,
        sell_mint: fee_withdrawal_accounts.fee_mint,
        buy_token_account: fee_withdrawal_accounts.treasury,
        buy_mint: fee_withdrawal_accounts.buy_mint,
        sell_amount,
        buy_amount,
        flags: Flags {
            created_on_chain: true,
            kind: OrderKind::Sell,
            partially_fillable: false,
        },
        app_data: [0; 32],
        valid_to: 0xdead_beef,
    }
}
