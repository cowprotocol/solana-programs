import { copyFileSync, mkdirSync } from "node:fs";
import {
  createFromRoot,
  resolverValueNode,
  argumentValueNode,
  bottomUpTransformerVisitor,
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
    ["createOrder", "createSettlementOwnedOrder"].map((instruction) => ({
      instruction,
      account: "orderPda",
      defaultValue: resolverValueNode("resolveOrderPda", {
        dependsOn: [argumentValueNode("intent")],
      }),
    })),
  ),
);

// PDAs are named after the IDL account they derive, and codama names each
// PDA's finder `find<Name>Pda`, so `state_pda` would become `findStatePdaPda`.
// Rename the PDAs (and every link to them) to drop the redundant suffix.
const PDA_RENAMES = { statePda: "state", bufferPda0: "buffer" };
codama.update(
  bottomUpTransformerVisitor(
    ["pdaNode", "pdaLinkNode"].map((kind) => ({
      select: (path) => {
        const node = path.at(-1);
        return node.kind === kind && node.name in PDA_RENAMES;
      },
      transform: (node) => ({ ...node, name: PDA_RENAMES[node.name] }),
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
