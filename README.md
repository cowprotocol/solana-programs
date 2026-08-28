# CoW Protocol on Solana

[CoW Protocol](https://cow.fi) is an open-source trading protocol that settles user intents in batch auctions. It supports direct matching between users (Coincidence of Wants) as well as on-chain liquidity sources.

This repository hosts the Solana implementation, currently in early development. The protocol is already live on Ethereum and other EVM chains; the Solidity contracts are at [cowprotocol/contracts](https://github.com/cowprotocol/contracts).

> [!CAUTION]
> These Solana programs are a work in progress and are **not ready for production use**. The code is unaudited, subject to change, and may contain significant vulnerabilities that could lead to loss of funds. Please do not rely on it with real assets. Any deployment to mainnet is for testing purposes only. We're sharing it openly to develop in the open and welcome your feedback.

## Design

The design of the program is documented in [DESIGN.md](./DESIGN.md).
It contains a high-level technical description of what the program does and points out meaningful differences from the [Ethereum implementation](https://github.com/cowprotocol/contracts).

## Development

Install the Solana toolchain (Rust, Solana CLI, and friends) by following the [Solana quick setup](https://solana.com/docs/intro/installation).

Common dev tasks are exposed via [`just`](https://just.systems/) recipes (see `Justfile`).
Most package managers provide this package, see [list of available Just packages](https://just.systems/man/en/packages.html).
Run `just --list` to see what's available.

## Repository layout

The repository is a Cargo workspace following the program / client / interface split:

- [`interface/`](./interface): shared types and the `Instruction` builders. Depends only on the lightweight crates so it can be consumed from both on-chain and off-chain code.
- [`programs/settlement/`](./programs/settlement): the on-chain settlement program.
- [`client/`](./client): off-chain client helpers that re-export the builders from `interface` and add small convenience wrappers.


### How to build

Build the on-chain program (produces `target/deploy/settlement.so`):

```sh
just build-program
```

Build everything (workspace crates plus the on-chain program):

```sh
just build
```

### How to test

```sh
just test
```

### Benchmarks

`just bench` runs the test suite and regenerates `bench-report.json`:

```sh
just bench
```

## How to build a verified (reproducible) build

Requires [Docker](https://docs.docker.com/engine/install/).

```sh
just build-verified
```

## How to deploy

Requires [Docker](https://docs.docker.com/engine/install/) (for the verified build step).

You will need a **deployer keypair** — a Solana wallet funded with enough SOL to cover program storage rent and transaction fees. This wallet becomes the **upgrade authority** for the deployed program.

> Do not fund the program address itself. Only the deployer wallet needs SOL.

There are two distinct flows depending on whether this is a first-time deploy or an upgrade to an existing program.

### Initial deployment

Pass the **program keypair file** as the first argument. Solana derives the program address from it and registers the deployer as the upgrade authority:

```sh
just deploy ./program-keypair.json ./deployer-keypair.json
```

### Upgrading an existing program

> [!IMPORTANT]
> Before upgrading, if the storage format has been changed for any existing PDAs, ensure that the MINOR (aka v0.x.0) version in the Cargo.toml is bumped. This will relocate all program storage, preventing unintended collisions with incompatible data.

Pass the **program's public key (address)** as the first argument. The deployer wallet must already be the upgrade authority:

```sh
just deploy J516Mv7YvvvJyMvNEca8tWNTJyDHbFpzwDZD96BNfR3w ./deployer-keypair.json
```

`just deploy` finishes by running `initialize` to create the program's state PDA.

If the deployment upgrades an existing program without bumping the major or minor cargo package version, 
then this latter step fails and prints a warning that can be safely ignored.

### Publishing the cargo packages

Authenticate your cargo cli with `cargo login`. Ensure you have permission to publish `cow-settlement-interface`, `cow-settlement-client`, and `cow-test-cli`.

Then, all packages can published in one go:

```sh
cargo publish
```

### Devnet example

```sh
solana config set --url devnet
just deploy J516Mv7YvvvJyMvNEca8tWNTJyDHbFpzwDZD96BNfR3w ~/solana-keys/deployer.json
```

The deployer for the canonical devnet program (`J516Mv7YvvvJyMvNEca8tWNTJyDHbFpzwDZD96BNfR3w`) is stored in the team password manager under `B6acm3swJK9pJ7fe4i4GQgP7x5A3RndvsdV2bKhcA1i5`.

## Alpha releases

There are two possible release flows while the project is in alpha:
- Program breaking change: the logic of the program changes meaningfully or there's some change on the layout of _any_ PDA.
- Patch update: it's mostly a hotfix for the program that doesn't meaningfully change the program state. The old client/interface should still work with the new program code. 

You can use the settle CLI for a smoke test of the programs after a release. See `cargo run -p cow-test-cli -- sell --help` and `cargo run -p cow-test-cli -- settle --help`.

### Breaking change

- [Bump the crate version](#bumping-the-crate-version) *by at least a minor version*.
- Generate a new account (`solana-keygen new --no-bip39-passphrase -o ../deploy-v$VERSION.json`). This will be the address of the new deployment.
- Store the newly generated account in 1password (under "Settlement account by version").
- Update the account in `solana_pubkey::declare_id!` to the new account. Search and replace entries with the old account to the newly generated address.
- Commit the code changes resulting from the steps above (excluding the key of the generated account).
- [Deploy the programs](#how-to-deploy). The deployer keypair is in 1password (under "Solana Deployer"). The program keypair file is the key that was generated before.
- Create a PR with the changes and wait for approval.
- [Publish the cargo packages](#publishing-the-cargo-packages).

### Patch update

- [Bump the crate version](#bumping-the-crate-version) by a patch version.
- Commit the code changes resulting from the changes above.
- Create a PR with the changes and wait for approval.
- [Update the programs](#how-to-deploy). The deployer keypair and the program keypair are in 1password (stored respectively under "Solana Deployer" and "Settlement account by version").
- [Publish the cargo packages](#publishing-the-cargo-packages).

### Bumping the crate version

You need to update Cargo's toml and lock file.
Here is a list of commands to help bumping all relevant strings:

```sh
export VERSION=0.42.1337
perl -i -pe '
  s/^version = ".*"/version = "$ENV{VERSION}"/;
  s/(path = "[^"]*", version = )"[^"]*"/$1"$ENV{VERSION}"/;
' ./Cargo.toml
just build
```

## License

[![License: MIT](https://img.shields.io/badge/License-MIT-green.svg)](./COPYING.MIT) [![License: Apache v2.0](https://img.shields.io/badge/License-Apache_2.0-green.svg)](./COPYING.APACHE) [![License: LGPL v3](https://img.shields.io/badge/License-LGPLv3-blue.svg)](./COPYING.LESSER)

The core settlement program rust crate is licensed under the terms of the GNU Lesser General Public License v3.0.

All other crates are dual licensed under the terms of the MIT or Apache 2.0 license.

Copyright (c) 2026 CoW Foundation
