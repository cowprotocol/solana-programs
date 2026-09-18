import { LiteSVM } from "litesvm";
import { beforeEach, describe, expect, it } from "vitest";
import { generateKeyPairSigner, lamports } from "@solana/kit";
import {
  findStatePdaPda,
  getCreateSelfOrderInstructionAsync,
  getInitializeInstructionAsync,
} from "../src/generated";
import { buildOrderIntent, fetchOrderAccount, newSvm, sendInstruction } from "./fixtures";

describe("createSelfOrder", () => {
  let svm: LiteSVM;

  beforeEach(() => {
    svm = newSvm();
  });

  it("resolves the order PDA and creates an order owned by the state PDA", async () => {
    const [payer, manager, solverAuthority, reclaimAuthority, selfOrderAuthority] =
      await Promise.all(Array.from({ length: 5 }, () => generateKeyPairSigner()));
    svm.airdrop(payer.address, lamports(1_000_000_000n));

    // Put a self-order authority on record so it can place the order.
    const initialize = await getInitializeInstructionAsync({
      payer,
      manager: manager.address,
      solverAuthority: solverAuthority.address,
      reclaimAuthority: reclaimAuthority.address,
      selfOrderAuthority: selfOrderAuthority.address,
    });
    await sendInstruction(svm, payer, initialize, "initialize");

    // A self order must be owned by the state PDA.
    const [statePda] = await findStatePdaPda();
    const intent = await buildOrderIntent({ owner: statePda });

    // orderPda is omitted on purpose: the codama resolver must derive it from
    // the intent, the same as it does for createOrder.
    const instruction = await getCreateSelfOrderInstructionAsync({
      authority: selfOrderAuthority,
      createdBy: payer,
      intent,
    });
    await sendInstruction(svm, payer, instruction, "createSelfOrder");

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
