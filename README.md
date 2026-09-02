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

A Node.JS runtime is also necessary for generating client libraries from the IDL.

Common dev tasks are exposed via [`just`](https://just.systems/) recipes (see `Justfile`).
Most package managers provide this package, see [list of available Just packages](https://just.systems/man/en/packages.html).
Run `just --list` to see what's available.

## Repository layout

The repository is a Cargo workspace following the program / client / interface split:

- [`interface/`](./interface): shared types and the `Instruction` builders. Depends only on the lightweight crates so it can be consumed from both on-chain and off-chain code.
- [`programs/settlement/`](./programs/settlement): the on-chain settlement program.
  - [`programs/settlement/idl/`](./programs/settlement/idl): the IDL of the settlement program and its generated associated client libraries.
    - [`programs/settlement/idl/client/js/`](./programs/settlement/idl/client/js/): A Typescript client generated from the IDL
- [`client/`](./client): off-chain client helpers that re-export the builders from `interface` and add small convenience wrappers.


### How to build

Build the on-chain program (produces `target/deploy/settlement.so`):

```sh
just build-program
```

Build everything except IDL client libraries (workspace crates plus the on-chain program):

```sh
just build
```

The IDL client libraries can be generated with:

```sh
just generate
```

### How to test

```sh
just test
```

The IDL client libraries can be tested with:

```sh
just test-idl-generated
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
just deploy FYp8R5K4B3B1Kfr7QuWzMz4TwoT7wptjYtxgCrY5sRXb ./deployer-keypair.json
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

### Publishing the npm package

The TS/JS client (`@cowprotocol/solana-settlement-client`, generated from `programs/settlement/idl/cow_settlement.json` via Codama) is published automatically by [`publish-npm.yml`](.github/workflows/publish-npm.yml) whenever a GitHub release is cut — its version must already match the release tag (see [Bumping the crate version](#bumping-the-crate-version), which bumps it alongside the crates). The release itself is also created automatically, by [`auto-release.yml`](.github/workflows/auto-release.yml), as soon as a version-bump PR merges into `main` — see the [Breaking change](#breaking-change) and [Patch update](#patch-update) flows above. Merging the bump PR is the only manual step left before a release goes out; npm publishing still needs manual approval (below).

Publishing itself requires a manual approval in the `npm-publish` GitHub Environment. Before approving, check the job summary the workflow posts: it lists the exact tarball contents about to be published and a dependency diff against the previously published version. Approve only if both look as expected for the changes in this release.

Authentication to npm uses [Trusted Publishing](https://docs.npmjs.com/trusted-publishers) (OIDC) — no long-lived npm token is stored. This requires a one-time setup on npmjs.com *after* the package's first publish (a Trusted Publisher is configured on the package's own settings page, so it can't be set up before the package exists): add a Trusted Publisher for this exact repo, `publish-npm.yml`, and the `npm-publish` environment, then delete the `NPM_TOKEN` secret — it's a bootstrap-only fallback for that first publish.

### Devnet example

```sh
solana config set --url devnet
just deploy FYp8R5K4B3B1Kfr7QuWzMz4TwoT7wptjYtxgCrY5sRXb ~/solana-keys/deployer.json
```

The deployer for the canonical devnet program (`FYp8R5K4B3B1Kfr7QuWzMz4TwoT7wptjYtxgCrY5sRXb`) is stored in the team password manager under `B6acm3swJK9pJ7fe4i4GQgP7x5A3RndvsdV2bKhcA1i5`.

## Alpha releases

There are two possible release flows while the project is in alpha:
- Program breaking change: the logic of the program changes meaningfully or there's some change on the layout of _any_ PDA.
- Patch update: it's mostly a hotfix for the program that doesn't meaningfully change the program state. The old client/interface should still work with the new program code. 

You can use the settle CLI for a smoke test of the programs after a release. See `cargo run -p cow-test-cli -- sell --help` and `cargo run -p cow-test-cli -- settle --help`.

### Breaking change

- Check out the `main` branch. Make sure there are no local changes (`git status --porcelain` is empty).
- [Bump the crate version](#bumping-the-crate-version) *by at least a minor version*.
- Generate a new account (`solana-keygen new --no-bip39-passphrase -o ../deploy-v$VERSION.json`). This will be the address of the new deployment.
- Store the newly generated account in 1password (under "Settlement account by version").
- Update the account in `solana_pubkey::declare_id!` to the new account. Search and replace entries with the old account to the newly generated address.
- Commit the code changes resulting from the steps above (excluding the key of the generated account).
- Switch your network to mainnet (`solana config set --url mainnet-beta`). You should try out the next steps before the PR on devnet first, but switch to mainnet for the actual release.
- [Deploy the programs](#how-to-deploy). The deployer keypair is in 1password (under "Solana Deployer"). The program keypair file is the key that was generated before.
- Authorize all [currently existing solver](https://app.notion.com/p/cownation/Solvers-for-Solana-Dev-Contracts-3ca8da5f04ca80968642e85640178cbd) using the solver CLI (`cow solver add --help`).
- Make sure the package installs without errors: run `cargo install --path /mnt/lima-solana/repos/solana-programs/solana-program-workbench/test-cli --locked` (it depends on all other packages).
- Create a PR with the changes and wait for approval, then merge it. Merging automatically creates a GitHub release (tag `v$VERSION`, e.g. `v0.42.0`) via [`auto-release.yml`](.github/workflows/auto-release.yml), which in turn triggers the npm package publish workflow — see [Publishing the npm package](#publishing-the-npm-package).
- [Publish the cargo packages](#publishing-the-cargo-packages).

### Patch update

- Check out the `main` branch. Make sure there are no local changes (`git status --porcelain` is empty).
- [Bump the crate version](#bumping-the-crate-version) by a patch version.
- Commit the code changes resulting from the changes above.
- Create a PR with the changes and wait for approval, then merge it. Merging automatically creates a GitHub release (tag `v$VERSION`, e.g. `v0.42.1`) via [`auto-release.yml`](.github/workflows/auto-release.yml), which in turn triggers the npm package publish workflow — see [Publishing the npm package](#publishing-the-npm-package).
- [Update the programs](#how-to-deploy). The deployer keypair and the program keypair are in 1password (stored respectively under "Solana Deployer" and "Settlement account by version").
- [Publish the cargo packages](#publishing-the-cargo-packages).

### Bumping the crate version

You need to update Cargo's toml and lock file, and the npm package's version (kept in lockstep so a release tag maps to one version everywhere).
Here is a list of commands to help bumping all relevant strings:

```sh
export VERSION=0.42.1337
perl -i -pe '
  s/^version = ".*"/version = "$ENV{VERSION}"/;
  s/(path = "[^"]*", version = )"[^"]*"/$1"$ENV{VERSION}"/;
' ./Cargo.toml
perl -i -pe 's/^(\s*"version": )".*"/$1"$ENV{VERSION}"/' ./programs/settlement/idl/client/js/package.json
just build
```

## License

[![License: MIT](https://img.shields.io/badge/License-MIT-green.svg)](./COPYING.MIT) [![License: Apache v2.0](https://img.shields.io/badge/License-Apache_2.0-green.svg)](./COPYING.APACHE) [![License: LGPL v3](https://img.shields.io/badge/License-LGPLv3-blue.svg)](./COPYING.LESSER)

The core settlement program rust crate is licensed under the terms of the GNU Lesser General Public License v3.0.

All other crates are dual licensed under the terms of the MIT or Apache 2.0 license.

Copyright (c) 2026 CoW Foundation
