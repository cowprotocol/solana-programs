use std::num::NonZero;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

use clap::Args;
use cow_settlement_client::cow_settlement_interface::Pubkey;
use solana_sdk::signature::{write_keypair_file, Signer};
use solana_sdk::signer::keypair::Keypair;

use crate::utils::output::print_summary;
use crate::utils::squads::{multisig_address, vault_address};

const BASE58_ALPHABET: &str = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
const PROGRESS_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Args)]
pub struct GrindMultisigArgs {
    /// Characters the address must start with
    #[arg(long)]
    starts_with: Option<String>,

    /// Characters the address must end with
    #[arg(long)]
    ends_with: Option<String>,

    /// Match regardless of case
    #[arg(long)]
    ignore_case: bool,

    /// Match the multisig address rather than its vault's
    #[arg(long)]
    multisig: bool,

    /// Index of the vault to match
    #[arg(long, default_value_t = 0, conflicts_with = "multisig")]
    vault_index: u8,

    /// Where to write the `create_key` keypair (defaults to
    /// `<create key>.json`)
    #[arg(long)]
    outfile: Option<PathBuf>,
}

pub fn run(args: GrindMultisigArgs) -> anyhow::Result<()> {
    let pattern = Pattern::new(
        args.starts_with.unwrap_or_default(),
        args.ends_with.unwrap_or_default(),
        args.ignore_case,
    )?;
    let target = if args.multisig {
        Target::Multisig
    } else {
        Target::Vault(args.vault_index)
    };

    let start = Instant::now();
    let (create_key, attempts) = grind(&pattern, target, |attempts| {
        let rate = attempts as f64 / start.elapsed().as_secs_f64();
        eprintln!("{attempts} attempts ({rate:.0}/s)");
    });

    let outfile = args
        .outfile
        .unwrap_or_else(|| format!("{}.json", create_key.pubkey()).into());
    anyhow::ensure!(
        !outfile.exists(),
        "{} already exists, refusing to overwrite it",
        outfile.display()
    );
    write_keypair_file(&create_key, &outfile)
        .map_err(|e| anyhow::anyhow!("failed to write {}: {e}", outfile.display()))?;

    let multisig = multisig_address(&create_key.pubkey());
    print_summary(&[
        ("createKey", &create_key.pubkey()),
        ("createKeyFile", &outfile.display()),
        ("squadsMultisig", &multisig),
        ("squadsVault", &vault_address(&multisig, args.vault_index)),
        ("squadsVaultIndex", &args.vault_index),
        ("attempts", &attempts),
    ]);
    Ok(())
}

/// The address being ground.
#[derive(Clone, Copy)]
enum Target {
    Multisig,
    Vault(u8),
}

impl Target {
    fn address(self, create_key: &Pubkey) -> Pubkey {
        let multisig = multisig_address(create_key);
        match self {
            Target::Multisig => multisig,
            Target::Vault(index) => vault_address(&multisig, index),
        }
    }
}

struct Pattern {
    starts_with: String,
    ends_with: String,
    ignore_case: bool,
}

impl Pattern {
    fn new(starts_with: String, ends_with: String, ignore_case: bool) -> anyhow::Result<Self> {
        anyhow::ensure!(
            !starts_with.is_empty() || !ends_with.is_empty(),
            "give --starts-with, --ends-with or both"
        );
        for c in starts_with.chars().chain(ends_with.chars()) {
            let matchable = |c: char| BASE58_ALPHABET.contains(c);
            anyhow::ensure!(
                matchable(c)
                    || ignore_case
                        && (matchable(c.to_ascii_lowercase()) || matchable(c.to_ascii_uppercase())),
                "'{c}' never appears in a base58 address"
            );
        }
        let normalize = |s: String| {
            if ignore_case {
                s.to_ascii_lowercase()
            } else {
                s
            }
        };
        Ok(Self {
            starts_with: normalize(starts_with),
            ends_with: normalize(ends_with),
            ignore_case,
        })
    }

    fn matches(&self, address: &Pubkey) -> bool {
        let mut address = address.to_string();
        if self.ignore_case {
            address.make_ascii_lowercase();
        }
        address.starts_with(&self.starts_with) && address.ends_with(&self.ends_with)
    }
}

/// A `create_key` whose `target` address matches `pattern`, and how many keys
/// were tried to find it. Grinds on every core, calling `on_progress` with
/// the attempts so far every [`PROGRESS_INTERVAL`].
fn grind(pattern: &Pattern, target: Target, on_progress: impl Fn(u64)) -> (Keypair, u64) {
    let attempts = AtomicU64::new(0);
    let found = AtomicBool::new(false);
    let threads = std::thread::available_parallelism().map_or(1, NonZero::get);

    let create_key = std::thread::scope(|scope| {
        let (tx, rx) = mpsc::channel();
        for _ in 0..threads {
            let tx = tx.clone();
            let (attempts, found) = (&attempts, &found);
            scope.spawn(move || {
                while !found.load(Ordering::Relaxed) {
                    let create_key = Keypair::new();
                    attempts.fetch_add(1, Ordering::Relaxed);
                    if pattern.matches(&target.address(&create_key.pubkey())) {
                        found.store(true, Ordering::Relaxed);
                        // Only the first match is received; the rest are
                        // dropped with the channel.
                        let _ = tx.send(create_key);
                    }
                }
            });
        }
        loop {
            match rx.recv_timeout(PROGRESS_INTERVAL) {
                Ok(create_key) => break create_key,
                Err(RecvTimeoutError::Timeout) => on_progress(attempts.load(Ordering::Relaxed)),
                Err(RecvTimeoutError::Disconnected) => {
                    unreachable!("a sender is held until a key is received")
                }
            }
        }
    });
    (create_key, attempts.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pattern(starts_with: &str, ends_with: &str, ignore_case: bool) -> anyhow::Result<Pattern> {
        Pattern::new(starts_with.into(), ends_with.into(), ignore_case)
    }

    #[test]
    fn requires_a_pattern() {
        let err = pattern("", "", false).err().unwrap().to_string();
        assert!(err.contains("--starts-with"), "{err}");
    }

    #[test]
    fn rejects_characters_outside_base58() {
        for c in ["0", "O", "I", "l", "_"] {
            let err = pattern(c, "", false).err().unwrap().to_string();
            assert!(err.contains("never appears"), "{c}: {err}");
        }
        // Each has a case variant in the alphabet, apart from `0` and `_`.
        for c in ["O", "I", "l"] {
            assert!(pattern("", c, true).is_ok(), "{c}");
        }
        for c in ["0", "_"] {
            assert!(pattern("", c, true).is_err(), "{c}");
        }
    }

    #[test]
    fn matches_start_and_end() {
        let address: Pubkey = "2hEa7H3w53crQTC2yrmr9gjCRrzAUioVAMpf7rhvog4E"
            .parse()
            .unwrap();

        assert!(pattern("2hEa", "og4E", false).unwrap().matches(&address));
        assert!(pattern("2hEa", "", false).unwrap().matches(&address));
        assert!(pattern("", "og4E", false).unwrap().matches(&address));
        assert!(!pattern("og4E", "2hEa", false).unwrap().matches(&address));
        assert!(!pattern("2HEA", "", false).unwrap().matches(&address));
        assert!(pattern("2HEA", "OG4e", true).unwrap().matches(&address));
    }

    #[test]
    fn grinds_matching_vault() {
        // Suffixes rather than prefixes, since the first character of a
        // base58 address isn't uniformly distributed.
        let pattern = pattern("", "a", false).unwrap();
        let (create_key, attempts) = grind(&pattern, Target::Vault(3), |_| {});
        let vault = vault_address(&multisig_address(&create_key.pubkey()), 3);
        assert!(vault.to_string().ends_with('a'), "{vault}");
        assert!(attempts >= 1);
    }

    #[test]
    fn grinds_matching_multisig() {
        let pattern = pattern("", "B", false).unwrap();
        let (create_key, _) = grind(&pattern, Target::Multisig, |_| {});
        let multisig = multisig_address(&create_key.pubkey());
        assert!(multisig.to_string().ends_with('B'), "{multisig}");
    }
}
