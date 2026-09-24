//! On-chain order construction shared by the settlement integration tests.

use cow_settlement_client::cow_settlement_interface::data::intent::{
    Asset, Flags, OrderIntent, OrderKind, TokenAsset,
};
use cow_settlement_client::cow_settlement_interface::pda::state::find_state_pda;
use cow_settlement_client::instruction::{CreateOrder, CreateSelfOrder};
use cow_settlement_client::pda::order::DecodedOrderAccount;
use cow_settlement_interface::data::intent::ENCODED_NATIVE_SOL_TRANSFER;
use litesvm::LiteSVM;
use solana_sdk::{
    pubkey::Pubkey,
    signature::{Keypair, Signer},
};

use super::{buffer, signed_tx, token, unique_pubkey};

/// Decode the [`DecodedOrderAccount`] stored at an order PDA.
pub fn read_order(svm: &LiteSVM, pda: &Pubkey) -> DecodedOrderAccount {
    let account = svm.get_account(pda).expect("order PDA must exist");
    DecodedOrderAccount::try_from(&account.data[..]).expect("order PDA must decode")
}

/// A default valid sell order owned by `owner`, using placeholders for all
/// token accounts and mints.
/// `salt` is folded into `app_data` so callers can mint several orders that hash
/// to different UIDs (and therefore different order PDAs).
pub fn sample_intent(owner: Pubkey, salt: u8) -> OrderIntent {
    OrderIntent {
        owner,
        sell: TokenAsset {
            token_account: Pubkey::new_from_array([0x22; 32]),
            mint: Pubkey::new_from_array([0x33; 32]),
        },
        buy: Asset::try_from(TokenAsset {
            token_account: Pubkey::new_from_array([0x44; 32]),
            mint: Pubkey::new_from_array([0x55; 32]),
        })
        .expect("not native SOL"),
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

/// The buy mint of a token order. Panics on a native SOL buy, which has no
/// mint; those are placed with [`OrderBuilder::buy_sol`] and their recipient is
/// read back with [`buy_sol_account`].
pub fn buy_mint(intent: &OrderIntent) -> Pubkey {
    match &intent.buy {
        Asset::TokenProgram(token) => token.mint,
        Asset::Native(_) => panic!("expected a token buy, got native SOL"),
    }
}

/// The buy token account of a token order. Panics on a native SOL buy, whose
/// recipient is read with [`buy_sol_account`] instead.
pub fn buy_account(intent: &OrderIntent) -> Pubkey {
    match &intent.buy {
        Asset::TokenProgram(token) => token.token_account,
        Asset::Native(_) => panic!("expected a token buy, got native SOL"),
    }
}

/// The address a native SOL buy credits its lamports to. Panics on a token buy,
/// whose proceeds land in the token account [`buy_account`] returns.
pub fn buy_sol_account(intent: &OrderIntent) -> Pubkey {
    match &intent.buy {
        Asset::Native(account) => *account,
        Asset::TokenProgram(_) => panic!("expected a native SOL buy, got a token"),
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
        sell: TokenAsset {
            mint: sell_mint,
            token_account: token::create_token_account(svm, payer, &sell_mint, &owner),
        },
        buy: Asset::try_from(TokenAsset {
            mint: buy_mint,
            token_account: token::create_token_account(svm, payer, &buy_mint, &owner),
        })
        .expect("not native SOL"),
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

/// Place `intent` as a self order through `CreateSelfOrder`: an
/// order owned by the state PDA, funded by `payer` and gated by the self-order
/// `authority`, which co-signs.
fn create_self_order_pda(
    svm: &mut LiteSVM,
    program_id: &Pubkey,
    payer: &Keypair,
    authority: &Keypair,
    intent: &OrderIntent,
) {
    let ix = CreateSelfOrder {
        program_id: *program_id,
        authority: authority.pubkey(),
        created_by: payer.pubkey(),
        intent,
    };
    // Fee-paid by `payer`, co-signed by the self-order `authority`.
    let tx = signed_tx(svm, payer, authority, ix);
    svm.send_transaction(tx)
        .expect("create_self_order should succeed");
}

/// How an [`OrderBuilder`] sources one side of an order.
enum TokenSource {
    /// A fresh account of a freshly generated mint (the default).
    FreshMint,
    /// Indicates native token (only for buy side)
    Native,
    /// A fresh account of the given mint.
    Mint(Pubkey),
    /// The given existing account.
    Account(Pubkey),
}

impl TokenSource {
    /// Resolve this source into the [`TokenAsset`] it names, creating the token
    /// account it needs.
    fn resolve(
        self,
        svm: &mut LiteSVM,
        program_id: &Pubkey,
        payer: &Keypair,
        use_buffer: bool,
    ) -> Asset {
        let create_account = |svm: &mut LiteSVM, mint: &Pubkey| {
            if use_buffer {
                buffer::ensure_buffer_exists(svm, program_id, payer, mint)
            } else {
                token::create_token_account(svm, payer, mint, &payer.pubkey())
            }
        };
        match self {
            TokenSource::Account(account) => TokenAsset {
                mint: token::mint_of(svm, &account),
                token_account: account,
            }
            .try_into(),
            TokenSource::Native => Ok(Asset::Native(unique_pubkey())),
            TokenSource::Mint(mint) => TokenAsset {
                mint,
                token_account: create_account(svm, &mint),
            }
            .try_into(),
            TokenSource::FreshMint => {
                let mint = token::create_mint(svm, payer);
                TokenAsset {
                    mint,
                    token_account: create_account(svm, &mint),
                }
                .try_into()
            }
        }
        .expect("should not be native mint")
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
/// Calling [`OrderBuilder::self_order`] switches `build` to place a self order
/// instead of a regular one.
pub struct OrderBuilder<'a> {
    svm: &'a mut LiteSVM,
    program_id: &'a Pubkey,
    payer: &'a Keypair,
    intent: OrderIntent,
    sell: TokenSource,
    buy: TokenSource,
    self_order_authority: Option<&'a Keypair>,
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
            self_order_authority: None,
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
    /// Use `buy_sol` to indicate purchase of native lamports
    pub fn buy_mint(mut self, mint: &Pubkey) -> Self {
        assert_ne!(mint, &ENCODED_NATIVE_SOL_TRANSFER, "use buy_sol() instead");
        self.buy = TokenSource::Mint(*mint);
        self
    }

    /// Pin the buy token to be native lamports
    pub fn buy_sol(mut self) -> Self {
        self.buy = TokenSource::Native;
        self
    }

    /// Pin the order's sell token account with an existing account, instead of
    /// creating a fresh one for this order. The account's mint becomes the sell
    /// mint, so this overrides any prior [`sell_mint`](OrderBuilder::sell_mint).
    pub fn sell_token_account(mut self, account: &Pubkey) -> Self {
        self.sell = TokenSource::Account(*account);
        self
    }

    /// This will be a self order, not a normal order.
    pub fn self_order(mut self, authority: &'a Keypair) -> Self {
        self.self_order_authority = Some(authority);
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
            self_order_authority,
        } = self;

        let Asset::TokenProgram(sell) =
            sell.resolve(svm, program_id, payer, self_order_authority.is_some())
        else {
            panic!("sell side cannot be native SOL");
        };
        intent.sell = sell;

        intent.buy = buy.resolve(svm, program_id, payer, false);

        match self_order_authority {
            None => {
                intent.owner = payer.pubkey();
                create_order_pda(svm, program_id, payer, &intent);
            }
            Some(authority) => {
                intent.owner = find_state_pda(program_id).0;
                create_self_order_pda(svm, program_id, payer, authority, &intent);
            }
        }
        intent
    }
}
