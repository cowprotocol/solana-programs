//! Off-chain decoded snapshot of a settlement state account.

use cow_settlement_interface::{data::state::StateAccount, Pubkey, Role};
use solana_program_error::ProgramError;

/// An owned, decoded snapshot of a settlement state account.
/// Similar to [`StateAccount`], but it fully owns its data.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecodedStateAccount {
    pub manager: Pubkey,
    pub reclaim_authority: Pubkey,
    pub fee_withdrawal_authority: Pubkey,
}

impl TryFrom<&[u8]> for DecodedStateAccount {
    type Error = ProgramError;

    fn try_from(bytes: &[u8]) -> Result<Self, Self::Error> {
        let state = StateAccount::attach(bytes)?;
        Ok(Self {
            manager: state.authority(Role::Manager),
            reclaim_authority: state.authority(Role::ReclaimAuthority),
            fee_withdrawal_authority: state.authority(Role::FeeWithdrawalAuthority),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cow_settlement_interface::data::state::{StateInitArgs, WIDTH_HEADER};
    use cow_settlement_interface::fixtures::pubkey_from_seed;

    fn state_bytes(init_args: &StateInitArgs) -> [u8; WIDTH_HEADER] {
        let mut bytes = [0u8; WIDTH_HEADER];
        StateAccount::initialize(&mut bytes[..], init_args).expect("header fits");
        bytes
    }

    #[test]
    fn decodes_the_header() {
        let manager = pubkey_from_seed("manager");
        let reclaim_authority = pubkey_from_seed("reclaim authority");
        let fee_withdrawal_authority = pubkey_from_seed("fee withdrawal authority");
        let bytes = state_bytes(&StateInitArgs {
            manager,
            reclaim_authority,
            fee_withdrawal_authority,
        });

        let decoded = DecodedStateAccount::try_from(&bytes[..]).expect("valid state account");
        assert_eq!(
            decoded,
            DecodedStateAccount {
                manager,
                reclaim_authority,
                fee_withdrawal_authority,
            },
        );
    }

    #[test]
    fn rejects_non_state_account() {
        // A zeroed buffer: right length, but its leading byte isn't the state
        // discriminator.
        let bytes = [0u8; WIDTH_HEADER];
        assert!(DecodedStateAccount::try_from(&bytes[..]).is_err());
    }

    #[test]
    fn rejects_too_short_account() {
        let bytes = state_bytes(&StateInitArgs {
            manager: pubkey_from_seed("manager"),
            reclaim_authority: pubkey_from_seed("reclaim authority"),
            fee_withdrawal_authority: pubkey_from_seed("fee withdrawal authority"),
        });
        assert!(DecodedStateAccount::try_from(&bytes[..WIDTH_HEADER - 1]).is_err());
    }
}
