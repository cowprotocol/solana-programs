import { LiteSVM } from "litesvm";
import { beforeEach, describe, expect, it } from "vitest";
import { generateKeyPairSigner, lamports } from "@solana/kit";
import {
  findStatePda,
  getCreateSettlementOwnedOrderInstructionAsync,
  getInitializeInstructionAsync,
} from "../src/generated";
import {
  buildOrderIntent,
  initializerSigner,
  fetchOrderAccount,
  newSvm,
  sendInstruction,
  sendUnverifiedInstruction,
} from "./fixtures";

describe("createSettlementOwnedOrder", () => {
  let svm: LiteSVM;

  beforeEach(() => {
    svm = newSvm();
  });

  it("resolves the order PDA and creates an order owned by the state PDA", async () => {
    const [payer, manager, solverAuthority, reclaimAuthority, settlementOwnedOrderAuthority] =
      await Promise.all(Array.from({ length: 5 }, () => generateKeyPairSigner()));
    svm.airdrop(payer.address, lamports(1_000_000_000n));

    // Put a settlement-owned-order authority on record so it can place the order.
    const initializer = initializerSigner();
    svm.airdrop(initializer.address, lamports(1_000_000_000n));
    const initialize = await getInitializeInstructionAsync({
      payer: initializer,
      manager: manager.address,
      solverAuthority: solverAuthority.address,
      reclaimAuthority: reclaimAuthority.address,
      settlementOwnedOrderAuthority: settlementOwnedOrderAuthority.address,
    });
    await sendUnverifiedInstruction(svm, payer, initialize, "initialize");

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
