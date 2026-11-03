import { LiteSVM } from "litesvm";
import { beforeEach, describe, expect, it } from "vitest";
import { generateKeyPairSigner, lamports } from "@solana/kit";
import {
  COW_SETTLEMENT_PROGRAM_ADDRESS,
  findStatePda,
  getCancelSettlementOwnedOrderInstructionAsync,
  getCreateSettlementOwnedOrderInstructionAsync,
  getInitializeInstructionAsync,
} from "../src/generated";
import { resolveOrderPda } from "../src/hooked";
import {
  buildOrderIntent,
  initializerSigner,
  fetchOrderAccount,
  newSvm,
  sendInstruction,
  sendUnverifiedInstruction,
} from "./fixtures";

describe("cancelSettlementOwnedOrder", () => {
  let svm: LiteSVM;

  beforeEach(() => {
    svm = newSvm();
  });

  it("cancels an order owned by the state PDA", async () => {
    const [payer, manager, solverAuthority, reclaimAuthority, settlementOwnedOrderAuthority] =
      await Promise.all(Array.from({ length: 5 }, () => generateKeyPairSigner()));
    svm.airdrop(payer.address, lamports(1_000_000_000n));

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

    const [statePda] = await findStatePda();
    const intent = await buildOrderIntent({ owner: statePda });
    const create = await getCreateSettlementOwnedOrderInstructionAsync({
      authority: settlementOwnedOrderAuthority,
      createdBy: payer,
      intent,
    });
    await sendInstruction(svm, payer, create, "createSettlementOwnedOrder");

    // The instruction carries no intent, so orderPda can't be resolved from
    // its arguments: the caller derives it.
    const { value: orderPda } = await resolveOrderPda({
      programAddress: COW_SETTLEMENT_PROGRAM_ADDRESS,
      args: { intent },
    });
    const cancel = await getCancelSettlementOwnedOrderInstructionAsync({
      authority: settlementOwnedOrderAuthority,
      orderPda,
    });
    await sendInstruction(svm, payer, cancel, "cancelSettlementOwnedOrder");

    const { cancelled, intent: decodedIntent } = await fetchOrderAccount(svm, intent);
    expect(cancelled).toBe(true);
    expect(decodedIntent).toEqual(intent);
  });
});
