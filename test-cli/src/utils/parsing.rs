//! Parsing of command-line inputs.

use std::fmt;

use anyhow::Context as _;
use cow_settlement_client::cow_settlement_interface::{pda::order, Pubkey};
use solana_hash::Hash;

/// An order, named by its PDA or by its UID.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OrderArg {
    Pda(Pubkey),
    Uid(Hash),
}

impl OrderArg {
    /// Accept either a 64-char hex UID or a base58 pubkey (the PDA directly).
    pub fn parse(s: &str) -> anyhow::Result<Self> {
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

/// A pubkey, and the most of its buffer's balance that may be burned.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BufferArg {
    pub address: Pubkey,
    pub max_burn: u64,
}

impl BufferArg {
    /// Parses `<pubkey>` (no burning) or `<pubkey>:<max burn>`.
    pub fn parse(arg: &str) -> anyhow::Result<Self> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn order_arg_passes_through_base58_pda() {
        let pda = Pubkey::new_unique();
        assert_eq!(
            OrderArg::parse(&pda.to_string()).unwrap(),
            OrderArg::Pda(pda),
        );
    }

    #[test]
    fn order_arg_derives_pda_from_hex_uid() {
        let program_id = Pubkey::new_unique();
        let uid = Hash::new_from_array(*Pubkey::new_unique().as_array());
        let (expected, _) = order::find_order_pda(&program_id, &uid);

        // `Hash`'s `Display` is base58, so spell the hex out from the raw bytes.
        let hex: String = uid.as_ref().iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hex.len(), 64);

        let arg = OrderArg::parse(&hex).unwrap();
        assert_eq!(arg, OrderArg::Uid(uid));
        assert_eq!(arg.pda(&program_id), expected);
        assert_eq!(arg.to_string(), hex);
    }

    #[test]
    fn order_arg_rejects_input_that_is_neither() {
        // Not base58 (so not a pubkey) and not 64 chars long.
        let err = OrderArg::parse("0x1234")
            .expect_err("short non-base58 input is not a valid order reference")
            .to_string();
        assert!(err.contains("expected a base58 order PDA"), "{err}");

        // Right length, but not hex.
        let err = OrderArg::parse(&"z".repeat(64))
            .expect_err("64 non-hex chars are not a valid UID")
            .to_string();
        assert!(err.contains("invalid hex in UID"), "{err}");
    }
}
