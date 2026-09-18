//! Off-chain decoded snapshot of a settlement state account.

use cow_settlement_interface::{data::state::StateAccount, Pubkey, Role};
use solana_program_error::ProgramError;

/// An owned, decoded snapshot of a settlement state account.
/// Similar to [`StateAccount`], but it fully owns its data.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecodedStateAccount {
    pub manager: Pubkey,
    pub solver_authority: Pubkey,
    pub reclaim_authority: Pubkey,
    pub self_order_authority: Pubkey,
}

impl TryFrom<&[u8]> for DecodedStateAccount {
    type Error = ProgramError;

    fn try_from(bytes: &[u8]) -> Result<Self, Self::Error> {
        let state = StateAccount::attach(bytes)?;
        Ok(Self {
            manager: state.authority(Role::Manager),
            solver_authority: state.authority(Role::SolverAuthority),
            reclaim_authority: state.authority(Role::ReclaimAuthority),
            self_order_authority: state.authority(Role::SelfOrderAuthority),
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
        let solver_authority = pubkey_from_seed("solver authority");
        let reclaim_authority = pubkey_from_seed("reclaim authority");
        let self_order_authority = pubkey_from_seed("self-order authority");
        let bytes = state_bytes(&StateInitArgs {
            manager,
            solver_authority,
            reclaim_authority,
            self_order_authority,
        });

        let decoded = DecodedStateAccount::try_from(&bytes[..]).expect("valid state account");
        assert_eq!(
            decoded,
            DecodedStateAccount {
                manager,
                solver_authority,
                reclaim_authority,
                self_order_authority,
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
            solver_authority: pubkey_from_seed("solver authority"),
            reclaim_authority: pubkey_from_seed("reclaim authority"),
            self_order_authority: pubkey_from_seed("self-order authority"),
        });
        assert!(DecodedStateAccount::try_from(&bytes[..WIDTH_HEADER - 1]).is_err());
    }
}
