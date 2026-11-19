//! Reading signer keypairs from CLI arguments.

use std::ops::Deref;

use solana_sdk::signer::keypair::Keypair;

use super::parsing::KeypairArg;

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

/// The keypair `arg` names, or `default` borrowed when no `arg` is given.
pub fn read_keypair_or(
    arg: Option<KeypairArg>,
    default: &Keypair,
) -> anyhow::Result<MaybeKeypair<'_>> {
    match arg {
        Some(arg) => arg.signer().map(MaybeKeypair::Owned),
        None => Ok(MaybeKeypair::Borrowed(default)),
    }
}
