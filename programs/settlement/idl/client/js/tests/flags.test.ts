import { describe, expect, it } from "vitest";
import { decodeFlags, encodeFlags, type Flags } from "../src/order";
import { OrderKind } from "../src/generated";

// Every combination of the two settings, with the byte the program's
// `Flags` encoding gives it.
const CASES: [Flags, number][] = [
  [{ kind: OrderKind.Sell, partiallyFillable: false }, 0b000],
  [{ kind: OrderKind.Buy, partiallyFillable: false }, 0b010],
  [{ kind: OrderKind.Sell, partiallyFillable: true }, 0b100],
  [{ kind: OrderKind.Buy, partiallyFillable: true }, 0b110],
];

describe("flags", () => {
  it.each(CASES)("encodes %j as %d", (flags, byte) => {
    expect(encodeFlags(flags)).toBe(byte);
  });

  it.each(CASES)("decodes %j from %d", (flags, byte) => {
    expect(decodeFlags(byte)).toEqual(flags);
  });

  // Bytes outside the defined bits carry no meaning to the program, so decoding
  // has to reject them.
  it.each([0b001, 0b1000, 0b111, 0xff, 2 ** 32, -1])("rejects %d as reserved", (byte) => {
    expect(() => decodeFlags(byte)).toThrow(/reserved/);
  });
});
