import { createFromRoot } from "codama";
import { rootNodeFromAnchor } from "@codama/nodes-from-anchor";
import { renderVisitor } from "@codama/renderers-rust";
import IDL from "./squads_multisig_program.json" with { type: "json" };

// The CLI only proposes and approves vault transactions, so only render what
// that needs rather than the whole Squads program.
const INSTRUCTIONS = ["vaultTransactionCreate", "proposalCreate", "proposalApprove"];
const ACCOUNTS = ["Multisig", "Proposal", "VaultTransaction"];

const instructions = IDL.instructions.filter(({ name }) => INSTRUCTIONS.includes(name));
const accounts = IDL.accounts.filter(({ name }) => ACCOUNTS.includes(name));

// Keep the types the kept instructions and accounts reference, transitively.
const typesByName = new Map(IDL.types.map((type) => [type.name, type]));
const types = new Map();
const collect = (node) => {
  if (Array.isArray(node)) {
    node.forEach(collect);
  } else if (node !== null && typeof node === "object") {
    const name = node.defined;
    if (typeof name === "string" && !types.has(name)) {
      types.set(name, typesByName.get(name));
      collect(typesByName.get(name));
    }
    Object.values(node).forEach(collect);
  }
};
collect([instructions, accounts]);

const codama = createFromRoot(
  rootNodeFromAnchor({ ...IDL, instructions, accounts, types: [...types.values()] }),
);

// The renderer assumes the output is the root of its own crate. Point its
// imports at the module it actually lands in. The program ID constant is still
// referenced from the crate root, so `main.rs` re-exports it there.
const MODULE = "crate::utils::squads::program";

codama.accept(
  renderVisitor("..", {
    generatedFolder: "src/utils/squads/program",
    dependencyMap: {
      generated: MODULE,
      generatedAccounts: `${MODULE}::accounts`,
      generatedErrors: `${MODULE}::errors`,
      generatedInstructions: `${MODULE}::instructions`,
      generatedTypes: `${MODULE}::types`,
    },
    // The output is formatted by `cargo fmt` with the rest of the crate.
    formatCode: false,
    anchorTraits: false,
  }),
);
