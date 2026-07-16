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
