use std::collections::HashMap;

use anyhow::Context as _;
use clap::Args as ClapArgs;
use cow_settlement_client::{
    cow_settlement_interface::{pda::buffer::find_buffer_pda, Pubkey},
    instruction::{CreateBuffers, TokenProgram},
};
use solana_instruction::Instruction;
use solana_sdk::{account::Account, signature::Signer};

use super::send;
use crate::cmd::Context;
use crate::utils::output::print_summary;
use crate::utils::parsing::BufferArg;
use crate::utils::token::{token_program_of, unpack_mint};

#[derive(ClapArgs)]
pub struct CreateArgs {
    /// The mints to create a buffer for, in one transaction. The payer funds
    /// each new buffer's rent; mints whose buffer already exists are skipped.
    #[arg(required = true, value_parser = parse_mint)]
    mints: Vec<Pubkey>,
}

fn parse_mint(arg: &str) -> anyhow::Result<Pubkey> {
    let BufferArg { address, max_burn } = BufferArg::parse(arg)?;
    anyhow::ensure!(
        max_burn == 0,
        "max burn {max_burn} given, but creating a buffer burns nothing"
    );
    Ok(address)
}

/// Creates the missing buffers in a single transaction. If any mint is not associated
/// with a supported token program, no transaction is created and the first offending mint is shown.
pub fn run(ctx: Context, args: CreateArgs) -> anyhow::Result<()> {
    let payer = ctx.payer.pubkey();
    let buffers: Vec<Pubkey> = args
        .mints
        .iter()
        .map(|mint| find_buffer_pda(&ctx.program_id, mint).0)
        .collect();
    let accounts = ctx
        .rpc
        .get_multiple_accounts(&[&args.mints[..], &buffers].concat())
        .context("failed to fetch mint and buffer accounts")?;
    let (mint_accounts, buffer_accounts) = accounts.split_at(args.mints.len());

    let mut by_program: HashMap<TokenProgram, Vec<Pubkey>> = HashMap::new();
    let mut created = Vec::new();
    let mut existing = Vec::new();
    for (i, mint) in args.mints.iter().enumerate() {
        if buffer_accounts[i].is_some() {
            existing.push(buffers[i]);
            continue;
        }
        let token_program = mint_accounts[i]
            .as_ref()
            .context("mint account not found")
            .and_then(mint_token_program)
            .with_context(|| format!("bad mint {mint}, nothing was created"))?;
        by_program.entry(token_program).or_default().push(*mint);
        created.push(buffers[i]);
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
