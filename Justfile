[private]
default:
    @{{ just_executable() }} --list

# Build the on-chain program for Solana.
build:
    cargo build-sbf

# Format the source code.
fmt:
    cargo fmt

# Check that the source code is formatted.
fmt-check:
    cargo fmt -- --check
