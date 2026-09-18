//! The token program the running test exercises.
//!
//! [`program`] reports it and [`run_under`] scopes it to one test, which is how
//! [`also_under_token_2022`] runs a single test body against both programs.
//!
//! Helpers that act on an existing account read the program off the account
//! instead (see [`super::token::program_of`]). This module is for the places
//! with nothing to read it from: creating a mint, sizing a buffer, and aiming a
//! hardcoded-legacy instruction at the program under test.

use cow_settlement_interface::{token_program::TokenProgram, AccountMeta, Instruction};
use solana_program_pack::Pack;
use solana_sdk::pubkey::Pubkey;
use spl_token_2022_interface::state::Account;
use std::cell::Cell;

use crate::common::assemble_tx;

use super::token_2022::Extensions;

thread_local! {
    /// The token program [`program`] reports, scoped to one test by
    /// [`run_under`]. Thread local because the test harness runs each test on
    /// its own thread, so a per-thread value is a per-test value.
    static ACTIVE: Cell<TokenProgram> = const { Cell::new(TokenProgram::SplToken) };
}

/// The token program the running test exercises, which is what
/// [`super::token::create_mint`] creates under and what [`retarget`] aims a
/// test's instructions at.
pub fn program() -> TokenProgram {
    ACTIVE.get()
}

/// The address of [`program`].
pub fn address() -> Pubkey {
    program().address()
}

/// The length a buffer for a [`super::token::create_mint`] mint is allocated at
/// under [`program`].
pub fn buffer_len() -> usize {
    match program() {
        TokenProgram::SplToken => Account::LEN,
        TokenProgram::Token2022 => Extensions::default().token_account_len(),
    }
}

/// Run `test` with `token_program` as the active one.
///
/// [`also_under_token_2022`] is the way tests reach this; call it directly only
/// to nest a differently-programmed section inside a test.
pub fn run_under(token_program: TokenProgram, test: impl FnOnce()) {
    ACTIVE.replace(token_program);
    test();
}

/// Repoint every legacy-SPL-Token account of `instructions` at [`program`], so
/// a test written against the legacy program submits the same transaction aimed
/// at whichever program it is being run under.
pub fn retarget(instructions: &mut [Instruction]) {
    let active = address();
    for account in instructions
        .iter_mut()
        .flat_map(|instruction| &mut instruction.accounts)
    {
        if account.pubkey == TokenProgram::SplToken.address() {
            account.pubkey = active;
        }
    }
}

/// Also run `$test` against Token-2022, as `<test>_token_2022`.
///
/// Written in front of the test it applies to:
///
/// ```ignore
/// common::also_under_token_2022!(settles_a_single_order);
/// #[test]
/// fn settles_a_single_order() { .. }
/// ```
///
/// The test keeps its own `#[test]`, so it runs twice: once under the legacy SPL
/// Token program, which is what [`program`] reports by default, and once under
/// Token-2022.
macro_rules! also_under_token_2022 {
    ($($test:ident),+ $(,)?) => {
        $(
            pastey::paste! {
                #[test]
                fn [<$test _token_2022>]() {
                    $crate::common::active_token::run_under(
                        cow_settlement_interface::token_program::TokenProgram::Token2022,
                        $test,
                    );
                }
            }
        )+
    };
}
#[allow(
    unused_imports,
    reason = "re-exported for the suites that use the macro; the others never name it"
)]
pub(crate) use also_under_token_2022;

also_under_token_2022!(verify_active_token_macro_effectiveness);
/// Guards the two pieces the suite's Token-2022 coverage rests on: that
/// [`also_under_token_2022`] actually swaps the active program, and that the
/// transaction [`assemble_tx`] builds out of an instruction written against the
/// legacy program comes out aimed at the active one.
///
/// Deliberately has no `#[test]` of its own since we just want to test the circumstances of the macro being enabled.
fn verify_active_token_macro_effectiveness() {
    assert_ne!(program(), TokenProgram::SplToken);

    let (svm, _program_id, payer) = crate::common::setup();
    let untouched = crate::common::unique_pubkey();
    let instructions = [Instruction {
        program_id: crate::common::unique_pubkey(),
        accounts: vec![
            AccountMeta::new_readonly(TokenProgram::SplToken.address(), false),
            AccountMeta::new_readonly(untouched, false),
        ],
        data: Vec::new(),
    }];

    let tx = assemble_tx(&svm, &payer, &[], &instructions);
    let message = &tx.message;
    let compiled = &message.instructions[0];
    let accounts: Vec<Pubkey> = compiled
        .accounts
        .iter()
        .map(|&index| message.account_keys[usize::from(index)])
        .collect();

    // The token program slot is the active program, and only that slot moved.
    assert_eq!(accounts, [address(), untouched]);
    assert!(
        !message
            .account_keys
            .contains(&TokenProgram::SplToken.address()),
        "no trace of the legacy program should survive in the compiled message",
    );
}
