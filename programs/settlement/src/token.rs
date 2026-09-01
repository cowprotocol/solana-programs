//! Token-program validation and token-account reads

use cow_settlement_interface::{token_program::TokenProgram, SettlementError};
use pinocchio::{cpi::get_return_data, error::ProgramError, AccountView};
use pinocchio_token::{instructions::GetAccountDataSize, state::Mint};

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
    // If the mint is of base SPL Mint length, the token accounts must be of base length accordingly.
    if mint.data_len() <= Mint::LEN {
        return Ok(BASE_TOKEN_ACCOUNT_LEN);
    }

    let token_program = token_program.address();
    // SPL token provides a function to get the actual required account data size
    GetAccountDataSize::new(mint).invoke_with_unverified_program(&token_program)?;

    let reported = get_return_data().ok_or(SettlementError::BufferSizeUnavailable)?;
    if reported.program_id() != &token_program {
        return Err(SettlementError::BufferSizeUnavailable.into());
    }
    let length: [u8; 8] = reported
        .as_slice()
        .try_into()
        .map_err(|_| SettlementError::BufferSizeUnavailable)?;
    let length = u64::from_le_bytes(length);
    // A token account is at least its base layout, whatever its mint carries.
    if length < BASE_TOKEN_ACCOUNT_LEN {
        return Err(SettlementError::BufferSizeUnavailable.into());
    }

    Ok(length)
}

/// The base-layout fields of a token account, as read by
/// [`read_token_account`].
/// For our purposes, we only need the `amount`.
pub struct TokenAccount {
    pub amount: u64,
}

/// Read the base fields of the token account at `account`, which must be owned
/// by `token_program`.
pub fn read_token_account(
    token_program: TokenProgram,
    account: &AccountView,
) -> Result<TokenAccount, ProgramError> {
    let amount = match token_program {
        TokenProgram::SplToken => {
            pinocchio_token::state::Account::from_account_view(account)?.amount()
        }
        TokenProgram::Token2022 => {
            pinocchio_token_2022::state::Account::from_account_view(account)?.amount()
        }
    };
    Ok(TokenAccount { amount })
}

#[cfg(test)]
mod tests {
    use super::*;
    use cow_settlement_interface::instruction::fixtures::{fake_account, fake_account_owned_by};
    use pinocchio::Address;
    use pinocchio_token_2022::state::AccountType;

    /// An address that is not a token program.
    const UNRELATED: Address = Address::new_from_array([0x99; 32]);

    /// The length of a token account holding nothing but the base layout. Both
    /// programs share it: it is Token-2022's `BASE_LEN` and the whole of a
    /// legacy account.
    const BASE_LEN: usize = pinocchio_token::state::Account::LEN;

    /// The base layout of a token account holding `amount` of `mint` for
    /// `owner`, with every other field left zeroed.
    fn base_layout(mint: Address, owner: Address, amount: u64) -> Vec<u8> {
        let mut data = vec![0u8; BASE_LEN];
        data[..32].copy_from_slice(mint.as_array());
        data[32..64].copy_from_slice(owner.as_array());
        data[64..72].copy_from_slice(&amount.to_le_bytes());
        data
    }

    /// The addresses the off-chain crate offers are the ones the on-chain token
    /// crates CPI into. Both sides name the same programs from their own
    /// dependency, so this is what keeps them from drifting apart.
    #[test]
    fn interface_and_pinocchio_agree_on_the_program_ids() {
        assert_eq!(TokenProgram::SplToken.address(), pinocchio_token::ID);
        assert_eq!(TokenProgram::Token2022.address(), pinocchio_token_2022::ID);
    }

    /// The base layout is the same under both programs, so one reader's idea of
    /// its length is the other's too.
    #[test]
    fn both_programs_share_the_base_layout_length() {
        assert_eq!(BASE_LEN, pinocchio_token_2022::state::Account::BASE_LEN);
    }

    #[test]
    fn token_account_len_is_the_base_layout_for_a_plain_mint() {
        for program in TokenProgram::ALL {
            let mint = fake_account_owned_by(UNRELATED, program.address(), &[0u8; Mint::LEN]);
            assert_eq!(
                token_account_len(program, &mint),
                Ok(BASE_TOKEN_ACCOUNT_LEN),
                "a base-layout mint should need a base-layout account under {program:?}",
            );
        }
    }

    #[test]
    fn token_account_len_is_the_base_layout_for_a_too_short_account() {
        let mint = fake_account(UNRELATED);
        assert_eq!(
            token_account_len(TokenProgram::SplToken, &mint),
            Ok(BASE_TOKEN_ACCOUNT_LEN),
        );
    }

    #[test]
    fn token_account_len_reports_unavailable_without_an_answer() {
        let mint = fake_account_owned_by(
            UNRELATED,
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
        let account = fake_account(UNRELATED);
        assert_eq!(
            validate_token_program(&account),
            Err(ProgramError::IncorrectProgramId),
        );
    }

    #[test]
    fn read_token_account_reads_a_base_layout_account() {
        let mint = Address::new_from_array([0x11; 32]);
        let owner = Address::new_from_array([0x22; 32]);
        for program in TokenProgram::ALL {
            let account = fake_account_owned_by(
                UNRELATED,
                program.address(),
                &base_layout(mint, owner, 4_200),
            );
            let read = read_token_account(program, &account)
                .unwrap_or_else(|error| panic!("{program:?} account should read: {error:?}"));
            assert_eq!(read.amount, 4_200);
        }
    }

    #[test]
    fn read_token_account_reads_past_token_2022_extensions() {
        let mint = Address::new_from_array([0x33; 32]);
        let owner = Address::new_from_array([0x44; 32]);
        let mut data = base_layout(mint, owner, 7);
        // Extensions are preceded by the account-type marker, which is what
        // distinguishes a longer account from a mint of the same size.
        data.push(AccountType::Account as u8);
        data.extend_from_slice(&[0xab; 16]);

        let account = fake_account_owned_by(UNRELATED, TokenProgram::Token2022.address(), &data);
        let read = read_token_account(TokenProgram::Token2022, &account)
            .expect("an extended Token-2022 account should read");
        assert_eq!(read.amount, 7);
    }

    #[test]
    fn read_token_account_rejects_an_extended_mint() {
        let mut data = base_layout(UNRELATED, UNRELATED, 7);
        data.push(AccountType::Mint as u8);

        let account = fake_account_owned_by(UNRELATED, TokenProgram::Token2022.address(), &data);
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
                UNRELATED,
                other.address(),
                &base_layout(UNRELATED, UNRELATED, 0),
            );
            assert_eq!(
                read_token_account(program, &account).err(),
                Some(ProgramError::InvalidAccountData),
                "an account owned by {other:?} should not read under {program:?}",
            );
        }
    }
}
