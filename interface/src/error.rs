//! Program-side errors surfaced by the settlement program.

/// Program-side errors surfaced by the settlement program.
/// The discriminant value is the on-chain `ProgramError::Custom` code.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum SettlementError {
    /// The `FinalizeSettle` included as input to `BeginSettle` isn't before
    /// the actual `BeginSettle` index.
    FinalizeBeforeInitialize = 0,
    /// Another `BeginSettle`/`FinalizeSettle` of this program appears strictly
    /// between this pair's bounds, nesting or overlapping two settlements.
    BeginFinalizePairOverlap = 1,
    /// The counterpart index points past the end of the transaction's
    /// instruction list, so no instruction sits there.
    MissingCounterpartInstruction = 2,
    /// The instruction at the counterpart index belongs to a different program.
    CounterpartIsExternal = 3,
    /// The counterpart instruction's discriminator byte couldn't be recovered
    /// from its data.
    InvalidCounterpartDiscriminator = 4,
    /// The counterpart instruction's own counterpart index couldn't be
    /// recovered from its data.
    InvalidCounterpartCounterpart = 5,
    /// The counterpart's discriminator isn't the expected
    /// `BeginSettle`/`FinalizeSettle` kind, or its counterpart index doesn't
    /// point back at this instruction.
    MismatchedCounterpartDiscriminator = 6,
    /// An order-creation instruction's intent owner isn't the owner it must
    /// have: the signer for `CreateOrder`, the settlement state PDA for
    /// `CreateSelfOrder`.
    OwnerMismatch = 7,
    /// An account was provided that cannot be derived from the seeds recognized by the program
    AccountNotDerivable = 8,
    /// `BeginSettle`'s order accounts aren't passed strictly increasing by
    /// address.
    OrdersNotStrictlyIncreasing = 9,
    /// A `BeginSettle` sell token account doesn't match the
    /// `sell_token_account` recorded in the order's intent.
    SellTokenAccountMismatch = 10,
    /// A `BeginSettle` sell token account isn't a valid SPL token account
    /// (wrong data length or not owned by the token program).
    SellTokenAccountInvalid = 11,
    /// A `BeginSettle` sell token account's SPL owner isn't the order's intent
    /// owner.
    SellTokenOwnerMismatch = 12,
    /// `BeginSettle`'s order-account count doesn't match the structure its
    /// instruction data expects: `n` orders each contribute an order PDA and a
    /// sell token account, plus one destination account per transfer.
    AccountCountNotMatchingOrderCount = 13,
    /// `BeginSettle` or `FinalizeSettle` was invoked via CPI rather than as a
    /// top-level transaction instruction.
    CalledViaCpi = 14,
    /// A `BeginSettle` order has been cancelled by its owner and can no longer
    /// be settled.
    OrderCancelled = 15,
    /// A `BeginSettle` order's `valid_to` lies in the past: the order has
    /// expired and can no longer be settled.
    OrderExpired = 16,
    /// The transfer counts in `BeginSettle` don't sum to the number of transfer
    /// amounts, so destinations and amounts can't be paired up exactly.
    TransferCountMismatch = 17,
    /// `BeginSettle`'s state account isn't the canonical settlement state PDA,
    /// which must sign the pulls as the user's token delegate.
    StateAccountMismatch = 18,
    /// `FinalizeSettle`'s push-account count doesn't match its instruction
    /// data: each push contributes a source buffer and a destination account,
    /// so the count must be twice the number of push amounts.
    AccountCountNotMatchingPushCount = 19,
    /// `BeginSettle`: the number of pushes carried by the paired `FinalizeSettle`
    /// doesn't equal the number of settled orders. Each order must be paid by
    /// exactly one push.
    SettledOrderPushCountMismatch = 20,
    /// `BeginSettle`: a paired `FinalizeSettle` push doesn't send its proceeds
    /// to the order's buy token account; its destination differs from the
    /// `buy_token_account` in the order's intent.
    PushDestinationMismatch = 21,
    /// `BeginSettle`: a paired `FinalizeSettle` push doesn't draw funds from the
    /// canonical buffer for the order's `buy_mint`.
    PushSourceNotBuffer = 22,
    /// `BeginSettle`: the OrderIntent `sell_token_account` holds a different
    /// mint than the declared `sell_mint`.
    SellMintMismatch = 23,
    /// `BeginSettle`: a settled order's executed price (`amount_out/amount_in`)
    /// is worse than the order's limit price (`buy_amount/sell_amount`).
    LimitPriceViolated = 24,
    /// `BeginSettle`: an order's pull amounts sum to more than `u64::MAX`.
    PullAmountOverflow = 25,
    /// `BeginSettle`: filling this order would consume more tokens than the
    /// maximum the user is willing to trade on this intent.
    /// Sell: `amount_in > sell_amount`; buy: `amount_out > buy_amount`.
    FillExceedsOrderAmount = 26,
    /// `BeginSettle`: a non-`partially_fillable` order isn't filled exactly to
    /// its amount (either under- or over-filled).
    /// Sell: `amount_in != sell_amount`; buy: total `amount_out != buy_amount`.
    OrderNotExactlyFilled = 27,
    /// `BeginSettle`: the order's cumulative `amount_withdrawn` would exceed
    /// `u64::MAX` once this settlement's pulls are added.
    AmountWithdrawnOverflow = 28,
    /// `BeginSettle`: the order's cumulative `amount_received` would exceed
    /// `u64::MAX` once this settlement's push is added.
    AmountReceivedOverflow = 29,
    /// `ReclaimOrder` was called on an order that has is not yet eligible for reclaim.
    OrderNotReclaimable = 30,
    /// `ReclaimOrder`'s `reclaim_recipient` account doesn't match the
    /// `created_by` address recorded in the order.
    ReclaimRecipientMismatch = 31,
    /// `ReclaimBuffer`'s `reclaim_authority` account isn't a signer, or doesn't
    /// match the `reclaim_authority` address recorded in the settlement state
    /// PDA.
    ReclaimAuthorityMismatch = 32,
    /// A `ReclaimBuffer` `buffer_pda` doesn't sit at the canonical buffer PDA
    /// derived from its paired `mint`.
    ReclaimBufferNotCanonical = 33,
    /// `TransferAuthority`'s signer is neither the manager nor the current
    /// holder of the role being transferred, so it may not transfer it.
    UnauthorizedAuthorityTransfer = 34,
    /// `AddSolver`/`RemoveSolver`'s manager account isn't a signer, or doesn't
    /// match the `manager` recorded in the settlement state PDA, so it may not
    /// change the solver list.
    UnauthorizedSolverManagement = 35,
    /// `AddSolver`'s solver is already in the state PDA's solver list.
    SolverAlreadyExists = 36,
    /// `BeginSettle`'s solver account isn't a signer or isn't in the state PDA's
    /// solver list, so it may not settle.
    UnauthorizedSolver = 37,
    /// `RemoveSolver`'s solver isn't in the state PDA's solver list.
    SolverNotFound = 38,
    /// A created order's intent isn't set with the `created_on_chain` flag
    /// corresponding to the behavior of the invoked order creation instruction.
    OrderCreatedOnChainMismatch = 39,
    /// `CreateBuffer` asked the token program how long a token account for a
    /// mint has to be and couldn't read the answer, so it can't size the
    /// buffer.
    BufferSizeUnavailable = 40,
    /// The token program for a given token or mint is not supported.
    InvalidTokenProgram = 41,
    /// `CreateSelfOrder`'s self-order-authority account isn't a signer, or
    /// doesn't match the `self_order_authority` recorded in the settlement state
    /// PDA, so it may not create self orders.
    UnauthorizedSelfOrder = 42,
}

/// Decodes an on-chain `ProgramError::Custom` code back into the error, for
/// consumers reading simulation results and transaction metadata. An unknown
/// code (a newer program) is returned unchanged.
impl TryFrom<u32> for SettlementError {
    type Error = u32;

    fn try_from(code: u32) -> Result<Self, u32> {
        match code {
            0 => Ok(Self::FinalizeBeforeInitialize),
            1 => Ok(Self::BeginFinalizePairOverlap),
            2 => Ok(Self::MissingCounterpartInstruction),
            3 => Ok(Self::CounterpartIsExternal),
            4 => Ok(Self::InvalidCounterpartDiscriminator),
            5 => Ok(Self::InvalidCounterpartCounterpart),
            6 => Ok(Self::MismatchedCounterpartDiscriminator),
            7 => Ok(Self::OwnerMismatch),
            8 => Ok(Self::AccountNotDerivable),
            9 => Ok(Self::OrdersNotStrictlyIncreasing),
            10 => Ok(Self::SellTokenAccountMismatch),
            11 => Ok(Self::SellTokenAccountInvalid),
            12 => Ok(Self::SellTokenOwnerMismatch),
            13 => Ok(Self::AccountCountNotMatchingOrderCount),
            14 => Ok(Self::CalledViaCpi),
            15 => Ok(Self::OrderCancelled),
            16 => Ok(Self::OrderExpired),
            17 => Ok(Self::TransferCountMismatch),
            18 => Ok(Self::StateAccountMismatch),
            19 => Ok(Self::AccountCountNotMatchingPushCount),
            20 => Ok(Self::SettledOrderPushCountMismatch),
            21 => Ok(Self::PushDestinationMismatch),
            22 => Ok(Self::PushSourceNotBuffer),
            23 => Ok(Self::SellMintMismatch),
            24 => Ok(Self::LimitPriceViolated),
            25 => Ok(Self::PullAmountOverflow),
            26 => Ok(Self::FillExceedsOrderAmount),
            27 => Ok(Self::OrderNotExactlyFilled),
            28 => Ok(Self::AmountWithdrawnOverflow),
            29 => Ok(Self::AmountReceivedOverflow),
            30 => Ok(Self::OrderNotReclaimable),
            31 => Ok(Self::ReclaimRecipientMismatch),
            32 => Ok(Self::ReclaimAuthorityMismatch),
            33 => Ok(Self::ReclaimBufferNotCanonical),
            34 => Ok(Self::UnauthorizedAuthorityTransfer),
            35 => Ok(Self::UnauthorizedSolverManagement),
            36 => Ok(Self::SolverAlreadyExists),
            37 => Ok(Self::UnauthorizedSolver),
            38 => Ok(Self::SolverNotFound),
            39 => Ok(Self::OrderCreatedOnChainMismatch),
            40 => Ok(Self::BufferSizeUnavailable),
            41 => Ok(Self::InvalidTokenProgram),
            42 => Ok(Self::UnauthorizedSelfOrder),
            unknown => Err(unknown),
        }
    }
}

impl From<SettlementError> for u32 {
    fn from(e: SettlementError) -> Self {
        e as u32
    }
}

impl From<SettlementError> for solana_program_error::ProgramError {
    fn from(e: SettlementError) -> Self {
        Self::Custom(e.into())
    }
}

impl From<SettlementError> for solana_instruction_error::InstructionError {
    fn from(e: SettlementError) -> Self {
        Self::Custom(e.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The discriminants are contiguous, so the whole range must decode and
    /// round-trip, and the first code past it must not.
    #[test]
    fn custom_codes_round_trip() {
        for code in 0..=42 {
            let error = SettlementError::try_from(code).unwrap();
            assert_eq!(u32::from(error), code);
        }
        assert_eq!(SettlementError::try_from(43), Err(43));
    }
}
