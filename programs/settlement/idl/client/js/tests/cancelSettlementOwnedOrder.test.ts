import { LiteSVM } from "litesvm";
import { beforeEach, describe, expect, it } from "vitest";
import { generateKeyPairSigner, lamports } from "@solana/kit";
import {
  COW_SETTLEMENT_PROGRAM_ADDRESS,
  findStatePda,
  getCancelOrderInstructionAsync,
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

describe("cancelOrder on a settlement-owned order", () => {
  let svm: LiteSVM;

  beforeEach(() => {
    svm = newSvm();
  });

  /// Initializes the program and returns a funded payer and the
  /// settlement-owned-order authority.
  async function initialize() {
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
    return { payer, settlementOwnedOrderAuthority };
  }

  it("cancels an order owned by the state PDA", async () => {
    const { payer, settlementOwnedOrderAuthority } = await initialize();
    const [statePda] = await findStatePda();
    const intent = await buildOrderIntent({ owner: statePda });
    const create = await getCreateSettlementOwnedOrderInstructionAsync({
      authority: settlementOwnedOrderAuthority,
      createdBy: payer,
      intent,
    });
    await sendInstruction(svm, payer, create, "createSettlementOwnedOrder");

    // cancelOrder has no orderPda resolver, so the caller derives it. The
    // state PDA is filled in by default.
    const { value: orderPda } = await resolveOrderPda({
      programAddress: COW_SETTLEMENT_PROGRAM_ADDRESS,
      args: { intent },
    });
    const cancel = await getCancelOrderInstructionAsync({
      owner: settlementOwnedOrderAuthority,
      createdBy: payer,
      orderPda,
      intent,
    });
    await sendInstruction(svm, payer, cancel, "cancelOrder");

    const { cancelled, intent: decodedIntent } = await fetchOrderAccount(svm, intent);
    expect(cancelled).toBe(true);
    expect(decodedIntent).toEqual(intent);
  });
});
