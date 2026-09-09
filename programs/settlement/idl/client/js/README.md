# @cowprotocol/solana-settlement-client

TypeScript/JavaScript client for the CoW Protocol Solana settlement program, generated from its IDL ([`cow_settlement.json`](https://github.com/cowprotocol/solana-programs/blob/main/programs/settlement/idl/cow_settlement.json)) via [Codama](https://github.com/codama-idl/codama), built on [`@solana/kit`](https://github.com/anza-xyz/kit).

> [!CAUTION]
> The settlement program is a work in progress and **not ready for production use**. See the [repository README](https://github.com/cowprotocol/solana-programs/blob/main/README.md) for details.

## Usage

Building a `createOrder` instruction:

```typescript
import { type TransactionSigner, type Address } from "@solana/kit";
import {
  getCreateOrderInstructionAsync,
  resolveOrderPda,
  OrderKind,
  encodeFlags,
  COW_SETTLEMENT_PROGRAM_ADDRESS,
} from "@cowprotocol/solana-settlement-client";

declare const owner: TransactionSigner; // e.g. from generateKeyPairSigner() or a wallet adapter
declare const buyTokenAccount: Address, buyMint: Address;
declare const sellTokenAccount: Address, sellMint: Address;

const intent = {
  owner: owner.address,
  buyTokenAccount,
  buyMint,
  sellTokenAccount,
  sellMint,
  sellAmount: 1_000_000n,
  buyAmount: 2_000_000n,
  validTo: Math.floor(Date.now() / 1000) + 3600,
  flags: encodeFlags({ createdOnChain: true, kind: OrderKind.Sell, partiallyFillable: false }),
  appData: new Uint8Array(32),
};

// The order account's address, for fetching it once the instruction lands.
const { value: orderPda } = await resolveOrderPda({
  programAddress: COW_SETTLEMENT_PROGRAM_ADDRESS,
  args: { intent },
});

const instruction = await getCreateOrderInstructionAsync({
  owner,
  createdBy: owner, // pays the order PDA's rent; may be a different signer than owner
  intent,
});
```

From here, `instruction` is added to a transaction message and sent like any `@solana/kit` instruction (`createTransactionMessage`, `appendTransactionMessageInstruction`, sign, and send via an RPC). Every other instruction (`getAddSolverInstruction`, `getReclaimOrderInstructionAsync`, etc.) follows the same shape: build the args, get the instruction.

If your project uses classic `@solana/web3.js` instead of `@solana/kit`, the instructions this package returns (`{ programAddress, accounts, data }`) can be converted to a `web3.TransactionInstruction` by mapping `accounts` (each `{ address, role }`, where `role` is a bitmask: bit 0 = writable, bit 1 = signer) to `web3.AccountMeta`.

See the [repository README](https://github.com/cowprotocol/solana-programs/blob/main/README.md) and [DESIGN.md](https://github.com/cowprotocol/solana-programs/blob/main/DESIGN.md) for the settlement program's design and this client's generation pipeline.

## Development

This package is generated and built from the parent repository; see its [Justfile](https://github.com/cowprotocol/solana-programs/blob/main/Justfile) (`just generate-js-client`, `just build-js-client`, `just test-js-client`) rather than running commands directly here.
