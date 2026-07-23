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

# Format the source code.
fmt:
    cargo fmt

# Check that the source code is formatted.
fmt-check:
    cargo fmt -- --check

# Lint the source code with clippy.
lint:
    cargo clippy --workspace --all-targets --all-features -- --deny=warnings

# Generate the crate documentation.
doc:
    cargo doc --workspace --no-deps --all-features

# Check that the documentation builds with no warnings, including private docs.
doc-check:
    cargo doc --workspace --no-deps --all-features --document-private-items --config 'build.rustdocflags=["--deny=warnings"]'

# Build the settlement program using solana-verify's reproducible Docker build.
# Installs solana-verify via cargo if not already present (same as CI).
build-verified:
    cargo install solana-verify --version $(cat .solana-verify-version.txt) --root .cargo-root/
    ./.cargo-root/bin/solana-verify build --library-name cow_settlement

deploy programid keypair: build-verified
    solana program deploy ./target/deploy/cow_settlement.so --program-id {{programid}} --keypair {{keypair}}

all: build test lint fmt-check doc-check
