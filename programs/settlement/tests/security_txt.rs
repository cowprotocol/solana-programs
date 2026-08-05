//! Checks that the `security.txt` notice survives compilation and linking into
//! the deployed artifact.
//!
//! The blob is emitted by a macro and never referenced from Rust, so nothing
//! about the host build proves it reaches the program binary — only reading the
//! `.so` back does. This scans it the same naive way a researcher's parser
//! would: search the raw bytes for the standard markers.

mod common;

/// Markers delimiting the notice, per the `security.txt` standard. Both include
/// the trailing null byte.
const BEGIN: &[u8] = b"=======BEGIN SECURITY.TXT V1=======\0";
const END: &[u8] = b"=======END SECURITY.TXT V1=======\0";

/// Number of occurrences of `needle` in `haystack`. Occurrences are counted
/// rather than just located because the standard requires the markers to be
/// unique: a second copy anywhere in the binary makes a naive parse ambiguous.
fn count(haystack: &[u8], needle: &[u8]) -> usize {
    haystack
        .windows(needle.len())
        .filter(|window| *window == needle)
        .count()
}

#[test]
fn program_binary_contains_security_txt() {
    let program = std::fs::read(common::PROGRAM_SO)
        .expect("compiled program .so not found, run `just build-program` first");

    assert_eq!(
        count(&program, BEGIN),
        1,
        "program binary should contain exactly one security.txt begin marker",
    );
    assert_eq!(
        count(&program, END),
        1,
        "program binary should contain exactly one security.txt end marker",
    );
}
