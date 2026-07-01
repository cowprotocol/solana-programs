use settlement_client::settlement_interface::Pubkey;

use crate::Cli;

pub mod create_order;

/// Shared context threaded through every subcommand.
pub struct Context {
    pub rpc_url: String,
    pub keypair: String,
    pub program_id: Pubkey,
}

impl Context {

    pub fn from_args(cli: &Cli) -> Self {
        Self {
            rpc_url: cli.rpc_url.clone(),
            keypair: cli.keypair.clone(),
            program_id: cli.program_id.unwrap_or(settlement_client::settlement_interface::ID.into()),
        }
    }

    pub fn load_payer(&self) -> anyhow::Result<solana_sdk::signer::keypair::Keypair> {
        let path = expand_tilde(&self.keypair);
        solana_sdk::signature::read_keypair_file(&path)
            .map_err(|e| anyhow::anyhow!("failed to read keypair from {}: {e}", path.display()))
    }

    pub fn rpc(&self) -> solana_rpc_client::rpc_client::RpcClient {
        solana_rpc_client::rpc_client::RpcClient::new_with_commitment(
            self.rpc_url.clone(),
            solana_commitment_config::CommitmentConfig::confirmed(),
        )
    }
}

fn expand_tilde(path: &str) -> std::path::PathBuf {
    match path.strip_prefix("~/") {
        Some(rest) => {
            std::path::PathBuf::from(std::env::var("HOME").expect("`HOME` env not available")).join(rest)
        }
        None => std::path::PathBuf::from(path),
    }
}
