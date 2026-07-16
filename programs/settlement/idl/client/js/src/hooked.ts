// Hand-written — Codama's "hooked" convention: functions referenced by a
// `resolverValueNode` default value live here, imported by generated code
// via the fixed "../../hooked" specifier (see generate.mjs). Never touched
// by rendering, which only manages src/generated.
//
// create_order's order PDA is seeded by ["settlement", sha256(borsh(intent)), "order"].
// The middle seed is a hash of the whole `intent` argument rather than a plain
// field/account reference, which the Anchor PDA-seed grammar (const / arg / account)
// can't express — see cow_settlement.json's create_order docs — so it's handed off
// to this resolver instead of a plain `pda` node. Codama calls this with a
// `resolverScope` object and splices the returned `{ value }` onto the account
// it's resolving, so its shape is dictated by the renderer, not chosen here.

import { getProgramDerivedAddress, type Address } from "@solana/kit";
import { getOrderIntentEncoder, type OrderIntentArgs } from "./generated";

export async function resolveOrderPda({
  programAddress,
  args,
}: {
  programAddress: Address;
  args: { intent: OrderIntentArgs };
}): Promise<{ value: Address }> {
  const intentBytes = getOrderIntentEncoder().encode(args.intent);
  const orderUid = new Uint8Array(
    await crypto.subtle.digest("SHA-256", intentBytes),
  );
  const [address] = await getProgramDerivedAddress({
    programAddress,
    seeds: ["settlement", orderUid, "order"],
  });
  return { value: address };
}
