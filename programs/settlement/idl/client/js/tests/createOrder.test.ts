import { LiteSVM } from "litesvm";
import { beforeEach, describe, expect, it } from "vitest";
import {
  appendTransactionMessageInstruction,
  assertAccountExists,
  createTransactionMessage,
  generateKeyPairSigner,
  lamports,
  pipe,
  setTransactionMessageFeePayerSigner,
  signTransactionMessageWithSigners,
} from "@solana/kit";
import {
  COW_SETTLEMENT_PROGRAM_ADDRESS,
  getCreateOrderInstructionAsync,
  getOrderAccountDecoder,
} from "../src/generated";
import { resolveOrderPda } from "../src/hooked";
import { buildOrderIntent, COW_SETTLEMENT_SO_PATH } from "./fixtures";

describe("createOrder", () => {
  let svm: LiteSVM;

  beforeEach(() => {
    svm = new LiteSVM();
    svm.addProgramFromFile(COW_SETTLEMENT_PROGRAM_ADDRESS, COW_SETTLEMENT_SO_PATH);
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

    const tx = await pipe(
      createTransactionMessage({ version: 0 }),
      (t) => setTransactionMessageFeePayerSigner(owner, t),
      (t) => svm.setTransactionMessageLifetimeUsingLatestBlockhash(t),
      (t) => appendTransactionMessageInstruction(instruction, t),
      signTransactionMessageWithSigners,
    );

    const result = svm.sendTransaction(tx);
    if ("err" in result) {
      throw new Error(
        `createOrder failed: ${result.toString()}\n${result.meta().prettyLogs()}`,
      );
    }

    const { value: orderPda } = await resolveOrderPda({
      programAddress: COW_SETTLEMENT_PROGRAM_ADDRESS,
      args: { intent },
    });
    
    const account = svm.getAccount(orderPda);
    expect(account.exists).toBe(true);
    assertAccountExists(account);

    const { cancelled, amountWithdrawn, amountReceived, createdBy, intent: decodedIntent, ...rest } = getOrderAccountDecoder().decode(account.data);
    // Compile error the day someone adds a field to OrderAccount and doesn't list it above:
    const _: Record<string, never> = rest;

    expect(cancelled).toBe(false);
    expect(amountWithdrawn).toBe(0n);
    expect(amountReceived).toBe(0n);
    expect(createdBy).toBe(owner.address);
    expect(decodedIntent).toEqual(intent);
  });
});
