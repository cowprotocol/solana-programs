//! Shared types and instruction builders for the CoW Protocol settlement program.

pub use solana_instruction::{AccountMeta, Instruction};
use solana_program_error::ProgramError;
pub use solana_pubkey::Pubkey;

solana_pubkey::declare_id!("MooohhPEAAHwAwEozL7JPEmnDvaahuUpccYN4Yb8ccK");

pub mod data;
pub mod instruction;
pub mod pda;

#[derive(Clone, Copy, Debug, Eq, PartialEq, num_enum::TryFromPrimitive)]
#[repr(u8)]
#[num_enum(error_type(
    name = ProgramError,
    constructor = SettlementInstruction::unknown_discriminator,
))]
pub enum SettlementInstruction {
    BeginSettle = 0,
    FinalizeSettle = 1,
    CreateOrder = 2,
    Initialize = 3,
    CreateBuffer = 4,
    ReclaimOrder = 5,
}

impl SettlementInstruction {
    pub fn discriminator(self) -> u8 {
        self as u8
    }

    fn unknown_discriminator(_: u8) -> ProgramError {
        ProgramError::InvalidInstructionData
    }
}

/// Identifies the account type a given account's data belongs to, via the
/// single discriminator byte stored at its front. Starts at 128 to keep
/// account discriminators visually distinct from instruction discriminators.
#[derive(Clone, Copy, Debug, Eq, PartialEq, num_enum::TryFromPrimitive)]
#[repr(u8)]
#[num_enum(error_type(
    name = ProgramError,
    constructor = SettlementAccount::unknown_discriminator,
))]
pub enum SettlementAccount {
    OrderAccount = 128,
    SettlementState = 129,
}

impl SettlementAccount {
    pub const fn discriminator(self) -> u8 {
        self as u8
    }

    fn unknown_discriminator(_: u8) -> ProgramError {
        ProgramError::InvalidAccountData
    }
}

/// Recover the discriminator from the first byte of the payload and the
/// remaining bytes to parse.
/// Returns `InvalidInstructionData` for an insufficient length or an
/// unknown discriminator.
pub fn recover_discriminator(
    instruction_data: &[u8],
) -> Result<(SettlementInstruction, &[u8]), ProgramError> {
    let discriminator = instruction_data
        .first()
        .copied()
        .ok_or(ProgramError::InvalidInstructionData)
        .and_then(SettlementInstruction::try_from)?;
    Ok((discriminator, &instruction_data[1..]))
}

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
    /// `CreateOrder` instruction wasn't signed by the created `OrderIntent`
    /// owner.
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
    /// `FinalizeSettle`: a push doesn't draw funds from the canonical buffer
    /// for its destination's mint.
    PushSourceNotBuffer = 22,
    /// `FinalizeSettle`: a push's destination isn't a valid SPL token account
    /// (wrong data length or not owned by the token program), so its mint can't
    /// be read to derive the buffer.
    InvalidBuyTokenAccount = 23,
    /// `BeginSettle`: a settled order's executed price (`amount_out/amount_in`)
    /// is worse than the order's limit price (`buy_amount/sell_amount`).
    LimitPriceViolated = 24,
    /// `BeginSettle`: an order's pull amounts sum to more than `u64::MAX`.
    PullAmountOverflow = 25,
    /// `BeginSettle`: filling this order would consume more tokens than the
    /// maximum the user is willing to trade on this intent.
    /// Sell: `amount_in > sell_amount`; buy: `amount_out > buy_amount`.
    FillExceedsOrderAmount = 26,
    /// `BeginSettle`: a non-`partially_fillable` order isn't filled completely
    /// Sell: `amount_in != sell_amount`; buy: total `amount_out != buy_amount`.
    OrderNotFullyFilled = 27,
    /// `BeginSettle`: the order's cumulative `amount_withdrawn` would exceed
    /// `u64::MAX` once this settlement's pulls are added.
    AmountWithdrawnOverflow = 28,
    /// `BeginSettle`: the order's cumulative `amount_received` would exceed
    /// `u64::MAX` once this settlement's push is added.
    AmountReceivedOverflow = 29,
    /// `ReclaimOrder` was called before the order's `valid_to` has elapsed.
    OrderNotExpired = 30,
    /// `ReclaimOrder`'s `reclaim_recipient` account doesn't match the
    /// `created_by` address recorded in the order.
    ReclaimRecipientMismatch = 31,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_empty_payload() {
        assert_eq!(
            recover_discriminator(&[]),
            Err(ProgramError::InvalidInstructionData),
        );
    }

    #[test]
    fn rejects_unknown_discriminator() {
        // 42 is outside the set of valid discriminators.
        assert_eq!(
            recover_discriminator(&[42]),
            Err(ProgramError::InvalidInstructionData),
        );
    }

    #[test]
    fn forwards_trailing_bytes() {
        assert!(matches!(
            recover_discriminator(&[
                SettlementInstruction::BeginSettle.discriminator(),
                42 // unused
            ]),
            Ok((SettlementInstruction::BeginSettle, [42])),
        ));
    }

    #[test]
    fn settlement_instruction_try_from_partitions_all_bytes() {
        for i in u8::MIN..=u8::MAX {
            match SettlementInstruction::try_from(i) {
                Ok(ix) => assert_eq!(ix as u8, i),
                Err(err) => assert_eq!(err, ProgramError::InvalidInstructionData),
            }
        }
    }

    #[test]
    fn settlement_instruction_try_from_matches_begin_settle() {
        assert_eq!(
            SettlementInstruction::try_from(0),
            Ok(SettlementInstruction::BeginSettle)
        );
    }

    #[test]
    fn settlement_account_try_from_partitions_all_bytes() {
        for i in u8::MIN..=u8::MAX {
            match SettlementAccount::try_from(i) {
                Ok(account) => assert_eq!(account as u8, i),
                Err(err) => assert_eq!(err, ProgramError::InvalidAccountData),
            }
        }
    }

    #[test]
    fn settlement_account_discriminators_are_distinct() {
        assert_ne!(
            SettlementAccount::OrderAccount.discriminator(),
            SettlementAccount::SettlementState.discriminator(),
        );
    }
}
