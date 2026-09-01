import { copyFileSync, mkdirSync } from "node:fs";
import {
  createFromRoot,
  resolverValueNode,
  argumentValueNode,
  setInstructionAccountDefaultValuesVisitor,
} from "codama";
import { rootNodeFromAnchor } from "@codama/nodes-from-anchor";
import { renderVisitor } from "@codama/renderers-js";
import IDL from "./cow_settlement.json" with {type: 'json'};

const codama = createFromRoot(rootNodeFromAnchor(IDL));

// order_pda seed generation requires hashing the input intent in `createOrder`
// so we use codama's `resolverValueNode` to inject custom code for this.
codama.update(
  setInstructionAccountDefaultValuesVisitor([
    {
      instruction: "createOrder",
      account: "orderPda",
      defaultValue: resolverValueNode("resolveOrderPda", {
        dependsOn: [argumentValueNode("intent")],
      }),
    },
  ]),
);

codama.accept(
  renderVisitor("./client/js", {
    // resolveOrderPda hashes with crypto.subtle.digest, which is async, so
    // codama must await it and only wire it into the *Async instruction
    // builder rather than the sync one.
    asyncResolvers: ["resolveOrderPda"],
  }),
);

// codama inlines each PDA's const seeds into the function that derives it and
// exports no constant for them, so resolveOrderPda — which builds the order PDA
// seeds by hand — has nothing to import. Ship the IDL itself alongside the
// rendered client and let the resolver read the seed it needs off it, so the
// version-dependent bytes keep exactly one source and follow it across version
// bumps. Copied after the render because renderVisitor wipes its output
// directory first.
mkdirSync("./client/js/src/generated", { recursive: true });

copyFileSync("./cow_settlement.json", "./client/js/src/generated/idl.json");
