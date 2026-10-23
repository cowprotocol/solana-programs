//! Parsing of command-line inputs.

use std::ffi::OsString;
use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;

use anyhow::Context as _;
use clap::ValueEnum;
use cow_settlement_client::cow_settlement_interface::{pda::order, Pubkey, BARN_ID, PROD_ID};
use solana_hash::Hash;
use solana_sdk::signature::{read_keypair_file, Keypair};

/// Spelled as its lowercase clap name (`prod` / `barn`), so it round-trips
/// through [`ProgramIdArg`]'s [`FromStr`].
impl fmt::Display for Env {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.to_possible_value()
            .expect("no env is skipped")
            .get_name()
            .fmt(f)
    }
}

/// The settlement program to target, named either by a known [`Env`] or by a
/// raw program address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProgramIdArg {
    Env(Env),
    Address(Pubkey),
}

/// Accept a known environment name (`prod` / `barn`, case-insensitive) or a
/// base58 program address.
impl FromStr for ProgramIdArg {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> anyhow::Result<Self> {
        if let Ok(env) = Env::from_str(s, true) {
            return Ok(Self::Env(env));
        }
        if let Ok(pubkey) = s.parse::<Pubkey>() {
            return Ok(Self::Address(pubkey));
        }
        anyhow::bail!(
            "expected a settlement environment ({}) or a base58 program address, got '{s}'",
            Env::value_variants()
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("/"),
        )
    }
}

impl ProgramIdArg {
    /// The resolved program address.
    pub fn pubkey(self) -> Pubkey {
        match self {
            Self::Env(env) => env.program_id(),
            Self::Address(address) => address,
        }
    }
}

/// Spelled as parsed: the environment name for an [`Env`], base58 for an address.
impl fmt::Display for ProgramIdArg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Env(env) => env.fmt(f),
            Self::Address(address) => address.fmt(f),
        }
    }
}

/// An order, named by its PDA or by its UID.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OrderArg {
    Pda(Pubkey),
    Uid(Hash),
}

/// Accept either a 64-char hex UID or a base58 pubkey (the PDA directly).
impl FromStr for OrderArg {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> anyhow::Result<Self> {
        if let Ok(pubkey) = s.parse::<Pubkey>() {
            return Ok(Self::Pda(pubkey));
        }
        anyhow::ensure!(
            s.len() == 64,
            "expected a base58 order PDA or a 64-char hex UID, got '{s}'"
        );

        // TODO: after a bit of research, this appears to be the most recommended way in std + solana_hash to
        // convert a string into a hash. We might want to move this into a proper function later.
        let mut bytes = [0u8; 32];
        for (i, piece) in s.as_bytes().chunks(2).enumerate() {
            bytes[i] = u8::from_str_radix(
                std::str::from_utf8(piece).context("Should return to utf8 string")?,
                16,
            )
            .with_context(|| format!("invalid hex in UID '{s}' at byte {i}"))?;
        }
        Ok(Self::Uid(Hash::new_from_array(bytes)))
    }
}

impl OrderArg {
    /// The order's PDA under `program_id`.
    pub fn pda(&self, program_id: &Pubkey) -> Pubkey {
        match self {
            Self::Pda(pda) => *pda,
            Self::Uid(uid) => order::find_order_pda(program_id, uid).0,
        }
    }
}

/// Spelled as parsed: base58 for a PDA, lowercase hex for a UID.
impl fmt::Display for OrderArg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pda(pda) => pda.fmt(f),
            Self::Uid(uid) => uid.as_ref().iter().try_for_each(|b| write!(f, "{b:02x}")),
        }
    }
}

/// A named settlement deployment. Each environment resolves to a well-known
/// program address, so `prod` / `barn` can stand in for the full program ID.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Env {
    /// The production deployment ([`PROD_ID`]).
    Prod,
    /// The staging ("barn") deployment ([`BARN_ID`]).
    Barn,
}

impl Env {
    /// The settlement program ID this environment is deployed at.
    pub fn program_id(self) -> Pubkey {
        match self {
            Self::Prod => PROD_ID,
            Self::Barn => BARN_ID,
        }
    }
}

/// A pubkey, and the most of its buffer's balance that may be burned.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BufferArg {
    pub address: Pubkey,
    pub max_burn: u64,
}

/// Parses `<pubkey>` (no burning) or `<pubkey>:<max burn in atoms>`.
impl FromStr for BufferArg {
    type Err = anyhow::Error;

    fn from_str(arg: &str) -> anyhow::Result<Self> {
        let (address, max_burn) = match arg.split_once(':') {
            Some((address, max_burn)) => (address, Some(max_burn)),
            None => (arg, None),
        };
        // Clap prints only the top-level message, so causes go inline rather
        // than as anyhow context.
        let address = address
            .parse::<Pubkey>()
            .map_err(|e| anyhow::anyhow!("invalid pubkey '{address}': {e}"))?;
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

/// The path of a keypair file, for a key that must sign.
///
/// The file is read on use rather than when parsing, as clap needs parsed
/// values to be `Clone` and a `Keypair` deliberately isn't.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeypairArg(PathBuf);

impl From<OsString> for KeypairArg {
    fn from(s: OsString) -> Self {
        Self(s.into())
    }
}

impl KeypairArg {
    /// Reads the keypair from the file.
    pub fn signer(&self) -> anyhow::Result<Keypair> {
        read_keypair_file(&self.0)
            .map_err(|e| anyhow::anyhow!("failed to read keypair from {}: {e}", self.0.display()))
    }
}

#[cfg(test)]
mod tests {
    use ::proptest::prelude::*;

    use super::*;
    use solana_sdk::signature::Signer;

    #[test]
    fn program_id_parses_env_names_case_insensitively() {
        for (input, env) in [
            ("prod", Env::Prod),
            ("PROD", Env::Prod),
            ("barn", Env::Barn),
            ("Barn", Env::Barn),
        ] {
            assert_eq!(
                ProgramIdArg::from_str(input).unwrap(),
                ProgramIdArg::Env(env)
            );
        }
    }

    #[test]
    fn program_id_resolves_envs_to_the_known_deployment_ids() {
        assert_eq!(ProgramIdArg::Env(Env::Prod).pubkey(), PROD_ID);
        assert_eq!(ProgramIdArg::Env(Env::Barn).pubkey(), BARN_ID);
    }

    #[test]
    fn program_id_rejects_input_that_is_neither() {
        let err = ProgramIdArg::from_str("not-an-env-or-address")
            .expect_err("a word that is neither an env nor a base58 address")
            .to_string();
        assert!(err.contains("expected a settlement environment"), "{err}");
    }

    proptest! {
        #[test]
        fn program_id_passes_through_base58_address(address in any::<[u8; 32]>()) {
            let address = Pubkey::new_from_array(address);
            let parsed = ProgramIdArg::from_str(&address.to_string()).unwrap();
            prop_assert_eq!(parsed, ProgramIdArg::Address(address));
            prop_assert_eq!(parsed.pubkey(), address);
        }
    }

    #[test]
    fn program_id_round_trips_through_display() {
        for program_id in [
            ProgramIdArg::Env(Env::Prod),
            ProgramIdArg::Env(Env::Barn),
            ProgramIdArg::Address(Pubkey::new_from_array([7; 32])),
        ] {
            assert_eq!(
                ProgramIdArg::from_str(&program_id.to_string()).unwrap(),
                program_id
            );
        }
    }

    proptest! {
        #[test]
        fn order_arg_passes_through_base58_pda(pda in any::<[u8; 32]>()) {
            let pda = Pubkey::new_from_array(pda);
            prop_assert_eq!(OrderArg::from_str(&pda.to_string()).unwrap(), OrderArg::Pda(pda));
        }

        #[test]
        fn order_arg_derives_pda_from_hex_uid(
            program_id in any::<[u8; 32]>(),
            uid in any::<[u8; 32]>(),
        ) {
            let program_id = Pubkey::new_from_array(program_id);
            let uid = Hash::new_from_array(uid);
            let (expected, _) = order::find_order_pda(&program_id, &uid);

            // `Hash`'s `Display` is base58, so spell the hex out from the raw bytes.
            let hex: String = uid.as_ref().iter().map(|b| format!("{b:02x}")).collect();
            prop_assert_eq!(hex.len(), 64);

            let arg = OrderArg::from_str(&hex).unwrap();
            prop_assert_eq!(arg, OrderArg::Uid(uid));
            prop_assert_eq!(arg.pda(&program_id), expected);
            prop_assert_eq!(arg.to_string(), hex);
        }
    }

    #[test]
    fn order_arg_rejects_input_that_is_neither() {
        // Not base58 (so not a pubkey) and not 64 chars long.
        let err = OrderArg::from_str("0x1234")
            .expect_err("short non-base58 input is not a valid order reference")
            .to_string();
        assert!(err.contains("expected a base58 order PDA"), "{err}");

        // Right length, but not hex.
        let err = OrderArg::from_str(&"z".repeat(64))
            .expect_err("64 non-hex chars are not a valid UID")
            .to_string();
        assert!(err.contains("invalid hex in UID"), "{err}");
    }

    #[test]
    fn keypair_arg_reads_signer_from_file() {
        let keypair = Keypair::new();
        let path = std::env::temp_dir().join(format!("keypair-arg-{}.json", keypair.pubkey()));
        solana_sdk::signature::write_keypair_file(&keypair, &path).unwrap();

        let arg = KeypairArg::from(path.clone().into_os_string());
        assert_eq!(arg.signer().unwrap().pubkey(), keypair.pubkey());
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn keypair_arg_reports_unreadable_file() {
        let arg = KeypairArg::from(OsString::from("/nonexistent/keypair.json"));
        let err = arg
            .signer()
            .expect_err("a missing file has no keypair")
            .to_string();
        assert!(
            err.contains("failed to read keypair from /nonexistent/keypair.json"),
            "{err}"
        );
    }
}
