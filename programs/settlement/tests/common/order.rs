//! On-chain order construction shared by the settlement integration tests.

use litesvm::LiteSVM;
use settlement_client::instructions::CreateOrder;
use settlement_client::settlement_interface::data::intent::{OrderIntent, OrderKind};
use solana_sdk::{
    pubkey::Pubkey,
    signature::{Keypair, Signer},
};

use super::{signed_tx, token};

/// A default valid sell order owned by `owner`, selling from `sell_token_account`.
/// `salt` is folded into `app_data` so callers can mint several orders that hash
/// to different UIDs (and therefore different order PDAs).
pub fn sample_intent(owner: Pubkey, sell_token_account: Pubkey, salt: u8) -> OrderIntent {
    OrderIntent {
        owner,
        buy_token_account: Pubkey::new_from_array([0x22; 32]),
        sell_token_account,
        sell_amount: 1_000_000,
        buy_amount: 2_000_000,
        valid_to: 0xdead_beef,
        kind: OrderKind::Sell,
        partially_fillable: true,
        app_data: [salt; 32],
    }
}

/// Create `intent`'s order PDA on-chain, signed and paid for by `owner`.
pub fn create_order_pda(
    svm: &mut LiteSVM,
    program_id: &Pubkey,
    owner: &Keypair,
    intent: &OrderIntent,
) {
    let ix = CreateOrder {
        program_id: *program_id,
        owner: owner.pubkey(),
        created_by: owner.pubkey(),
        intent,
    };
    let tx = signed_tx(svm, owner, owner, ix);
    svm.send_transaction(tx)
        .expect("create_order should succeed");
}

/// Builder that mints a valid settleable order on-chain and returns its intent.
/// If nothing else is specified, it uses default parameters to build the order.
/// Individual parameters can be changed before building the order.
pub struct OrderBuilder<'a> {
    svm: &'a mut LiteSVM,
    program_id: &'a Pubkey,
    payer: &'a Keypair,
    intent: OrderIntent,
}

impl<'a> OrderBuilder<'a> {
    pub fn new(
        svm: &'a mut LiteSVM,
        program_id: &'a Pubkey,
        payer: &'a Keypair,
        mint: &'a Pubkey,
    ) -> Self {
        let sell_token = token::create_token_account(svm, payer, mint, &payer.pubkey());
        let intent = sample_intent(payer.pubkey(), sell_token, 0);
        Self {
            svm,
            program_id,
            payer,
            intent,
        }
    }

    /// Make this order distinct from its siblings: `salt` is folded into
    /// `app_data` so each value hashes to a different UID (and order PDA).
    pub fn salt(mut self, salt: u8) -> Self {
        self.intent.app_data = [salt; 32];
        self
    }

    pub fn valid_to(mut self, valid_to: u32) -> Self {
        self.intent.valid_to = valid_to;
        self
    }

    pub fn build(self) -> OrderIntent {
        create_order_pda(self.svm, self.program_id, self.payer, &self.intent);
        self.intent
    }
}
