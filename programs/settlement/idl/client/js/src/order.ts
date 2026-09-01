// Helpers for working with OrderIntent

import { OrderKind } from "./generated";

// re-export
export { OrderKind } from "./generated";

// The bit each field occupies
const CREATED_ON_CHAIN = 1 << 0;
const KIND = 1 << 1;
const PARTIALLY_FILLABLE = 1 << 2;

/** The settings an `OrderIntent`'s `flags` byte packs. */
export type Flags = {
  /**
   * How the order is authenticated: `true` if the owner creates it themselves
   * with a `create_order` instruction they sign, `false` if it's authenticated
   * off-chain by an Ed25519 signature, which lets anyone holding that signature
   * create the order.
   */
  createdOnChain: boolean;

  /**
   * Whether `sellAmount` or `buyAmount` is the exact figure; the other side is
   * treated as the limit (minimum to receive for `Sell`, maximum to spend for
   * `Buy`).
   */
  kind: OrderKind;

  /**
   * If `true`, the order may be filled across multiple settlements. If `false`,
   * a single settlement must consume the full sell amount (fill-or-kill).
   */
  partiallyFillable: boolean;
};

/** Packs the settings into the canonical flags byte. Reserved bits are left clear. */
export function encodeFlags({
  createdOnChain,
  kind,
  partiallyFillable,
}: Flags): number {
  return (
    (createdOnChain ? CREATED_ON_CHAIN : 0) |
    (kind === OrderKind.Buy ? KIND : 0) |
    (partiallyFillable ? PARTIALLY_FILLABLE : 0)
  );
}

/**
 * Unpacks a flags byte
 */
export function decodeFlags(byte: number): Flags {
  return {
    createdOnChain: (byte & CREATED_ON_CHAIN) !== 0,
    kind: (byte & KIND) === 0 ? OrderKind.Sell : OrderKind.Buy,
    partiallyFillable: (byte & PARTIALLY_FILLABLE) !== 0,
  };
}
