use anyhow::Context as _;
use cow_settlement_client::cow_settlement_interface::Pubkey;
use solana_commitment_config::CommitmentConfig;
use solana_instruction::Instruction;
use solana_rpc_client::api::{
    config::RpcSimulateTransactionConfig, response::RpcSimulateTransactionResult,
};
use solana_rpc_client::rpc_client::RpcClient;
use solana_sdk::message::Message;
use solana_sdk::signature::{read_keypair_file, Signature, Signer};
use solana_sdk::signer::keypair::Keypair;
use solana_sdk::transaction::Transaction;

use crate::utils::keypair::{read_keypair_or, MaybeKeypair};
use crate::utils::squads::Multisig;
use crate::Cli;

pub mod authority;
pub mod create_order;
pub mod initialize;
pub mod settle;
pub mod solver;

/// Shared context threaded through every subcommand.
pub struct Context {
    /// The local keypair, which signs and pays for every transaction the CLI
    /// sends.
    pub signer: Keypair,
    pub program_id: Pubkey,
    pub rpc: RpcClient,
    /// The Squads vault that transactions are proposed to instead of sent, if
    /// any.
    pub squads: Option<SquadsVault>,
}

pub struct SquadsVault {
    pub multisig: Pubkey,
    pub vault_index: u8,
    pub vault: Pubkey,
}

impl Context {
    pub fn from_args(cli: &Cli) -> anyhow::Result<Self> {
        let signer = read_keypair_file(&cli.keypair)
            .map_err(|e| anyhow::anyhow!("failed to read keypair from {}: {e}", cli.keypair))?;
        let rpc =
            RpcClient::new_with_commitment(cli.rpc_url.clone(), CommitmentConfig::confirmed());
        let squads = match cli.squads_multisig {
            Some(multisig) => {
                let vault_index = cli.squads_vault_index.unwrap_or(0);
                Some(SquadsVault {
                    multisig,
                    vault_index,
                    vault: Multisig::fetch(&rpc, &multisig)?.vault(vault_index),
                })
            }
            None => {
                // Checked here rather than with clap's `requires`, which
                // misses a global argument given before the subcommand.
                anyhow::ensure!(
                    cli.squads_vault_index.is_none(),
                    "--squads-vault-index requires --squads-multisig"
                );
                None
            }
        };
        Ok(Self {
            signer,
            program_id: cli.program_id,
            rpc,
            squads,
        })
    }

    /// The account that pays for and signs the generated instructions: the
    /// Squads vault when proposing, the local keypair otherwise.
    pub fn payer(&self) -> Pubkey {
        match &self.squads {
            Some(squads) => squads.vault,
            None => self.signer.pubkey(),
        }
    }

    /// Retrieves the signer at the specified path
    ///
    /// Returns an error if the signer configuration is not supported (ex. squads does not support co-signing.)
    pub fn signer_or(&self, path: Option<String>) -> anyhow::Result<TxSigner<'_>> {
        match (&self.squads, path) {
            (Some(squads), None) => Ok(TxSigner::Vault(squads.vault)),
            (Some(_), Some(path)) => anyhow::bail!(
                "{path} was specified to sign, but it can't be used with --multisig-address as co-signing is not possible with multisigs."
            ),
            (None, path) => read_keypair_or(path, &self.signer).map(TxSigner::Keypair),
        }
    }

    /// Sends a transaction using the configured wallet, or proposes it to the
    /// configured multisig.
    pub fn submit(&self, ixs: &[Instruction], signers: &[&TxSigner]) -> anyhow::Result<Submission> {
        // Are we signing the transaction with a local EOA file or multisig?
        // If its an EOA (aka, NOT a multisig), then send the transaction and break off early.
        let Some(squads) = &self.squads else {
            let mut keypairs: Vec<&dyn Signer> = vec![&self.signer];
            keypairs.extend(signers.iter().filter_map(|signer| match signer {
                TxSigner::Keypair(keypair) => Some(&**keypair as &dyn Signer),
                TxSigner::Vault(_) => None,
            }));
            return self.send(ixs, &keypairs).map(Submission::Sent);
        };

        // Fetched again rather than kept from `from_args`, since its
        // transaction index must be current.
        let multisig = Multisig::fetch(&self.rpc, &squads.multisig)?;
        let proposal = multisig.propose(&self.signer.pubkey(), squads.vault_index, ixs)?;
        let signature = self.send(&proposal.instructions, &[&self.signer])?;
        Ok(Submission::Proposed(Proposed {
            signature,
            multisig: squads.multisig,
            vault: squads.vault,
            transaction_index: proposal.transaction_index,
            transaction: proposal.transaction,
            proposal: proposal.proposal,
            approved: proposal.approved,
        }))
    }

    fn send(&self, ixs: &[Instruction], signers: &[&dyn Signer]) -> anyhow::Result<Signature> {
        let blockhash = self
            .rpc
            .get_latest_blockhash()
            .context("failed to fetch blockhash")?;
        let tx = Transaction::new_signed_with_payer(
            ixs,
            Some(&self.signer.pubkey()),
            signers,
            blockhash,
        );
        self.rpc
            .send_and_confirm_transaction(&tx)
            .context("transaction failed")
    }

    /// Simulate `ixs` as paid for by [`Self::payer`]. Signatures aren't
    /// verified, so this also covers a Squads vault that can only sign once
    /// the proposal executes.
    pub fn simulate(&self, ixs: &[Instruction]) -> anyhow::Result<RpcSimulateTransactionResult> {
        let tx = Transaction::new_unsigned(Message::new(ixs, Some(&self.payer())));
        Ok(self
            .rpc
            .simulate_transaction_with_config(
                &tx,
                RpcSimulateTransactionConfig {
                    sig_verify: false,
                    replace_recent_blockhash: true,
                    ..Default::default()
                },
            )?
            .value)
    }
}

/// A signer of the generated instructions.
pub enum TxSigner<'a> {
    Keypair(MaybeKeypair<'a>),
    /// The Squads vault, which signs when the proposal executes.
    Vault(Pubkey),
}

impl TxSigner<'_> {
    pub fn pubkey(&self) -> Pubkey {
        match self {
            TxSigner::Keypair(keypair) => keypair.pubkey(),
            TxSigner::Vault(vault) => *vault,
        }
    }
}

/// What [`Context::submit`] did with the instructions.
pub enum Submission {
    Sent(Signature),
    Proposed(Proposed),
}

/// Instructions proposed to a Squads vault.
pub struct Proposed {
    /// The transaction creating the proposal.
    pub signature: Signature,
    pub multisig: Pubkey,
    pub vault: Pubkey,
    pub transaction_index: u64,
    /// The vault transaction account holding the proposed instructions.
    pub transaction: Pubkey,
    pub proposal: Pubkey,
    /// Whether the local keypair approved the proposal.
    pub approved: bool,
}

impl Submission {
    /// The rows describing the submission, followed by `rows`, for a
    /// [`print_summary`](crate::utils::output::print_summary).
    pub fn summary<'a>(
        &'a self,
        rows: &[(&'a str, &'a dyn ToString)],
    ) -> Vec<(&'a str, &'a dyn ToString)> {
        let mut summary = match self {
            Submission::Sent(signature) => vec![("signature", signature as &dyn ToString)],
            Submission::Proposed(proposed) => proposed.summary(),
        };
        summary.extend_from_slice(rows);
        summary
    }
}

impl Proposed {
    pub fn summary(&self) -> Vec<(&'static str, &dyn ToString)> {
        vec![
            ("signature", &self.signature),
            ("squadsMultisig", &self.multisig),
            ("squadsVault", &self.vault),
            ("squadsTransactionIndex", &self.transaction_index),
            ("squadsTransaction", &self.transaction),
            ("squadsProposal", &self.proposal),
            ("squadsApproved", &self.approved),
        ]
    }
}
