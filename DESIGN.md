# Solana program high-level specs

There's a single dedicated settlement program, deployed once and used for executing all settlements.

The settlement program stores funds through dedicated token accounts (_buffer accounts_), one per token.

It uses a dedicated state account to:

- Manage solver authentication (including fee access by the protocol).
- Act as a token delegate to manage user funds.

Its state is stored in a PDA generated using seed `["settlement"]`.

Once deployed, we will make the code at that account unchangeable.

## Buffer accounts

Buffer accounts are token accounts that hold funds on behalf of the settlement contract.

These token accounts are accessible to all solvers and effectively work like the current buffers. They are used to collect user funds, send out funds to the user, and collect fees, which stay on the buffers after the settlement. This means that the current fee accounting and withdrawal mechanism would be based on balance changes (like on Ethereum).

Corresponding PDAs are generated using seed `["settlement", token, "buffer"]`.

Differences with Ethereum:

- In a settlement, a solver can only access buffers for tokens that are part of some order.
- Traded funds aren't sent automatically to the buffers at the start of the trade. Solvers can specify arbitrary receiver accounts for the user's sell tokens, as well as send the trade proceeds from any arbitrary account. In practice, buffers may still be used by solvers for pooling funds before interactions for efficiency reasons.

## Solver authentication

Solver authentication is managed by the settlement program.

There are two roles for authentication:

- The _solvers_, accounts that can execute settlements and withdraw from the buffers.
- The _manager_, an account that can add and remove solvers, as well as transfer its own role to another account.

The settlement program state PDA stores the state used for authentication.

On settlement program deployment, the program state is initialized with a fixed initial manager controlled by CoW and an empty list of solvers.

Transferring the role of manager is done in a two-step process: first the current manager proposes a new manager; then the new manager accepts the role, finalizing the role transfer.

Differences with Ethereum:

- There's no dedicated authenticator contract, the settlement program performs the role of the authenticator contract. This also means that the authenticator isn't upgradable: there's no `owner`, only a `manager`.

Limitation:

- Storing all solvers on the same account limits the amount of possible solver accounts to ~64k.

## Fee withdrawals

Fees accumulate in the buffer accounts after a settlement is concluded.

Any solver can withdraw funds from the protocol’s buffers. Standard protocol fee withdrawal will be managed by a dedicated solver account.

Withdrawing is triggered by the `CollectFromBuffer` instruction.

Differences with Ethereum:

- We don’t want to let the settlement program create orders for itself because, unlike in the EVM, it requires a specific code branch for that. This means that we can't follow the same withdraw mechanism we use right now. Pragmatically, at the start we should use the solver to withdraw the funds to a dedicated "dump" account and create orders to swap all funds to the same token (SOL). We don't plan to improve on this on the current iteration of the program.
- Withdrawing fees is done outside of a settlement.

## User delegation (i.e., "approvals")

As we'll see, trades work through signed intents.

A user who wants to use CoW Protocol for a token needs to set their [delegate](https://solana.com/docs/tokens/basics/approve-delegate) for that token to the settlement state PDA.

Similar to Ethereum's approvals, this needs to be done by each user once per token in order to trade.

As long as the settlement program is immutable, there's no other way to access user funds except through the execution of an order.

Differences with Ethereum:

- There can only be a single delegate. This could create situations where the user delegates us, creates an order, and then another dapp delegates a different program and the order can't be settled anymore. This is very different from approvals, where an approval for one dapp doesn't affect approvals for other dapps.
- There's no dedicated vault relayer, the user delegates their tokens to the settlement state PDA (_not_ the settlement program!). This is because "interactions" aren't executed by the settlement program but as dedicated instructions originating from the transaction signer.

## Orders

### Intents

Users interact with the protocol by [signing](#authenticating-an-order) an order intent off-chain.

An order intent is the following list of parameters:

```rust
struct OrderIntent {
	owner: Pubkey
	// Origin and destination of funds in this order.
	// They implicitly encode both the receiver account and the traded tokens.
	buy_token_account: Pubkey
	sell_token_account: Pubkey
	// Amounts are interpreted as exact or maximum depending on kind.
	sell_amount: u64
	buy_amount: u64
	// Unix timestamp
	valid_to: u32
	// Either Buy or Sell
	kind: OrderKind
	partially_fillable: bool
	// Usual app data field, it isn't directly used in the program.
	app_data: [u8; 32]
}
```

Differences with Ethereum:

- In Solana, the spender token account (and the owner) is part of the intent, while in Ethereum it is implied in the signature.
- The `receiver` field in Ethereum is captured by the field `buy_token_account`.
- Sell and buy amounts are 64 bits instead of Ethereum's 256.

### Orders are accounts

For processing an order in a settlement, the data of that order needs to be stored in a dedicated account. Storing this data is, in general, the responsibility of the solver who settles the order the first time, but anyone can do it if a user signed an order.

Useful information can be recovered from the order PDA. Notably:

```rust
cancelled: bool
amount_withdrawn: u64
amount_received: u64
// The account that created this order, used for refunding rent
created_by: Pubkey
intent: OrderIntent
```

There are also other parameters, like `snapshot`, used internally during the settlement.

An order PDA can only exist and hold data if the order has been [authenticated](#authenticating-an-order). If this account exists and is not cancelled, filled, or expired, then the order can be traded.

At the time of order creation, the executor can specify a different address as the `created_by` address. This allows rent to be reclaimed by a different account than the one that signed the instruction.

Corresponding PDAs are generated using seed `["settlement", hash(intent), "order"]`.

The serialization of the parameters before hashing and the hashing function will be formally specified at a later point.

### Identifying an order (order UID)

We use the hash of the intent parameters to represent the order, `hash(parameters)`. This is the same hash used to generate the order PDA.

Differences with Ethereum:

- Solana's order UIDs are 32 bytes, compared to 56 bytes in Ethereum. This is because:
  - The owner isn't added to the UID. The owner is already included in the parameters, unlike in Ethereum, thus the owner doesn't need to be appended for disambiguation.
  - The expiration isn't added to the UID. In Ethereum it was only added because of state clearing, here it isn't needed.

### Invalidating an order

Invalidating an order is an operation executed by the user to make it impossible to trade that order in the protocol.

Invalidating an order requires an on-chain operation. This operation can be authenticated in two ways:

- Directly, sending an invalidation instruction from the order owner account.
- By anyone through a signed intent, signing the following cancellation struct:
  ```rust
  struct CancelIntent {
    intent: OrderIntent
  }
  ```

Creating the order in advance is _not_ needed: if the order wasn’t created before invalidating, the corresponding order PDA is created and then invalidated.

Note that deleting the order PDA is _not_ enough to invalidate an order. In fact, if an order signature is available, the same order could always be created again until it expires.

### Order clearing

Allocating an order PDA requires paying rent.

If the order is expired, anyone can close the order account. On account closure, the rent is sent to the original creator of the order.

This is useful for solvers who need to allocate the order for executing it, but the allocation itself would be orders of magnitude more expensive than the compute cost for executing an instruction. This is particularly relevant to make small orders economically viable.

## Authenticating an order

We want to support two ways to authenticate that an order comes from a user:

- [Off-chain] Through an [Ed25519 signature](#ed25519), encoded as Solana’s off-chain message.
- [On-chain] In an [instruction](#on-chain-order-creation) by the owner that directly creates the order PDA.

### Encoding the signed data

The data to sign is encoded using Solana’s [off-chain message signing standard](https://docs.anza.xyz/proposals/off-chain-message-signing). It’s similar to the ERC-712 standard to encode structured data, in that it guarantees that:

- Users cannot sign Solana transactions instead of messages by accident.
- The signature cannot be used by other protocols that rely on the same signature encoding: there is strict namespacing of signatures, and each signed message is only valid for the intended purpose.

Differences with Ethereum:

- Off-chain message signing seems to be much less supported by wallets compared to ERC-712. For example, there doesn’t seem to be a Phantom wallet API to encode off-chain messages (see [documentation](https://docs.phantom.com/sdks/browser-sdk/sign-messages), Solana only has a simple `signMessage`). Further research is needed to determine wallet compatibility, but there’s reason to believe it’s very low and it will compromise user signing experience.
- Off-chain message signing doesn’t have a native representation of structured data. Only UTF8 and ASCII data can be signed, meaning that even if wallets were supporting this standard, many of them would just present, in the best case, a sequence of bytes.

### Ed25519

Raw Ed25519 signatures are supported by all native Solana accounts.

The data to be signed is encoded as an off-chain message and signed with raw Ed25519 signatures.

Differences with Ethereum:

- Unlike ECDSA signatures in Ethereum, the owner account address cannot be recovered from the Ed25519 signature. This means that the address needs to be included as part of the signed data.
- Signatures are 64 bytes (unlike EVM’s 65).

### On-chain order creation

Orders can be created by the owner by executing an instruction on-chain.

The order owner executes the `OwnerCreateOrderIntent` instruction. The settlement program checks that the order comes from the owner and [creates the order PDA](#orders-are-accounts).

In this authentication flow, the user needs to pay for the rent in SOL necessary to create the PDA. Note that the rent may be significantly higher than the expected trading fee. The rent can be recovered by the user once the order has expired by [clearing the order](#order-clearing).

This flow supports both standard ("on-curve") accounts and PDA signatures.

This flow is the only one allowing on-chain programs to trade through the settlement program.

Advantages:

- Smart wallets can sign an order without needing an Ed25519 private key.

Disadvantages:

- Not intent-based, the user needs to sign and execute an instruction for every order.

Differences with Ethereum:

- The rent refund process isn’t part of Ethereum and adds extra costs for the user.
- Unlike Ethereum’s pre-signing, this flow isn’t considered a "signature scheme" but it’s a direct way to publish in a trusted way all order information on-chain. This is because, in Solana, it’s more complex and expensive to manage dedicated storage for pre-signatures than just creating the order on-chain.

## Settlements

### A settlement

A settlement is composed of:

- The orders to settle.
- The list of token transfers to perform from the orders’ sell token accounts and to the orders’ buy token account.
- A list of interactions to execute.

Security guarantees:

- User:
  - Funds can only be taken in the context of a signed order.
  - No more than the order’s sell amount can be taken from the user.
  - The user receives at least the specified buy amount (proportional to how much of the sell amount was used in the settlement).
- Protocol:
  - Only approved solvers can settle.

Differences with Ethereum:

- There’s no per-settlement list of prices.
- Fund transfers are explicit instead of being automatically done as part of the order inclusion.

### The settlement transaction

A settlement transaction is split into multiple instructions. All settlement operations occur between a `BeginSettle` and a `FinalizeSettle` instruction with the exception of arbitrary interactions, which can take place at any point of a transaction. Except for that, the order of instructions in the transaction is arbitrary.

- `BeginSettle`: Snapshots each order's receiver token account, spender token account, and withdrawal balances. Grants the solver token-spending authority on each buffer account. Carries an explicit `finalize_ix_index` pointing to its paired `FinalizeSettle`.
- `Pull`: Takes tokens from an order’s sell token account and sends them to any token account specified in the instruction.
- `Push`: It references a unique SPL transfer token instruction between `BeginSettle` and `FinalizeSettle` that sends the proceeds of an order to its buy token account.
- (arbitrary interactions): Any instruction from the solver. This could be a token transfer, an AMM swap, or anything else.
- `FinalizeSettle`: Reads balances again, computes deltas against the snapshots, validates clearing/limit prices, updates `amount_received` and order status, revokes solver approvals. Carries an explicit `begin_ix_index` pointing to its paired `BeginSettle`.

Additionally, a settlement transaction will include the batch number as part of the instruction bytes of `BeginSettle`.

Only a single settlement can be executed at a time. This means that there can’t be a `BeginSettle` instruction or a spurious `FinalizeSettle` instruction between a coupled pair of `Begin`/`FinalizeSettle`.

Differences with Ethereum:

- Interactions aren’t executed from the context of the settlement program but from the context of the solver as completely separate instructions.
  - Notably: a solver doesn’t have to set approvals from the settlement contract to on-chain contracts. Once solver privileges are removed, there’s no way for old solvers to access the buffers anymore, unlike in the current Ethereum contract.
- Fund transfers are explicit instead of being automatically done as part of the order inclusion. These transfers aren’t automatically done to the buffers, they may go to different accounts.

## Selling SOL (a.k.a. ETH flow)

Selling SOL is supported through a separate program (called SOL flow).

Selling native tokens requires taking control of the user funds and wrapping them.

While the technical details are different, for the user this will be very similar to the ETH flow experience.

Differences with Ethereum:

- Minimum order size: needed to pay for rent by the user when the order is created. (But the rent will be used in the trade, there won’t be leftovers or need to recover this SOL by the user.)
- No unlimited order deadline.
- Different execution flow for solvers compared to EVM.

### Creating a SOL sell order (user facing)

The user interacts with the SolFlow program through the instruction `CreateSolOrder`. This instruction takes the traded amount of SOL and the data describing a [user intent](#intents) minus the owner (implicitly assumed to be the instruction signer), the sell token account, and the amount (assumed to be the SOL amount in the instruction).

This will create an SPL token account for WSOL, owned by the SolFlow PDA, with seeds equal to (`intent`, `owner`), storing the intent data and the user’s lamports. We call this the _custodial_ account of the user for that intent.

The token account will be created automatically with the delegate set to the settlement program.

### Enabling a SOL sell order (solver facing)

Any unprivileged participant can interact with the SolFlow program to have it create an Order in the Settlement program using the funds in the dedicated custodial account. We call that unprivileged participant the _enabler_.

The enabler calls the `SetUpOrder` instruction passing on the user accounts (custodial and buy token accounts) and the user order intent.

Then, the `SolFlow` processor creates an order on the `SettlementProgram` using a modified version of the original intent:

- Order owner becomes the `SolFlow` PDA.
- Sell token becomes the _custodial_ token account.
- Order `created_by` field is set to enabler’s address.

The `SetUpOrder` validates the given intent by successfully re-deriving the custodial account.

### Recovering funds by the user

Each intermediate step can be reverted by the original owner to recover the funds, as long as the order hasn’t been executed. It supports partial execution.

### Comparison with direct wrapping and order creation

Wrapping and creating an order is easier in Solana and everything can be executed in the same transaction. The delegation instruction can be bundled as well.

The main reason to prefer the SOL flow described here is the handling of the rent amount. When wrapping and creating an order, the user needs to pay rent possibly for creating the order account. This is particularly notable for small orders: the order account would have to be refunded later to recover the user’s SOL.

## Token 2022

The settlement program will natively support [Token-2022](https://www.solana-program.com/docs/token-2022) tokens. All operations available to standard tokens will be usable for tokens based on this standard, and no major front-end or back-end changes are expected in order to support the majority of tokens based on this standard.
