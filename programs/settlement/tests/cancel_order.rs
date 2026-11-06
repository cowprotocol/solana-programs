use cow_settlement::pda::state::STATE_PDA;
use cow_settlement_client::cow_settlement_interface::{
    data::intent::{EncodedOrderIntent, OrderIntent},
    instruction::cancel_order::CancelOrder,
    pda::order::find_order_pda,
    Instruction, SettlementError,
};
use cow_settlement_client::instruction::{
    CancelSettlementOwnedOrder, CreateOrder, CreateSettlementOwnedOrder,
};
use cow_settlement_client::pda::order::DecodedOrderAccount;
use litesvm::{types::TransactionMetadata, LiteSVM};
use solana_sdk::{
    account::Account,
    instruction::InstructionError,
    program_error::ProgramError,
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    transaction::{Transaction, TransactionError},
};

use crate::common::{
    assert_instruction_error, assert_rent_exempt,
    benchmark::{send_transaction_metered, BenchLabel},
    create_account,
    order::{read_order, sample_intent},
    send_with_signers, setup_init, strip_signer, unique_keypair, unique_pubkey, InitializedParams,
};

mod common;

/// Who owns the order being cancelled.
#[derive(Clone, Copy)]
enum OrderOwner {
    /// A user.
    User,
    /// The settlement state PDA. The settlement-owned-order authority signs in
    /// its place, with the state PDA passed along.
    StatePda,
    /// The settlement-owned-order authority's own key: a plain user order that
    /// happens to belong to the authority.
    SettlementOwnedOrderAuthority,
}

struct Fixture {
    svm: LiteSVM,
    params: InitializedParams,
    intent: OrderIntent,
    bump: u8,
    /// Signs for the `owner` slot, whenever the instruction asks for it.
    owner: Keypair,
    cancel: CancelOrder,
    /// Accounts whose signer flag is stripped from the instruction.
    unsigned: Vec<Pubkey>,
}

impl Fixture {
    /// When `existing`, the order is placed before the cancellation; otherwise
    /// the cancellation creates it.
    fn new(order_owner: OrderOwner, existing: bool) -> Self {
        let (mut svm, params) = setup_init();
        let (owner, intent_owner, state_pda) = match order_owner {
            OrderOwner::User => {
                let owner = unique_keypair();
                let key = owner.pubkey();
                (owner, key, None)
            }
            OrderOwner::StatePda => (
                params.settlement_owned_order.insecure_clone(),
                STATE_PDA,
                Some(STATE_PDA),
            ),
            OrderOwner::SettlementOwnedOrderAuthority => {
                let authority = params.settlement_owned_order.insecure_clone();
                let key = authority.pubkey();
                (authority, key, None)
            }
        };

        let intent = sample_intent(intent_owner, 0);
        let encoded = EncodedOrderIntent::from(&intent);
        let (order_pda, bump) = find_order_pda(&params.program_id, &encoded.hash());

        if existing {
            let place: Instruction = match order_owner {
                OrderOwner::StatePda => CreateSettlementOwnedOrder {
                    program_id: params.program_id,
                    authority: owner.pubkey(),
                    created_by: params.payer.pubkey(),
                    intent: &intent,
                }
                .into(),
                OrderOwner::User | OrderOwner::SettlementOwnedOrderAuthority => CreateOrder {
                    program_id: params.program_id,
                    owner: owner.pubkey(),
                    created_by: params.payer.pubkey(),
                    intent: &intent,
                }
                .into(),
            };
            send_with_signers(&mut svm, &params.payer, &[&owner], &[place])
                .expect("placing the order should succeed");
        }

        let cancel = CancelOrder {
            program_id: params.program_id,
            owner: owner.pubkey(),
            created_by: params.payer.pubkey(),
            order_pda,
            intent_bytes: Some((&encoded).into()),
            state_pda,
        };
        Self {
            svm,
            params,
            intent,
            bump,
            owner,
            cancel,
            unsigned: Vec::new(),
        }
    }

    /// The canonical order PDA of the intent, whatever the instruction names.
    fn order_pda(&self) -> Pubkey {
        find_order_pda(&self.params.program_id, &self.intent.uid()).0
    }

    fn instruction(&self) -> Instruction {
        let mut ix = Instruction::from(self.cancel.clone());
        for key in &self.unsigned {
            strip_signer(&mut ix, key);
        }
        ix
    }

    fn transaction(&self) -> Transaction {
        let ix = self.instruction();
        let owner_signs = ix
            .accounts
            .iter()
            .any(|meta| meta.is_signer && meta.pubkey == self.owner.pubkey());
        let mut signers = vec![&self.params.payer];
        if owner_signs {
            signers.push(&self.owner);
        }
        Transaction::new_signed_with_payer(
            &[ix],
            Some(&self.params.payer.pubkey()),
            &signers,
            self.svm.latest_blockhash(),
        )
    }

    fn send(&mut self) -> Result<TransactionMetadata, TransactionError> {
        let tx = self.transaction();
        self.svm.send_transaction(tx).map_err(|failed| failed.err)
    }

    fn send_metered(&mut self) -> Result<TransactionMetadata, TransactionError> {
        let tx = self.transaction();
        send_transaction_metered(&mut self.svm, tx, BenchLabel::CancelOrder)
            .map_err(|failed| failed.err)
    }

    /// Assert the order PDA holds the order, cancelled and funded by the
    /// payer, and nothing else changed.
    #[track_caller]
    fn assert_order_cancelled(&self) {
        let account = self
            .svm
            .get_account(&self.order_pda())
            .expect("order PDA must exist");
        assert_eq!(
            account.owner, self.params.program_id,
            "PDA must be program-owned"
        );
        let expected = DecodedOrderAccount {
            bump: self.bump,
            cancelled: true,
            amount_withdrawn: 0,
            amount_received: 0,
            created_by: self.params.payer.pubkey(),
            intent: self.intent.clone(),
        };
        assert_eq!(
            account.data,
            expected.encode(),
            "the order must be cancelled and otherwise as placed"
        );
        assert_rent_exempt(&self.svm, &account);
    }

    /// Send the cancellation, expecting it to fail with `expected` and to leave
    /// the order PDA (or its absence) untouched.
    #[track_caller]
    fn assert_rejected(mut self, expected: impl Into<InstructionError>) {
        let before = self.svm.get_account(&self.order_pda());
        let result = self.send();
        assert_instruction_error(result, expected);
        assert_eq!(
            self.svm.get_account(&self.order_pda()),
            before,
            "a rejected cancellation must leave the order PDA untouched"
        );
    }

    /// Sign as an account that isn't allowed to cancel the order.
    fn use_wrong_owner(&mut self) {
        self.owner = unique_keypair();
        self.cancel.owner = self.owner.pubkey();
    }

    /// Sign as the settlement-owned-order authority, passing the state PDA as
    /// for a settlement-owned order.
    fn sign_as_settlement_owned_order_authority(&mut self) {
        self.owner = self.params.settlement_owned_order.insecure_clone();
        self.cancel.owner = self.owner.pubkey();
        self.cancel.state_pda = Some(STATE_PDA);
    }

    /// Keep the owner in its slot, but without its signature.
    fn unsign_owner(&mut self) {
        self.unsigned.push(self.owner.pubkey());
    }

    /// Pass as `created_by` an account other than the payer, without its
    /// signature.
    fn use_unsigned_created_by(&mut self) {
        let created_by = unique_pubkey();
        self.cancel.created_by = created_by;
        self.unsigned.push(created_by);
    }

    /// Send no intent bytes, so the order can only be recovered from its PDA.
    fn omit_intent_bytes(&mut self) {
        self.cancel.intent_bytes = None;
    }

    /// Name `order_pda` instead of the canonical order PDA.
    fn use_order_pda(&mut self, order_pda: Pubkey) {
        self.cancel.order_pda = order_pda;
    }

    /// Drop the trailing state PDA account.
    fn omit_state_pda(&mut self) {
        assert_eq!(
            self.cancel.state_pda.take(),
            Some(STATE_PDA),
            "the cancellation must pass the state PDA"
        );
    }

    /// Copy the state pda to a new PDA to validate non canonical state pda
    fn use_non_canonical_state_pda(&mut self) {
        let state = self
            .svm
            .get_account(&STATE_PDA)
            .expect("state account must exist after initialization");
        let copy = create_account(&mut self.svm, &state.owner, &state.data);
        assert_eq!(
            self.cancel.state_pda.replace(copy),
            Some(STATE_PDA),
            "the cancellation must pass the state PDA"
        );
    }

    /// Wipe the state account, as before `Initialize`: there's no authority
    /// left to check against.
    fn uninitialize_state(&mut self) {
        self.svm
            .set_account(STATE_PDA, Account::default())
            .expect("set_account should succeed");
    }
}

/// Orders owned by a user, who cancels them by signing as the owner.
mod user_owned {
    use super::*;

    /// Cancelling an order whose PDA already exists.
    mod existing {
        use super::*;

        fn fixture() -> Fixture {
            Fixture::new(OrderOwner::User, true)
        }

        #[test]
        fn cancels_the_order() {
            let mut f = fixture();
            f.send_metered().expect("cancel_order should succeed");
            f.assert_order_cancelled();
        }

        #[test]
        fn recovers_the_order_without_intent_bytes() {
            let mut f = fixture();
            f.omit_intent_bytes();
            f.send_metered()
                .expect("cancelling an existing order must not require its intent bytes");
            f.assert_order_cancelled();
        }

        #[test]
        fn does_not_need_created_by_signature() {
            let mut f = fixture();
            // An unrelated `created_by` is ignored for an existing order: it
            // needn't sign, and the stored funder stays the payer.
            f.use_unsigned_created_by();
            f.send()
                .expect("cancelling an existing order must not require created_by to sign");
            f.assert_order_cancelled();
        }

        #[test]
        fn is_idempotent() {
            let mut f = fixture();
            f.send().expect("first cancellation should succeed");
            let after_first = f.svm.get_account(&f.order_pda());

            // A fresh blockhash so the byte-identical second cancellation isn't
            // rejected as a duplicate transaction before it reaches the program.
            f.svm.expire_blockhash();
            f.send().expect("second cancellation should succeed");
            assert_eq!(
                f.svm.get_account(&f.order_pda()),
                after_first,
                "cancelling an already-cancelled order must not change it"
            );
        }

        #[test]
        fn rejects_wrong_owner() {
            let mut f = fixture();
            f.use_wrong_owner();
            f.assert_rejected(SettlementError::OwnerMismatch);
        }

        #[test]
        fn rejects_unsigned_owner() {
            let mut f = fixture();
            f.unsign_owner();
            f.assert_rejected(InstructionError::MissingRequiredSignature);
        }

        #[test]
        fn rejects_the_settlement_owned_order_authority() {
            let mut f = fixture();
            f.sign_as_settlement_owned_order_authority();
            f.assert_rejected(SettlementError::OwnerMismatch);
        }

        #[test]
        fn lets_the_settlement_owned_order_authority_cancel_its_own_order() {
            // An order owned by the authority's own key is a plain user order: it
            // cancels as its owner, without the state PDA.
            let mut f = Fixture::new(OrderOwner::SettlementOwnedOrderAuthority, true);
            f.send()
                .expect("the authority should cancel its own order like any owner");
            f.assert_order_cancelled();
        }
    }

    /// Cancelling an order whose PDA doesn't exist yet, which creates it
    /// already cancelled.
    mod not_existing {
        use super::*;

        fn fixture() -> Fixture {
            Fixture::new(OrderOwner::User, false)
        }

        #[test]
        fn creates_the_order_cancelled() {
            let mut f = fixture();
            f.send_metered()
                .expect("cancel_order should create the order already cancelled");
            f.assert_order_cancelled();

            // Creating the same order now must be rejected: its PDA already
            // exists, and the rejection must leave the cancelled flag set.
            // Otherwise a signed intent could be un-cancelled by racing a
            // creation in after the cancellation.
            let cancelled = f.svm.get_account(&f.order_pda());
            let create = CreateOrder {
                program_id: f.params.program_id,
                owner: f.owner.pubkey(),
                created_by: f.params.payer.pubkey(),
                intent: &f.intent,
            };
            let result =
                send_with_signers(&mut f.svm, &f.params.payer, &[&f.owner], &[create.into()]);
            assert_instruction_error(result, InstructionError::AccountAlreadyInitialized);
            assert_eq!(
                f.svm.get_account(&f.order_pda()),
                cancelled,
                "a rejected creation must leave the cancelled order byte-for-byte unchanged",
            );
        }

        #[test]
        fn rejects_missing_intent_bytes() {
            // Neither intent bytes nor an existing PDA: nothing to recover and
            // nothing to create from.
            let mut f = fixture();
            f.omit_intent_bytes();
            f.assert_rejected(InstructionError::UninitializedAccount);
        }

        #[test]
        fn rejects_unsigned_created_by() {
            // The rent-funding CPI to the System program rejects the unsigned
            // funder.
            let mut f = fixture();
            f.use_unsigned_created_by();
            f.assert_rejected(InstructionError::PrivilegeEscalation);
        }

        #[test]
        fn rejects_wrong_owner() {
            let mut f = fixture();
            f.use_wrong_owner();
            f.assert_rejected(SettlementError::OwnerMismatch);
        }

        #[test]
        fn rejects_unsigned_owner() {
            let mut f = fixture();
            f.unsign_owner();
            f.assert_rejected(InstructionError::MissingRequiredSignature);
        }

        #[test]
        fn rejects_the_settlement_owned_order_authority() {
            let mut f = fixture();
            f.sign_as_settlement_owned_order_authority();
            f.assert_rejected(SettlementError::OwnerMismatch);
        }

        #[test]
        fn rejects_wrong_order_pda() {
            let mut f = fixture();
            let wrong_pda = unique_pubkey();
            f.use_order_pda(wrong_pda);
            let tx = f.transaction();
            common::pda::assert_rejected_as_noncanonical(&mut f.svm, tx, &wrong_pda);
        }
    }
}

/// Orders owned by the settlement state PDA, which the settlement-owned-order
/// authority signs for in place of the owner.
mod settlement_owned {
    use super::*;

    /// The client builder for a settlement-owned cancellation, which must
    /// match the fixture's instruction.
    fn client_instruction(f: &Fixture) -> Instruction {
        CancelSettlementOwnedOrder {
            program_id: f.params.program_id,
            authority: f.owner.pubkey(),
            created_by: f.params.payer.pubkey(),
            intent: &f.intent,
        }
        .into()
    }

    /// Cancelling a settlement-owned order whose PDA already exists.
    mod existing {
        use super::*;

        fn fixture() -> Fixture {
            Fixture::new(OrderOwner::StatePda, true)
        }

        #[test]
        fn cancels_the_order() {
            let mut f = fixture();
            assert_eq!(f.instruction(), client_instruction(&f));
            f.send_metered()
                .expect("cancelling a settlement-owned order should succeed");
            f.assert_order_cancelled();
        }

        #[test]
        fn rejects_missing_state_pda() {
            let mut f = fixture();
            f.omit_state_pda();
            // The runtime maps the handler's error to a deprecated
            // `InstructionError` variant, so go through its code rather than
            // naming the variant.
            f.assert_rejected(InstructionError::from(u64::from(
                ProgramError::NotEnoughAccountKeys,
            )));
        }

        #[test]
        fn rejects_non_canonical_state_pda() {
            let mut f = fixture();
            f.use_non_canonical_state_pda();
            f.assert_rejected(SettlementError::StateAccountMismatch);
        }

        #[test]
        fn rejects_wrong_authority() {
            let mut f = fixture();
            f.use_wrong_owner();
            f.assert_rejected(SettlementError::UnauthorizedSettlementOwnedOrder);
        }

        #[test]
        fn rejects_unsigned_authority() {
            let mut f = fixture();
            f.unsign_owner();
            f.assert_rejected(InstructionError::MissingRequiredSignature);
        }

        #[test]
        fn rejects_uninitialized_state() {
            let mut f = fixture();
            f.uninitialize_state();
            f.assert_rejected(InstructionError::InvalidAccountData);
        }
    }

    /// Cancelling a settlement-owned order whose PDA doesn't exist yet, which
    /// creates it already cancelled.
    mod not_existing {
        use super::*;

        fn fixture() -> Fixture {
            Fixture::new(OrderOwner::StatePda, false)
        }

        #[test]
        fn creates_the_order_cancelled() {
            let mut f = fixture();
            assert_eq!(f.instruction(), client_instruction(&f));
            f.send()
                .expect("creating a cancelled settlement-owned order should succeed");
            f.assert_order_cancelled();

            // The cancelled PDA occupies the uid, so the order can't be placed.
            let place = CreateSettlementOwnedOrder {
                program_id: f.params.program_id,
                authority: f.owner.pubkey(),
                created_by: f.params.payer.pubkey(),
                intent: &f.intent,
            };
            let result =
                send_with_signers(&mut f.svm, &f.params.payer, &[&f.owner], &[place.into()]);
            assert_instruction_error(result, InstructionError::AccountAlreadyInitialized);
            assert!(read_order(&f.svm, &f.order_pda()).cancelled);
        }

        #[test]
        fn rejects_missing_state_pda() {
            let mut f = fixture();
            f.omit_state_pda();
            f.assert_rejected(InstructionError::from(u64::from(
                ProgramError::NotEnoughAccountKeys,
            )));
        }

        #[test]
        fn rejects_non_canonical_state_pda() {
            let mut f = fixture();
            f.use_non_canonical_state_pda();
            f.assert_rejected(SettlementError::StateAccountMismatch);
        }

        #[test]
        fn rejects_wrong_authority() {
            let mut f = fixture();
            f.use_wrong_owner();
            f.assert_rejected(SettlementError::UnauthorizedSettlementOwnedOrder);
        }

        #[test]
        fn rejects_unsigned_authority() {
            let mut f = fixture();
            f.unsign_owner();
            f.assert_rejected(InstructionError::MissingRequiredSignature);
        }

        #[test]
        fn rejects_uninitialized_state() {
            let mut f = fixture();
            f.uninitialize_state();
            f.assert_rejected(InstructionError::InvalidAccountData);
        }
    }
}
