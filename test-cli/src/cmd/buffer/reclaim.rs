use std::collections::HashMap;

use anyhow::Context as _;
use clap::Args as ClapArgs;
use cow_settlement_client::{
    cow_settlement_interface::{
        instruction::reclaim_buffer::ReclaimBuffer, pda::buffer::find_buffer_pda, Pubkey,
    },
    instruction::TokenProgram,
};
use solana_instruction::Instruction;
use solana_sdk::{account::Account, signature::Signer};

use super::{dedup, send};
use crate::cmd::Context;
use crate::utils::keypair::read_keypair_or;
use crate::utils::output::{print_failures, print_summary};
use crate::utils::pda::find_state_pda;
use crate::utils::token::{token_program_of, unpack_token_account};

/// Most buffers that always fit into one transaction, reached when the
/// buffers span both token programs and so need an instruction each, and the
/// reclaim authority signs separately from the payer.
const MAX_BUFFERS: usize = 12;

#[derive(ClapArgs)]
pub struct ReclaimArgs {
    /// The account receiving the closed buffers' rent
    target: Pubkey,

    /// The buffer PDAs to close, at most 12 so they fit into one transaction.
    /// Buffers still holding tokens are left open.
    #[arg(required = true, num_args = 1..=MAX_BUFFERS)]
    buffers: Vec<Pubkey>,

    /// Path to the reclaim-authority keypair, which must sign (defaults to the
    /// payer keypair)
    #[arg(long)]
    reclaim_authority: Option<String>,
}

/// Closes the empty buffers in a single transaction. Buffers holding tokens
/// are left open and listed, as the program would skip them anyway. If any
/// other buffer is bad, nothing is reclaimed and the offending buffers are
/// summarized.
pub fn run(ctx: Context, args: ReclaimArgs) -> anyhow::Result<()> {
    let reclaim_authority = read_keypair_or(args.reclaim_authority, &ctx.payer)?;
    let mut failures: Vec<(String, String)> = Vec::new();

    let buffers = dedup(&args.buffers);
    let accounts = ctx
        .rpc
        .get_multiple_accounts(&buffers)
        .context("failed to fetch buffer accounts")?;

    let mut by_program: HashMap<TokenProgram, Vec<(Pubkey, Pubkey)>> = HashMap::new();
    let mut to_close = Vec::new();
    let mut non_empty = Vec::new();
    for (buffer, account) in buffers.iter().zip(accounts) {
        let decoded = account
            .context("buffer account not found")
            .and_then(|account| decode_buffer(&account, buffer, &ctx.program_id));
        match decoded {
            Ok(Buffer {
                amount: 0,
                token_program,
                mint,
            }) => {
                by_program
                    .entry(token_program)
                    .or_default()
                    .push((*buffer, mint));
                to_close.push(*buffer);
            }
            Ok(Buffer { .. }) => non_empty.push(*buffer),
            Err(e) => failures.push((buffer.to_string(), format!("{e:#}"))),
        }
    }
    if !failures.is_empty() {
        print_failures(&failures);
        anyhow::bail!(
            "{} of {} buffers are bad, nothing was reclaimed",
            failures.len(),
            buffers.len()
        );
    }

    let mut reclaimed = Vec::new();
    let sig = if to_close.is_empty() {
        None
    } else {
        let ixs = instructions(
            &ctx.program_id,
            &reclaim_authority.pubkey(),
            &args.target,
            &by_program,
        );
        let sig = send(&ctx, &ixs, &[&reclaim_authority])?;
        // The program skips rather than rejects a buffer that received tokens
        // since it was fetched, so check which ones are gone.
        let accounts = ctx
            .rpc
            .get_multiple_accounts(&to_close)
            .with_context(|| format!("sent {sig}, but failed to check the outcome"))?;
        for (buffer, account) in to_close.iter().zip(accounts) {
            match account {
                None => reclaimed.push(*buffer),
                Some(_) => non_empty.push(*buffer),
            }
        }
        Some(sig)
    };

    let mut summary: Vec<(&str, &dyn ToString)> = Vec::new();
    if let Some(sig) = &sig {
        summary.push(("signature", sig));
    }
    summary.push(("reclaimRecipient", &args.target));
    summary.extend(
        reclaimed
            .iter()
            .map(|pda| ("bufferPda", pda as &dyn ToString)),
    );
    summary.extend(
        non_empty
            .iter()
            .map(|pda| ("nonEmptyBufferPda", pda as &dyn ToString)),
    );
    print_summary(&summary);

    Ok(())
}

/// One `ReclaimBuffer` instruction per token program, as each serves only one.
fn instructions(
    program_id: &Pubkey,
    reclaim_authority: &Pubkey,
    target: &Pubkey,
    by_program: &HashMap<TokenProgram, Vec<(Pubkey, Pubkey)>>,
) -> Vec<Instruction> {
    by_program
        .iter()
        .map(|(token_program, buffers)| {
            ReclaimBuffer {
                program_id: *program_id,
                state_pda: find_state_pda(program_id),
                reclaim_authority: *reclaim_authority,
                reclaim_recipient: *target,
                token_program: token_program.address(),
                buffers,
            }
            .into()
        })
        .collect()
}

/// What closing a buffer depends on.
#[derive(Debug, PartialEq)]
struct Buffer {
    token_program: TokenProgram,
    mint: Pubkey,
    amount: u64,
}

/// Decodes `account`, stored at `address`. Fails unless it's a token account
/// of a supported token program at the buffer PDA of its mint under
/// `program_id`.
fn decode_buffer(
    account: &Account,
    address: &Pubkey,
    program_id: &Pubkey,
) -> anyhow::Result<Buffer> {
    let token_program = token_program_of(account)?;
    let token_account =
        unpack_token_account(&account.data).context("account is not a token account")?;
    let mint = token_account.mint;
    anyhow::ensure!(
        find_buffer_pda(program_id, &mint).0 == *address,
        "not the buffer of mint {mint} under {program_id}"
    );
    Ok(Buffer {
        token_program,
        mint,
        amount: token_account.amount,
    })
}

#[cfg(test)]
mod tests {
    use solana_program_pack::Pack as _;
    use spl_token_2022_interface::state::{Account as TokenAccount, AccountState};

    use super::*;
    use solana_sdk::transaction::Transaction;

    const PROGRAM_ID: Pubkey = Pubkey::new_from_array([1; 32]);
    const PAYER: Pubkey = Pubkey::new_from_array([3; 32]);

    /// Largest serialized transaction the network accepts
    /// (`solana_packet::PACKET_DATA_SIZE`).
    const MAX_TRANSACTION_SIZE: u64 = 1232;

    fn fits(ixs: &[Instruction], payer: &Pubkey) -> bool {
        let tx = Transaction::new_with_payer(ixs, Some(payer));
        bincode::serialized_size(&tx).unwrap() <= MAX_TRANSACTION_SIZE
    }
    const MINT: Pubkey = Pubkey::new_from_array([2; 32]);

    fn token_account(owner: Pubkey, mint: Pubkey, amount: u64) -> Account {
        let mut data = vec![0; TokenAccount::LEN];
        TokenAccount {
            mint,
            owner: Pubkey::new_unique(),
            amount,
            state: AccountState::Initialized,
            ..Default::default()
        }
        .pack_into_slice(&mut data);
        Account {
            lamports: 1,
            data,
            owner,
            executable: false,
            rent_epoch: 0,
        }
    }

    /// `len` buffers split across both token programs and a reclaim
    /// authority other than the payer, the costliest layout.
    fn split_instructions(len: usize) -> Vec<Instruction> {
        let buffers: Vec<(Pubkey, Pubkey)> = (0..len)
            .map(|_| (Pubkey::new_unique(), Pubkey::new_unique()))
            .collect();
        let (spl, token_2022) = buffers.split_at(1);
        let by_program = HashMap::from([
            (TokenProgram::SplToken, spl.to_vec()),
            (TokenProgram::Token2022, token_2022.to_vec()),
        ]);
        instructions(
            &PROGRAM_ID,
            &Pubkey::new_unique(),
            &Pubkey::new_unique(),
            &by_program,
        )
    }

    #[test]
    fn max_buffers_is_the_most_that_always_fit() {
        assert!(fits(&split_instructions(MAX_BUFFERS), &PAYER));
        assert!(!fits(&split_instructions(MAX_BUFFERS + 1), &PAYER));
    }

    #[test]
    fn instructions_serve_one_token_program_each() {
        let ixs = split_instructions(3);
        assert_eq!(ixs.len(), 2);
        let mut programs: Vec<Pubkey> = ixs.iter().map(|ix| ix.accounts[3].pubkey).collect();
        programs.sort();
        let mut expected = TokenProgram::ALL.map(TokenProgram::address);
        expected.sort();
        assert_eq!(programs, expected);
    }

    #[test]
    fn decode_buffer_reads_mint_and_amount() {
        let buffer = find_buffer_pda(&PROGRAM_ID, &MINT).0;
        for token_program in TokenProgram::ALL {
            let account = token_account(token_program.address(), MINT, 7);
            assert_eq!(
                decode_buffer(&account, &buffer, &PROGRAM_ID).unwrap(),
                Buffer {
                    token_program,
                    mint: MINT,
                    amount: 7
                }
            );
        }
    }

    #[test]
    fn decode_buffer_rejects_other_addresses() {
        let account = token_account(TokenProgram::SplToken.address(), MINT, 0);
        let other_program = find_buffer_pda(&Pubkey::new_unique(), &MINT).0;
        let err = decode_buffer(&account, &other_program, &PROGRAM_ID)
            .unwrap_err()
            .to_string();
        assert!(err.starts_with("not the buffer of mint"), "{err}");
    }

    #[test]
    fn decode_buffer_rejects_foreign_owner() {
        let buffer = find_buffer_pda(&PROGRAM_ID, &MINT).0;
        let account = token_account(Pubkey::new_unique(), MINT, 0);
        assert!(decode_buffer(&account, &buffer, &PROGRAM_ID).is_err());
    }

    #[test]
    fn decode_buffer_rejects_non_token_accounts() {
        let buffer = find_buffer_pda(&PROGRAM_ID, &MINT).0;
        let mut account = token_account(TokenProgram::SplToken.address(), MINT, 0);
        account.data.truncate(1);
        let err = decode_buffer(&account, &buffer, &PROGRAM_ID)
            .unwrap_err()
            .to_string();
        assert_eq!(err, "account is not a token account");
    }
}
