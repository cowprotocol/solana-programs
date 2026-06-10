# CoW Protocol on Solana

[CoW Protocol](https://cow.fi) is an open-source, permissionless trading protocol that settles user intents in batch auctions. It supports direct matching between users (Coincidence of Wants) as well as on-chain liquidity sources.

This repository hosts the Solana implementation, currently in early development. The protocol is already live on Ethereum and other EVM chains; the Solidity contracts are at [cowprotocol/contracts](https://github.com/cowprotocol/contracts).

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

Pass the **program's public key (address)** as the first argument. The deployer wallet must already be the upgrade authority:

```sh
just deploy MooohhPEAAHwAwEozL7JPEmnDvaahuUpccYN4Yb8ccK ./deployer-keypair.json
```

### Devnet example

```sh
solana config set --url devnet
just deploy MooohhPEAAHwAwEozL7JPEmnDvaahuUpccYN4Yb8ccK ~/solana-keys/deployer.json
```

The deployer for the canonical devnet program (`MooohhPEAAHwAwEozL7JPEmnDvaahuUpccYN4Yb8ccK`) is stored in the team password manager under `B6acm3swJK9pJ7fe4i4GQgP7x5A3RndvsdV2bKhcA1i5`.
