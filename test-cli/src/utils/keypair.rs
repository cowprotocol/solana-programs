//! Reading signer keypairs from CLI arguments.

use std::ops::Deref;

use solana_sdk::signature::read_keypair_file;
use solana_sdk::signer::keypair::Keypair;

/// A keypair the caller either owns or borrows.
///
/// This wouldn't be needed if we could just `.clone()` a keypair ref, however
/// this is explicitly discouraged to avoid keeping key material in memory.
#[allow(clippy::large_enum_variant)]
pub enum MaybeKeypair<'a> {
    Owned(Keypair),
    Borrowed(&'a Keypair),
}

impl Deref for MaybeKeypair<'_> {
    type Target = Keypair;

    fn deref(&self) -> &Keypair {
        match self {
            MaybeKeypair::Owned(keypair) => keypair,
            MaybeKeypair::Borrowed(keypair) => keypair,
        }
    }
}

/// The keypair at `path`, or `default` borrowed when no `path` is given.
pub fn read_keypair_or(
    path: Option<String>,
    default: &Keypair,
) -> anyhow::Result<MaybeKeypair<'_>> {
    match path {
        Some(path) => read_keypair_file(&path)
            .map(MaybeKeypair::Owned)
            .map_err(|e| anyhow::anyhow!("failed to read keypair from {path}: {e}")),
        None => Ok(MaybeKeypair::Borrowed(default)),
    }
}
