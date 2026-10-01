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
    /// The existing transaction to approve rather than proposing a new one.
    pub transaction_index: Option<u64>,
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
                    transaction_index: cli.squads_transaction_index,
                })
            }
            None => {
                // Checked here rather than with clap's `requires`, which
                // misses a global argument given before the subcommand.
                anyhow::ensure!(
                    cli.squads_vault_index.is_none() && cli.squads_transaction_index.is_none(),
                    "--squads-vault-index and --squads-transaction-index require --squads-multisig"
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

    /// The keypair at `path` to sign as some role, defaulting to [`Self::payer`].
    /// A Squads vault can't co-sign with other keypairs, so `path` is rejected
    /// when proposing.
    pub fn signer_or(&self, path: Option<String>) -> anyhow::Result<TxSigner<'_>> {
        match (&self.squads, path) {
            (Some(squads), None) => Ok(TxSigner::Vault(squads.vault)),
            (Some(_), Some(path)) => anyhow::bail!(
                "can't sign with {path} when proposing to a Squads multisig: the vault signs instead"
            ),
            (None, path) => read_keypair_or(path, &self.signer).map(TxSigner::Keypair),
        }
    }

    /// Send `ixs`, signed by the local keypair and `signers`, or propose them
    /// to the Squads vault, or approve the existing Squads transaction holding
    /// them.
    pub fn submit(&self, ixs: &[Instruction], signers: &[&TxSigner]) -> anyhow::Result<Submission> {
        let Some(squads) = &self.squads else {
            let mut keypairs: Vec<&dyn Signer> = vec![&self.signer];
            keypairs.extend(signers.iter().filter_map(|signer| match signer {
                TxSigner::Keypair(keypair) => Some(&**keypair as &dyn Signer),
                TxSigner::Vault(_) => None,
            }));
            return self.send(ixs, &keypairs).map(Submission::Sent);
        };

        // Fetched again rather than kept from `from_args`, since its
        // transaction indexes must be current.
        let multisig = Multisig::fetch(&self.rpc, &squads.multisig)?;
        let member = self.signer.pubkey();
        let proposal = match squads.transaction_index {
            Some(index) => multisig.approve(
                &member,
                squads.vault_index,
                ixs,
                &multisig.fetch_transaction(&self.rpc, index)?,
            )?,
            None => multisig.propose(&member, squads.vault_index, ixs)?,
        };
        let signature = self.send(&proposal.instructions, &[&self.signer])?;
        Ok(Submission::Proposed(Proposed {
            signature,
            multisig: squads.multisig,
            vault: squads.vault,
            transaction_index: proposal.transaction_index,
            transaction: proposal.transaction,
            proposal: proposal.proposal,
            created: proposal.created,
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
    /// Whether the vault transaction is new, rather than an existing one the
    /// local keypair approved.
    pub created: bool,
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
            ("squadsCreated", &self.created),
            ("squadsApproved", &self.approved),
        ]
    }
}
