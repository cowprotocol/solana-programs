//! Token-program validation and token-account reads

use cow_settlement_interface::{token_program::TokenProgram, SettlementError};
use pinocchio::{cpi::get_return_data, error::ProgramError, AccountView, Address};
use pinocchio_token::instructions::GetAccountDataSize;

/// The length of a SPL token program account. Token2022 extensions may make
/// the actual token account longer than this.
const BASE_TOKEN_ACCOUNT_LEN: u64 = pinocchio_token::state::Account::LEN as u64;

/// Validate that `token_program_account` is a token program this program may
/// issue CPIs against, returning the program for the instruction to target.
#[must_use = "not consuming skips validation"]
pub fn validate_token_program(
    token_program_account: &AccountView,
) -> Result<TokenProgram, ProgramError> {
    TokenProgram::try_from(token_program_account.address())
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

    #[test]
    fn validate_token_program_accepts_every_supported_program() {
        for program in TokenProgram::ALL {
            let account = fake_account(program.address());
            assert_eq!(validate_token_program(&account), Ok(program));
        }
    }

    #[test]
    fn validate_token_program_rejects_unrelated_program() {
        let account = fake_account(pubkey_from_seed("not a token program"));
        assert_eq!(
            validate_token_program(&account),
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
        let read = read_token_account(TokenProgram::Token2022, &account)
            .expect("an extended Token-2022 account should read");
        assert_eq!(read.mint, mint);
        assert_eq!(read.owner, owner);
        assert_eq!(read.amount, 7);
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
