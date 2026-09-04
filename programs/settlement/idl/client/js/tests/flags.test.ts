import { describe, expect, it } from "vitest";
import { decodeFlags, encodeFlags, type Flags } from "../src/order";
import { OrderKind } from "../src/generated";

// Every combination of the three settings, with the byte the program's
// `Flags` encoding gives it.
const CASES: [Flags, number][] = [
  [{ createdOnChain: false, kind: OrderKind.Sell, partiallyFillable: false }, 0b000],
  [{ createdOnChain: true, kind: OrderKind.Sell, partiallyFillable: false }, 0b001],
  [{ createdOnChain: false, kind: OrderKind.Buy, partiallyFillable: false }, 0b010],
  [{ createdOnChain: true, kind: OrderKind.Buy, partiallyFillable: false }, 0b011],
  [{ createdOnChain: false, kind: OrderKind.Sell, partiallyFillable: true }, 0b100],
  [{ createdOnChain: true, kind: OrderKind.Sell, partiallyFillable: true }, 0b101],
  [{ createdOnChain: false, kind: OrderKind.Buy, partiallyFillable: true }, 0b110],
  [{ createdOnChain: true, kind: OrderKind.Buy, partiallyFillable: true }, 0b111],
];

describe("flags", () => {
  it.each(CASES)("encodes %j as %d", (flags, byte) => {
    expect(encodeFlags(flags)).toBe(byte);
  });

  it.each(CASES)("decodes %j from %d", (flags, byte) => {
    expect(decodeFlags(byte)).toEqual(flags);
  });
});
