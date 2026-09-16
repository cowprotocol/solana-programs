//! On-chain order construction shared by the settlement integration tests.

use cow_settlement_client::cow_settlement_interface::data::intent::{
    Flags, OrderIntent, OrderKind,
};
use cow_settlement_client::cow_settlement_interface::data::order::OrderAccount;
use cow_settlement_client::cow_settlement_interface::pda::state::find_state_pda;
use cow_settlement_client::instruction::{CreateOrder, CreateWithdrawalOrder};
use litesvm::LiteSVM;
use solana_sdk::{
    pubkey::Pubkey,
    signature::{Keypair, Signer},
};

use super::{buffer, signed_tx, token};

/// Decode the [`OrderAccount`] stored at an order PDA.
pub fn read_order(svm: &LiteSVM, pda: &Pubkey) -> OrderAccount {
    let account = svm.get_account(pda).expect("order PDA must exist");
    OrderAccount::try_from(&account.data[..]).expect("order PDA must decode")
}

/// A default valid sell order owned by `owner`, using placeholders for all
/// token accounts and mints.
/// `salt` is folded into `app_data` so callers can mint several orders that hash
/// to different UIDs (and therefore different order PDAs).
pub fn sample_intent(owner: Pubkey, salt: u8) -> OrderIntent {
    OrderIntent {
        owner,
        sell_token_account: Pubkey::new_from_array([0x22; 32]),
        sell_mint: Pubkey::new_from_array([0x33; 32]),
        buy_token_account: Pubkey::new_from_array([0x44; 32]),
        buy_mint: Pubkey::new_from_array([0x55; 32]),
        sell_amount: 1_000_000,
        buy_amount: 2_000_000,
        valid_to: 0xdead_beef,
        flags: Flags {
            created_on_chain: true,
            kind: OrderKind::Sell,
            partially_fillable: true,
        },
        app_data: [salt; 32],
    }
}

/// [`sample_intent`] with all four token-account fields filled in with freshly
/// created applicable data. This fulfills the minimum requirements for an order
/// to be settlable.
pub fn settlable_intent(
    svm: &mut LiteSVM,
    payer: &Keypair,
    owner: Pubkey,
    salt: u8,
) -> OrderIntent {
    let sell_mint = token::create_mint(svm, payer);
    let buy_mint = token::create_mint(svm, payer);
    OrderIntent {
        sell_token_account: token::create_token_account(svm, payer, &sell_mint, &owner),
        sell_mint,
        buy_token_account: token::create_token_account(svm, payer, &buy_mint, &owner),
        buy_mint,
        ..sample_intent(owner, salt)
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

/// Place `intent` as a withdrawal order through `CreateWithdrawalOrder`: an
/// order owned by the state PDA, funded by `payer` and gated by the withdrawal
/// `authority`, which co-signs.
fn create_withdrawal_order_pda(
    svm: &mut LiteSVM,
    program_id: &Pubkey,
    payer: &Keypair,
    authority: &Keypair,
    intent: &OrderIntent,
) {
    let ix = CreateWithdrawalOrder {
        program_id: *program_id,
        authority: authority.pubkey(),
        created_by: payer.pubkey(),
        intent,
    };
    // Fee-paid by `payer`, co-signed by the withdrawal `authority`.
    let tx = signed_tx(svm, payer, authority, ix);
    svm.send_transaction(tx)
        .expect("create_withdrawal_order should succeed");
}

/// How an [`OrderBuilder`] sources one side of an order.
enum TokenSource {
    /// A fresh account of a freshly generated mint (the default).
    FreshMint,
    /// A fresh account of the given mint.
    Mint(Pubkey),
    /// The given existing account.
    Account(Pubkey),
}

impl TokenSource {
    /// Resolve this source into the `(mint, token_account)`.
    fn resolve(
        self,
        svm: &mut LiteSVM,
        program_id: &Pubkey,
        payer: &Keypair,
        use_buffer: bool,
    ) -> (Pubkey, Pubkey) {
        let create_account = |svm: &mut LiteSVM, mint: &Pubkey| {
            if use_buffer {
                buffer::ensure_buffer_exists(svm, program_id, payer, mint)
            } else {
                token::create_token_account(svm, payer, mint, &payer.pubkey())
            }
        };
        match self {
            TokenSource::Account(account) => (token::mint_of(svm, &account), account),
            TokenSource::Mint(mint) => (mint, create_account(svm, &mint)),
            TokenSource::FreshMint => {
                let mint = token::create_mint(svm, payer);
                (mint, create_account(svm, &mint))
            }
        }
    }
}

/// Builder that mints a valid settleable order on-chain and returns its intent.
/// If nothing else is specified, it uses default parameters to build the order.
/// Individual parameters can be changed before building the order.
///
/// `build` always creates real sell and buy token accounts. Each side gets its
/// own freshly generated mint, so the two differ unless a test pins one with
/// [`OrderBuilder::sell_mint`] / [`OrderBuilder::buy_mint`].
///
/// Calling [`OrderBuilder::withdrawal`] switches `build` to place a fee
/// withdrawal order.
pub struct OrderBuilder<'a> {
    svm: &'a mut LiteSVM,
    program_id: &'a Pubkey,
    payer: &'a Keypair,
    intent: OrderIntent,
    sell: TokenSource,
    buy: TokenSource,
    withdrawal_authority: Option<&'a Keypair>,
}

impl<'a> OrderBuilder<'a> {
    pub fn new(svm: &'a mut LiteSVM, program_id: &'a Pubkey, payer: &'a Keypair) -> Self {
        // The sell and buy token accounts are created at `build` time;
        // `sample_intent`'s placeholder addresses stand in until then.
        let intent = sample_intent(payer.pubkey(), 0);
        Self {
            svm,
            program_id,
            payer,
            intent,
            sell: TokenSource::FreshMint,
            buy: TokenSource::FreshMint,
            withdrawal_authority: None,
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

    /// Set the order's sell amount (exact or maximum depending on `kind`).
    pub fn sell_amount(mut self, sell_amount: u64) -> Self {
        self.intent.sell_amount = sell_amount;
        self
    }

    /// Set the order's buy amount (exact or minimum depending on `kind`).
    pub fn buy_amount(mut self, buy_amount: u64) -> Self {
        self.intent.buy_amount = buy_amount;
        self
    }

    /// Set the order's kind (`Sell` or `Buy`). Defaults to `Sell`.
    pub fn kind(mut self, kind: OrderKind) -> Self {
        self.intent.flags.kind = kind;
        self
    }

    /// Set whether the order may be filled partially. Defaults to `true`.
    pub fn partially_fillable(mut self, partially_fillable: bool) -> Self {
        self.intent.flags.partially_fillable = partially_fillable;
        self
    }

    /// Pin the mint of the order's sell token account. Defaults to a fresh mint.
    /// Overrides any prior [`sell_token_account`](OrderBuilder::sell_token_account).
    pub fn sell_mint(mut self, mint: &Pubkey) -> Self {
        self.sell = TokenSource::Mint(*mint);
        self
    }

    /// Pin the mint of the order's buy token account. Defaults to a fresh mint.
    pub fn buy_mint(mut self, mint: &Pubkey) -> Self {
        self.buy = TokenSource::Mint(*mint);
        self
    }

    /// Pin the order's sell token account with an existing account, instead of
    /// creating a fresh one for this order. The account's mint becomes the sell
    /// mint, so this overrides any prior [`sell_mint`](OrderBuilder::sell_mint).
    pub fn sell_token_account(mut self, account: &Pubkey) -> Self {
        self.sell = TokenSource::Account(*account);
        self
    }

    /// This will be a withdrawal order, not a normal order.
    pub fn withdrawal(mut self, authority: &'a Keypair) -> Self {
        self.withdrawal_authority = Some(authority);
        self
    }

    pub fn build(self) -> OrderIntent {
        let Self {
            svm,
            program_id,
            payer,
            mut intent,
            sell,
            buy,
            withdrawal_authority,
        } = self;
        // The buy side always uses a fresh payer-owned treasury; only a
        // withdrawal order's sell side draws from a buffer.
        (intent.buy_mint, intent.buy_token_account) = buy.resolve(svm, program_id, payer, false);
        (intent.sell_mint, intent.sell_token_account) =
            sell.resolve(svm, program_id, payer, withdrawal_authority.is_some());

        match withdrawal_authority {
            None => {
                intent.owner = payer.pubkey();
                create_order_pda(svm, program_id, payer, &intent);
            }
            Some(authority) => {
                intent.owner = find_state_pda(program_id).0;
                create_withdrawal_order_pda(svm, program_id, payer, authority, &intent);
            }
        }
        intent
    }
}
