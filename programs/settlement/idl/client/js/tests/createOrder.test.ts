import { LiteSVM } from "litesvm";
import { beforeEach, describe, expect, it } from "vitest";
import { generateKeyPairSigner, lamports } from "@solana/kit";
import { getCreateOrderInstructionAsync } from "../src/generated";
import { buildOrderIntent, fetchOrderAccount, newSvm, sendInstruction } from "./fixtures";

describe("createOrder", () => {
  let svm: LiteSVM;

  beforeEach(() => {
    svm = newSvm();
  });

  it("creates an order account matching the submitted intent", async () => {
    const owner = await generateKeyPairSigner();
    svm.airdrop(owner.address, lamports(1_000_000_000n));

    const intent = await buildOrderIntent({ owner: owner.address });
    const instruction = await getCreateOrderInstructionAsync({
      owner,
      createdBy: owner,
      intent,
    });
    await sendInstruction(svm, owner, instruction, "createOrder");

    const {
      discriminator,
      bump,
      cancelled,
      amountWithdrawn,
      amountReceived,
      createdBy,
      intent: decodedIntent,
      ...rest
    } = await fetchOrderAccount(svm, intent);
    // Compile error the day someone adds a field to OrderAccount and doesn't list it above:
    const _: Record<string, never> = rest;

    expect(typeof discriminator).toBe("object");
    expect(typeof bump).toBe("number");
    expect(cancelled).toBe(false);
    expect(amountWithdrawn).toBe(0n);
    expect(amountReceived).toBe(0n);
    expect(createdBy).toBe(owner.address);
    expect(decodedIntent).toEqual(intent);
  });
});
