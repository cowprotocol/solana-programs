//! Token-program execution and token-account reads

use core::slice;

use cow_settlement_interface::{
    instruction::settle::{MaybeMint, MAX_EXTRA_TRANSFER_ACCOUNTS},
    token_program::TokenProgram,
    SettlementError,
};
use pinocchio::{
    cpi::{get_return_data, invoke_signed_with_slice, Signer, MAX_CPI_ACCOUNTS},
    error::ProgramError,
    instruction::{InstructionAccount, InstructionView},
    AccountView, Address, ProgramResult,
};
use pinocchio_token::instructions::{GetAccountDataSize, Transfer, TransferChecked};

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

const _: () = assert!(
    TRANSFER_CHECKED_ACCOUNTS + MAX_EXTRA_TRANSFER_ACCOUNTS == MAX_CPI_ACCOUNTS,
    "MAX_EXTRA_TRANSFER_ACCOUNTS must track the runtime's CPI account limit",
);

/// A resolved [`MaybeMint`]: the real mint with its decimals, or `None` for the
/// placeholder. Only [`read_mint_decimals`] builds one, so a
/// [`TransferMaybeChecked`] can't be issued without the mint having been read.
pub struct MintDecimals<'a>(Option<(&'a AccountView, u8)>);

/// Retrieves the decimals from `mint`, or `None` if `mint` is
/// [`MINT_PLACEHOLDER`](cow_settlement_interface::instruction::settle::MINT_PLACEHOLDER).
/// `token_program` must be the program that owns `mint`.
#[inline(always)]
pub fn read_mint_decimals(
    token_program: TokenProgram,
    mint: MaybeMint<'_, AccountView>,
) -> Result<MintDecimals<'_>, ProgramError> {
    let Some(account) = mint.get() else {
        return Ok(MintDecimals(None));
    };
    let decimals = match token_program {
        TokenProgram::SplToken => pinocchio_token::state::Mint::from_account_view(account)
            .map_err(|_| SettlementError::InvalidMint)?
            .decimals(),
        TokenProgram::Token2022 => pinocchio_token_2022::state::Mint::from_account_view(account)
            .map_err(|_| SettlementError::InvalidMint)?
            .decimals(),
    };
    Ok(MintDecimals(Some((account, decimals))))
}

/// `TransferChecked`'s account lists, in instruction and CPI form. Built by the
/// first [`TransferMaybeChecked`] that needs them and reused by every one after
/// that is handed the same `Option`.
pub struct CpiLists<'a> {
    instruction_accounts: Vec<InstructionAccount<'a>>,
    account_views: Vec<&'a AccountView>,
}

impl<'a> CpiLists<'a> {
    fn new(
        fixed: &[(&'a AccountView, InstructionAccount<'a>); TRANSFER_CHECKED_ACCOUNTS],
        extra_accounts: &'a [AccountView],
    ) -> Self {
        let len = TRANSFER_CHECKED_ACCOUNTS
            .checked_add(extra_accounts.len())
            .expect("the account count is bounded by the transaction size");
        let mut instruction_accounts = Vec::with_capacity(len);
        let mut account_views = Vec::with_capacity(len);
        for (account, meta) in fixed {
            instruction_accounts.push(meta.clone());
            account_views.push(*account);
        }
        for extra in extra_accounts {
            instruction_accounts.push(InstructionAccount::new(
                extra.address(),
                extra.is_writable(),
                extra.is_signer(),
            ));
            account_views.push(extra);
        }
        Self {
            instruction_accounts,
            account_views,
        }
    }
}

/// Token transfers of one mint under `token_program`, signed by `authority`
/// through `signer`. A real mint issues a `TransferChecked` against the
/// decimals [`read_mint_decimals`] read from it, with `extra_accounts` appended
/// (for example, the accounts a transfer hook needs); the placeholder issues a
/// plain `Transfer`.
pub struct TransferMaybeChecked<'a, 'b> {
    token_program: TokenProgram,
    mint: &'b MintDecimals<'a>,
    authority: &'a AccountView,
    signer: &'b Signer<'b, 'b>,
    extra_accounts: &'a [AccountView],
    cpi_lists: &'b mut Option<CpiLists<'a>>,
}

impl<'a, 'b> TransferMaybeChecked<'a, 'b> {
    pub fn new(
        token_program: TokenProgram,
        mint: &'b MintDecimals<'a>,
        authority: &'a AccountView,
        signer: &'b Signer<'b, 'b>,
        extra_accounts: &'a [AccountView],
        cpi_lists: &'b mut Option<CpiLists<'a>>,
    ) -> Self {
        Self {
            token_program,
            mint,
            authority,
            signer,
            extra_accounts,
            cpi_lists,
        }
    }

    /// Move `amount` from `from` to `to`.
    #[inline(always)]
    pub fn invoke(
        &mut self,
        from: &'a AccountView,
        to: &'a AccountView,
        amount: u64,
    ) -> ProgramResult {
        let signers = slice::from_ref(self.signer);
        let program = self.token_program.address();
        match self.mint.0 {
            None => Transfer::new(from, to, self.authority, amount)
                .invoke_signed_with_unverified_program(signers, &program),
            Some((mint, decimals)) if self.extra_accounts.is_empty() => {
                TransferChecked::new(from, mint, to, self.authority, amount, decimals)
                    .invoke_signed_with_unverified_program(signers, &program)
            }
            Some((mint, decimals)) => {
                self.invoke_checked_with_extra_accounts(from, mint, to, amount, decimals)
            }
        }
    }

    /// A `TransferChecked` with `extra_accounts` appended, which
    /// `pinocchio_token`'s builder can't carry.
    #[inline(always)]
    fn invoke_checked_with_extra_accounts(
        &mut self,
        from: &'a AccountView,
        mint: &'a AccountView,
        to: &'a AccountView,
        amount: u64,
        decimals: u8,
    ) -> ProgramResult {
        let authority = self.authority;
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
            account_views,
        } = self
            .cpi_lists
            .get_or_insert_with(|| CpiLists::new(&fixed, extra_accounts));
        for ((account, meta), (instruction_account, account_view)) in fixed.into_iter().zip(
            instruction_accounts
                .iter_mut()
                .zip(account_views.iter_mut()),
        ) {
            *instruction_account = meta;
            *account_view = account;
        }

        let mut data = [0u8; 10];
        data[0] = TransferChecked::DISCRIMINATOR;
        data[1..9].copy_from_slice(&amount.to_le_bytes());
        data[9] = decimals;

        // Borrow-checks every writable account, the extra accounts included,
        // and fails with `InvalidArgument` past `MAX_EXTRA_TRANSFER_ACCOUNTS`.
        invoke_signed_with_slice(
            &InstructionView {
                program_id: &self.token_program.address(),
                accounts: instruction_accounts,
                data: &data,
            },
            account_views,
            slice::from_ref(self.signer),
        )
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
