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

// order_pda seed generation requires hashing the input intent, which codama
// can't express from the IDL, so we inject a custom `resolveOrderPda` resolver
// for every instruction that creates an order at the canonical order PDA.
codama.update(
  setInstructionAccountDefaultValuesVisitor(
    ["createOrder", "createSelfOrder"].map((instruction) => ({
      instruction,
      account: "orderPda",
      defaultValue: resolverValueNode("resolveOrderPda", {
        dependsOn: [argumentValueNode("intent")],
      }),
    })),
  ),
);

// build the TS library
codama.accept(
  renderVisitor("./client/js", {
    asyncResolvers: ["resolveOrderPda"],
  }),
);

// add a copy of the IDl JSON to the generated output. Useful for resolved node hooks.
mkdirSync("./client/js/src/generated", { recursive: true });

copyFileSync("./cow_settlement.json", "./client/js/src/generated/idl.json");
