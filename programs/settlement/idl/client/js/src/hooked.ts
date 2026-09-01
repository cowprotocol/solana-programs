// Hand-written — Codama's "hooked" convention: functions referenced by a
// `resolverValueNode` default value live here, imported by generated code
// via the fixed "../../hooked" specifier (see generate.mjs). Never touched
// by rendering, which only manages src/generated.
//
// create_order's order PDA is seeded by [SETTLEMENT_SEED, sha256(intent_bytes), "order"].
// The middle seed is a hash of the whole `intent` argument rather than a plain
// field/account reference, which the Anchor PDA-seed grammar (const / arg / account)
// can't express — see cow_settlement.json's create_order docs — so it's handed off
// to this resolver instead of a plain `pda` node. Codama calls this with a
// `resolverScope` object and splices the returned `{ value }` onto the account
// it's resolving, so its shape is dictated by the renderer, not chosen here.

import { getProgramDerivedAddress, type Address } from "@solana/kit";
import { getOrderIntentEncoder, type OrderIntentArgs } from "./generated";
import IDL from "./generated/idl.json" with { type: "json" };

/**
 * The slice of the IDL this file reads. Annotating the import replaces the shape
 * TypeScript infers from the JSON — a union over every instruction, account and
 * seed kind — with just the path walked below, on which `pda` is reachable.
 */
type SeedBearingIdl = {
  instructions: {
    name: string;
    accounts: {
      name: string;
      pda?: { seeds: { kind: string; value?: number[] }[] };
    }[];
  }[];
};

/**
 * Prefix seed shared by every PDA this program derives: the ASCII "settlement v"
 * followed by the program's major.minor version, right-padded with spaces to a
 * fixed 19 bytes. The fixed width keeps one version's seeds from being a prefix
 * of another's. These bytes change on every minor version bump, so they're read
 * off the IDL — the state PDA's sole const seed — rather than written out here.
 */
const SETTLEMENT_SEED: Uint8Array = (() => {
  const idl: SeedBearingIdl = IDL;
  const seed = idl.instructions
    .find((instruction) => instruction.name === "initialize")
    ?.accounts.find((account) => account.name === "state_pda")
    ?.pda?.seeds[0]?.value;
  if (!seed) {
    throw new Error("IDL: initialize's state_pda has no const seed");
  }
  return new Uint8Array(seed);
})();

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
    seeds: [SETTLEMENT_SEED, orderUid, "order"],
  });
  return { value: address };
}
