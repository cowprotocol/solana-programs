import { LiteSVM } from "litesvm";
import { beforeEach, describe, expect, it } from "vitest";
import type { TransactionSigner } from "@solana/kit";
import {
  findStatePda,
  getCancelOrderInstructionAsync,
  getCreateSettlementOwnedOrderInstructionAsync,
  type OrderIntentArgs,
} from "../src/generated";
import {
  buildOrderIntent,
  fetchOrderAccount,
  findOrderPda,
  initializeProgram,
  newSvm,
  sendInstruction,
} from "./fixtures";

/// Create an order owned by the state PDA and return its intent.
async function createSettlementOwnedOrder(
  svm: LiteSVM,
  payer: TransactionSigner,
  authority: TransactionSigner,
): Promise<OrderIntentArgs> {
  const [statePda] = await findStatePda();
  const intent = await buildOrderIntent({ owner: statePda });
  const create = await getCreateSettlementOwnedOrderInstructionAsync({
    authority,
    createdBy: payer,
    intent,
  });
  await sendInstruction(svm, payer, create, "createSettlementOwnedOrder");
  return intent;
}

describe("cancelSettlementOwnedOrder", () => {
  let svm: LiteSVM;

  beforeEach(() => {
    svm = newSvm();
  });

  it("cancels an order owned by the state PDA", async () => {
    const { payer, settlementOwnedOrderAuthority } = await initializeProgram(svm);
    const intent = await createSettlementOwnedOrder(svm, payer, settlementOwnedOrderAuthority);

    // The instruction carries no intent, so orderPda can't be resolved from
    // its arguments: the caller derives it.
    const cancel = await getCancelOrderInstructionAsync({
      owner: settlementOwnedOrderAuthority,
      orderPda: await findOrderPda(intent),
      createdBy: settlementOwnedOrderAuthority,
      intent,
    });
    await sendInstruction(svm, payer, cancel, "cancelOrder");

    const { cancelled, intent: decodedIntent } = await fetchOrderAccount(svm, intent);
    expect(cancelled).toBe(true);
    expect(decodedIntent).toEqual(intent);
  });
});
