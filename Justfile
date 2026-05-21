[private]
default:
    @{{ just_executable() }} --list

# Build the on-chain program for Solana.
build:
    cargo build-sbf
