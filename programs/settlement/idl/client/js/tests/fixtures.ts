import path from "node:path";
import { generateKeyPairSigner, type Address } from "@solana/kit";
import { encodeFlags } from "../src/order";
import { OrderKind, type OrderIntentArgs } from "../src/generated";

export const COW_SETTLEMENT_SO_PATH = path.join(
  import.meta.dirname,
  "../../../../../../target/deploy/cow_settlement.so",
);

export async function buildOrderIntent(
  overrides: Partial<OrderIntentArgs> & { owner: Address },
): Promise<OrderIntentArgs> {
  // create_order doesn't actually check the token accounts or mints the intent names, so
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
    flags: encodeFlags({
      createdOnChain: true,
      kind: OrderKind.Sell,
      partiallyFillable: false,
    }),
    appData: new Uint8Array(32),
    ...overrides,
  };
}
