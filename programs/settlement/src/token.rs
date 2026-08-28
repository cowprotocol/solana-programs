//! Token-program validation and token-account reads shared by `CreateBuffer`
//! and `ReclaimBuffer`.
//!
//! Each takes one `token_program` account, validates it with
//! [`validate_token_program`], and issues all of its CPIs against the address
//! that returns. Token-2022 encodes the instructions this program issues
//! exactly as the legacy program does, so only the CPI target changes; nothing
//! else about them depends on which program it is.
//!
//! What does differ is the account data. A Token-2022 account carrying
//! extensions is longer than the base layout, so the legacy reader (which
//! insists on an exact length and the legacy owner) rejects it. Read token
//! accounts through [`read_token_account`], which dispatches on the validated
//! program.

use cow_settlement_interface::{
    token_program::{is_supported, SPL_TOKEN_PROGRAM_ID, TOKEN_2022_PROGRAM_ID},
    SettlementError,
};
use pinocchio::{cpi::get_return_data, error::ProgramError, AccountView, Address};
use pinocchio_token::{instructions::GetAccountDataSize, state::Mint};

/// The length of a token account holding nothing but the base layout, which is
/// every legacy token account and a Token-2022 one carrying no extensions.
const BASE_TOKEN_ACCOUNT_LEN: u64 = pinocchio_token::state::Account::LEN as u64;

/// Validate that `token_program_account` is a token program this program may
/// issue CPIs against, returning its address for the instruction to target.
///
/// This is the single gate in front of every token CPI: the callers pass the
/// address it returns to `invoke_*_with_unverified_program`, which skips the
/// program check that this already made.
#[must_use = "the returned address is the program the transfers must target"]
pub fn validate_token_program(
    token_program_account: &AccountView,
) -> Result<&Address, ProgramError> {
    let address = token_program_account.address();
    if !is_supported(address) {
        return Err(ProgramError::IncorrectProgramId);
    }
    Ok(address)
}

/// The data length a token account holding `mint` has to be allocated at,
/// under `token_program`.
///
/// A mint carrying no extension data needs no extension space on the accounts
/// that hold it, which is the base layout. Every legacy mint is exactly that
/// long, and so is a Token-2022 mint without extensions; a Token-2022 mint that
/// carries any is padded out past a token account's base layout to make room
/// for its account-type marker, so nothing that short can be one. Anything
/// shorter still isn't a mint at all — including the native mint, which the
/// token program recognizes by address without reading an account — and
/// `InitializeAccount3` is what rejects the ones that matter, as it always has.
///
/// Otherwise the token program is asked, the way the associated-token-account
/// program asks it. That keeps the answer authoritative at run time instead of
/// freezing a mint-extension-to-account-extension table into this program,
/// which would need a redeploy every time Token-2022 grows one.
///
/// Nothing trusts this length for safety, only for liveness: too short and
/// `InitializeAccount3` rejects the account, reverting the whole instruction;
/// too long and the only cost is rent, paid by this instruction's own payer and
/// recovered when the buffer is reclaimed.
// `get_return_data` returns its 1 KiB buffer by value, so keep it in a leaf
// frame of its own rather than the caller's: SBF stack frames are 4 KiB and
// don't grow.
#[inline(never)]
pub fn token_account_len(token_program: &Address, mint: &AccountView) -> Result<u64, ProgramError> {
    if mint.data_len() <= Mint::LEN {
        return Ok(BASE_TOKEN_ACCOUNT_LEN);
    }

    // The CPI below targets whatever address it's handed, so re-establish that
    // it is a token program at all before handing it the mint. Callers have
    // validated it already; this keeps the guarantee local, as
    // `read_token_account` does.
    if !is_supported(token_program) {
        return Err(ProgramError::IncorrectProgramId);
    }

    GetAccountDataSize::new(mint).invoke_with_unverified_program(token_program)?;

    // The token program reports the length as return data. That buffer is a
    // per-transaction global, so it's the program that last set it which makes
    // the value trustworthy. Everything below is defensive: a token program
    // that can't answer this query fails the CPI, which aborts the instruction
    // without returning here at all.
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
///
/// Held by value rather than borrowed from the account so the caller can go on
/// to use the same account in a CPI that touches it: a live borrow would make
/// that CPI fail.
pub struct TokenAccount {
    pub amount: u64,
}

/// Read the base fields of the token account at `account`, which must be owned
/// by `token_program`.
///
/// `token_program` must have come from [`validate_token_program`]; any other
/// address is rejected. The two programs share the base layout, and differ only
/// in what else may follow it, so which reader applies is decided by the
/// program rather than by the data:
///
/// - under SPL Token the data is exactly the base layout;
/// - under Token-2022 extensions may follow it, and an account that carries any
///   is recognized by the account-type marker sitting just past the base.
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

    /// A mint carrying no extension data — every legacy mint, and a Token-2022
    /// mint without extensions — needs only a base-layout token account, and
    /// that is settled without asking the token program.
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

    /// An account too short to be a mint at all still gets the base layout,
    /// leaving `InitializeAccount3` to reject it — which is also how the native
    /// mint works, since the token program knows it by address and litesvm
    /// leaves the account itself absent.
    #[test]
    fn token_account_len_is_the_base_layout_for_a_too_short_account() {
        let mint = fake_account(UNRELATED);
        assert_eq!(
            token_account_len(&SPL_TOKEN_PROGRAM_ID, &mint),
            Ok(BASE_TOKEN_ACCOUNT_LEN),
        );
    }

    /// A longer mint has to be asked about, and off-chain there is nobody to
    /// ask: the CPI is a no-op and no return data comes back. On-chain a token
    /// program that can't answer aborts the instruction instead of returning
    /// here, so this is the error's only reachable path.
    #[test]
    fn token_account_len_reports_unavailable_without_an_answer() {
        let mint = fake_account_owned_by(UNRELATED, TOKEN_2022_PROGRAM_ID, &[0u8; Mint::LEN + 1]);
        assert_eq!(
            token_account_len(&TOKEN_2022_PROGRAM_ID, &mint).err(),
            Some(SettlementError::BufferSizeUnavailable.into()),
        );
    }

    /// The query is a CPI, so an unsupported program is turned away before it
    /// is handed the mint.
    #[test]
    fn token_account_len_rejects_an_unsupported_program() {
        let mint = fake_account_owned_by(UNRELATED, UNRELATED, &[0u8; Mint::LEN + 1]);
        assert_eq!(
            token_account_len(&UNRELATED, &mint).err(),
            Some(ProgramError::IncorrectProgramId),
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

    /// A plain account, the only shape the legacy program has and the shape a
    /// Token-2022 account without extensions also takes, reads under either.
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

    /// The point of the Token-2022 reader: an account whose extensions push it
    /// past the base layout still reads, where the legacy reader's exact-length
    /// check would have rejected it.
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

    /// An over-long account marked as a mint rather than a token account is
    /// still rejected, so the tolerance for extensions doesn't let a mint be
    /// read as if it held a balance.
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

    /// Each reader is tied to its own program: an otherwise well-formed account
    /// owned by one token program can't be read as if it belonged to the other,
    /// which is what stops an instruction from mixing the two.
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
