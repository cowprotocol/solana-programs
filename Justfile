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

# Runs all the generated code jobs
generate: generate-js-client

# Builds the JS/TS client from IDL.
generate-js-client:
    cd programs/settlement/idl && corepack pnpm install --frozen-lockfile && node generate.mjs

# Run the test suite (builds the program first so the .so exists).
test: build-program build-test-programs
    cargo test

# Run tests from the generated clients from the IDL
test-idl-generated: test-js-client

# Run the JS client's tests
test-js-client: build-program generate-js-client
    cd programs/settlement/idl/client/js && corepack pnpm install --frozen-lockfile && corepack pnpm exec vitest run

# Each test outputs its consumption during test execution to a series of target/bench-report/*.jsonl files.
# Assembles into a single `bench-report.json`
bench: build-program build-test-programs
    #!/usr/bin/env bash
    set -euo pipefail
    rm -rf target/bench-report
    TEST_BENCHMARK=true cargo test


    shopt -s nullglob
    shards=(target/bench-report/*.jsonl)
    if [[ ${#shards[@]} -eq 0 ]]; then
        echo "no compute-unit measurements recorded" >&2
        exit 1
    fi
    # Read all files and zip into a easily readable unified file
    jq --slurp --sort-keys '{
        "compute_units": (map({(.label): .compute_units}) | add),
        "accounts": (map({(.label): .accounts}) | add),
        "transaction_bytes": (map({(.label): .transaction_bytes}) | add)
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

# Build the publishable TS/JS client package (bundles the Codama-generated code plus hand-written wrappers).
build-js-client: generate-js-client
    cd programs/settlement/idl/client/js && corepack pnpm install --frozen-lockfile && corepack pnpm run build

# Build the settlement program using solana-verify's reproducible Docker build.
# Installs solana-verify via cargo if not already present (same as CI).
build-verified:
    cargo install solana-verify --version $(cat .solana-verify-version.txt) --root .cargo-root/
    ./.cargo-root/bin/solana-verify build --library-name cow_settlement

# Deploy the settlement program, then create its state PDA.
deploy programid keypair: build-verified
    #!/usr/bin/env bash
    set -euo pipefail
    solana program deploy ./target/deploy/cow_settlement.so --program-id {{programid}} --keypair {{keypair}}

    # `programid` is a keypair file on a first deploy and an address on an upgrade,
    # but the CLI only takes the address.
    program_id=$(solana address --keypair "{{programid}}" 2>/dev/null || echo "{{programid}}")
    # A failure here is expected when upgrading a program whose state PDA already
    # exists, so don't fail the deploy over it.
    cargo run -p cow-test-cli -- \
        --rpc-url "$(solana config get json_rpc_url | awk '{print $NF}')" \
        --program-id "$program_id" \
        --keypair "{{keypair}}" \
        initialize \
        || echo "warning: \`initialize\` failed, the state PDA may already exist" >&2

all: build bench test-js-client lint fmt-check doc-dev
