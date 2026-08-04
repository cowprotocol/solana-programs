//! Runs the on-chain keccak256 and sha256 syscalls through the `test-hasher`
//! helper program and checks their output against known-good values, including
//! the keccak256 digest Ethereum tooling produces for the same input (e.g.
//! `cast keccak "hello world"`). This pins down that Solana's keccak256 syscall
//! is the EVM-compatible variant rather than finalized SHA-3.
//!
//! It also reports the compute units each hash syscall costs, isolated from the
//! fixed per-invocation overhead by subtracting a no-op baseline.

use hex_literal::hex;
use litesvm::LiteSVM;
use solana_sdk::{instruction::Instruction, pubkey::Pubkey, signature::Keypair};
use test_hasher::HashOp;

mod common;

const HELLO_WORLD: &[u8] = b"hello world";

/// keccak256("hello world") as produced by the EVM, e.g. `cast keccak
/// "hello world"`.
const EVM_KECCAK: [u8; 32] =
    hex!("47173285a8d7341e5e972fc677286384f802f8ef42a5ec5f03bbfa254cb01fad");

/// sha256("hello world"), `echo -n "hello world" | sha256sum``
const EVM_SHA256: [u8; 32] =
    hex!("b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9");

/// Invoke the hasher with `op` applied to `message`, returning the digest it
/// hands back through return data and the compute units the transaction
/// consumed.
fn invoke(
    svm: &mut LiteSVM,
    program_id: Pubkey,
    payer: &Keypair,
    op: HashOp,
    message: &[u8],
) -> (Vec<u8>, u64) {
    let mut data = vec![op as u8];
    data.extend_from_slice(message);

    let ix = Instruction {
        program_id,
        accounts: vec![],
        data,
    };

    let meta = common::send(svm, payer, vec![ix]).expect("hasher invocation should succeed");
    (meta.return_data.data, meta.compute_units_consumed)
}

#[test]
fn keccak_and_sha256_match_evm() {
    let (mut svm, program_id, payer) = common::setup_hasher();

    let (keccak, _) = invoke(&mut svm, program_id, &payer, HashOp::Keccak256, HELLO_WORLD);
    let (sha256, _) = invoke(&mut svm, program_id, &payer, HashOp::Sha256, HELLO_WORLD);

    assert_eq!(
        keccak, EVM_KECCAK,
        "on-chain keccak256 must match the EVM keccak256 digest"
    );
    assert_eq!(
        sha256, EVM_SHA256,
        "on-chain sha256 must match the EVM sha256 precompile digest"
    );
}

#[test]
fn reports_compute_units_per_hash() {
    let (mut svm, program_id, payer) = common::setup_hasher();

    // Baseline: the no-op hashes nothing and returns immediately, so its cost
    // is the fixed per-invocation overhead, independent of the message.
    // Subtracting it isolates the hash syscall (plus the `sol_set_return_data`
    // write, which the no-op skips) from that overhead. The message content is
    // irrelevant to a cost measurement, so it's a fixed filler byte.
    let (_, baseline_cu) = invoke(&mut svm, program_id, &payer, HashOp::Noop, &[]);

    // Visible with `cargo test -- --nocapture`.
    println!("hash syscall compute units by message length (baseline {baseline_cu} CU):");
    println!("    bytes  keccak256  sha256");
    // Powers of ten, to show the base cost giving way to the per-byte cost.
    // Plus a number not too far from max bytes in an instruction.
    for len in [1, 10, 100, 1000, 10000, 60000] {
        let message = vec![b'a'; len];
        let (_, keccak_cu) = invoke(&mut svm, program_id, &payer, HashOp::Keccak256, &message);
        let (_, sha256_cu) = invoke(&mut svm, program_id, &payer, HashOp::Sha256, &message);

        assert!(
            keccak_cu > baseline_cu,
            "keccak256 of {len} bytes ({keccak_cu} CU) should cost more than the baseline ({baseline_cu} CU)"
        );
        assert!(
            sha256_cu > baseline_cu,
            "sha256 of {len} bytes ({sha256_cu} CU) should cost more than the baseline ({baseline_cu} CU)"
        );

        println!(
            "  {len:>7}  {:>9}  {:>6}",
            keccak_cu.saturating_sub(baseline_cu),
            sha256_cu.saturating_sub(baseline_cu)
        );
    }
}
