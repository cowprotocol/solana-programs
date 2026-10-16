//! Parsing of command-line inputs.

use anyhow::Context as _;
use cow_settlement_client::cow_settlement_interface::{pda::order, Pubkey};
use solana_hash::Hash;

/// Accept either a 64-char hex UID or a base58 pubkey (the PDA directly).
pub fn parse_order_input(program_id: &Pubkey, s: &str) -> anyhow::Result<Pubkey> {
    if let Ok(pubkey) = s.parse::<Pubkey>() {
        return Ok(pubkey);
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
    let uid = Hash::new_from_array(bytes);
    let (pda, _) = order::find_order_pda(program_id, &uid);
    Ok(pda)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_order_input_passes_through_base58_pda() {
        let pda = Pubkey::new_unique();
        assert_eq!(
            parse_order_input(&Pubkey::new_unique(), &pda.to_string()).unwrap(),
            pda,
        );
    }

    #[test]
    fn parse_order_input_derives_pda_from_hex_uid() {
        let program_id = Pubkey::new_unique();
        let uid = Hash::new_from_array(*Pubkey::new_unique().as_array());
        let (expected, _) = order::find_order_pda(&program_id, &uid);

        // `Hash`'s `Display` is base58, so spell the hex out from the raw bytes.
        let hex: String = uid.as_ref().iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hex.len(), 64);

        assert_eq!(parse_order_input(&program_id, &hex).unwrap(), expected);
    }

    #[test]
    fn parse_order_input_rejects_input_that_is_neither() {
        // Not base58 (so not a pubkey) and not 64 chars long.
        let err = parse_order_input(&Pubkey::new_unique(), "0x1234")
            .expect_err("short non-base58 input is not a valid order reference")
            .to_string();
        assert!(err.contains("expected a base58 order PDA"), "{err}");

        // Right length, but not hex.
        let err = parse_order_input(&Pubkey::new_unique(), &"z".repeat(64))
            .expect_err("64 non-hex chars are not a valid UID")
            .to_string();
        assert!(err.contains("invalid hex in UID"), "{err}");
    }
}
