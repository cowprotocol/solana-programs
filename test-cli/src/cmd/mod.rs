use settlement_client::settlement_interface::Pubkey;

use crate::Cli;

pub mod create_order;

/// Shared context threaded through every subcommand.
pub struct Context {
    pub payer: solana_sdk::signer::keypair::Keypair,
    pub program_id: Pubkey,
    pub rpc: solana_rpc_client::rpc_client::RpcClient,
}

impl Context {
    pub fn from_args(cli: &Cli) -> anyhow::Result<Self> {
        let payer = solana_sdk::signature::read_keypair_file(&cli.keypair)
            .map_err(|e| anyhow::anyhow!("failed to read keypair from {}: {e}", cli.keypair))?;
        Ok(Self {
            payer,
            program_id: cli
                .program_id
                .unwrap_or(settlement_client::settlement_interface::ID),
            rpc: solana_rpc_client::rpc_client::RpcClient::new_with_commitment(
                cli.rpc_url.clone(),
                solana_commitment_config::CommitmentConfig::confirmed(),
            ),
        })
    }
}
