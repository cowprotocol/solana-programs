use std::{collections::HashMap, str::FromStr};

use anyhow::Context as _;
use clap::Args as ClapArgs;
use cow_settlement_client::{
    cow_settlement_interface::{instruction::reclaim_buffer::ReclaimBuffer, Pubkey},
    instruction::TokenProgram,
};
use solana_instruction::Instruction;
use solana_sdk::{account::Account, signature::Signer};

use super::send;
use crate::cmd::Context;
use crate::utils::keypair::read_keypair_or;
use crate::utils::output::{print_failures, print_summary};
use crate::utils::pda::SettlementSeed;
use crate::utils::token::{token_program_of, unpack_token_account};

/// Most buffers that always fit into one transaction, reached when the
/// buffers span both token programs and so need an instruction each, and the
/// reclaim authority signs separately from the payer.
const MAX_BUFFERS: usize = 11;

#[derive(ClapArgs)]
pub struct ReclaimArgs {
    /// The account receiving the closed buffers' rent
    target: Pubkey,

    /// The buffer PDAs to close, at most 11 so they fit into one transaction.
    /// Buffers holding more tokens than their burn limit are left open.
    /// Specify just the buffer pubkey to close without burning, or if burning some
    /// tokens is desired, use the syntax `<pubkey>:<max burn>`.
    #[arg(required = true, num_args = 1..=MAX_BUFFERS, value_parser = BufferArg::parse)]
    buffers: Vec<BufferArg>,

    /// Path to the reclaim-authority keypair, which must sign (defaults to the
    /// payer keypair)
    #[arg(long)]
    reclaim_authority: Option<String>,

    /// `<major>.<minor>` release the program runs, which its state and buffer
    /// PDAs are derived from. Set it to reclaim the buffers of a deployment of
    /// an older release (e.g. `0.4`)
    #[arg(
        long,
        default_value = SettlementSeed::CURRENT_VERSION,
        value_parser = SettlementSeed::parse
    )]
    settlement_version: SettlementSeed,
}

/// Closes the buffers within their burn limit in a single transaction,
/// burning what they hold. Buffers over their limit are left open and listed,
/// as the program would skip them anyway. If any other buffer is bad, nothing
/// is reclaimed and the offending buffers are summarized.
pub fn run(ctx: Context, args: ReclaimArgs) -> anyhow::Result<()> {
    let reclaim_authority = read_keypair_or(args.reclaim_authority, &ctx.payer)?;
    let mut failures: Vec<(String, String)> = Vec::new();

    let addresses: Vec<Pubkey> = args.buffers.iter().map(|b| b.address).collect();
    let accounts = ctx
        .rpc
        .get_multiple_accounts(&addresses)
        .context("failed to fetch buffer accounts")?;

    let mut by_program: HashMap<TokenProgram, Vec<(Pubkey, Pubkey, u64)>> = HashMap::new();
    let mut to_close = Vec::new();
    let mut non_empty = Vec::new();
    for (i, (buffer, account)) in args.buffers.iter().zip(accounts).enumerate() {
        let address = buffer.address;
        if addresses[..i].contains(&address) {
            failures.push((
                address.to_string(),
                "buffer listed more than once".to_string(),
            ));
            continue;
        }
        let decoded = account
            .context("buffer account not found")
            .and_then(|account| {
                decode_buffer(
                    &account,
                    &address,
                    &ctx.program_id,
                    &args.settlement_version,
                )
            });
        match decoded {
            Ok(Buffer {
                amount,
                token_program,
                mint,
            }) if amount <= buffer.max_burn => {
                by_program
                    .entry(token_program)
                    .or_default()
                    .push((address, mint, buffer.max_burn));
                to_close.push(address);
            }
            Ok(Buffer { .. }) => non_empty.push(address),
            Err(e) => failures.push((address.to_string(), format!("{e:#}"))),
        }
    }
    if !failures.is_empty() {
        print_failures(&failures);
        anyhow::bail!(
            "{} of {} buffers are bad, nothing was reclaimed",
            failures.len(),
            args.buffers.len()
        );
    }

    let mut reclaimed = Vec::new();
    let sig = if to_close.is_empty() {
        None
    } else {
        let ixs = instructions(
            &ctx.program_id,
            &args.settlement_version.state_pda(&ctx.program_id),
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
    state_pda: &Pubkey,
    reclaim_authority: &Pubkey,
    target: &Pubkey,
    by_program: &HashMap<TokenProgram, Vec<(Pubkey, Pubkey, u64)>>,
) -> Vec<Instruction> {
    by_program
        .iter()
        .map(|(token_program, buffers)| {
            ReclaimBuffer {
                program_id: *program_id,
                state_pda: *state_pda,
                reclaim_authority: *reclaim_authority,
                reclaim_recipient: *target,
                token_program: token_program.address(),
                buffers,
            }
            .into()
        })
        .collect()
}

/// A buffer to close, and the most of its balance that may be burned to do so.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct BufferArg {
    address: Pubkey,
    max_burn: u64,
}

impl BufferArg {
    /// Parses `<pubkey>` (no burning) or `<pubkey>:<max burn>`.
    fn parse(arg: &str) -> anyhow::Result<Self> {
        let (address, max_burn) = match arg.split_once(':') {
            Some((address, max_burn)) => (address, Some(max_burn)),
            None => (arg, None),
        };
        // Clap prints only the top-level message, so causes go inline rather
        // than as anyhow context.
        let address = Pubkey::from_str(address)
            .map_err(|e| anyhow::anyhow!("invalid buffer pubkey '{address}': {e}"))?;
        let max_burn = max_burn
            .map(|m| {
                m.parse()
                    .map_err(|e| anyhow::anyhow!("invalid max burn '{m}': {e}"))
            })
            .transpose()?
            .unwrap_or(0);
        Ok(Self { address, max_burn })
    }
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
/// `program_id`, derived from `seed`.
fn decode_buffer(
    account: &Account,
    address: &Pubkey,
    program_id: &Pubkey,
    seed: &SettlementSeed,
) -> anyhow::Result<Buffer> {
    let token_program = token_program_of(account)?;
    let token_account =
        unpack_token_account(&account.data).context("account is not a token account")?;
    let mint = token_account.mint;
    anyhow::ensure!(
        seed.buffer_pda(program_id, &mint) == *address,
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
    use cow_settlement_client::cow_settlement_interface::pda::buffer::find_buffer_pda;
    use solana_sdk::transaction::Transaction;

    const PROGRAM_ID: Pubkey = Pubkey::new_from_array([1; 32]);
    const PAYER: Pubkey = Pubkey::new_from_array([3; 32]);

    /// Largest serialized transaction the network accepts for legacy transactions
    /// (`solana_packet::PACKET_DATA_SIZE`).
    const MAX_TRANSACTION_SIZE: u64 = 1232;

    fn current() -> SettlementSeed {
        SettlementSeed::parse(SettlementSeed::CURRENT_VERSION).unwrap()
    }

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
        let buffers: Vec<(Pubkey, Pubkey, u64)> = (0..len)
            .map(|_| (Pubkey::new_unique(), Pubkey::new_unique(), u64::MAX))
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
                decode_buffer(&account, &buffer, &PROGRAM_ID, &current()).unwrap(),
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
        let err = decode_buffer(&account, &other_program, &PROGRAM_ID, &current())
            .unwrap_err()
            .to_string();
        assert!(err.starts_with("not the buffer of mint"), "{err}");
    }

    #[test]
    fn decode_buffer_derives_the_buffer_from_the_given_release() {
        let old = SettlementSeed::parse("0.4").unwrap();
        let buffer = old.buffer_pda(&PROGRAM_ID, &MINT);
        let account = token_account(TokenProgram::SplToken.address(), MINT, 0);
        assert!(decode_buffer(&account, &buffer, &PROGRAM_ID, &old).is_ok());
        assert!(decode_buffer(&account, &buffer, &PROGRAM_ID, &current()).is_err());
    }

    #[test]
    fn decode_buffer_rejects_foreign_owner() {
        let buffer = find_buffer_pda(&PROGRAM_ID, &MINT).0;
        let account = token_account(Pubkey::new_unique(), MINT, 0);
        assert!(decode_buffer(&account, &buffer, &PROGRAM_ID, &current()).is_err());
    }

    #[test]
    fn decode_buffer_rejects_non_token_accounts() {
        let buffer = find_buffer_pda(&PROGRAM_ID, &MINT).0;
        let mut account = token_account(TokenProgram::SplToken.address(), MINT, 0);
        account.data.truncate(1);
        let err = decode_buffer(&account, &buffer, &PROGRAM_ID, &current())
            .unwrap_err()
            .to_string();
        assert_eq!(err, "account is not a token account");
    }

    #[test]
    fn buffer_arg_defaults_to_no_burning() {
        let arg = BufferArg::parse(&MINT.to_string()).unwrap();
        assert_eq!(
            arg,
            BufferArg {
                address: MINT,
                max_burn: 0
            }
        );
    }

    #[test]
    fn buffer_arg_reads_max_burn() {
        let arg = BufferArg::parse(&format!("{MINT}:42")).unwrap();
        assert_eq!(
            arg,
            BufferArg {
                address: MINT,
                max_burn: 42
            }
        );
    }

    #[test]
    fn buffer_arg_rejects_bad_pubkey() {
        let err = BufferArg::parse("nope:1").unwrap_err().to_string();
        assert!(err.starts_with("invalid buffer pubkey 'nope': "), "{err}");
    }

    #[test]
    fn buffer_arg_rejects_bad_max_burn() {
        for max_burn in ["", "-1", "1.5", "18446744073709551616"] {
            let err = BufferArg::parse(&format!("{MINT}:{max_burn}"))
                .unwrap_err()
                .to_string();
            assert!(
                err.starts_with(&format!("invalid max burn '{max_burn}': ")),
                "{err}"
            );
        }
    }
}
