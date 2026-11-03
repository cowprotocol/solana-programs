# Integrating Solana: Brief for Solvers

## What is being integrated?

CoW Protocol is expanding its settlement process to Solana. Solvers who want to take part in the Solana auction settle orders through the CoW settlement program, using the Rust client SDK (or the IDL) described below.

> [!NOTE]
> The APIs that matter to solvers are now in a stable state, and the reference driver, REST API and autopilot are built. CoW Protocol for Solana is still under active development, though, so expect occasional breaking changes between releases (see [Deployments](#cow-settlement-program-deployments)).

## How to Settle on Solana

To get started with testing, we suggest placing your own orders on-chain and settling them yourself in an isolated auction. This lets you build up your liquidity sources and test the most important flows end to end.

### Tooling

We provide an official [Rust client SDK](https://crates.io/crates/cow-settlement-client) for building instructions against the settlement program. The rest of this guide assumes you are using the `cow_settlement_client` crate. It re-exports the lower-level `cow_settlement_interface` crate as `cow_settlement_client::cow_settlement_interface`.

If you are *not* using Rust, there is a [Solana IDL](https://github.com/cowprotocol/solana-programs/blob/main/programs/settlement/idl/cow_settlement.json) that you can feed to Codama to generate a client. The repository includes a Codama-generated TypeScript client under [`programs/settlement/idl/client/js`](https://github.com/cowprotocol/solana-programs/tree/main/programs/settlement/idl/client/js) (build it with `just generate`). Some addresses and call patterns can't be derived from the IDL alone (PDA derivation, account ordering in `BeginSettle`/`FinalizeSettle`), so expect some manual work.

We recommend installing the [Solana Developer SDK](https://solana.com/docs/intro/installation) if you haven't already.

To place test orders for your solver, install the CoW testing CLI:

```sh
cargo install cow-test-cli@0.5.2
cargo install cow-test-cli@0.5.2 --locked
# Make sure ~/.cargo/bin is on your PATH; then this should run:
cow --help
```

You may also want to generate a key for your solver:

```sh
# Writes a new key to `~/.config/solana/id.json`
solana-keygen new
# Print out the address of your key
solana address
```

### CoW Settlement Program Deployments

There are two environments, each with its own settlement program and its own CoW API. As of v0.5.2, they are deployed to these addresses:

| Environment | API | Settlement program |
|---|---|---|
| Staging ("barn") | `https://api.barn.cow.fi` | `EzHqcdJssenE2R8xogJ8wpL66PRQet9M6ubpsX4QxpPb` |
| Production | `https://api.cow.fi` | `Moook87DzJ25dELx3LXJ3dnu5a4ERVFwKo1PWWF4p7Y` |

Both APIs run against **Solana mainnet beta**. Both programs are also deployed on **devnet**, but no API is connected there, so devnet is only useful for testing in isolation with your own orders.

The production address is also available as `cow_settlement_client::cow_settlement_interface::ID`, which is what the client and the CLI use by default. To target staging, pass its address as `program_id` to the instruction builders; with the CLI, add `--program-id EzHqcdJssenE2R8xogJ8wpL66PRQet9M6ubpsX4QxpPb`. The client derives every PDA (state, buffers, orders) from the program ID you pass, so the same crate works for both environments.

During the initial release phase, the address of the settlement program may change. Our team follows a pattern to indicate when a new address deployment has been completed:

* **Patch releases** (e.g. v0.5.1 → v0.5.2) upgrade the program in place, so the address doesn't change.
* **Minor releases** (e.g. v0.4 → v0.5) deploy to a **new address**. All data is effectively wiped, so users must re-approve, and orders and buffer balances from the previous version can't be used with the new one.

### How to upgrade to a newer version

Though there may be breaking changes on the minor/major releases, the process typically comes down to:

1. Update your Cargo crate dependency (if using rust) or IDL file and regeneate (if a different programming language)
2. Update the constants used for staging and production as defined for that release.

#### Solver allowlist

Only allowlisted solvers can settle. `BeginSettle` must be signed by a solver registered in the settlement state, otherwise it fails.

If your solver is non-colocated, the team will provide your solver address to you, and you only need to fund it with SOL.

If your **not** coloated, you should generate your own addresses. We recommend doing this with KMS or a similar key management solution.
Allowlisting occurs separately on each environment.

### Placing an Order

> [!WARNING]
> While the settlement program is nearing final release, it has **not yet been audited**. Only use play/testing funds, and don't approve the settlement program from a wallet that holds significant funds.

You can place orders with the CLI, without the CoW API, for testing your own settlements. Every order field is stored in plain form on-chain, so a driver can read the order directly.

```sh
cow sell 0.1 SOL for 5 USDC --keypair <my-key.json> --rpc-url https://api.<net>.solana.com
cow buy 5 USDC for 0.1 SOL --keypair <my-key.json> --rpc-url https://api.<net>.solana.com
```

Run `cow sell --help` for every supported form. Tokens can be a built-in symbol (ex. `SOL`, `WSOL`, `USDC`), a mint address or a token account address. `--valid-to` sets the expiry (default: 5 minutes from now) and `--partially-fillable` allows partial fills. When selling `SOL`, the CLI wraps it into your WSOL account first as needed.

This way of placing orders is for testing only. In production, orders reach solvers through the auction.

### Example Settlement

The CoW CLI includes an example settlement flow. It supports:
* coincidence of wants only: the orders must fund each other, and it won't touch existing buffer balances
* sell and buy orders, always filled completely at exactly their limit price
* SPL Token and Token-2022 tokens
* **not** orders that buy native SOL

There is also an [experimental branch](https://github.com/cowprotocol/solana-programs/tree/kaze/sc-144-test-settlement-with-on-chain-liquidity) with support for trading on the [Orca DEX](https://www.orca.so/pools) which can serve as a good reference for getting started.

> [!CAUTION]
> The experimental branch is based on a pre-v0.3 version of the program and doesn't match the current client API. Use it only as a reference for the general approach to routing through on-chain liquidity.

Your keypair must be a [whitelisted solver](#solver-whitelist) for the environment you target (add `--program-id` for staging). Settle placed orders by order PDA address or CoW order UID:

```sh
cow settle <order A> <order B> <order C>... --keypair <my-key.json>
# Build and simulate without sending:
cow settle <order A> <order B> --keypair <my-key.json> --dry-run
```

For convenience, `cow settle` creates any missing buffers before settling. Solvers settling in the auction shouldn't rely on doing this (see [Buffers](#buffers)).

Read on for how your solver can build its own settlement and which pieces matter most.

#### Typical Instructions on Order Placement

The user does all of the following, usually in a single transaction:

* Approve the settlement **state PDA** (not the settlement program) as delegate on the sell token account (`find_state_pda`).
  * NOTE: unlike on Ethereum, a token account has only one delegate. An approval can be overwritten by interacting with another dapp, which makes the order unsettleable.
* Create and fund the buffer for the buy token (`CreateBuffers`), if it doesn't exist yet. See [Buffers](#buffers).
* Create the order PDA with the `CreateOrder` instruction, signed by the order owner. The rent payer (`created_by`) gets the rent back when the order is reclaimed.

Oftentimes, this transaction is submitted through a sponsored flow in which the CoW API only creates the order in the block preceeding actual settlement.

A typical solver settlement should contain all of the following:
* If applicable, creating any buy-side token ATAs that the user will need to receive. The funding for the buy ATA creation is covered by the solver
and should deduct it from the settlement output.
* The settlement itself: the swap route including `BeginSettle` and `FinalizeSettle`.
* For orders which were settled, return the order PDAs rent back to the creators with `ReclaimOrder`. This costs the solver a bit of gas and sends the order PDA rent back to `created_by`. Solvers will be reimbursed the added CU cost of doing this during the settlement.

### The Shape of a Solana Settlement

A settlement is **one transaction** containing a matched pair of instructions:

```text
[ ...setup...            ] # e.g. create the users' buy ATAs if missing
[ i   ] BeginSettle        # pulls user sell funds
[ ... ] interactions       # any instructions you like
[ j   ] FinalizeSettle     # sends swap output to users
[ ...reclaim orders...   ] # Returns the cost of rent back to the users who created it
```

`BeginSettle` carries `finalize_ix_index = j` and `FinalizeSettle` carries `begin_ix_index = i`. Each is checked to point back at the other. No other `BeginSettle`/`FinalizeSettle` of the settlement program may sit between the pair. Every other instruction can be whatever the settlement needs.

### The `OrderIntent`

This is the order the user places on-chain. Its parameters are similar to Ethereum's:

```rust
pub struct OrderIntent {
    /// Account authorized to create and invalidate this order and whose
    /// signature authenticates it.
    pub owner: Pubkey,

    /// What the order sells, and the token account the funds are pulled from.
    /// The settlement state PDA must hold the SPL `delegate` on it, and it
    /// must be owned by the intent owner.
    pub sell: TokenAsset, // { mint, token_account }

    /// What the order buys, and the account that receives the proceeds:
    /// either `Asset::TokenProgram(TokenAsset { mint, token_account })`, or
    /// `Asset::Native(account)` for native SOL paid out as lamports.
    pub buy: Asset,

    /// For `Sell` orders the exact amount to sell; for `Buy` orders the
    /// maximum to spend. Non-zero.
    pub sell_amount: NonZeroU64,

    /// For `Buy` orders the exact amount to receive; for `Sell` orders the
    /// minimum to receive. Non-zero.
    pub buy_amount: NonZeroU64,

    /// Unix timestamp after which the order can't be executed.
    pub valid_to: u32,

    /// `kind` (`OrderKind::Sell` / `OrderKind::Buy`) and `partially_fillable`.
    pub flags: Flags,

    /// Opaque 32 bytes, not interpreted on-chain.
    pub app_data: [u8; 32],
}
```

The order UID is `intent.uid()`, the SHA-256 of the 213-byte canonical encoding. The order PDA is `find_order_pda(&program_id, &intent.uid())`. To read an order from chain, fetch the PDA's account data and decode it with `cow_settlement_client::pda::order::DecodedOrderAccount::try_from(&data[..])`. That gives you the `intent`, the `cancelled` flag, and the cumulative `amount_withdrawn`/`amount_received` from previous partial fills.

### Buy Token Accounts

**Don't assume the user's buy token account exists.** If it doesn't, create it in the settlement transaction, before `FinalizeSettle`, with the Associated Token Account program's `CreateIdempotent` instruction. Pay for the rent yourself and pass the cost on to the user through a worse fill (pay out less of the buy token). The fill must still meet the order's limit price, so this only works when the order has enough surplus to cover the rent.

You can only create the account when `buy.token_account` is the associated token account of the **order owner** for the buy mint (and that mint's token program). If the recipient isn't the owner, the intent only names the token account address, not the wallet it belongs to, so you can't derive or create it. Such an order can't be settled until the recipient creates the account.

### Buffers

Buffers are token accounts owned by the settlement state PDA, one per mint (`find_buffer_pda(&program_id, &mint)`). `FinalizeSettle` pays each order out of the buffer for its buy mint, so before `FinalizeSettle` the solver has to get the swap outputs into those buffers. Unlike Ethereum, the settlement program doesn't execute interactions on your behalf. In most cases buffers act only as the **receivers** of swap outputs, which `FinalizeSettle` then forwards to users.

**The user creates the buffer PDA**, normally when placing the order. Solvers shouldn't have to create one. You can still add a `CreateBuffers` instruction if a buffer is missing, but you pay its rent and **won't be reimbursed**.

```rust
pub struct CreateBuffers<'a> {
    pub program_id: Pubkey,
    pub payer: Pubkey,
    pub token_program: TokenProgram,
    pub mints: &'a [Pubkey],
}
```

* `program_id`: the settlement program ID
* `payer`: the signer paying the rent
* `token_program`: `TokenProgram::SplToken` or `TokenProgram::Token2022`. All `mints` must belong to this program; mints from both programs need two instructions.
* `mints`: the mints that need buffers. Creation is idempotent, so an existing buffer is left alone.

Orders that **buy native SOL** ("lamports") are paid in lamports from the native SOL buffer (`find_native_sol_buffer_pda`). Fund it by transferring SOL into it before `FinalizeSettle`. A payout can't take the native SOL buffer below its rent-exempt minimum.

### Begin Settle

Pulls the users' sell funds. The funds go straight from each user's sell token account to whatever accounts you choose.

```rust
pub struct BeginSettle<'a> {
    pub program_id: Pubkey,
    pub solver: Pubkey,
    pub finalize_ix_index: u16,
    pub auction_id: i64,
    pub only_token_program: Option<TokenProgram>,
    pub orders: &'a [InitializedIntent<'a>],
}
```

* `program_id`: the settlement program ID
* `solver`: your whitelisted solver key. It must sign the transaction.
* `finalize_ix_index`: index of the paired `FinalizeSettle` instruction in the same transaction
* `auction_id`: not read on-chain, used for backend indexing. Use the ID of the auction you're settling (anything works for testing).
* `only_token_program`: `None` includes both the SPL Token and Token-2022 programs. If every transfer uses one program, `Some(...)` saves an account and some bytes.
* `orders`:

```rust
pub struct Pull {
    pub destination: Pubkey,
    pub amount: u64,
}

pub struct InitializedIntent<'a> {
    pub intent: &'a OrderIntent,
    pub pulls: &'a [Pull],
    pub use_transfer_checked: bool,
}
```

* `intent`: the order to settle (see [`OrderIntent`](#the-orderintent))
* `pulls`: transfers out of the order's sell token account. `destination` can be your own token account, a DEX input, a settlement buffer, etc.
* `use_transfer_checked`: use `TransferChecked` instead of `Transfer`. It costs more compute and an extra account, but some Token-2022 extensions (transfer fee, transfer hook, pausable) require it.

The builder sorts the orders by order PDA; the program requires them strictly increasing, so the same order can't appear twice. For every order, the program checks that it is:
* not cancelled and not expired
* fill-or-kill orders (`partially_fillable = false`) are filled completely in this settlement: for `Sell` orders the pulls total exactly `sell_amount`; for `Buy` orders the payout is exactly `buy_amount`
* not filled beyond its amount, counting earlier partial fills (`Sell`: total pulled ≤ `sell_amount`; `Buy`: total paid out ≤ `buy_amount`)
* paid at or above its limit price **in this settlement alone**: `paid_out * sell_amount >= pulled * buy_amount`

### Finalize Settle

Sends the swap outputs to users. Each order is paid from the settlement buffer for its buy mint (or the native SOL buffer) into the intent's buy account.

```rust
pub struct FinalizeSettle<'a> {
    pub program_id: Pubkey,
    pub begin_ix_index: u16,
    pub only_token_program: Option<TokenProgram>,
    pub orders: &'a [FinalizedIntent<'a>],
}
```

* `program_id`: same as `BeginSettle`
* `begin_ix_index`: index of the paired `BeginSettle` instruction
* `only_token_program`: same as `BeginSettle`
* `orders`:

```rust
pub struct FinalizedIntent<'a> {
    pub intent: &'a OrderIntent,
    pub amount: u64,
    pub use_transfer_checked: bool,
}
```

* `intent`: same as `BeginSettle`. Include exactly one entry for each order in `BeginSettle`.
* `amount`: how much of the buy token (or how many lamports) the user receives
* `use_transfer_checked`: same as `BeginSettle` (ignored for native SOL payouts)

The source buffer and destination come from the intent; the builder sorts the entries so they line up with `BeginSettle`.

### Fee-on-Transfer Tokens

Token-2022 mints with the transfer fee extension withhold a fee from what each transfer delivers. The sender is debited `amount` and the destination receives `amount - fee`. The settlement does **nothing special** about these tokens: every pull and payout amount is nominal (what is sent), and all limit-price and fill checks use those nominal amounts.

* **Use `TransferChecked`.** Set `use_transfer_checked: true` on every order that sells or buys a fee mint. Token-2022 rejects a plain `Transfer` of one with `MintRequiredForTransfer`.
* **Pulls are charged too.** A pull of `amount` counts in full against the order, but the destination receives `amount - fee`. That shortfall is your cost; price it into the fill.
* **Avoid surplus transfers.** Every hop is charged again. Send funds straight to where they're needed (pull directly into the DEX input, route the DEX output directly into the buffer), and don't put more into a buffer than you'll pay out. Extra hops and leftover buffer balances are fees you pay for nothing.
* **The user bears the cost of the payout.** A payout of `amount` from the buffer counts as `amount` toward the order, even though the user receives `amount - fee`. Don't increase the payout to cover the fee, and fund the buffer with the nominal `amount`, since the fee comes out of what is delivered, not on top of it.

The fee rate set on the mint can change at an epoch boundary, so read the mint's current transfer fee config when pricing.

### Other Important Things to Know for Settlement

* Every order is created on-chain by its owner (`CreateOrder`) before it can be settled; there are no off-chain signed orders.
* You can't withdraw funds from the buffers directly. Keep your own inventory and only send into the buffers the amount you expect to pay out to users. Whatever stays in a buffer counts as protocol fees.
* Token-2022 transfer fees are borne by the receiver; the settlement only guarantees the nominal transfer amount. See [Fee-on-Transfer Tokens](#fee-on-transfer-tokens).
* Solana caps how many accounts a transaction can lock (64) and limits a transaction bytes. These will likely be the main constraint on solutions, since each order takes at least 5–6 accounts. In particular:
  * Send funds directly where they need to go whenever possible, without intermediate accounts.
  * Leave `use_transfer_checked` off unless the token needs it, and set `only_token_program` when you can.
  * Use address lookup tables for accounts that recur across settlements (token programs, buffers, common mints).
  * Use V1 transactions, not legacy. They support a [much higher transaction size limit (4096 bytes over 1232!)](https://simd.watch/simd/0296) and are [likely to support more accounts](https://simd.watch/simd/0596) in the near future.

## Troubleshooting

Errors from the settlement program sometimes come back as a numeric custom error code. Look the code up in [`SettlementError`](https://github.com/cowprotocol/solana-programs/blob/main/interface/src/error.rs), or decode it in Rust with `SettlementError::try_from(code)`.

## Reference Code

* Design overview: <https://github.com/cowprotocol/solana-programs/blob/main/DESIGN.md>
* Security design: <https://github.com/cowprotocol/solana-programs/blob/main/SECURITY_DESIGN.md>
* Solana Settlement Program: <https://github.com/cowprotocol/solana-programs/tree/main/programs/settlement>
* Testing CLI: <https://github.com/cowprotocol/solana-programs/tree/main/test-cli>
* All officially supported Rust crates: <https://crates.io/users/kaze-cow>
