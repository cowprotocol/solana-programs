import { generateKeyPairSigner, lamports } from "@solana/kit";
import { describe, expect, it } from "vitest";
import {
  COW_SETTLEMENT_PROGRAM_ADDRESS,
  getCancelOrderInstruction,
  getReclaimOrderInstruction,
} from "../src/generated";
import { resolveOrderPda } from "../src/hooked";
import { buildOrderIntent, fetchOrderAccount, newSvm, sendInstruction } from "./fixtures";

describe("reclaimOrder", () => {
  it.each([false, true])(
    "reclaims a sponsored cancellation with owner authorization or expiry (expired=%s)",
    async (expired) => {
      const svm = newSvm();
      const [sponsor, owner] = await Promise.all([
        generateKeyPairSigner(),
        generateKeyPairSigner(),
      ]);
      svm.airdrop(sponsor.address, lamports(1_000_000_000n));
      const intent = await buildOrderIntent({ owner: owner.address });
      const { value: orderPda } = await resolveOrderPda({
        programAddress: COW_SETTLEMENT_PROGRAM_ADDRESS,
        args: { intent },
      });
      await sendInstruction(
        svm,
        sponsor,
        getCancelOrderInstruction({ owner, createdBy: sponsor, orderPda, intent }),
        "cancelOrder",
      );

      // Codama uses the program address as a placeholder for an omitted owner.
      await expect(
        sendInstruction(
          svm,
          sponsor,
          getReclaimOrderInstruction({ orderPda, reclaimRecipient: sponsor.address }),
          "reclaimOrder",
        ),
      ).rejects.toThrow("MissingRequiredSignature");
      expect((await fetchOrderAccount(svm, intent)).cancelled).toBe(true);

      if (expired) {
        const clock = svm.getClock();
        clock.unixTimestamp = BigInt(intent.validTo + 1);
        svm.setClock(clock);
        svm.expireBlockhash();
      }
      await sendInstruction(
        svm,
        sponsor,
        getReclaimOrderInstruction({
          orderPda,
          reclaimRecipient: sponsor.address,
          owner: expired ? undefined : owner,
        }),
        "reclaimOrder",
      );
      expect(svm.getAccount(orderPda).exists).toBe(false);
    },
  );
});
