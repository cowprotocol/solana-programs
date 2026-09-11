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

/// Decimals every test mint carries, matching [`super::token::create_mint`] so
/// a legacy and a Token-2022 mint differ only in their program.
const DECIMALS: u8 = 8;

/// Transfer-fee parameters for [`Extensions::CloseAuthorityAndTransferFee`]. Arbitrary;
/// nothing reads them back, but `InitializeTransferFeeConfig` demands values.
const FEE_BASIS_POINTS: u16 = 50;
const MAXIMUM_FEE: u64 = 1_000;

/// Defines a set of token account/mint configurations we are interested in testing
#[derive(Clone, Copy, Debug)]
pub enum Extensions {
    None,
    CloseAuthorityOnly,
    CloseAuthorityAndNonTransferable,
    CloseAuthorityAndTransferFee,
}

pub struct RequiredInitAccountExtensionType(ExtensionType);

impl RequiredInitAccountExtensionType {
    /// Copied from the unnecessarily private function in spl_token_2022_interface
    /// https://docs.rs/spl-token-2022-interface/latest/src/spl_token_2022_interface/extension/mod.rs.html#1296
    pub fn required_init_account_extensions(&self) -> &'static [ExtensionType] {
        match self.0 {
            ExtensionType::TransferFeeConfig => &[ExtensionType::TransferFeeAmount],
            ExtensionType::NonTransferable => &[
                ExtensionType::NonTransferableAccount,
                ExtensionType::ImmutableOwner,
            ],
            ExtensionType::TransferHook => &[ExtensionType::TransferHookAccount],
            ExtensionType::Pausable => &[ExtensionType::PausableAccount],
            _ => &[],
        }
    }
}

impl Extensions {
    /// The extensions which should be configured on the mint
    fn mint(self) -> &'static [ExtensionType] {
        match self {
            Self::None => &[],
            Self::CloseAuthorityOnly => &[ExtensionType::MintCloseAuthority],
            Self::CloseAuthorityAndNonTransferable => &[
                ExtensionType::MintCloseAuthority,
                ExtensionType::NonTransferable,
            ],
            Self::CloseAuthorityAndTransferFee => &[
                ExtensionType::MintCloseAuthority,
                ExtensionType::TransferFeeConfig,
            ],
        }
    }

    /// The extensions which should be configured on the token account
    /// Includes the required mint accounts by default, plus any additionally
    /// specified optional token account extensions
    fn token_account(self) -> Vec<ExtensionType> {
        let mut extensions = vec![];

        // required extensions by mint
        for mint_extension in self.mint() {
            extensions.extend_from_slice(
                RequiredInitAccountExtensionType(*mint_extension)
                    .required_init_account_extensions(),
            );
        }

        // additional extensions for this configuration
        // more will be added in the future
        #[allow(clippy::match_single_binding)]
        extensions.extend_from_slice(match self {
            _ => &[],
        });

        extensions
    }

    /// The data length a token account holding the mint has to be allocated at,
    /// which is what `create_buffer` asks the token program for.
    pub fn token_account_len(self) -> usize {
        ExtensionType::try_calculate_account_len::<Account>(&self.token_account())
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
                        &TokenProgram::Token2022.address(),
                        mint,
                        Some(authority),
                    ),
                    ExtensionType::NonTransferable => {
                        initialize_non_transferable_mint(&TokenProgram::Token2022.address(), mint)
                    }
                    ExtensionType::TransferFeeConfig => initialize_transfer_fee_config(
                        &TokenProgram::Token2022.address(),
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
        &TokenProgram::Token2022.address(),
    )];
    instructions.extend(extensions.initializers(&mint.pubkey(), &payer.pubkey()));
    instructions.push(
        initialize_mint2(
            &TokenProgram::Token2022.address(),
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
        &TokenProgram::Token2022.address(),
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
