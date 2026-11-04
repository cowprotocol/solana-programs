import { LiteSVM } from "litesvm";
import { beforeEach, describe, expect, it } from "vitest";
import { findStatePda, getCreateSettlementOwnedOrderInstructionAsync } from "../src/generated";
import {
  buildOrderIntent,
  fetchOrderAccount,
  initializeProgram,
  newSvm,
  sendInstruction,
} from "./fixtures";

describe("createSettlementOwnedOrder", () => {
  let svm: LiteSVM;

  beforeEach(() => {
    svm = newSvm();
  });

  it("resolves the order PDA and creates an order owned by the state PDA", async () => {
    // Put a settlement-owned-order authority on record so it can place the order.
    const { payer, settlementOwnedOrderAuthority } = await initializeProgram(svm);

    // A settlement-owned order must be owned by the state PDA.
    const [statePda] = await findStatePda();
    const intent = await buildOrderIntent({ owner: statePda });

    // orderPda is omitted on purpose: the codama resolver must derive it from
    // the intent, the same as it does for createOrder.
    const instruction = await getCreateSettlementOwnedOrderInstructionAsync({
      authority: settlementOwnedOrderAuthority,
      createdBy: payer,
      intent,
    });
    await sendInstruction(svm, payer, instruction, "createSettlementOwnedOrder");

    const {
      cancelled,
      amountWithdrawn,
      amountReceived,
      createdBy,
      intent: decodedIntent,
    } = await fetchOrderAccount(svm, intent);
    expect(decodedIntent).toEqual(intent);
    expect(createdBy).toBe(payer.address);
    expect(cancelled).toBe(false);
    expect(amountWithdrawn).toBe(0n);
    expect(amountReceived).toBe(0n);
  });
});
