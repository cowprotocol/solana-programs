[private]
default:
    @{{ just_executable() }} --list

# Build the on-chain settlement program (.so) for Solana.
build-program:
    cargo build-sbf --manifest-path programs/settlement/Cargo.toml

# Build supplementary test programs (.so)
build-test-programs:
    cargo build-sbf --manifest-path programs/test/cpi-caller/Cargo.toml

# Build everything: host-side workspace crates plus the on-chain program.
build: build-program
    cargo build

# Run the test suite (builds the program first so the .so exists).
test: build-program build-test-programs
    cargo test

# Each test outputs its consumption during test execution to a series of target/bench-report/*.jsonl files.
# Assembles into a single `bench-report.json`
bench: build-program build-test-programs
    #!/usr/bin/env bash
    set -euo pipefail
    rm -rf target/cu-report
    TEST_BENCHMARK=true cargo test


    shopt -s nullglob
    shards=(target/bench-report/*.jsonl)
    if [[ ${#shards[@]} -eq 0 ]]; then
        echo "no compute-unit measurements recorded" >&2
        exit 1
    fi
    # Slurp every record into one array, turn each into a single-key object, and
    # fold them together; a repeated label keeps the last measurement.
    jq --slurp --sort-keys '{
        "compute_units": (map({(.label): .compute_units}) | add),
        "accounts_readable": (map({(.label): .accounts_readable}) | add),
        "accounts_writable": (map({(.label): .accounts_writable}) | add),
        "instruction_bytes": (map({(.label): .instruction_bytes}) | add)
    }' "${shards[@]}" \
        > bench-report.json

# Format the source code.
fmt:
    cargo fmt

# Check that the source code is formatted.
fmt-check:
    cargo fmt -- --check

# Lint the source code with clippy.
lint:
    cargo clippy --workspace --all-targets --all-features -- --deny=warnings

# Generate the crate documentation. Extra arguments are forwarded to `cargo doc` (e.g., `just doc --open`).
doc *args:
    cargo doc --workspace --no-deps --all-features {{ args }}

# Generate extended documentation for devs. Fails on warnings, so we catch documentation issues early. Extra arguments are forwarded to `cargo doc` (e.g., `just doc-dev --open`).
doc-dev *args:
    cargo doc --workspace --no-deps --all-features --document-private-items --config 'build.rustdocflags=["--deny=warnings"]' {{ args }}

# Build the settlement program using solana-verify's reproducible Docker build.
# Installs solana-verify via cargo if not already present (same as CI).
build-verified:
    cargo install solana-verify --version $(cat .solana-verify-version.txt) --root .cargo-root/
    ./.cargo-root/bin/solana-verify build --library-name cow_settlement

deploy programid keypair: build-verified
    solana program deploy ./target/deploy/cow_settlement.so --program-id {{programid}} --keypair {{keypair}}

all: build test lint fmt-check doc-dev
