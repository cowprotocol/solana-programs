//! Token-program execution and token-account reads

use core::{mem::MaybeUninit, slice};

use cow_settlement_interface::{
    instruction::settle::UNCHECKED_MINT, token_program::TokenProgram, SettlementError,
};
use pinocchio::{
    address::address_eq,
    cpi::{get_return_data, invoke_signed_unchecked, CpiAccount, Signer},
    error::ProgramError,
    instruction::{InstructionAccount, InstructionView},
    AccountView, Address, ProgramResult,
};
use pinocchio_token::instructions::{GetAccountDataSize, Transfer};

/// The length of a SPL token program account. Token2022 extensions may make
/// the actual token account longer than this.
const BASE_TOKEN_ACCOUNT_LEN: u64 = pinocchio_token::state::Account::LEN as u64;

/// Resolve the token program behind the given token account.
/// Throws if the owning token program isn't supported.
#[must_use = "not consuming skips the owner check"]
pub fn owning_token_program(account: &AccountView) -> Result<TokenProgram, ProgramError> {
    TokenProgram::try_from(account.owner())
}

/// The data length a token account holding `mint` has to be allocated at.
pub fn token_account_len(
    token_program: TokenProgram,
    mint: &AccountView,
) -> Result<u64, ProgramError> {
    match token_program {
        // SPL token accounts are always the base length, so skip the CPI.
        TokenProgram::SplToken => Ok(BASE_TOKEN_ACCOUNT_LEN),
        // Token-2022 accounts vary with the mint's extensions. This mirrors the
        // SPL Associated Token Account program's `get_account_len`:
        // https://github.com/solana-program/associated-token-account/blob/2dc55ee1009d787eea7e1c401b8f27e6892bff4b/program/src/tools/account.rs#L72-L97
        TokenProgram::Token2022 => {
            GetAccountDataSize::new(mint)
                .invoke_with_unverified_program(&TokenProgram::Token2022.address())?;
            get_return_data()
                .ok_or(SettlementError::BufferSizeUnavailable.into())
                .and_then(|reported| {
                    if reported.program_id() != &TokenProgram::Token2022.address() {
                        return Err(SettlementError::BufferSizeUnavailable.into());
                    }
                    reported
                        .as_slice()
                        .try_into()
                        .map(u64::from_le_bytes)
                        .map_err(|_| SettlementError::BufferSizeUnavailable.into())
                })
        }
    }
}

/// `TransferChecked`'s own accounts: `[source, mint, destination, authority]`.
const TRANSFER_CHECKED_ACCOUNTS: usize = 4;

/// The decimals of the mint behind `mint`, or `None` if `mint` is
/// [`UNCHECKED_MINT`], which selects plain `Transfer` over `TransferChecked`.
#[inline(always)]
pub fn mint_decimals(
    token_program: TokenProgram,
    mint: &AccountView,
) -> Result<Option<u8>, SettlementError> {
    if address_eq(mint.address(), &UNCHECKED_MINT) {
        return Ok(None);
    }
    match token_program {
        TokenProgram::SplToken => {
            pinocchio_token::state::Mint::from_account_view(mint).map(|mint| mint.decimals())
        }
        TokenProgram::Token2022 => {
            pinocchio_token_2022::state::Mint::from_account_view(mint).map(|mint| mint.decimals())
        }
    }
    .map(Some)
    .map_err(|_| SettlementError::InvalidMint)
}

/// Token transfers signed by the state PDA, issued as `TransferChecked` with
/// the instruction's extra accounts appended (for example, the accounts a
/// transfer hook needs), or as plain `Transfer` when no decimals are given.
///
/// The first `TransferChecked` allocates the CPI account lists and writes the
/// extra accounts into them; each transfer after only rewrites the leading
/// [`TRANSFER_CHECKED_ACCOUNTS`].
pub struct TokenTransfers<'a> {
    extra_accounts: &'a [AccountView],
    cpi_lists: Option<CpiLists<'a>>,
}

/// `TransferChecked`'s account lists, in instruction and CPI form.
struct CpiLists<'a> {
    instruction_accounts: Box<[MaybeUninit<InstructionAccount<'a>>]>,
    cpi_accounts: Box<[MaybeUninit<CpiAccount<'a>>]>,
}

impl<'a> CpiLists<'a> {
    fn new(extra_accounts: &'a [AccountView]) -> Self {
        let len = TRANSFER_CHECKED_ACCOUNTS
            .checked_add(extra_accounts.len())
            .expect("the account count is bounded by the transaction size");
        let mut instruction_accounts = Box::new_uninit_slice(len);
        let mut cpi_accounts = Box::new_uninit_slice(len);
        for ((instruction_account, cpi_account), extra) in instruction_accounts
            [TRANSFER_CHECKED_ACCOUNTS..]
            .iter_mut()
            .zip(&mut cpi_accounts[TRANSFER_CHECKED_ACCOUNTS..])
            .zip(extra_accounts)
        {
            instruction_account.write(InstructionAccount::new(
                extra.address(),
                extra.is_writable(),
                extra.is_signer(),
            ));
            CpiAccount::init_from_account_view(extra, cpi_account);
        }
        Self {
            instruction_accounts,
            cpi_accounts,
        }
    }
}

impl<'a> TokenTransfers<'a> {
    pub fn new(extra_accounts: &'a [AccountView]) -> Self {
        Self {
            extra_accounts,
            cpi_lists: None,
        }
    }

    /// Move `amount` from `from` to `to` under `token_program`, signed by
    /// `authority` through `signer`. `decimals` comes from [`mint_decimals`]:
    /// `Some` issues a `TransferChecked` against `mint`, `None` a `Transfer`.
    #[allow(clippy::too_many_arguments)]
    #[inline(always)]
    pub fn transfer(
        &mut self,
        token_program: TokenProgram,
        from: &'a AccountView,
        mint: &'a AccountView,
        to: &'a AccountView,
        authority: &'a AccountView,
        amount: u64,
        decimals: Option<u8>,
        signer: &Signer,
    ) -> ProgramResult {
        let signers = slice::from_ref(signer);
        let Some(decimals) = decimals else {
            return Transfer::new(from, to, authority, amount)
                .invoke_signed_with_unverified_program(signers, &token_program.address());
        };

        // The token program writes to `from` and `to`, so neither may be
        // borrowed here. The extra accounts aren't checked: the only account
        // this program holds borrowed across a transfer is an order PDA, which
        // only this program can write and which it can't be reentered to do.
        if from.is_borrowed() | to.is_borrowed() {
            return Err(ProgramError::AccountBorrowFailed);
        }
        let fixed = [
            (from, InstructionAccount::writable(from.address())),
            (mint, InstructionAccount::readonly(mint.address())),
            (to, InstructionAccount::writable(to.address())),
            (
                authority,
                InstructionAccount::readonly_signer(authority.address()),
            ),
        ];
        let extra_accounts = self.extra_accounts;
        let CpiLists {
            instruction_accounts,
            cpi_accounts,
        } = self
            .cpi_lists
            .get_or_insert_with(|| CpiLists::new(extra_accounts));
        for (((account, meta), instruction_account), cpi_account) in fixed
            .into_iter()
            .zip(instruction_accounts.iter_mut())
            .zip(cpi_accounts.iter_mut())
        {
            instruction_account.write(meta);
            CpiAccount::init_from_account_view(account, cpi_account);
        }

        let mut data = [0u8; 10];
        data[0] = pinocchio_token::instructions::TransferChecked::DISCRIMINATOR;
        data[1..9].copy_from_slice(&amount.to_le_bytes());
        data[9] = decimals;

        // SAFETY: `CpiLists::new` and the loop above initialized every element of both
        // lists, and the borrow check above covers the accounts the token
        // program writes.
        unsafe {
            invoke_signed_unchecked(
                &InstructionView {
                    program_id: &token_program.address(),
                    accounts: slice::from_raw_parts(
                        instruction_accounts.as_ptr().cast(),
                        instruction_accounts.len(),
                    ),
                    data: &data,
                },
                slice::from_raw_parts(cpi_accounts.as_ptr().cast(), cpi_accounts.len()),
                signers,
            );
        }
        Ok(())
    }
}

/// The base-layout fields of a token account, as read by
/// [`read_token_account`].
/// For our purposes, we only need the `amount`.
pub struct TokenAccount {
    pub mint: Address,
    pub owner: Address,
    pub amount: u64,
}

/// Read the base fields of the token account at `account`, which must be owned
/// by `token_program`.
pub fn read_token_account(
    token_program: TokenProgram,
    account: &AccountView,
) -> Result<TokenAccount, ProgramError> {
    Ok(match token_program {
        TokenProgram::SplToken => {
            let decoded = pinocchio_token::state::Account::from_account_view(account)?;
            TokenAccount {
                amount: decoded.amount(),
                mint: *decoded.mint(),
                owner: *decoded.owner(),
            }
        }
        TokenProgram::Token2022 => {
            let decoded = pinocchio_token_2022::state::Account::from_account_view(account)?;
            TokenAccount {
                amount: decoded.amount(),
                mint: *decoded.mint(),
                owner: *decoded.owner(),
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use cow_settlement_interface::{
        fixtures::pubkey_from_seed,
        instruction::fixtures::{fake_account, fake_account_owned_by},
    };
    use litesvm_token::spl_token::state::{Account as SplTokenAccount, AccountState};
    use pinocchio::Address;
    use pinocchio_token::state::Mint;
    use pinocchio_token_2022::state::AccountType;
    use solana_program_pack::Pack;
    use spl_token_2022_interface::{
        extension::{
            transfer_fee::TransferFeeAmount, BaseStateWithExtensionsMut, ExtensionType,
            StateWithExtensionsMut,
        },
        state::{Account as Token2022TokenAccount, AccountState as Token2022AccountState},
    };

    /// The length of a token account holding nothing but the base layout. Both
    /// programs share it: it is Token-2022's `BASE_LEN` and the whole of a
    /// legacy account.
    const BASE_LEN: usize = pinocchio_token::state::Account::LEN;

    /// The base layout of a token account holding `amount` of `mint` for
    /// `owner`, encoded by the SPL token program's own packer so the fixture
    /// cannot drift from the layout the readers parse.
    fn base_account_layout(mint: Address, owner: Address, amount: u64) -> Vec<u8> {
        let mut data = vec![0u8; BASE_LEN];
        SplTokenAccount {
            mint,
            owner,
            amount,
            state: AccountState::Initialized,
            ..Default::default()
        }
        .pack_into_slice(&mut data);
        data
    }

    /// A Token-2022 account holding `amount` of `mint` for `owner`, extended
    /// with the `TransferFeeAmount` extension. Built through the token program's own TLV
    /// writers.
    fn extended_token_2022_account_layout(mint: Address, owner: Address, amount: u64) -> Vec<u8> {
        let len = ExtensionType::try_calculate_account_len::<Token2022TokenAccount>(&[
            ExtensionType::TransferFeeAmount,
        ])
        .expect("TransferFeeAmount has a fixed length");
        let mut data = vec![0u8; len];

        let mut state =
            StateWithExtensionsMut::<Token2022TokenAccount>::unpack_uninitialized(&mut data)
                .expect("a zeroed buffer of the right length is an uninitialized account");
        state
            .init_extension::<TransferFeeAmount>(true)
            .expect("the buffer is sized for the extension");
        state.base = Token2022TokenAccount {
            mint,
            owner,
            amount,
            state: Token2022AccountState::Initialized,
            ..Default::default()
        };
        state.pack_base();
        state
            .init_account_type()
            .expect("the extension belongs to a token account");

        data
    }

    /// The addresses the off-chain crate offers are the ones the on-chain token
    /// crates CPI into. Both sides name the same programs from their own
    /// dependency, so this is what keeps them from drifting apart.
    #[test]
    fn interface_and_pinocchio_agree_on_the_program_ids() {
        for program in TokenProgram::ALL {
            let pinocchio_id = match program {
                TokenProgram::SplToken => pinocchio_token::ID,
                TokenProgram::Token2022 => pinocchio_token_2022::ID,
            };
            assert_eq!(program.address(), pinocchio_id);
        }
    }

    /// The base layout is the same under both programs, so one reader's idea of
    /// its length is the other's too.
    #[test]
    fn sanity_check_both_programs_share_the_base_layout_length() {
        assert_eq!(BASE_LEN, pinocchio_token_2022::state::Account::BASE_LEN);
    }

    #[test]
    fn token_account_len_is_base_length_for_spl_program() {
        let mint = fake_account_owned_by(
            pubkey_from_seed("mint"),
            TokenProgram::SplToken.address(),
            &[0u8; Mint::LEN],
        );
        assert_eq!(
            token_account_len(TokenProgram::SplToken, &mint),
            Ok(BASE_TOKEN_ACCOUNT_LEN),
            "SPL owned mint should yield ase length",
        );
    }

    #[test]
    fn token_account_len_reports_unavailable_without_an_answer() {
        let mint = fake_account_owned_by(
            pubkey_from_seed("mint"),
            TokenProgram::Token2022.address(),
            &[0u8; Mint::LEN + 1],
        );
        assert_eq!(
            token_account_len(TokenProgram::Token2022, &mint).err(),
            Some(SettlementError::BufferSizeUnavailable.into()),
        );
    }

    /// Creates a legacy SPL-compliant token account of `program`
    fn token_account_of(program: Address) -> AccountView {
        fake_account_owned_by(
            pubkey_from_seed("token_account_of's token account"),
            program,
            &base_account_layout(
                pubkey_from_seed("token_account_of's mint"),
                pubkey_from_seed("token_account_of's owner"),
                0,
            ),
        )
    }

    /// Every token account executes against the program that owns it. This is what
    /// one instruction moving tokens under both programs rests on: nothing has
    /// to tell it which, each account already says.
    #[test]
    fn owning_token_program_returns_on_the_accounts_owner() {
        for program in TokenProgram::ALL {
            assert_eq!(
                owning_token_program(&token_account_of(program.address())),
                Ok(program),
                "an account owned by {program:?} should be settled against it",
            );
        }
    }

    /// An account under neither program is no token account at all, which the
    /// caller reports as whatever the account failed to be.
    #[test]
    fn owning_token_program_rejects_an_account_under_an_unrelated_program() {
        let unrelated = pubkey_from_seed("not a token program");
        assert_eq!(
            owning_token_program(&token_account_of(unrelated)),
            Err(ProgramError::IncorrectProgramId),
        );
    }

    /// An account that was never allocated is owned by the system program, so
    /// it is refused like any other non-token account rather than read as one.
    #[test]
    fn owning_token_program_rejects_an_unallocated_account() {
        let account = fake_account(pubkey_from_seed("never allocated"));
        assert_eq!(
            owning_token_program(&account),
            Err(ProgramError::IncorrectProgramId),
        );
    }

    #[test]
    fn read_token_account_reads_a_base_layout_account() {
        let mint = pubkey_from_seed("mint");
        let owner = pubkey_from_seed("owner");
        for program in TokenProgram::ALL {
            let account = fake_account_owned_by(
                pubkey_from_seed("token account"),
                program.address(),
                &base_account_layout(mint, owner, 4_200),
            );
            let read = read_token_account(program, &account)
                .unwrap_or_else(|error| panic!("{program:?} account should read: {error:?}"));
            assert_eq!(read.amount, 4_200);
        }
    }

    #[test]
    fn read_token_account_reads_past_token_2022_extensions() {
        let mint = pubkey_from_seed("extended mint");
        let owner = pubkey_from_seed("extended owner");
        let account = fake_account_owned_by(
            pubkey_from_seed("token account"),
            TokenProgram::Token2022.address(),
            &extended_token_2022_account_layout(mint, owner, 7),
        );
        let TokenAccount {
            mint: read_mint,
            owner: read_owner,
            amount,
        } = read_token_account(TokenProgram::Token2022, &account)
            .expect("an extended Token-2022 account should read");
        assert_eq!(read_mint, mint);
        assert_eq!(read_owner, owner);
        assert_eq!(amount, 7);
    }

    #[test]
    fn read_token_account_rejects_an_extended_mint() {
        let mut data = base_account_layout(pubkey_from_seed("mint"), pubkey_from_seed("owner"), 7);
        data.push(AccountType::Mint as u8);

        let account = fake_account_owned_by(
            pubkey_from_seed("mint account"),
            TokenProgram::Token2022.address(),
            &data,
        );
        assert_eq!(
            read_token_account(TokenProgram::Token2022, &account).err(),
            Some(ProgramError::InvalidAccountData),
        );
    }

    #[test]
    fn read_token_account_rejects_the_other_programs_account() {
        for [program, other] in [
            [TokenProgram::SplToken, TokenProgram::Token2022],
            [TokenProgram::Token2022, TokenProgram::SplToken],
        ] {
            let account = fake_account_owned_by(
                pubkey_from_seed("token account"),
                other.address(),
                &base_account_layout(pubkey_from_seed("mint"), pubkey_from_seed("owner"), 0),
            );
            assert_eq!(
                read_token_account(program, &account).err(),
                Some(ProgramError::InvalidAccountData),
                "an account owned by {other:?} should not read under {program:?}",
            );
        }
    }
}
