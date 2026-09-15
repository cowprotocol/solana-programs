import { LiteSVM } from "litesvm";
import { beforeEach, describe, expect, it } from "vitest";
import { generateKeyPairSigner, lamports } from "@solana/kit";
import {
  findStatePdaPda,
  getCreateWithdrawalOrderInstructionAsync,
  getInitializeInstructionAsync,
} from "../src/generated";
import { buildOrderIntent, fetchOrderAccount, newSvm, sendInstruction } from "./fixtures";

describe("createWithdrawalOrder", () => {
  let svm: LiteSVM;

  beforeEach(() => {
    svm = newSvm();
  });

  it("resolves the order PDA and creates an order owned by the state PDA", async () => {
    const [payer, manager, reclaimAuthority, withdrawalAuthority] = await Promise.all([
      generateKeyPairSigner(),
      generateKeyPairSigner(),
      generateKeyPairSigner(),
      generateKeyPairSigner(),
    ]);
    svm.airdrop(payer.address, lamports(1_000_000_000n));

    // Put a withdrawal authority on record so it can place the order.
    const initialize = await getInitializeInstructionAsync({
      payer,
      manager: manager.address,
      reclaimAuthority: reclaimAuthority.address,
      withdrawalAuthority: withdrawalAuthority.address,
    });
    await sendInstruction(svm, payer, initialize, "initialize");

    // A withdrawal order must be owned by the state PDA.
    const [statePda] = await findStatePdaPda();
    const intent = await buildOrderIntent({ owner: statePda });

    // orderPda is omitted on purpose: the codama resolver must derive it from
    // the intent, the same as it does for createOrder.
    const instruction = await getCreateWithdrawalOrderInstructionAsync({
      authority: withdrawalAuthority,
      payer,
      intent,
    });
    await sendInstruction(svm, payer, instruction, "createWithdrawalOrder");

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
