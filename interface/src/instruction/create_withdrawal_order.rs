//! `CreateWithdrawalOrder` instruction builder.

use solana_instruction::{AccountMeta, Instruction};
use solana_program_error::ProgramError;
use solana_pubkey::Pubkey;

pub use solana_system_interface::program::ID as SYSTEM_PROGRAM_ID;

use super::InstructionInputParsing;
use crate::{data::intent::EncodedOrderIntent, SettlementInstruction};

/// Builder for a `CreateWithdrawalOrder` instruction.
///
/// Allocates a per-order PDA (see [`crate::pda::order`]) for an order owned by
/// the settlement state PDA, so the fees that accumulate in the buffer accounts
/// can be sold through a regular settlement. The PDA's storage layout is the
/// same as any other order's, [`crate::data::order::EncodedOrderAccount`].
///
/// Unlike [`CreateOrder`](crate::instruction::create_order::CreateOrder), the
/// order isn't authenticated by its owner's signature: the owner is the
/// settlement state PDA, which has no key. Instead the instruction is gated by
/// the [`WithdrawalAuthority`](crate::Role::WithdrawalAuthority), a privileged
/// account that signs it. The program forces `intent.owner` to be the state
/// PDA, so a withdrawal order can only sell funds stored in the buffers.
/// Otherwise, the order this function created is a normal order and settles
/// through the standard `BeginSettle`/`FinalizeSettle` flow.
///
/// The withdrawal authority is trusted to choose sound order parameters.
/// The only enforced parameters are `created_on_chain` (should be true) and
/// the owner (should be the state PDA).
///
/// `payer` funds the new order PDA's rent and is recorded as its `created_by`
/// address, so `ReclaimOrder` refunds the rent there.
///
/// Wire format: `[discriminator=10, ..intent bytes]`,
/// `1 + EncodedOrderIntent::SIZE` bytes. Required accounts:
/// `[authority (S), payer (W,S), state_pda (R), order_pda (W),
/// system_program (R)]`. The system program needs to be available but doesn't
/// need to sit at that specific position, unlike the others.
pub struct CreateWithdrawalOrder {
    pub program_id: Pubkey,
    pub authority: Pubkey,
    pub payer: Pubkey,
    pub state_pda: Pubkey,
    pub order_pda: Pubkey,
    pub intent_bytes: [u8; EncodedOrderIntent::SIZE],
}

impl From<CreateWithdrawalOrder> for Instruction {
    fn from(builder: CreateWithdrawalOrder) -> Self {
        let mut data = Vec::with_capacity(1 + EncodedOrderIntent::SIZE);
        data.push(SettlementInstruction::CreateWithdrawalOrder.discriminator());
        data.extend_from_slice(&builder.intent_bytes);

        Instruction {
            program_id: builder.program_id,
            accounts: vec![
                AccountMeta::new_readonly(builder.authority, true),
                AccountMeta::new(builder.payer, true),
                AccountMeta::new_readonly(builder.state_pda, false),
                AccountMeta::new(builder.order_pda, false),
                AccountMeta::new_readonly(SYSTEM_PROGRAM_ID, false),
            ],
            data,
        }
    }
}

/// Parsed inputs of a `CreateWithdrawalOrder` instruction.
pub struct CreateWithdrawalOrderInput<'a, A> {
    pub intent_bytes: [u8; EncodedOrderIntent::SIZE],
    pub authority: &'a A,
    pub payer: &'a A,
    pub state_pda: &'a A,
    pub order_pda: &'a A,
}

impl<'a, A> InstructionInputParsing<'a, A> for CreateWithdrawalOrderInput<'a, A> {
    const DISCRIMINATOR: SettlementInstruction = SettlementInstruction::CreateWithdrawalOrder;

    fn parse_body(instruction_data: &'a [u8], accounts: &'a [A]) -> Result<Self, ProgramError> {
        // Body (discriminator already stripped): exactly the intent bytes.
        let intent_bytes: [u8; EncodedOrderIntent::SIZE] = instruction_data
            .try_into()
            .map_err(|_| ProgramError::InvalidInstructionData)?;

        // Accounts: [authority (S), payer (W,S), state_pda (R), order_pda (W),
        // system_program (R)]. We check that there are five accounts because
        // the instruction needs to specify `SYSTEM_PROGRAM_ID` as one of them
        // but it doesn't have to be the fifth one.
        let [authority, payer, state_pda, order_pda, _system_program, ..] = accounts else {
            return Err(ProgramError::NotEnoughAccountKeys);
        };

        Ok(Self {
            intent_bytes,
            authority,
            payer,
            state_pda,
            order_pda,
        })
    }
}

/// Test scaffolding for `CreateWithdrawalOrder` parsing and handling, shared by
/// this crate's tests and the settlement program's via the `test-fixtures`
/// feature.
#[cfg(any(test, feature = "test-fixtures"))]
pub mod fixtures {
    use solana_address::Address;

    use super::{CreateWithdrawalOrder, EncodedOrderIntent, Instruction};
    use crate::data::intent::OrderIntent;

    /// Number of accounts `CreateWithdrawalOrder` expects: authority, payer, state
    /// PDA, order PDA, and the system program.
    pub const NUM_ACCOUNTS: usize = 5;

    /// `CreateWithdrawalOrder` instruction data carrying `intent`, with
    /// placeholder addresses for failure cases where the addresses don't matter.
    pub fn withdrawal_order_data(intent: &OrderIntent) -> Vec<u8> {
        let zero = Address::new_from_array([0; 32]);
        Instruction::from(CreateWithdrawalOrder {
            program_id: zero,
            authority: zero,
            payer: zero,
            state_pda: zero,
            order_pda: zero,
            intent_bytes: (&EncodedOrderIntent::from(intent)).into(),
        })
        .data
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::{withdrawal_order_data, NUM_ACCOUNTS};
    use super::*;
    use crate::data::intent::fixtures::sample_intent;
    use crate::data::intent::OrderIntent;
    use crate::fixtures::pubkey_from_seed;
    use crate::instruction::fixtures::{fake_account, fake_sequential_accounts};
    use crate::instruction::tests::{
        assert_readonly_nonsigner, assert_readonly_signer, assert_writable_nonsigner,
        assert_writable_signer,
    };
    use solana_account_view::AccountView;

    /// A well-formed sample withdrawal order intent for these tests. The owner
    /// and flags aren't checked at this layer (only the handler does), so any
    /// well-formed intent works.
    fn intent() -> OrderIntent {
        sample_intent(Default::default())
    }

    #[test]
    fn create_withdrawal_order_input_parses_valid_input() {
        let program_id = pubkey_from_seed("program id");
        let authority = pubkey_from_seed("authority");
        let payer = pubkey_from_seed("payer");
        let state_pda = pubkey_from_seed("state pda");
        let order_pda = pubkey_from_seed("order pda");
        let intent_bytes: [u8; EncodedOrderIntent::SIZE] =
            (&EncodedOrderIntent::from(&intent())).into();

        let data = Instruction::from(CreateWithdrawalOrder {
            program_id,
            authority,
            payer,
            state_pda,
            order_pda,
            intent_bytes,
        })
        .data;
        let accounts = [
            fake_account(authority),
            fake_account(payer),
            fake_account(state_pda),
            fake_account(order_pda),
            fake_account(pubkey_from_seed("system program")),
        ];

        let CreateWithdrawalOrderInput {
            intent_bytes: derived_intent_bytes,
            authority: derived_authority,
            payer: derived_payer,
            state_pda: derived_state_pda,
            order_pda: derived_order_pda,
        } = CreateWithdrawalOrderInput::parse(&data, &accounts).expect("parse should succeed");

        assert_eq!(derived_intent_bytes, intent_bytes);
        assert_eq!(*derived_authority.address(), authority);
        assert_eq!(*derived_payer.address(), payer);
        assert_eq!(*derived_state_pda.address(), state_pda);
        assert_eq!(*derived_order_pda.address(), order_pda);
    }

    #[test]
    fn create_withdrawal_order_input_rejects_short_data() {
        let mut data = withdrawal_order_data(&intent());
        data.pop();
        let accounts = fake_sequential_accounts::<NUM_ACCOUNTS>();
        assert_eq!(
            CreateWithdrawalOrderInput::parse(&data, &accounts).err(),
            Some(ProgramError::InvalidInstructionData),
        );
    }

    #[test]
    fn create_withdrawal_order_input_rejects_long_data() {
        let mut data = withdrawal_order_data(&intent());
        data.push(0); // trailing byte
        let accounts = fake_sequential_accounts::<NUM_ACCOUNTS>();
        assert_eq!(
            CreateWithdrawalOrderInput::parse(&data, &accounts).err(),
            Some(ProgramError::InvalidInstructionData),
        );
    }

    #[test]
    fn create_withdrawal_order_input_rejects_missing_accounts() {
        let data = withdrawal_order_data(&intent());
        let mut accounts: Vec<AccountView> = fake_sequential_accounts::<NUM_ACCOUNTS>().into();
        accounts.pop();
        assert_eq!(
            CreateWithdrawalOrderInput::parse(&data, &accounts).err(),
            Some(ProgramError::NotEnoughAccountKeys),
        );
    }

    #[test]
    fn instruction_data_has_expected_layout() {
        let intent_bytes = [0x42u8; EncodedOrderIntent::SIZE];

        let Instruction { data, .. } = CreateWithdrawalOrder {
            program_id: pubkey_from_seed("program id"),
            authority: pubkey_from_seed("authority"),
            payer: pubkey_from_seed("payer"),
            state_pda: pubkey_from_seed("state pda"),
            order_pda: pubkey_from_seed("order pda"),
            intent_bytes,
        }
        .into();

        assert_eq!(data.len(), 1 + EncodedOrderIntent::SIZE);
        assert_eq!(
            data[0],
            SettlementInstruction::CreateWithdrawalOrder.discriminator()
        );
        assert_eq!(&data[1..], &intent_bytes);
    }

    #[test]
    fn instruction_data_has_expected_accounts() {
        let program_id = pubkey_from_seed("program id");
        let authority = pubkey_from_seed("authority");
        let payer = pubkey_from_seed("payer");
        let state_pda = pubkey_from_seed("state pda");
        let order_pda = pubkey_from_seed("order pda");
        let intent_bytes = [0u8; EncodedOrderIntent::SIZE];

        let Instruction { accounts, .. } = CreateWithdrawalOrder {
            program_id,
            authority,
            payer,
            state_pda,
            order_pda,
            intent_bytes,
        }
        .into();

        assert_eq!(accounts.len(), 5);
        // authority gates the order without paying rent; payer funds the new
        // PDA's rent; state_pda is only read; order_pda is created. The system
        // program is only referenced.
        assert_readonly_signer(&accounts[0], authority);
        assert_writable_signer(&accounts[1], payer);
        assert_readonly_nonsigner(&accounts[2], state_pda);
        assert_writable_nonsigner(&accounts[3], order_pda);
        assert_readonly_nonsigner(&accounts[4], SYSTEM_PROGRAM_ID);
    }
}
