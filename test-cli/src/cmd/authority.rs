use anyhow::Context as _;
use clap::{Args as ClapArgs, Parser, Subcommand, ValueEnum};
use cow_settlement_client::{
    cow_settlement_interface::{pda::state::find_state_pda, Pubkey, Role},
    instruction::TransferAuthority,
};
use solana_sdk::{
    signature::{read_keypair_file, Signer},
    transaction::Transaction,
};

use crate::utils::output::print_summary;

use super::Context;

#[derive(Parser)]
pub struct AuthorityArgs {
    #[command(subcommand)]
    command: AuthorityCommand,
}

#[derive(Subcommand)]
enum AuthorityCommand {
    #[command(about = "Transfer a role to a new authority")]
    Transfer(TransferArgs),
}

/// Defines `RoleArg`, a `clap`-parseable copy of the interface's [`Role`].
macro_rules! role_arg {
    ($($variant:ident),+ $(,)?) => {
        #[derive(Clone, Copy, ValueEnum)]
        enum RoleArg {
            $($variant),+
        }

        impl From<RoleArg> for Role {
            fn from(role: RoleArg) -> Self {
                match role {
                    $(RoleArg::$variant => Role::$variant),+
                }
            }
        }

        impl From<Role> for RoleArg {
            fn from(role: Role) -> Self {
                match role {
                    $(Role::$variant => RoleArg::$variant),+
                }
            }
        }
    };
}

role_arg!(Manager, ReclaimAuthority, SelfOrderAuthority);

#[derive(ClapArgs)]
pub struct TransferArgs {
    /// Role to reassign
    role: RoleArg,

    /// Address that becomes the role's new holder
    new_authority: Pubkey,

    /// Path to the keypair authorizing the transfer, which must be the manager
    /// or the role's current holder and must sign it (defaults to the payer
    /// keypair)
    #[arg(long)]
    signer: Option<String>,
}

pub fn run(ctx: Context, args: AuthorityArgs) -> anyhow::Result<()> {
    match args.command {
        AuthorityCommand::Transfer(args) => transfer(ctx, args),
    }
}

fn transfer(ctx: Context, args: TransferArgs) -> anyhow::Result<()> {
    let payer = ctx.payer.pubkey();
    // Without `--signer` the payer authorizes the transfer, so reuse the
    // keypair the context already holds instead of reading a file again.
    let from_file = args
        .signer
        .map(|path| {
            read_keypair_file(&path)
                .map_err(|e| anyhow::anyhow!("failed to read signer keypair from {path}: {e}"))
        })
        .transpose()?;
    let signer = from_file.as_ref().unwrap_or(&ctx.payer);
    let signer_pubkey = signer.pubkey();
    let role = Role::from(args.role);

    let ix = TransferAuthority {
        program_id: ctx.program_id,
        signer: signer_pubkey,
        role,
        new_authority: args.new_authority,
    };

    let blockhash = ctx
        .rpc
        .get_latest_blockhash()
        .context("failed to fetch blockhash")?;
    let tx = Transaction::new_signed_with_payer(
        &[ix.into()],
        Some(&payer),
        &[&ctx.payer, signer],
        blockhash,
    );
    let sig = ctx
        .rpc
        .send_and_confirm_transaction(&tx)
        .context("transaction failed")?;

    let (state_pda, _) = find_state_pda(&ctx.program_id);
    print_summary(&[
        ("signature", &sig),
        ("role", &format!("{role:?}")),
        ("newAuthority", &args.new_authority),
        ("signer", &signer_pubkey),
        ("statePda", &state_pda),
    ]);

    Ok(())
}
