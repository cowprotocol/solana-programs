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

# Build the on-chain settlement program (.so) for Solana. Defaults to the
# production deployment; the `deploy-barn`/`deploy-custom` recipes build theirs.
build-program:
    cargo build-sbf --manifest-path programs/settlement/Cargo.toml --features prod

# Build supplementary test programs (.so)
build-test-programs:
    cargo build-sbf --manifest-path programs/test/cpi-caller/Cargo.toml

# Build everything: host-side workspace crates plus the on-chain program.
build: build-program
    cargo build --features settlement/prod

# Runs all the generated code jobs
generate: generate-js-client

# Builds the JS/TS client from IDL.
[working-directory: 'programs/settlement/idl']
@generate-js-client:
    corepack pnpm install --frozen-lockfile
    node generate.mjs

# Run the test suite (builds the program first so the .so exists).
test: build-program build-test-programs
    cargo test --features settlement/prod

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
    TEST_BENCHMARK=true cargo test --features settlement/prod


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

# Build the settlement program for `env` (prod or barn) with solana-verify's
# reproducible Docker build.
build-verified env: install-solana-verify
    {{solana_verify}} build --library-name {{settlement_program}} -- --features {{env}}

# Deploy (or upgrade) the settlement program for a committed deployment, then
# initialize it. `env` is prod or barn; `programid` is the program keypair on a
# first deploy, or the program address on an upgrade. Refuses to deploy unless
# `programid` resolves to that deployment's address, so the baked-in id (which
# the `env` feature fixes) always matches where it lands.
[private]
deploy-committed env programid keypair: install-solana-verify
    #!/usr/bin/env bash
    set -euo pipefail
    target=$(solana address --keypair "{{programid}}" 2>/dev/null || echo "{{programid}}")
    name=$(printf '%s' "{{env}}" | tr '[:lower:]' '[:upper:]')
    expected=$(sed -n "s/.*${name}_ID.*from_str_const(\"\([^\"]*\)\").*/\1/p" interface/src/lib.rs)
    if [ "$target" != "$expected" ]; then
        echo "refusing to deploy {{env}}: $target is not the {{env}} address ($expected)" >&2
        exit 1
    fi
    {{solana_verify}} build --library-name {{settlement_program}} -- --features {{env}}
    solana program deploy ./target/deploy/{{settlement_program}}.so --program-id {{programid}} --keypair {{keypair}}
    cargo run -p cow-test-cli -- --program-id "$target" --keypair "{{keypair}}" initialize

# Deploy (or upgrade) the production program.
deploy-prod programid keypair: (deploy-committed "prod" programid keypair)

# Deploy (or upgrade) the staging ("barn") program.
deploy-barn programid keypair: (deploy-committed "barn" programid keypair)

# Deploy (or upgrade) an ad-hoc program baked for the address of `programid`, an
# unverified build for devnet/local one-offs. The baked id is the deploy target
# by construction, so there is nothing to mismatch.
deploy-custom programid keypair:
    #!/usr/bin/env bash
    set -euo pipefail
    target=$(solana address --keypair "{{programid}}" 2>/dev/null || echo "{{programid}}")
    SETTLEMENT_PROGRAM_ID="$target" cargo build-sbf --manifest-path programs/settlement/Cargo.toml
    solana program deploy ./target/deploy/{{settlement_program}}.so --program-id {{programid}} --keypair {{keypair}}
    cargo run -p cow-test-cli -- --program-id "$target" --keypair "{{keypair}}" initialize

# Register the on-chain verification for an already-deployed program. `env` is
# the deployment it was built for (prod or barn), so the remote rebuild matches.
verify env programid keypair commit_hash="": install-solana-verify
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
        {{repo_url}} \
        -- --features {{env}}
    # Step 2: queue remote worker to rebuild from the PDA.
    {{solana_verify}} remote submit-job \
        --program-id "{{programid}}" \
        --uploader "$(solana address --keypair "{{keypair}}")"

all: build bench test-js-client lint fmt-check fmt-check-js-client doc-dev
