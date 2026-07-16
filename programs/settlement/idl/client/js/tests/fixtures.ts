import path from "node:path";
import { generateKeyPairSigner, type Address } from "@solana/kit";
import { OrderKind, type OrderIntentArgs } from "../src/generated";

export const COW_SETTLEMENT_SO_PATH = path.join(
  import.meta.dirname,
  "../../../../../../target/deploy/cow_settlement.so",
);

export async function buildOrderIntent(
  overrides: Partial<OrderIntentArgs> & { owner: Address },
): Promise<OrderIntentArgs> {
  const buyTokenAccount = await generateKeyPairSigner();
  const sellTokenAccount = await generateKeyPairSigner();
  return {
    buyTokenAccount: buyTokenAccount.address,
    sellTokenAccount: sellTokenAccount.address,
    sellAmount: 1_000_000n,
    buyAmount: 2_000_000n,
    validTo: Math.floor(Date.now() / 1000) + 3600,
    kind: OrderKind.Sell,
    partiallyFillable: false,
    appData: new Uint8Array(32),
    ...overrides,
  };
}
