// consumer-kit-v6 — obeys the client's declared peer (@solana/kit ^6.10.0).
//
// `pnpm install` succeeds and is silent. But importing anything from the client
// pulls its generated code into the type-check, where the two copies of
// @solana/addresses (v6 from our kit, v8 from the client's own deps) collide and
// produce incompatible `Address` types. Even this trivial pure helper is enough
// to make `tsc` fail — see README.md.

import { encodeFlags, OrderKind } from "cow-solana-settlement-client";

export const flags = encodeFlags({
  createdOnChain: true,
  kind: OrderKind.Sell,
  partiallyFillable: false,
});
