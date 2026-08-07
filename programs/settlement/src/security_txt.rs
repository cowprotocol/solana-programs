//! Machine-readable security contact information embedded in the program
//! binary, following the `security.txt` standard for Solana programs:
//! <https://github.com/neodyme-labs/solana-security-txt>

/// Copied from `solana-security-txt` v1.1.3 (dual-licensed MIT/Apache-2.0),
/// with the `link_section` gate corrected below. The upstream crate is a single
/// `macro_rules!` and no runtime code, so inlining the definition buys the same
/// output without adding a dependency to a program we compile on-chain.
macro_rules! security_txt {
    ($($name:ident: $value:expr),*) => {
        #[cfg_attr(target_os = "solana", link_section = ".security.txt")]
        #[allow(
            dead_code,
            reason = "read out of the compiled binary, never from Rust"
        )]
        #[no_mangle]
        pub static SECURITY_TXT: &str = concat! {
            "=======BEGIN SECURITY.TXT V1=======\0",
            $(stringify!($name), "\0", $value, "\0",)*
            "=======END SECURITY.TXT V1=======\0"
        };
    };
}

// Deliberately omitted:
//
// - `source_revision` / `source_release`, which upstream suggests populating
//   from the build environment. Doing so would make the binary depend on
//   environment variables and break the byte-for-byte reproducibility that
//   `just build-verified` establishes. `solana-verify` already pins the
//   deployed binary to a commit.
// - `expiry`, which would silently invalidate the notice on a date nothing in
//   this repository tracks.
security_txt! {
    // Required fields.
    name: "CoW Protocol Settlement",
    project_url: "https://cow.fi",
    contacts: "email:security@cow.fi,link:https://immunefi.com/bug-bounty/cowprotocol/",

    // Also required. Takes a link or free text; free text lets the one field
    // every parser surfaces carry the warning that this isn't production
    // software, which the standard has no dedicated field for.
    policy: "TESTING ONLY: this program is an unaudited work in progress, \
             deployed for testing purposes only. Do not approve this contract \
             to spend more funds than you can expect to lose.",

    // Optional fields.
    preferred_languages: "en",
    source_code: "https://github.com/cowprotocol/solana-programs",
    auditors: "none"
}
