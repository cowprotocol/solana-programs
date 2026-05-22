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

### How to build

```sh
just build
```
