# Security Design

Three kinds of party interact with the settlement program:

- **Users** place orders to trade, paying a fee out of their limit price.
- **Solvers** settle those orders against on-chain liquidity, implicitly collecting the fees. They are bonded off-chain.
- **Administrative roles** each own a narrow task: redistributing the collected fees, managing solvers and (off-chain) their bonds, and controlling access.

The settlement program connects these parties together. It's designed to reduce how much they have to trust each other.

## Users

- G1. User funds only move to settle one of the user's own orders.
- G2. Only an order's owner can authorize an order.
- G3. The amount of funds the user receives in a successful settlement is determined at transaction creation time and cannot change during execution (for MEV protection).
- G4. A user never gives more than `sell_amount`, and never receives less than the limit price implied by the requested `buy_amount`.
- G5. The parameters of a user intent are upheld.
- G6. Cancelled or expired orders can't be settled, and a cancellation can't be undone.
- G7. An order's rent can only be refunded to its `created_by`.

## Protocol funds

- G8. Only approved solvers can settle, and a removed solver has no access left.
- G9. Access to buffer funds is limited to solvers and dedicated administrative roles (barring access intrinsic to the token itself, such as a permanent delegate).
- G10. Nobody can block the creation or reclamation of an order or a buffer (barring controls intrinsic to the token itself, such as a freeze authority).

## Roles

- G11. A role can only be reassigned by the manager or by that role's current holder.
- G12. No role (apart from the program upgrade authority) can move user funds.

## Explicitly not guaranteed

Liveness. Any of the following can make an order unsettleable without breaking G1–G12: 
- The user changing their token account such that funds can no longer be drawn from the `sell_token_account` (ex. transfer tokens, approval reset)
- behavior enforced by a Token-2022 extension
- a mint authority acting on its own token

# Assumptions

In order for G1-G12 to operate as designed, the below assumptions are made.

## Scope

- The program is upgradeable until this authority has been revoked. We expect to make it immutable at a date in the future ([DESIGN.md:15](./DESIGN.md#L15)). Until then the upgrade authority can do anything, and all security outcomes are contingent on the operations of the upgrade authority.

## Privileged Roles

All roles, other than Solvers, are defined and manipulated using the same pattern in the state pda.

| Role | Trusted with | Source |
|---|---|---|
| Upgrade authority | Everything (while the program is mutable) | [DESIGN.md:15](./DESIGN.md#L15) |
| Manager | Ability to assign every role, its own included. Transfers take one step, with no acceptance or zero-address check | [role.rs](./interface/src/role.rs), [transfer_authority.rs](./programs/settlement/src/processor/transfer_authority.rs) |
| Solver authority | Adding and removing solvers, so it decides who can settle | [DESIGN.md:69-83](./DESIGN.md#L69) |
| Reclaim authority | Closing buffers and choosing where their rent goes. Can also burn surplus dust from the buffer to allow closing to happen | [reclaim_buffer.rs](./programs/settlement/src/processor/reclaim_buffer.rs) |
| Settlement-owned-order authority | Ability to place orders that are owned by the settlement program. | [role.rs:22-25](./interface/src/role.rs#L22), [DESIGN.md:91](./DESIGN.md#L91) |
| Solvers | Ability to call `BeginSettle` and `FinalizeSettle` | [DESIGN.md:62](./DESIGN.md#L62), [DESIGN.md:95](./DESIGN.md#L95) |

## Solana runtime and platform

1. **Transactions are atomic, and every top-level instruction runs.** `BeginSettle`, along with validating its own inputs, also validates `FinalizeSettle`s inputs through instruction introspection, with `FinalizeSettle` performing very little validation of its own. ([finalize_settle.rs:46-49](./programs/settlement/src/processor/finalize_settle.rs#L46)). It expects `FinalizeSettle` to execute, and to operate with the expected behavior.
2. **Instruction introspection is reliable.** `BeginSettle` uses instruction introspection to also validate `FinalizeSettle`.
3. **Only the owning program can change an account's data or debit its lamports, and only the program can sign for its PDAs.** `CanonicalPda::create_idempotent` treats "address is canonical and has the expected owner" as "already initialized by us" ([pda.rs:50-63](./programs/settlement/src/processor/utils/pda.rs#L50)). *(The untracked `programs/test/assign-owner` probes this.)*
4. **A valid Order PDA may only exist as owned by the settlement program's state PDA if it was initialized by the settlement program.** Otherwise, it would be possible for an order to be created on anyone's behalf by creating an account elsewhere and transferring ownership to the settlement program.
5. **A PDA is never created twice with the same seeds and different owners** (stated as an assumption in [pda.rs:42-45](./programs/settlement/src/processor/utils/pda.rs#L42) and [pda.rs:59-62](./programs/settlement/src/processor/utils/pda.rs#L59)).
6. **The runtime enforces rent exemption.** A native-SOL push uses `move_lamports`, which checks only for under- and overflow. The runtime's rent-state check is what stops a push from draining the state PDA below its rent (test: `rejects_a_push_spending_the_state_pdas_rent`). *(implicit)*
7. **Reentrancy is impossible.** The settlement program only CPIs into System, SPL Token, and Token-2022
8. **`Clock::unix_timestamp` is good enough for expiry.** An order can settle while `Clock::unix_timestamp <= valid_to` and can be reclaimed once `Clock::unix_timestamp > valid_to` *(implicit)*
9. **The CPI depth from `TRANSACTION_LEVEL_STACK_HEIGHT` is an accurate test to identify if a CPI is in use** ([cpi.rs](./programs/settlement/src/processor/utils/cpi.rs)).

## Addresses, PDAs and versioning

10. **The state PDA is pinned at compile time against `declare_id!`**, and handlers compare against that constant instead of deriving it. 
11. **Deploying anywhere the state PDA does not derive makes `Initialize` fail.** That way a fake/unrelated state PDA cannot be supplied. ([DESIGN.md:23](./DESIGN.md#L23), [README.md:96](./README.md#L96)).
12. **The `SETTLEMENT_SEED` version prefix is fixed-width**, so one version's seeds can never be a prefix of another's. Prevents unexpected state collisions if breaking changes were upgraded onto an existing settlement program. ([pda/mod.rs:23-30](./interface/src/pda/mod.rs#L23)).
13. **A minor- or major-version bump deploys a whole new contract.** Users have to re-delegate, and any funds left in buffers are stranded unless drained before the bump ([DESIGN.md:27-28](./DESIGN.md#L27), [pda/buffer.rs:12-16](./interface/src/pda/buffer.rs#L12)).
14. **PDA creation cannot be griefed by pre-funding.** The program uses `CreateAccountAllowPrefund` ([pda.rs:82-87](./programs/settlement/src/processor/utils/pda.rs#L82)).
15. **The program only ever creates PDAs at the canonical bump.** Not using the canonical bump could lead to an order being created twice/having two states.
16. **Initialize has no access control.** After contract deployment, anyone can call it first sets all four roles. It is up to the protocol admins to ensure the contract was initialized as expected. ([instruction/initialize.rs](./interface/src/instruction/initialize.rs)). Deployment assumes `just deploy` initializes before anyone else does ([README.md:115](./README.md#L115)). *(The front-running risk itself is not written down.)*

## Orders

17. **The existance of an order PDA owned by the settlement program serves as evidence that it was authorized by the owner.** Any functions creating order must validate owner authorization, and there is no way to transfer an order PDA shaped account ownership to the settlement program.
18. **The same functional intent cannot have multiple UIDs.** Exactly one byte string maps to each intent, so each intent has one UID and one PDA. In practice, this means reserved/unused bytes in the flags are rejected. ([intent.rs:89-92](./interface/src/data/intent.rs#L89), [intent.rs:401-406](./interface/src/data/intent.rs#L401)).
19. **An on-chain order can only be re-created with the owner's authorization.** The design treats that as new consent from the owner.
20. **Settle-time checks catch any account that changed since the order was created.** The sell token account's SPL owner and mint are re-checked against the intent at settle time, because the account can be closed and re-opened ([begin_settle.rs:313-327](./programs/settlement/src/processor/begin_settle.rs#L313)). Mints can likewise be closed and re-opened under another program or with other extensions (test: `reclaims_a_buffer_whose_mint_was_reopened_*`).

## Off-chain signed orders

21. **Off-chain orders are reclaimable only after expiry.** Otherwise anyone holding the signature could re-create the order and settle it again ([DESIGN.md:226](./DESIGN.md#L226)). *(forward-looking)*
22. **The owner must be an Ed25519 signer, so off-curve addresses such as the state PDA can never own an off-chain order.** *(forward-looking, implicit: this is what stops a forged "settlement-owned" order once off-chain auth exists)*

## Settlement

23. **BeginSettle and FinalizeSettle must be matched as a pair with no BeginSettle or FinalizeSettle instructions, or the transaction reverts.** Several settlements one after another in the same transaction are allowed. ([DESIGN.md:333](./DESIGN.md#L333), [begin_settle.rs:147-187](./programs/settlement/src/processor/begin_settle.rs#L147)). 
24. **Orders must be supplied to BeginSettle and FinalizeSettle in ascending PDA order.** This prevents duplicates and allows BeginSettle and FinalizeSettle to be validated more smoothly.
25. **Each order in a settlement is paid by exactly one push.** Orders must be strictly increasing by PDA address, which rejects duplicates, and are matched to pushes one-to-one ([begin_settle.rs:189-246](./programs/settlement/src/processor/begin_settle.rs#L189)).
26. **The token program enforces that mints match, or reverts.** Neither `BeginSettle` or `FinalizeSettle` validate that the buffer sending the funds is the correct one. ([finalize_settle.rs:60-68](./programs/settlement/src/processor/finalize_settle.rs#L60), [begin_settle.rs:291-295](./programs/settlement/src/processor/begin_settle.rs#L291)).
27. **The token-program account derivation/slots do not need to be validated to safely use.** ([settle/begin.rs:36-41](./interface/src/instruction/settle/begin.rs#L36)).
28. **Any arithmetic overflow or underflow causes a revert.** `clippy::arithmetic_side_effects = "deny"` is used workspace-wide ([Cargo.toml](./Cargo.toml)). The parser iterators are the documented exception, justified by transaction size limits ([settle/begin.rs:171-174](./interface/src/instruction/settle/begin.rs#L171)).

## Tokens

29. **Token-2022 extensions that destructively interfere with the settlement revert.** An extension that breaks settlement makes the affected orders unsettleable, nothing worse ([DESIGN.md:389](./DESIGN.md#L389)). Fee-on-transfer tokens are settleable, but the user will receive the post fee amount from their limit price. *(implicit)*
30. **Mint authorities are trusted with their own token**, buffer balances included: freeze authority, permanent delegate, pausable, default-frozen state. *(implicit)*
31. **`GetAccountDataSize` from Token-2022 is reliable when sizing buffers.** Its return data is accepted only if it comes from the Token-2022 program ([token.rs:26-44](./programs/settlement/src/processor/utils/token.rs#L26)).