import path from "node:path";
import { generateKeyPairSigner, type Address } from "@solana/kit";
import { OrderKind, type OrderIntentArgs } from "../src/generated";

export const COW_SETTLEMENT_SO_PATH = path.join(
  import.meta.dirname,
  "../../../../../../target/deploy/cow_settlement.so",
);

/**
 * Assembles the intent's `flags` byte from the settings it packs: bit 0 is
 * created_on_chain, bit 1 the order kind, bit 2 partially_fillable. Reserved
 * bits are left clear — the program rejects a byte carrying one.
 */
export function encodeFlags({
  createdOnChain,
  kind,
  partiallyFillable,
}: {
  createdOnChain: boolean;
  kind: OrderKind;
  partiallyFillable: boolean;
}): number {
  return (
    (createdOnChain ? 1 << 0 : 0) |
    (kind === OrderKind.Buy ? 1 << 1 : 0) |
    (partiallyFillable ? 1 << 2 : 0)
  );
}

export async function buildOrderIntent(
  overrides: Partial<OrderIntentArgs> & { owner: Address },
): Promise<OrderIntentArgs> {
  // create_order doesn't touch the token accounts or mints the intent names, so
  // they only have to be distinct addresses.
  const [buyTokenAccount, buyMint, sellTokenAccount, sellMint] =
    await Promise.all([
      generateKeyPairSigner(),
      generateKeyPairSigner(),
      generateKeyPairSigner(),
      generateKeyPairSigner(),
    ]);
  return {
    buyTokenAccount: buyTokenAccount.address,
    buyMint: buyMint.address,
    sellTokenAccount: sellTokenAccount.address,
    sellMint: sellMint.address,
    sellAmount: 1_000_000n,
    buyAmount: 2_000_000n,
    validTo: Math.floor(Date.now() / 1000) + 3600,
    // This is the on-chain creation flow, so created_on_chain has to be set:
    // create_order rejects an intent whose bit is clear.
    flags: encodeFlags({
      createdOnChain: true,
      kind: OrderKind.Sell,
      partiallyFillable: false,
    }),
    appData: new Uint8Array(32),
    ...overrides,
  };
}
