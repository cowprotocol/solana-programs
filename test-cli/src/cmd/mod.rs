use settlement_client::settlement_interface::Pubkey;
use solana_commitment_config::CommitmentConfig;
use solana_rpc_client::rpc_client::RpcClient;
use solana_sdk::signature::read_keypair_file;
use solana_sdk::signer::keypair::Keypair;

use crate::Cli;

pub mod create_order;
pub mod settle;

/// Shared context threaded through every subcommand.
pub struct Context {
    pub payer: Keypair,
    pub program_id: Pubkey,
    pub rpc: RpcClient,
}

impl Context {
    pub fn from_args(cli: &Cli) -> anyhow::Result<Self> {
        let payer = read_keypair_file(&cli.keypair)
            .map_err(|e| anyhow::anyhow!("failed to read keypair from {}: {e}", cli.keypair))?;
        Ok(Self {
            payer,
            program_id: cli.program_id,
            rpc: RpcClient::new_with_commitment(cli.rpc_url.clone(), CommitmentConfig::confirmed()),
        })
    }
}
