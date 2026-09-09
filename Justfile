# Directory for locally-installed cargo packages.
cargo_root := justfile_directory() / ".cargo-root"
# The solana-verify binary that `install-solana-verify` produces.
solana_verify := cargo_root / "bin" / "solana-verify"
# The settlement program's cargo library name (the on-chain artifact is `<settlement_program>.so`).
settlement_program := "cow_settlement"
# The public repository URL.
repo_url := "https://github.com/cowprotocol/solana-programs"

[private]
default:
    @{{ just_executable() }} --list

# Install the pinned solana-verify on the current machine.
[private]
install-solana-verify:
    cargo install solana-verify --version "$(cat .solana-verify-version.txt)" --root {{cargo_root}}

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
[working-directory: 'programs/settlement/idl']
@generate-js-client:
    corepack pnpm install --frozen-lockfile
    node generate.mjs

# Run the test suite (builds the program first so the .so exists).
test: build-program build-test-programs
    cargo test

# Run tests from the generated clients from the IDL
test-idl-generated: test-js-client

# Run the JS client's tests
[working-directory: 'programs/settlement/idl/client/js']
@test-js-client: build-program generate-js-client
    corepack pnpm install --frozen-lockfile
    corepack pnpm exec vitest run

    # Needed because some tests rely on typescript generating errors if a type changes
    corepack pnpm run typecheck

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

# Format the JS client with prettier.
[working-directory: 'programs/settlement/idl/client/js']
fmt-js-client:
    corepack pnpm install --frozen-lockfile && corepack pnpm exec prettier --write .

# Check that the JS client is formatted.
[working-directory: 'programs/settlement/idl/client/js']
fmt-check-js-client:
    corepack pnpm install --frozen-lockfile && corepack pnpm exec prettier --check .

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
build-verified: install-solana-verify
    {{solana_verify}} build --library-name {{settlement_program}}

# Deploy the settlement program, then create its state PDA.
deploy programid keypair: build-verified
    #!/usr/bin/env bash
    set -euo pipefail
    solana program deploy ./target/deploy/{{settlement_program}}.so --program-id {{programid}} --keypair {{keypair}}

    # `programid` is a keypair file on a first deploy and an address on an upgrade,
    # but the CLI only takes the address.
    program_id=$(solana address --keypair "{{programid}}" 2>/dev/null || echo "{{programid}}")
    # A failure here is expected when upgrading a program whose state PDA already
    # exists, so don't fail the deploy over it.
    cargo run -p cow-test-cli -- \
        --program-id "$program_id" \
        --keypair "{{keypair}}" \
        initialize \
        || echo "warning: \`initialize\` failed, the state PDA may already exist" >&2

# Register the on-chain verification for an already-deployed program.
verify programid keypair commit_hash="": install-solana-verify
    #!/usr/bin/env bash
    set -euo pipefail
    commit_args=()
    if [ -n "{{commit_hash}}" ]; then
        commit_args=(--commit-hash "{{commit_hash}}")
    fi
    # Step 1: write the otter-verify PDA, signed by the upgrade authority.
    {{solana_verify}} verify-from-repo \
        --keypair "{{keypair}}" \
        --program-id "{{programid}}" \
        --library-name {{settlement_program}} \
        "${commit_args[@]}" \
        {{repo_url}}
    # Step 2: queue remote worker to rebuild from the PDA.
    {{solana_verify}} remote submit-job \
        --program-id "{{programid}}" \
        --uploader "$(solana address --keypair "{{keypair}}")"

all: build bench test-js-client lint fmt-check fmt-check-js-client doc-dev
