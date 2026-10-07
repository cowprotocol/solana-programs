//! A Token-2022 transfer hook for the settlement integration tests, backed by
//! the `test-transfer-hook` program.
//!
//! The hook's validation account lists one extra account, the hook's switch:
//! Token-2022 refuses a transfer that doesn't carry it, and the hook rejects
//! the transfer once the switch is flipped, which is how a test sees it ran.

use cow_settlement_interface::{token_program::TokenProgram, AccountMeta};
use litesvm::LiteSVM;
use solana_sdk::{account::Account, pubkey::Pubkey, signature::Keypair};
use spl_tlv_account_resolution::{account::ExtraAccountMeta, state::ExtraAccountMetaList};
use spl_transfer_hook_interface::{
    get_extra_account_metas_address, instruction::ExecuteInstruction,
};

use super::{create_account, create_account_at, token, token_2022::Extensions, unique_pubkey};

pub const TRANSFER_HOOK_SO: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../target/deploy/test_transfer_hook.so"
);

/// The error the hook rejects a transfer with once its switch is flipped.
pub const REJECTED: u32 = test_transfer_hook::REJECTED;

/// A deployed hook program and the switch account its mints require.
pub struct TransferHook {
    pub program: Pubkey,
    pub switch: Pubkey,
}

impl TransferHook {
    pub fn deploy(svm: &mut LiteSVM) -> Self {
        let program = unique_pubkey();
        svm.add_program_from_file(program, TRANSFER_HOOK_SO)
            .expect("test-transfer-hook .so not found, run `just build-test-programs` first");
        let switch = create_account(svm, &unique_pubkey(), &[0]);
        Self { program, switch }
    }

    /// Create a Token-2022 mint executing this hook, together with the hook's
    /// validation account listing the switch.
    pub fn create_mint(&self, svm: &mut LiteSVM, payer: &Keypair) -> Pubkey {
        let mint = token::create_mint_under(
            svm,
            payer,
            &TokenProgram::Token2022.address(),
            Extensions::TransferHook(self.program),
        );
        let extra_metas = [
            ExtraAccountMeta::new_with_pubkey(&self.switch, false, false)
                .expect("a literal account meta builds"),
        ];
        let mut data = vec![0; ExtraAccountMetaList::size_of(extra_metas.len()).unwrap()];
        ExtraAccountMetaList::init::<ExecuteInstruction>(&mut data, &extra_metas)
            .expect("the buffer is sized for the metas");
        create_account_at(
            svm,
            get_extra_account_metas_address(&mint, &self.program),
            &self.program,
            &data,
        );
        mint
    }

    /// The accounts a `TransferChecked` of `mint` needs for this hook.
    pub fn extra_accounts(&self, mint: &Pubkey) -> Vec<AccountMeta> {
        vec![
            AccountMeta::new_readonly(self.program, false),
            AccountMeta::new_readonly(get_extra_account_metas_address(mint, &self.program), false),
            AccountMeta::new_readonly(self.switch, false),
        ]
    }

    /// Make the hook reject every transfer from now on.
    pub fn flip_switch(&self, svm: &mut LiteSVM) {
        let mut switch: Account = svm.get_account(&self.switch).expect("the switch exists");
        switch.data = vec![1];
        svm.set_account(self.switch, switch)
            .expect("set_account should succeed");
    }
}
