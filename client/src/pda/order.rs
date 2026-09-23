//! Off-chain decoded snapshot of a settlement order account.

use cow_settlement_interface::{
    data::{
        intent::{EncodedOrderIntent, OrderIntent},
        order::{OrderAccount, SIZE},
    },
    Pubkey,
};
use solana_program_error::ProgramError;

/// The body stored in an order PDA.
#[cfg_attr(any(test, feature = "test-fixtures"), derive(Default))]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecodedOrderAccount {
    pub bump: u8,
    pub cancelled: bool,
    pub amount_withdrawn: u64,
    pub amount_received: u64,
    pub created_by: Pubkey,
    pub intent: OrderIntent,
}

impl DecodedOrderAccount {
    /// Encode back into the canonical order-account bytes; the inverse of
    /// `TryFrom<&[u8]>`.
    pub fn encode(&self) -> [u8; SIZE] {
        let mut bytes = [0u8; SIZE];
        OrderAccount::initialize(
            &mut bytes[..],
            self.bump,
            self.cancelled,
            self.amount_withdrawn,
            self.amount_received,
            &self.created_by,
            &EncodedOrderIntent::from(&self.intent),
        )
        .expect("a full-length buffer initializes");
        bytes
    }
}

impl TryFrom<&[u8]> for DecodedOrderAccount {
    type Error = ProgramError;

    fn try_from(bytes: &[u8]) -> Result<Self, Self::Error> {
        let order = OrderAccount::attach(bytes)?;
        let filled = order.filled_amounts();
        Ok(Self {
            bump: order.bump(),
            cancelled: order.cancelled()?,
            amount_withdrawn: filled.withdrawn,
            amount_received: filled.received,
            created_by: order.created_by(),
            intent: OrderIntent::try_from(order.intent_bytes())
                .map_err(|_| ProgramError::InvalidAccountData)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cow_settlement_interface::data::intent::fixtures::FLAGS_OFFSET;
    use cow_settlement_interface::data::order::fixtures::{
        sample_order_bytes, sample_order_fields, OrderFields, CANCELLED_OFFSET, INTENT_OFFSET,
    };
    use cow_settlement_interface::data::order::SIZE;

    impl From<OrderFields> for DecodedOrderAccount {
        fn from(fields: OrderFields) -> Self {
            let OrderFields {
                bump,
                cancelled,
                amount_withdrawn,
                amount_received,
                created_by,
                intent,
            } = fields;
            Self {
                bump,
                cancelled,
                amount_withdrawn,
                amount_received,
                created_by,
                intent,
            }
        }
    }

    #[test]
    fn decodes_a_stamped_order() {
        let decoded = DecodedOrderAccount::try_from(&sample_order_bytes(true)[..])
            .expect("valid order account");
        let expected: DecodedOrderAccount = sample_order_fields(true).into();
        assert_eq!(decoded, expected);
    }

    #[test]
    fn rejects_non_order_account() {
        // A zeroed buffer: right length, but its leading byte isn't the order
        // discriminator.
        let bytes = [0u8; SIZE];
        assert_eq!(
            DecodedOrderAccount::try_from(&bytes[..]),
            Err(ProgramError::InvalidAccountData),
        );
    }

    #[test]
    fn rejects_too_short_account() {
        let mut bytes: Vec<u8> = sample_order_bytes(true).into();
        bytes.pop();
        assert_eq!(
            DecodedOrderAccount::try_from(&bytes[..]),
            Err(ProgramError::InvalidAccountData),
        );
    }

    #[test]
    fn rejects_too_long_account() {
        let mut bytes: Vec<u8> = sample_order_bytes(true).into();
        bytes.push(0x42);
        assert_eq!(
            DecodedOrderAccount::try_from(&bytes[..]),
            Err(ProgramError::InvalidAccountData),
        );
    }

    #[test]
    fn rejects_non_boolean_cancelled() {
        let mut bytes = sample_order_bytes(true);
        bytes[CANCELLED_OFFSET] = 0x02;
        assert_eq!(
            DecodedOrderAccount::try_from(&bytes[..]),
            Err(ProgramError::InvalidAccountData),
        );
    }

    #[test]
    fn rejects_invalid_intent() {
        // A reserved flags bit inside the intent slot: the intent decoder rejects it.
        let mut bytes = sample_order_bytes(true);
        bytes[INTENT_OFFSET + FLAGS_OFFSET] = 0xff;
        assert_eq!(
            DecodedOrderAccount::try_from(&bytes[..]),
            Err(ProgramError::InvalidAccountData),
        );
    }
}
