# Integrating Solana: Brief for Solvers

## What is being integrated?

CoW Protocol is expanding its settlement process to Solana. As part of this expansion, solvers looking to participate in the auction on Solana will need to use

> [!NOTE]
> While most of the APIs relevant to solvers are reaching a stable state, CoW Protocol for Solana remains under active development, and may be unstable. 
> In particular, the following system compenents are not completed:
> * CoW Reference Driver
> * CoW REST API
> * CoW Autopilot (core auction engine)
> * Solver Authentication on the Settlement Program
>
> We expect the CoW reference driver to be ready very soon.
> We apologise for the inconvenience and appreciate your understanding. 

## How to Settle on Solana

To get started with testing, we suggest placing your own orders on-chain and settling through your own isolated auction. This will allow for you to start building up your liquidity sources and testing the most important flows early while waiting for the more complete backend implementation from the CoW Foundation. Later on we will provide the Autopilot/Auction API similar to what we have on EVM chains.

### Tooling

We provide an official [Rust client SDK](https://crates.io/crates/cow-settlement-client) to construct instructions against the settlement program. The below instructions in this guide all assume you are using the `cow_settlement_client` crate.

If you are *not* using Rust, there is also a [Solana IDL](https://github.com/cowprotocol/solana-programs/blob/idl/programs/settlement/idl/cow_settlement.json) which can be used through Codama to generate. Keep in mind that there may be some addresses or call construction patterns that are not automatically derivable from the IDL, so some additional manual effort may be required.

We recommend installing the [Solana Developer SDK](https://solana.com/docs/intro/installation) if you haven't already.

To place orders that can be tested with the solver, you will want to install the cow testing cli:

```
cargo install cow-test-cli --locked
# Verify path is properly set to include ~/.cargo/bin, and you should be able to execute:
cow
```

You may also want to generate a key for your solver. This can be done with:

```bash
# Writes a new key to `~/.config/solana/id.json`
solana-keygen new
# Print out the address of your key
solana address
```

### CoW Settlement Program Testing Deployments

Every release of the CoW settlement program is deployed to a new address.

The current/first official release, v0.2, is deployed to `J516Mv7YvvvJyMvNEca8tWNTJyDHbFpzwDZD96BNfR3w` on Solana mainnet and devnet. At this time, **there is no solver whitelist**, so anyone can settle a transaction.

As we continue to release updated settlement programs, you can always pull the current settlement program address from `cow_settlement_client::cow_settlement_interface::ID`, so updating the crate is all that is needed to keep up to date with the latest changes.

### Placing an Order

> [!WARNING]
> The settlement program is under active development and has **not yet been audited**. Only use play/testing funds, and do not set any 
> Approvals to the settlement program from a wallet that contains significant funds.

A CLI can be used to place orders without the CoW API for testing purposes. All fields and data for the order are written plain on-chain, so the driver can pick up the order directly.

For example, to use the CLI to place an order:

```sh
cow sell 0.1 SOL for 5 USDC --keypair <my-key.json> --rpc-url https://api.<net>.solana.com
```

Please note that this flow of placing an order on-chain is only for testing purposes. When Solana is officially released, order flows will be sent through the autopilot.

### Example Settlement

The CoW CLI includes an example settlement flow. It has support for:
* coincidence of wants
* kind=SELL, partiallyFillable=false orders *only*

There is also an [experimental branch](https://github.com/cowprotocol/solana-programs/tree/kaze/sc-144-test-settlement-with-on-chain-liquidity) with support for trading on [orca DEX](https://www.orca.so/pools).

Using either order PDA address or CoW Order ID, settle placed orders with:

```
cow settle <order A> <order B> <order C>... --keypair <my-key.json>
```

Read below for more information on how your solver can construct its own settlement and what the most important pieces are.

#### Typical Instructions on Order Placement

All of the following can be completed by using multiple instructions in a single transaction:

* The user approves spending of tokens to the settlement program
  * NOTE: unlike Ethereum a token approval can be overwritten by interacting with another dapp. Only one token approval can be open at a time.
* The Order PDA is created with the `CreateOrder` instruction
* Any needed Buffer Token accounts are created
* The settlement happens--all instructions from `BeginSettle` to `FinalizeSettle`
* The rent for the order PDA is refunded with `ReclaimOrder`

### The Shape of a Solana Settlement

A settlement is **one transaction** containing a matched pair of instructions:

```text
[ ...setup...            ] # e.g. CreateBuffers, ATA creation
[ i   ] BeginSettle        # pulls user sell funds
[ ... ] interactions       # any instructions you like
[ j   ] FinalizeSettle     # sends swap output to users
```

`BeginSettle` carries `finalize_ix_index = j`, `FinalizeSettle` carries `begin_ix_index = i`, and each is checked to point back at the other. Every other interaction is free to be whatever necessary to fulfill the settlement.

### The `OrderIntent`

This is the signed payload that the user delivers to submit an order. It has parameters similar to Ethereum:

```rust
pub struct OrderIntent {
    /// Account authorized to create and invalidate this order and whose
    /// signature authenticates it. For off-chain orders this is the Ed25519
    /// signer; for on-chain creation it must be the transaction signer.
    pub owner: Pubkey,

    /// Token account that receives the buy-side proceeds. Implicitly
    /// encodes both the recipient and the buy token, since SPL token
    /// accounts are token-specific.
    pub buy_token_account: Pubkey,

    /// Token account the sell-side funds are pulled from. Implicitly
    /// encodes both the spender and the sell token. The settlement state
    /// PDA must hold the SPL `delegate` on this account for the order to
    /// be settleable.
    /// This token account must be owned by the intent owner. An intent
    /// that doesn't satisfy this property will be rejected.
    pub sell_token_account: Pubkey,

    /// Amount of the sell token. For `Sell` orders this is the exact
    /// amount to be sold (subject to `partially_fillable`); for `Buy`
    /// orders it is the maximum the user is willing to spend.
    pub sell_amount: u64,

    /// Amount of the buy token. For `Buy` orders this is the exact amount
    /// to be received (subject to `partially_fillable`); for `Sell`
    /// orders it is the minimum the user is willing to receive.
    pub buy_amount: u64,

    /// Unix timestamp after which the order expires.
    /// The order cannot be executed after expiration.
    pub valid_to: u32,

    /// Whether `sell_amount` or `buy_amount` is the exact figure; the
    /// other side is treated as the limit (minimum to receive for `Sell`,
    /// maximum to spend for `Buy`).
    pub kind: OrderKind,

    /// If `true`, the order may be filled across multiple settlements;
    /// proceeds and consumption scale proportionally with the amount of
    /// the sell side that's been used. If `false`, a single settlement
    /// must consume the full sell amount (fill-or-kill).
    pub partially_fillable: bool,

    /// Opaque 32 bytes set by the order creator. Not interpreted by the
    /// settlement program; used off-chain for metadata such as the
    /// frontend version, slippage hints, or attribution.
    pub app_data: [u8; 32],
}
```

### Buffers

On Solana, the buffer accounts serve as the source acount for sending swap outputs from the user. Unlike ethereum, it is not normally possible to send transactions on behalf of the settlement program, so in most cases, you will only use buffers as the receiver of swap outputs instead.

Before a settlement can be started, any necessary buffer token account PDAs must be resolved and, if necessary, created. 

To obtain the buffer account, use the `find_buffer_pda` helper function.

If the buffer pda has not yet been created, you will need to add an instruction to `CreateBuffers` at the beginning of your settlement transaction:

```rust
pub struct CreateBuffers<'a> {
    pub program_id: Pubkey,
    pub payer: Pubkey,
    pub mints: &'a [Pubkey],
}
```

* `program_id`: The address of the settlement program you have been assigned
* `payer`: The address of the signer
* `mints`: List of Token Mint accounts for which buffers are required. Up to 30 can be supplied.

### Begin Settle

Used to receive the swap inputs for a trade. The funds are transferred directly from the user's source account to any accounts you specify.

```rust
pub struct BeginSettle<'a> {
    pub program_id: Pubkey,
    pub finalize_ix_index: u16,
    pub auction_id: i64,
    pub orders: &'a [InitializedIntent<'a>],
}
```

* `program_id`:
* `finalize_tx_index`: Within the same transaction, where the `FinalizeSettle` transaction has been encoded
* `auction_id`: Not read by onchain program. Used for backend indexing (can be anything for testing)
* `orders`

```rust
pub struct Pull {
    pub destination: Pubkey,
    pub amount: u64,
}

pub struct InitializedIntent<'a> {
    pub intent: &'a OrderIntent,
    pub pulls: &'a [Pull],
}
```

* intent: the actual order to settle
  * Can be retrieved directly from the onchain order PDA with `OrderAccount::load_from_pda`
  * Pulls: `destination` and `amount`, transfers to send the swap inputs to. Can be solver token account, DEX input, Settlement buffer, etc.
    * The sum of all pulls `amount` must be less than the remaining amount to sell in the order. If the order has partially_fillable=false, it must be exactly equal
    *

### Finalize Settle Parameters

Used to send the swap outputs for a trade. The funds are transferred from the settlement program buffer PDA to the specified `buy_token_account`.

```rust
pub struct FinalizeSettle<'a> {
    pub program_id: Pubkey,
    pub begin_ix_index: u16,
    pub orders: &'a [FinalizedIntent<'a>],
}
```

* `program_id`: Same as `BeginSettle`
* `begin_tx_index`: Similar to `BeginSettle`, except this is the index of the `BeginSettle` instruction.
* `orders`:

```rust
pub struct FinalizedIntent<'a> {
    pub intent: &'a OrderIntent,
    pub mint: Pubkey,
    pub amount: u64,
}
```

* intent: Same as `BeginSettle`
  * Mint: The buy token account's "mint" address.
    * Note this parameter will likely be removed in the future, as the intent itself will contain the mint address
  * Amount: How much swap output should be sent to the user.

### Other Important Things to Know for Settlement

* All orders must be constructed on-chain before they can be settled. This is already the case for testing today, but in the future it will likely be necessary to publish a separately signed transaction on-chain just before signing
* In solana, it is *not possible* to directly withdraw funds from the buffers. Instead, use your own buffers system, and only send the amount of funds which you expect will be delivered to the user.
* Solana limits the number of accounts which may be locked (i.e. touched) by a transaction to 32. This will likely become the bottleneck for most solver's solutions on Solana, as each order consumes at minimum 5-6 accounts. In particular:
  * Send funds directly to the accounts where they need to go whenever possible, without using intermediate accounts

## Troubleshooting

If you receive an error from the settlement contract, it will sometimes come back/be displayed as a numerical code. This numerical code can be [looked up on the interface](https://github.com/cowprotocol/solana-programs/blob/main/interface/src/lib.rs#L81) to determine the actual error cause.

## Reference Code

* Solana Settlement Program: <https://github.com/cowprotocol/solana-programs/tree/main/programs/settlement>
* Testing CLI: <https://github.com/cowprotocol/solana-programs/tree/main/test-cli>
* All officially supported Rust crates: https://crates.io/users/kaze-cow