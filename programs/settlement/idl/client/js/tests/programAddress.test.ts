import { describe, expect, it } from "vitest";
import { generateKeyPairSigner, type Address, type Instruction } from "@solana/kit";
import {
  COW_SETTLEMENT_PROGRAM_ADDRESS,
  findBufferPda,
  findNativeSolBufferPda,
  findStatePda,
  getAddSolverInstructionAsync,
  getBeginSettleInstructionAsync,
  getCreateBufferInstructionAsync,
  getCreateOrderInstructionAsync,
  getCreateSettlementOwnedOrderInstructionAsync,
  getFinalizeSettleInstructionAsync,
  getInitializeInstructionAsync,
  getReclaimBufferInstructionAsync,
  getRemoveSolverInstructionAsync,
  getTransferAuthorityInstructionAsync,
  Role,
} from "../src/generated";
import { resolveOrderPda } from "../src/hooked";
import { buildOrderIntent } from "./fixtures";

// Every PDA a builder can derive by itself, under `programAddress`.
async function derivablePdas(
  programAddress: Address,
  mint0: Address,
  intent: Parameters<typeof resolveOrderPda>[0]["args"]["intent"],
): Promise<Address[]> {
  const config = { programAddress };
  return [
    (await findStatePda(config))[0],
    (await findNativeSolBufferPda(config))[0],
    (await findBufferPda({ mint0 }, config))[0],
    (await resolveOrderPda({ programAddress, args: { intent } })).value,
  ];
}

describe("builders given another program address", async () => {
  const otherProgram = (await generateKeyPairSigner()).address;
  const [signer, other, mint0] = await Promise.all(
    Array.from({ length: 3 }, () => generateKeyPairSigner()),
  );
  const intent = await buildOrderIntent({ owner: signer.address });
  const config = { programAddress: otherProgram };

  const builders: { name: string; build: () => Promise<Instruction> }[] = [
    {
      name: "initialize",
      build: () =>
        getInitializeInstructionAsync(
          {
            payer: signer,
            manager: other.address,
            solverAuthority: other.address,
            reclaimAuthority: other.address,
            settlementOwnedOrderAuthority: other.address,
          },
          config,
        ),
    },
    {
      name: "createBuffer",
      build: () => getCreateBufferInstructionAsync({ payer: signer, mint0: mint0.address }, config),
    },
    {
      name: "createOrder",
      build: () =>
        getCreateOrderInstructionAsync({ owner: signer, createdBy: signer, intent }, config),
    },
    {
      name: "createSettlementOwnedOrder",
      build: () =>
        getCreateSettlementOwnedOrderInstructionAsync(
          { authority: signer, createdBy: signer, intent },
          config,
        ),
    },
    {
      name: "beginSettle",
      build: () =>
        getBeginSettleInstructionAsync(
          { solver: signer, finalizeIxIndex: 1, auctionId: 0n },
          config,
        ),
    },
    {
      name: "finalizeSettle",
      build: () => getFinalizeSettleInstructionAsync({ beginIxIndex: 0 }, config),
    },
    {
      name: "reclaimBuffer",
      build: () =>
        getReclaimBufferInstructionAsync(
          { reclaimAuthority: signer, reclaimRecipient: other.address, mint0: mint0.address },
          config,
        ),
    },
    {
      name: "transferAuthority",
      build: () =>
        getTransferAuthorityInstructionAsync(
          { signer, role: Role.Manager, newAuthority: other.address },
          config,
        ),
    },
    {
      name: "addSolver",
      build: () =>
        getAddSolverInstructionAsync(
          { authority: signer, payer: signer, solver: other.address },
          config,
        ),
    },
    {
      name: "removeSolver",
      build: () =>
        getRemoveSolverInstructionAsync(
          { authority: signer, rentRecipient: other.address, solver: other.address },
          config,
        ),
    },
  ];

  it.each(builders)("$name derives its PDAs under that address", async ({ build }) => {
    const canonicalPdas = await derivablePdas(
      COW_SETTLEMENT_PROGRAM_ADDRESS,
      mint0.address,
      intent,
    );
    const otherPdas = await derivablePdas(otherProgram, mint0.address, intent);

    const instruction = await build();
    const addresses = (instruction.accounts ?? []).map(({ address }) => address);

    expect(instruction.programAddress).toBe(otherProgram);
    expect(addresses.filter((address) => canonicalPdas.includes(address))).toEqual([]);
    expect(addresses.some((address) => otherPdas.includes(address))).toBe(true);
  });
});
