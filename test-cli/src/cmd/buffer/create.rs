use std::collections::HashMap;

use anyhow::Context as _;
use clap::Args as ClapArgs;
use cow_settlement_client::{
    cow_settlement_interface::{pda::buffer::find_buffer_pda, Pubkey},
    instruction::{CreateBuffers, TokenProgram},
};
use solana_instruction::Instruction;
use solana_sdk::{account::Account, signature::Signer};

use super::{dedup, send};
use crate::cmd::Context;
use crate::utils::output::{print_failures, print_summary};
use crate::utils::token::{token_program_of, unpack_mint};

/// Most mints whose buffers always fit into one transaction, reached when the
/// mints span both token programs and so need an instruction each.
const MAX_MINTS: usize = 14;

#[derive(ClapArgs)]
pub struct CreateArgs {
    /// The mints to create a buffer for, at most 14 so they fit into one
    /// transaction. The payer funds each new buffer's rent; mints whose buffer
    /// already exists are skipped.
    #[arg(required = true, num_args = 1..=MAX_MINTS)]
    mints: Vec<Pubkey>,
}

/// Creates the missing buffers in a single transaction. If any mint is bad,
/// nothing is created and the offending mints are summarized.
pub fn run(ctx: Context, args: CreateArgs) -> anyhow::Result<()> {
    let payer = ctx.payer.pubkey();
    let mut failures: Vec<(String, String)> = Vec::new();

    let mints = dedup(&args.mints);
    let buffers: Vec<Pubkey> = mints
        .iter()
        .map(|mint| find_buffer_pda(&ctx.program_id, mint).0)
        .collect();
    let accounts = ctx
        .rpc
        .get_multiple_accounts(&[&mints[..], &buffers].concat())
        .context("failed to fetch mint and buffer accounts")?;
    let (mint_accounts, buffer_accounts) = accounts.split_at(mints.len());

    let mut by_program: HashMap<TokenProgram, Vec<Pubkey>> = HashMap::new();
    let mut created = Vec::new();
    let mut existing = Vec::new();
    for (i, mint) in mints.iter().enumerate() {
        if buffer_accounts[i].is_some() {
            existing.push(buffers[i]);
            continue;
        }
        let token_program = mint_accounts[i]
            .as_ref()
            .context("mint account not found")
            .and_then(mint_token_program);
        match token_program {
            Ok(token_program) => {
                by_program.entry(token_program).or_default().push(*mint);
                created.push(buffers[i]);
            }
            Err(e) => failures.push((mint.to_string(), format!("{e:#}"))),
        }
    }
    if !failures.is_empty() {
        print_failures(&failures);
        anyhow::bail!(
            "{} of {} mints are bad, nothing was created",
            failures.len(),
            mints.len()
        );
    }

    let sig = if created.is_empty() {
        None
    } else {
        let ixs = instructions(&ctx.program_id, &payer, &by_program);
        Some(send(&ctx, &ixs, &[])?)
    };

    let mut summary: Vec<(&str, &dyn ToString)> = Vec::new();
    if let Some(sig) = &sig {
        summary.push(("signature", sig));
    }
    summary.extend(
        created
            .iter()
            .map(|pda| ("bufferPda", pda as &dyn ToString)),
    );
    summary.extend(
        existing
            .iter()
            .map(|pda| ("existingBufferPda", pda as &dyn ToString)),
    );
    print_summary(&summary);

    Ok(())
}

/// One `CreateBuffer` instruction per token program, as each serves only one.
fn instructions(
    program_id: &Pubkey,
    payer: &Pubkey,
    by_program: &HashMap<TokenProgram, Vec<Pubkey>>,
) -> Vec<Instruction> {
    by_program
        .iter()
        .map(|(&token_program, mints)| {
            CreateBuffers {
                program_id: *program_id,
                payer: *payer,
                token_program,
                mints,
            }
            .into()
        })
        .collect()
}

/// The token program owning `account`. Fails unless `account` is a mint of a
/// supported token program.
fn mint_token_program(account: &Account) -> anyhow::Result<TokenProgram> {
    let token_program = token_program_of(account)?;
    unpack_mint(&account.data).context("account is not a mint")?;
    Ok(token_program)
}

#[cfg(test)]
mod tests {
    use solana_program_pack::Pack as _;
    use spl_token_2022_interface::state::{Account as TokenAccount, Mint};

    use super::*;
    use solana_sdk::transaction::Transaction;

    const PAYER: Pubkey = Pubkey::new_from_array([1; 32]);

    /// Largest serialized transaction the network accepts
    /// (`solana_packet::PACKET_DATA_SIZE`).
    const MAX_TRANSACTION_SIZE: u64 = 1232;

    fn fits(ixs: &[Instruction], payer: &Pubkey) -> bool {
        let tx = Transaction::new_with_payer(ixs, Some(payer));
        bincode::serialized_size(&tx).unwrap() <= MAX_TRANSACTION_SIZE
    }

    fn account(owner: Pubkey, data: Vec<u8>) -> Account {
        Account {
            lamports: 1,
            data,
            owner,
            executable: false,
            rent_epoch: 0,
        }
    }

    fn mint_data() -> Vec<u8> {
        let mut data = vec![0; Mint::LEN];
        Mint {
            is_initialized: true,
            ..Default::default()
        }
        .pack_into_slice(&mut data);
        data
    }

    /// `len` mints split across both token programs, the costliest layout.
    fn split_instructions(len: usize) -> Vec<Instruction> {
        let mints: Vec<Pubkey> = (0..len).map(|_| Pubkey::new_unique()).collect();
        let (spl, token_2022) = mints.split_at(1);
        let by_program = HashMap::from([
            (TokenProgram::SplToken, spl.to_vec()),
            (TokenProgram::Token2022, token_2022.to_vec()),
        ]);
        instructions(&Pubkey::new_unique(), &PAYER, &by_program)
    }

    #[test]
    fn max_mints_is_the_most_that_always_fit() {
        assert!(fits(&split_instructions(MAX_MINTS), &PAYER));
        assert!(!fits(&split_instructions(MAX_MINTS + 1), &PAYER));
    }

    #[test]
    fn instructions_serve_one_token_program_each() {
        let ixs = split_instructions(3);
        assert_eq!(ixs.len(), 2);
        let mut programs: Vec<Pubkey> = ixs.iter().map(|ix| ix.accounts[2].pubkey).collect();
        programs.sort();
        let mut expected = TokenProgram::ALL.map(TokenProgram::address);
        expected.sort();
        assert_eq!(programs, expected);
    }

    #[test]
    fn mint_token_program_accepts_mints_of_every_supported_program() {
        for program in TokenProgram::ALL {
            let mint = account(program.address(), mint_data());
            assert_eq!(mint_token_program(&mint).unwrap(), program);
        }
    }

    #[test]
    fn mint_token_program_rejects_foreign_owner() {
        let mint = account(Pubkey::new_unique(), mint_data());
        assert!(mint_token_program(&mint).is_err());
    }

    #[test]
    fn mint_token_program_rejects_non_mints() {
        let token_account = account(TokenProgram::SplToken.address(), vec![0; TokenAccount::LEN]);
        let err = mint_token_program(&token_account).unwrap_err().to_string();
        assert_eq!(err, "account is not a mint");
    }
}
