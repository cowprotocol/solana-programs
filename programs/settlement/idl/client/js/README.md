# @cowprotocol/solana-settlement-client

TypeScript/JavaScript client for the CoW Protocol Solana settlement program, generated from its IDL ([`cow_settlement.json`](../../cow_settlement.json)) via [Codama](https://github.com/codama-idl/codama), built on [`@solana/kit`](https://github.com/anza-xyz/kit).

> [!CAUTION]
> The settlement program is a work in progress and **not ready for production use**. See the [repository README](../../../../../README.md) for details.

## Usage

```typescript
import {
  getCreateOrderInstructionAsync,
  resolveOrderPda,
  OrderKind,
  encodeFlags,
  COW_SETTLEMENT_PROGRAM_ADDRESS,
} from "@cowprotocol/solana-settlement-client";
```

See the [repository README](../../../../../README.md) and [DESIGN.md](../../../../../DESIGN.md) for the settlement program's design and this client's generation pipeline.

## Development

This package is generated and built from the parent repository; see its [Justfile](../../../../../Justfile) (`just generate-js-client`, `just build-js-client`, `just test-js-client`) rather than running commands directly here.
