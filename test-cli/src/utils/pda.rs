//! PDA derivation under the CLI's configurable program ID.

use cow_settlement_client::cow_settlement_interface::{
    pda::{
        buffer::BUFFER_SEED, settlement_seed_for_version, state::STATE_PDA_SEEDS,
        SETTLEMENT_SEED_LEN,
    },
    Pubkey,
};

/// Derive the settlement state PDA under `program_id`. The interface's
/// `STATE_PDA` constant only holds for the declared program ID, while the CLI
/// can target a program deployed anywhere.
pub fn find_state_pda(program_id: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&STATE_PDA_SEEDS, program_id).0
}

/// The seed every PDA of a deployment starts with. It carries the
/// `<major>.<minor>` version of the release the deployment runs, so the
/// interface's own derivations only hold for deployments of this release.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SettlementSeed([u8; SETTLEMENT_SEED_LEN]);

impl SettlementSeed {
    /// The `<major>.<minor>` release of this CLI, whose seed the interface
    /// uses.
    pub const CURRENT_VERSION: &str = concat!(
        env!("CARGO_PKG_VERSION_MAJOR"),
        ".",
        env!("CARGO_PKG_VERSION_MINOR")
    );

    /// Parses a `<major>.<minor>` release version, as passed on the command
    /// line.
    pub fn parse(version: &str) -> anyhow::Result<Self> {
        let well_formed = version.split_once('.').is_some_and(|(major, minor)| {
            [major, minor]
                .iter()
                .all(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        });
        anyhow::ensure!(
            well_formed,
            "expected a <major>.<minor> version such as 0.4, got '{version}'"
        );
        settlement_seed_for_version(version)
            .map(Self)
            .ok_or_else(|| anyhow::anyhow!("version '{version}' is too long for the seed"))
    }

    /// The settlement state PDA under `program_id`.
    pub fn state_pda(&self, program_id: &Pubkey) -> Pubkey {
        Pubkey::find_program_address(&[&self.0], program_id).0
    }

    /// The buffer PDA of `mint` under `program_id`.
    pub fn buffer_pda(&self, program_id: &Pubkey, mint: &Pubkey) -> Pubkey {
        Pubkey::find_program_address(&[&self.0, mint.as_ref(), BUFFER_SEED], program_id).0
    }
}

#[cfg(test)]
mod tests {
    use cow_settlement_client::cow_settlement_interface::{
        pda::{buffer::find_buffer_pda, SETTLEMENT_SEED},
        ID,
    };

    use super::*;

    fn current() -> SettlementSeed {
        SettlementSeed::parse(SettlementSeed::CURRENT_VERSION).unwrap()
    }

    #[test]
    fn current_version_matches_the_interface_seed() {
        assert_eq!(current().0.as_slice(), SETTLEMENT_SEED);
    }

    #[test]
    fn current_version_derives_the_interface_pdas() {
        let seed = current();
        let mint = Pubkey::new_unique();
        assert_eq!(seed.state_pda(&ID), find_state_pda(&ID));
        assert_eq!(seed.buffer_pda(&ID, &mint), find_buffer_pda(&ID, &mint).0);
    }

    #[test]
    fn other_versions_derive_other_pdas() {
        let old = SettlementSeed::parse("0.4").unwrap();
        assert_eq!(old.0.as_slice(), b"settlement v0.4    ");
        assert_ne!(old.state_pda(&ID), current().state_pda(&ID));
    }

    #[test]
    fn parse_rejects_malformed_versions() {
        for version in ["", "0", "0.", ".4", "0.4.1", "v0.4", "0.x", "123456.7"] {
            assert!(SettlementSeed::parse(version).is_err(), "{version}");
        }
    }
}
