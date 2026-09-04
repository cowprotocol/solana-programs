//! Token-2022 helpers for the settlement integration tests.
//!
//! Unlike the legacy program, Token-2022 lets a mint be closed and its address
//! reused for something else entirely. A buffer PDA is derived from the mint
//! address alone, so a buffer outlives the mint it was created for. These
//! helpers drive that lifecycle: create a mint under a chosen extension set,
//! close it, and put a different mint at the same address.

use cow_settlement_interface::token_program::TokenProgram;
use litesvm::LiteSVM;
use solana_sdk::{
    instruction::Instruction,
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    transaction::Transaction,
};
use solana_system_interface::instruction::create_account as system_create_account;
use spl_token_2022_interface::{
    extension::{transfer_fee::instruction::initialize_transfer_fee_config, ExtensionType},
    instruction::{
        close_account, initialize_mint2, initialize_mint_close_authority,
        initialize_non_transferable_mint,
    },
    state::{Account, Mint},
};

/// The Token-2022 program, the counterpart of [`super::SPL_TOKEN_PROGRAM_ID`].
const TOKEN_2022_PROGRAM_ID: Pubkey = TokenProgram::Token2022.address();

/// Decimals every test mint carries, matching [`super::token::create_mint`] so
/// a legacy and a Token-2022 mint differ only in their program.
const DECIMALS: u8 = 8;

/// Transfer-fee parameters for [`Extensions::WithTransferFee`]. Arbitrary;
/// nothing reads them back, but `InitializeTransferFeeConfig` demands values.
const FEE_BASIS_POINTS: u16 = 50;
const MAXIMUM_FEE: u64 = 1_000;

/// The extension set a test mint is created with.
#[derive(Clone, Copy, Debug)]
pub enum Extensions {
    CloseAuthorityOnly,
    CloseAuthorityAndNonTransferable,
    CloseAuthorityAndTransferFee,
}

impl Extensions {
    /// The extensions the mint itself is initialized with.
    fn mint(self) -> &'static [ExtensionType] {
        match self {
            Self::CloseAuthorityOnly => &[ExtensionType::MintCloseAuthority],
            Self::WithNonTransferable => &[
                ExtensionType::MintCloseAuthority,
                ExtensionType::NonTransferable,
            ],
            Self::WithTransferFee => &[
                ExtensionType::MintCloseAuthority,
                ExtensionType::TransferFeeConfig,
            ],
        }
    }

    /// The extensions Token-2022 requires of a token account holding the mint.
    /// Spelled out rather than derived from [`Self::mint`], so the length a test
    /// expects is stated independently of the program's own bookkeeping.
    fn token_account(self) -> &'static [ExtensionType] {
        match self {
            Self::CloseAuthorityOnly => &[],
            Self::WithNonTransferable => &[
                ExtensionType::NonTransferableAccount,
                ExtensionType::ImmutableOwner,
            ],
            Self::WithTransferFee => &[ExtensionType::TransferFeeAmount],
        }
    }

    /// The data length a token account holding the mint has to be allocated at,
    /// which is what `create_buffer` asks the token program for.
    pub fn token_account_len(self) -> usize {
        ExtensionType::try_calculate_account_len::<Account>(self.token_account())
            .expect("every account extension used here has a fixed length")
    }

    /// The instructions initializing the extensions on `mint`, with `authority`
    /// filling every authority they ask for. Token-2022 requires all of them to
    /// run before `InitializeMint`, and insists the mint be allocated at exactly
    /// the length they need.
    fn initializers(self, mint: &Pubkey, authority: &Pubkey) -> Vec<Instruction> {
        self.mint()
            .iter()
            .map(|extension| {
                match extension {
                    ExtensionType::MintCloseAuthority => initialize_mint_close_authority(
                        &TOKEN_2022_PROGRAM_ID,
                        mint,
                        Some(authority),
                    ),
                    ExtensionType::NonTransferable => {
                        initialize_non_transferable_mint(&TOKEN_2022_PROGRAM_ID, mint)
                    }
                    ExtensionType::TransferFeeConfig => initialize_transfer_fee_config(
                        &TOKEN_2022_PROGRAM_ID,
                        mint,
                        Some(authority),
                        Some(authority),
                        FEE_BASIS_POINTS,
                        MAXIMUM_FEE,
                    ),
                    other => panic!("no initializer is wired up for {other:?}"),
                }
                .expect("extension initializer should build")
            })
            .collect()
    }
}

/// Create a Token-2022 mint at `mint`'s address carrying `extensions`, with
/// `payer` as both its mint authority and its close authority, and return the
/// address. Taking the keypair rather than generating one lets a test close the
/// mint and put something else back at the same address.
pub fn create_mint(
    svm: &mut LiteSVM,
    payer: &Keypair,
    mint: &Keypair,
    extensions: Extensions,
) -> Pubkey {
    let space = ExtensionType::try_calculate_account_len::<Mint>(extensions.mint())
        .expect("every mint extension used here has a fixed length");
    let mut instructions = vec![system_create_account(
        &payer.pubkey(),
        &mint.pubkey(),
        svm.minimum_balance_for_rent_exemption(space),
        space as u64,
        &TOKEN_2022_PROGRAM_ID,
    )];
    instructions.extend(extensions.initializers(&mint.pubkey(), &payer.pubkey()));
    instructions.push(
        initialize_mint2(
            &TOKEN_2022_PROGRAM_ID,
            &mint.pubkey(),
            &payer.pubkey(),
            None,
            DECIMALS,
        )
        .expect("initialize_mint2 should build"),
    );

    let tx = Transaction::new_signed_with_payer(
        &instructions,
        Some(&payer.pubkey()),
        &[payer, mint],
        svm.latest_blockhash(),
    );
    svm.send_transaction(tx)
        .expect("Token-2022 mint creation should succeed");
    mint.pubkey()
}

/// Close `mint`, whose close authority must be `payer`, refunding its rent to
/// `payer`. Token-2022 hands the emptied account back to the System program, so
/// the address is free for [`create_mint`] or [`super::token::create_mint_at`]
/// to claim again.
pub fn close_mint(svm: &mut LiteSVM, payer: &Keypair, mint: &Pubkey) {
    let ix = close_account(
        &TOKEN_2022_PROGRAM_ID,
        mint,
        &payer.pubkey(),
        &payer.pubkey(),
        &[],
    )
    .expect("close_account should build");
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&payer.pubkey()),
        &[payer],
        svm.latest_blockhash(),
    );
    svm.send_transaction(tx)
        .expect("closing the mint should succeed");
    assert!(
        svm.get_account(mint)
            .is_none_or(|account| account.data.is_empty()),
        "a closed mint must leave no data behind at its address",
    );
}
