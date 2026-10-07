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
- G6. An order can only be settled once (fill-or-kill) or partially across several settlements, together filling at most the amount specified in the intent.
- G7. Cancelled or expired orders can't be settled, and a cancellation can't be undone.
- G8. An order's rent can only be refunded to its `created_by`.

## Protocol funds

- G9. Only approved solvers can settle, and a removed solver has no access left.
- G10. Access to buffer funds is limited to solvers and dedicated administrative roles (barring access intrinsic to the token itself, such as a permanent delegate).
- G11. Nobody can block the creation or reclamation of an order or a buffer (barring controls intrinsic to the token itself, such as a freeze authority).

## Roles

- G12. A role can only be reassigned by the manager or by that role's current holder.
- G13. No role can move user funds without a program upgrade, which revoking the authority prevents.

## Explicitly not guaranteed

- Liveness. There are circumstances when the order won't be settleable, for example:
  - The user changing their token account such that funds can no longer be drawn from the `sell_token_account` (ex. transfer tokens, approval reset, ownership transfer).
  - Specific token extensions (token freeze, non-transferable).
  - On-chain state changes make a route unfillable.
- Accounting for token fees. A settlement only guarantees that a transfer happens for the intended amount; the final transfer fees are borne by the receiver.
- Fair execution. While the overall protocol is built to incentivize fair prices, the program makes no such guarantees. From the program's perspective, the user is ultimately responsible for the fairness of the price in the intent.
- Solver accountability for misbehavior. Limited abuse is expected (e.g., a solver withdrawing some of the fees beyond what they're entitled to). This is intended to be covered off-chain by the solver bond.

# Assumptions

In order for G1-G13 to operate as designed, the below assumptions are made.

## Scope

- The program is upgradeable until this authority has been revoked. We expect to make it immutable at a date in the future ([DESIGN.md:15](./DESIGN.md#L15)). Until then the upgrade authority can do anything, and all security outcomes are contingent on the operations of the upgrade authority.

## Privileged Roles

All roles, other than Solvers, are defined and manipulated using the same pattern in the state pda.

| Role | Trusted with |
|---|---|
| Upgrade authority | Everything (while the program is mutable) |
| Manager | Ability to assign every role, its own included. Transfers take one step, with no acceptance or zero-address check |
| Solver authority | Adding and removing solvers, so it decides who can settle |
| Reclaim authority | Closing buffers and choosing where their rent goes. Can also burn tokens from the buffer to allow closing to happen |
| Settlement-owned-order authority | Ability to place orders that are owned by the settlement program, used to withdraw fees |
| Solvers | Ability to call `BeginSettle` and `FinalizeSettle` |
