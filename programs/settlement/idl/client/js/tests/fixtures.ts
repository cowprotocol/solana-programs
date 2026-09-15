import path from "node:path";
import {
  appendTransactionMessageInstruction,
  assertAccountExists,
  createTransactionMessage,
  generateKeyPairSigner,
  pipe,
  setTransactionMessageFeePayerSigner,
  signTransactionMessageWithSigners,
  type Address,
  type Instruction,
  type TransactionSigner,
} from "@solana/kit";
import { LiteSVM } from "litesvm";
import { encodeFlags } from "../src/order";
import {
  COW_SETTLEMENT_PROGRAM_ADDRESS,
  getOrderAccountDecoder,
  OrderKind,
  type OrderIntentArgs,
} from "../src/generated";
import { resolveOrderPda } from "../src/hooked";

export const COW_SETTLEMENT_SO_PATH = path.join(
  import.meta.dirname,
  "../../../../../../target/deploy/cow_settlement.so",
);

/// A fresh LiteSVM with the settlement program deployed under its canonical address.
export function newSvm(): LiteSVM {
  const svm = new LiteSVM();
  svm.addProgramFromFile(COW_SETTLEMENT_PROGRAM_ADDRESS, COW_SETTLEMENT_SO_PATH);
  return svm;
}

/// Sign `instruction` with `feePayer` and submit it, throwing a labelled error
/// (with the program logs) if it fails.
export async function sendInstruction(
  svm: LiteSVM,
  feePayer: TransactionSigner,
  instruction: Instruction,
  label: string,
): Promise<void> {
  const tx = await pipe(
    createTransactionMessage({ version: 0 }),
    (t) => setTransactionMessageFeePayerSigner(feePayer, t),
    (t) => svm.setTransactionMessageLifetimeUsingLatestBlockhash(t),
    (t) => appendTransactionMessageInstruction(instruction, t),
    signTransactionMessageWithSigners,
  );
  const result = svm.sendTransaction(tx);
  if ("err" in result) {
    throw new Error(`${label} failed: ${result.toString()}\n${result.meta().prettyLogs()}`);
  }
}

/// Resolve the order PDA `intent` hashes to, assert the account exists, and
/// return its decoded body.
export async function fetchOrderAccount(svm: LiteSVM, intent: OrderIntentArgs) {
  const { value: orderPda } = await resolveOrderPda({
    programAddress: COW_SETTLEMENT_PROGRAM_ADDRESS,
    args: { intent },
  });
  const account = svm.getAccount(orderPda);
  assertAccountExists(account);
  return getOrderAccountDecoder().decode(account.data);
}

export async function buildOrderIntent(
  overrides: Partial<OrderIntentArgs> & { owner: Address },
): Promise<OrderIntentArgs> {
  // create_order doesn't actually check the token accounts or mints the intent names, so
  // they only have to be distinct addresses.
  const [sellTokenAccount, sellMint, buyTokenAccount, buyMint] = await Promise.all([
    generateKeyPairSigner(),
    generateKeyPairSigner(),
    generateKeyPairSigner(),
    generateKeyPairSigner(),
  ]);
  return {
    sellTokenAccount: sellTokenAccount.address,
    sellMint: sellMint.address,
    buyTokenAccount: buyTokenAccount.address,
    buyMint: buyMint.address,
    sellAmount: 1_000_000n,
    buyAmount: 2_000_000n,
    validTo: Math.floor(Date.now() / 1000) + 3600,
    flags: encodeFlags({
      createdOnChain: true,
      kind: OrderKind.Sell,
      partiallyFillable: false,
    }),
    appData: new Uint8Array(32),
    ...overrides,
  };
}
