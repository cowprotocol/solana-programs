use std::collections::{HashMap, HashSet};

use anyhow::Context as _;
use clap::Args as ClapArgs;
use cow_settlement_client::{
    cow_settlement_interface::{pda::buffer::find_buffer_pda, Pubkey},
    instruction::{CreateBuffers, TokenProgram},
};
use solana_instruction::Instruction;
use solana_sdk::{account::Account, signature::Signer};

use crate::cmd::Context;
use crate::utils::output::print_summary;
use crate::utils::token::{token_program_of, unpack_mint};
use crate::utils::transaction::send;

#[derive(ClapArgs)]
pub struct CreateArgs {
    /// The mints to create a buffer for, in one transaction. The payer funds
    /// each new buffer's rent; mints whose buffer already exists are skipped.
    #[arg(required = true)]
    mints: Vec<Pubkey>,
}

struct ResolvedMint {
    /// The mint as given on the command line.
    mint: Pubkey,
    /// Its buffer PDA.
    buffer: Pubkey,
    /// The account found at `mint`.
    mint_account: Option<Account>,
    /// Whether an account already exists at `buffer`.
    buffer_exists: bool,
}

/// Creates the missing buffers in a single transaction. If any mint is not associated
/// with a supported token program, no transaction is created and the first offending mint is shown.
pub fn run(ctx: Context, mut args: CreateArgs) -> anyhow::Result<()> {
    let payer = ctx.payer.pubkey();
    // A repeated mint would otherwise be listed as created twice.
    let mut seen = HashSet::new();
    args.mints.retain(|mint| seen.insert(*mint));
    let mints: Vec<ResolvedMint> = {
        let buffers: Vec<Pubkey> = args
            .mints
            .iter()
            .map(|mint| find_buffer_pda(&ctx.program_id, mint).0)
            .collect();
        let mut accounts = ctx
            .rpc
            .get_multiple_accounts(&[&args.mints[..], &buffers].concat())
            .context("failed to fetch mint and buffer accounts")?;
        let buffer_accounts = accounts.split_off(args.mints.len());
        args.mints
            .iter()
            .zip(buffers)
            .zip(accounts)
            .zip(buffer_accounts)
            .map(
                |(((&mint, buffer), mint_account), buffer_account)| ResolvedMint {
                    mint,
                    buffer,
                    mint_account,
                    buffer_exists: buffer_account.is_some(),
                },
            )
            .collect()
    };

    let mut by_program: HashMap<TokenProgram, Vec<Pubkey>> = HashMap::new();
    let mut created = Vec::new();
    let mut existing = Vec::new();
    for resolved in &mints {
        if resolved.buffer_exists {
            existing.push(resolved.buffer);
            continue;
        }
        let token_program = resolved
            .mint_account
            .as_ref()
            .context("mint account not found")
            .and_then(mint_token_program)
            .with_context(|| format!("bad mint {}, nothing was created", resolved.mint))?;
        by_program
            .entry(token_program)
            .or_default()
            .push(resolved.mint);
        created.push(resolved.buffer);
    }

    let sig;
    let mut summary: Vec<(&str, &dyn ToString)> = Vec::new();
    if !created.is_empty() {
        let ixs = instructions(&ctx.program_id, &payer, &by_program);
        sig = send(&ctx, &ixs, &[])?;
        summary.push(("signature", &sig));
    }
    for (label, pdas) in [("bufferPda", &created), ("existingBufferPda", &existing)] {
        summary.extend(pdas.iter().map(|pda| (label, pda as &dyn ToString)));
    }
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
