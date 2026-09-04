import { describe, expect, it } from "vitest";
import { getAddressDecoder } from "@solana/kit";
import {
  getOrderIntentEncoder,
  OrderKind,
  type OrderIntentArgs,
} from "../src/generated";
import { encodeFlags } from "../src/order";

const address = (byte: number) =>
  getAddressDecoder().decode(new Uint8Array(32).fill(byte));

// Same as used in Rust (interface/src/data/intent.rs)
const SAMPLE_INTENT: OrderIntentArgs = {
  owner: address(0x11),
  buyTokenAccount: address(0x22),
  buyMint: address(0x33),
  sellTokenAccount: address(0x44),
  sellMint: address(0x55),
  sellAmount: 0x0123_4567_89ab_cdefn,
  buyAmount: 0xfedc_ba98_7654_3210n,
  validTo: 0xdead_beef,
  flags: encodeFlags({
    createdOnChain: true,
    kind: OrderKind.Buy,
    partiallyFillable: true,
  }),
  appData: new Uint8Array(32).fill(0x66),
};

// `uid_digest_regression` in interface/src/data/intent.rs.
const SAMPLE_UID =
  "de4096c6c100056f1e4636ea4fafefad40fc1d0b37692fe3ca1e0db3644b86bd";

const hex = (bytes: Uint8Array) =>
  Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");

describe("resolveOrderPda", () => {
  it("hashes the intent to the UID the program derives", async () => {
    const digest = await crypto.subtle.digest(
      "SHA-256",
      getOrderIntentEncoder().encode(SAMPLE_INTENT),
    );
    expect(hex(new Uint8Array(digest))).toBe(SAMPLE_UID);
  });
});
