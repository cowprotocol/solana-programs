//! Builders for the SPL token instructions the CLI needs in a settlement.

use super::{pda::find_state_pda, token};
use anyhow::Context as _;
use cow_settlement_client::cow_settlement_interface::Pubkey;
use solana_instruction::Instruction;
use spl_token_2022_interface::instruction::{self as token_ix};

/// Build the instructions that bring `wsol`'s balance up to `amount` by wrapping
/// native SOL into it.
///
/// Returns in order: create ATA (only when it does not exist yet), a system
/// transfer of the shortfall, and sync_native. Only the shortfall is wrapped, so
/// a balance that already covers `amount` yields no instructions at all and the
/// caller can extend unconditionally.
pub fn wrap_sol(
    payer: &Pubkey,
    wsol: &token::TokenAccountDetails,
    amount: u64,
) -> anyhow::Result<Vec<Instruction>> {
    let shortfall = amount.saturating_sub(wsol.balance);
    if shortfall == 0 {
        return Ok(Vec::new());
    }

    let mut ixs = Vec::with_capacity(3);
    ixs.extend(wsol.create_ata_ix(payer));

    ixs.push(solana_system_interface::instruction::transfer(
        payer,
        &wsol.handle.account,
        shortfall,
    ));

    ixs.push(
        token_ix::sync_native(&wsol.handle.token_program.address(), &wsol.handle.account)
            .context("failed to build SyncNative instruction")?,
    );

    Ok(ixs)
}

/// Build an `Approve` instruction delegating `amount` of `token` to the PDA
/// derived from `program_id`.
pub fn approve(
    program_id: &Pubkey,
    token: &token::TokenAccountInfo,
    owner: &Pubkey,
    amount: u64,
) -> anyhow::Result<Instruction> {
    let settlement_pda = find_state_pda(program_id);

    token_ix::approve(
        &token.token_program.address(),
        &token.account,
        &settlement_pda,
        owner,
        &[],
        amount,
    )
    .context("failed to build Approve instruction")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::token::{TokenAccountDetails, TokenAccountInfo};
    use cow_settlement_client::instruction::TokenProgram;
    use spl_token_2022_interface::state::Mint;

    /// A resolved WSOL account holding `balance` atoms, existing on-chain or not.
    fn wsol(balance: u64, exists: bool) -> TokenAccountDetails {
        TokenAccountDetails {
            handle: TokenAccountInfo {
                account: Pubkey::new_unique(),
                mint: spl_token_interface::native_mint::ID,
                token_program: TokenProgram::SplToken,
            },
            mint_data: Mint::default(),
            create_ata: (!exists).then(Pubkey::new_unique),
            balance,
            names_native_sol: true,
        }
    }

    /// The lamport amount carried by a System transfer: a 4-byte instruction
    /// discriminant followed by the little-endian amount.
    fn transferred(ix: &Instruction) -> u64 {
        u64::from_le_bytes(ix.data[4..12].try_into().expect("transfer payload"))
    }

    #[test]
    fn wraps_the_full_amount_into_a_missing_account() {
        let payer = Pubkey::new_unique();
        let ixs = wrap_sol(&payer, &wsol(0, false), 1_000).expect("instructions");

        assert_eq!(ixs.len(), 3, "create ATA, transfer, sync");
        assert_eq!(transferred(&ixs[1]), 1_000);
    }

    #[test]
    fn wraps_only_the_shortfall() {
        let payer = Pubkey::new_unique();
        let ixs = wrap_sol(&payer, &wsol(400, true), 1_000).expect("instructions");

        assert_eq!(ixs.len(), 2, "the account exists, so no create");
        assert_eq!(transferred(&ixs[0]), 600);
    }

    #[test]
    fn does_not_wrap_when_the_balance_suffices() {
        let payer = Pubkey::new_unique();

        for balance in [1_000, 1_001] {
            let ixs = wrap_sol(&payer, &wsol(balance, true), 1_000).expect("instructions");
            assert!(ixs.is_empty(), "balance {balance} covers the sell amount");
        }
    }
}
