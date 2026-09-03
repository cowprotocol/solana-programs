// consumer-kit-v8 — the "correct" consumer.
//
// This picks @solana/kit ^8.0.0, the line the client was developed and tested
// against (its devDependency) and the line its hard dependency
// `@solana/program-client-core@^8.x` drags in. With a single copy of kit in the
// tree, the client type-checks and would run fine.
//
// The problem is you can't get here: the client declares
//   "peerDependencies": { "@solana/kit": "^6.10.0" }
// and 8.x does not satisfy ^6.10.0, so the install is the broken step (a pnpm
// WARN by default, a hard error under --strict-peer-dependencies / npm). See
// README.md.

import { encodeFlags, OrderKind } from "cow-solana-settlement-client";

export const flags = encodeFlags({
  createdOnChain: true,
  kind: OrderKind.Sell,
  partiallyFillable: false,
});
