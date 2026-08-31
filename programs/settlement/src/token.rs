//! Token-program validation and token-account reads

use cow_settlement_interface::{
    token_program::{is_supported, SPL_TOKEN_PROGRAM_ID, TOKEN_2022_PROGRAM_ID},
    SettlementError,
};
use pinocchio::{cpi::get_return_data, error::ProgramError, AccountView, Address};
use pinocchio_token::{instructions::GetAccountDataSize, state::Mint};

/// The length of a SPL token program account. Token2022 extensions may make
/// the actual token account longer than this.
const BASE_TOKEN_ACCOUNT_LEN: u64 = pinocchio_token::state::Account::LEN as u64;

/// Validate that `token_program_account` is a token program this program may
/// issue CPIs against, returning its address for the instruction to target.
#[must_use = "not consuming skips validation"]
pub fn validate_token_program(
    token_program_account: &AccountView,
) -> Result<&Address, ProgramError> {
    let address = token_program_account.address();
    if !is_supported(address) {
        return Err(ProgramError::IncorrectProgramId);
    }
    Ok(address)
}

/// The data length a token account holding `mint` has to be allocated at.
/// It is assumed that `token_program` has already been validated with [`validate_token_program`].
#[inline(never)]
pub fn token_account_len(token_program: &Address, mint: &AccountView) -> Result<u64, ProgramError> {
    // If the mint is of base SPL Mint length, the token accounts must be of base length accordingly.
    if mint.data_len() <= Mint::LEN {
        return Ok(BASE_TOKEN_ACCOUNT_LEN);
    }

    // SPL token provides a function to get the actual required account data size
    GetAccountDataSize::new(mint).invoke_with_unverified_program(token_program)?;

    let reported = get_return_data().ok_or(SettlementError::BufferSizeUnavailable)?;
    if reported.program_id() != token_program {
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
/// It is assumed that `token_program` has already been validated with [`validate_token_program`],
/// or else a program error will be thrown.
pub fn read_token_account(
    token_program: &Address,
    account: &AccountView,
) -> Result<TokenAccount, ProgramError> {
    if token_program == &SPL_TOKEN_PROGRAM_ID {
        let account = pinocchio_token::state::Account::from_account_view(account)?;
        Ok(TokenAccount {
            amount: account.amount(),
        })
    } else if token_program == &TOKEN_2022_PROGRAM_ID {
        let account = pinocchio_token_2022::state::Account::from_account_view(account)?;
        Ok(TokenAccount {
            amount: account.amount(),
        })
    } else {
        Err(ProgramError::IncorrectProgramId)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cow_settlement_interface::instruction::fixtures::{fake_account, fake_account_owned_by};
    use cow_settlement_interface::token_program::SUPPORTED_TOKEN_PROGRAMS;
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
        assert_eq!(SPL_TOKEN_PROGRAM_ID, pinocchio_token::ID);
        assert_eq!(TOKEN_2022_PROGRAM_ID, pinocchio_token_2022::ID);
    }

    /// The base layout is the same under both programs, so one reader's idea of
    /// its length is the other's too.
    #[test]
    fn both_programs_share_the_base_layout_length() {
        assert_eq!(BASE_LEN, pinocchio_token_2022::state::Account::BASE_LEN);
    }

    #[test]
    fn token_account_len_is_the_base_layout_for_a_plain_mint() {
        for program in SUPPORTED_TOKEN_PROGRAMS {
            let mint = fake_account_owned_by(UNRELATED, program, &[0u8; Mint::LEN]);
            assert_eq!(
                token_account_len(&program, &mint),
                Ok(BASE_TOKEN_ACCOUNT_LEN),
                "a base-layout mint should need a base-layout account under {program}",
            );
        }
    }

    #[test]
    fn token_account_len_is_the_base_layout_for_a_too_short_account() {
        let mint = fake_account(UNRELATED);
        assert_eq!(
            token_account_len(&SPL_TOKEN_PROGRAM_ID, &mint),
            Ok(BASE_TOKEN_ACCOUNT_LEN),
        );
    }

    #[test]
    fn token_account_len_reports_unavailable_without_an_answer() {
        let mint = fake_account_owned_by(UNRELATED, TOKEN_2022_PROGRAM_ID, &[0u8; Mint::LEN + 1]);
        assert_eq!(
            token_account_len(&TOKEN_2022_PROGRAM_ID, &mint).err(),
            Some(SettlementError::BufferSizeUnavailable.into()),
        );
    }

    #[test]
    fn validate_token_program_accepts_every_supported_program() {
        for program in SUPPORTED_TOKEN_PROGRAMS {
            let account = fake_account(program);
            assert_eq!(validate_token_program(&account), Ok(&program));
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
        for program in SUPPORTED_TOKEN_PROGRAMS {
            let account =
                fake_account_owned_by(UNRELATED, program, &base_layout(mint, owner, 4_200));
            let read = read_token_account(&program, &account)
                .unwrap_or_else(|error| panic!("{program} account should read: {error:?}"));
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

        let account = fake_account_owned_by(UNRELATED, TOKEN_2022_PROGRAM_ID, &data);
        let read = read_token_account(&TOKEN_2022_PROGRAM_ID, &account)
            .expect("an extended Token-2022 account should read");
        assert_eq!(read.amount, 7);
    }

    #[test]
    fn read_token_account_rejects_an_extended_mint() {
        let mut data = base_layout(UNRELATED, UNRELATED, 7);
        data.push(AccountType::Mint as u8);

        let account = fake_account_owned_by(UNRELATED, TOKEN_2022_PROGRAM_ID, &data);
        assert_eq!(
            read_token_account(&TOKEN_2022_PROGRAM_ID, &account).err(),
            Some(ProgramError::InvalidAccountData),
        );
    }

    #[test]
    fn read_token_account_rejects_unvalidated_program() {
        // A well-formed legacy token account, so the rejection can only come
        // from the program address.
        let account = fake_account_owned_by(
            UNRELATED,
            SPL_TOKEN_PROGRAM_ID,
            &base_layout(UNRELATED, UNRELATED, 0),
        );
        assert_eq!(
            read_token_account(&UNRELATED, &account).err(),
            Some(ProgramError::IncorrectProgramId),
        );
    }

    #[test]
    fn read_token_account_rejects_the_other_programs_account() {
        for [program, other] in [
            [SPL_TOKEN_PROGRAM_ID, TOKEN_2022_PROGRAM_ID],
            [TOKEN_2022_PROGRAM_ID, SPL_TOKEN_PROGRAM_ID],
        ] {
            let account =
                fake_account_owned_by(UNRELATED, other, &base_layout(UNRELATED, UNRELATED, 0));
            assert_eq!(
                read_token_account(&program, &account).err(),
                Some(ProgramError::InvalidAccountData),
                "an account owned by {other} should not read under {program}",
            );
        }
    }
}
