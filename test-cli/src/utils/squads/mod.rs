//! Proposing transactions to a [Squads v4](https://github.com/Squads-Protocol/v4)
//! multisig instead of sending them directly.
//!
//! The accounts and instructions come from [`program`], generated from the
//! Squads IDL by `just generate-squads-client`. The PDA seeds and the compact
//! transaction message format aren't described by the IDL, so they're encoded
//! here.

use anyhow::Context as _;
use borsh::BorshDeserialize;
use cow_settlement_client::cow_settlement_interface::Pubkey;
use solana_instruction::Instruction;
use solana_rpc_client::rpc_client::RpcClient;
use solana_sdk::message::Message;

use program::accounts::{
    Multisig as MultisigAccount, Proposal as ProposalAccount, VaultTransaction,
    MULTISIG_DISCRIMINATOR, PROPOSAL_DISCRIMINATOR, VAULT_TRANSACTION_DISCRIMINATOR,
};
use program::instructions::{
    ProposalApprove, ProposalApproveInstructionArgs, ProposalCreate, ProposalCreateInstructionArgs,
    VaultTransactionCreate, VaultTransactionCreateInstructionArgs,
};
pub use program::programs::SQUADS_MULTISIG_PROGRAM_ID;
use program::types::{MultisigCompiledInstruction, ProposalStatus, VaultTransactionMessage};

#[allow(clippy::all, clippy::arithmetic_side_effects, unexpected_cfgs, unused)]
pub mod program;

const SEED_PREFIX: &[u8] = b"multisig";
const SEED_VAULT: &[u8] = b"vault";
const SEED_TRANSACTION: &[u8] = b"transaction";
const SEED_PROPOSAL: &[u8] = b"proposal";

/// `Permissions` mask bits a member needs to propose and to vote.
const PERMISSION_INITIATE: u8 = 1 << 0;
const PERMISSION_VOTE: u8 = 1 << 1;

/// An on-chain Squads multisig.
#[derive(Debug)]
pub struct Multisig {
    pub address: Pubkey,
    pub account: MultisigAccount,
}

/// A vault transaction on chain, with its proposal if one was opened.
#[derive(Debug)]
pub struct Existing {
    index: u64,
    transaction: VaultTransaction,
    proposal: Option<ProposalAccount>,
}

impl Multisig {
    /// Fetch and decode the multisig at `address`.
    pub fn fetch(rpc: &RpcClient, address: &Pubkey) -> anyhow::Result<Self> {
        let account = rpc
            .get_account(address)
            .with_context(|| format!("failed to fetch Squads multisig {address}"))?;
        Self::decode(*address, &account.owner, &account.data)
            .with_context(|| format!("account {address} is not a Squads multisig"))
    }

    fn decode(address: Pubkey, owner: &Pubkey, data: &[u8]) -> anyhow::Result<Self> {
        anyhow::ensure!(
            *owner == SQUADS_MULTISIG_PROGRAM_ID,
            "owned by {owner} rather than the Squads program"
        );
        Ok(Self {
            address,
            account: decode(data, &MULTISIG_DISCRIMINATOR).context("malformed multisig account")?,
        })
    }

    /// Fetch the vault transaction at `index` and its proposal.
    pub fn fetch_transaction(&self, rpc: &RpcClient, index: u64) -> anyhow::Result<Existing> {
        let accounts = rpc
            .get_multiple_accounts(&[self.transaction(index), self.proposal(index)])
            .with_context(|| format!("failed to fetch Squads transaction {index}"))?;
        let [transaction, proposal] = &accounts[..] else {
            anyhow::bail!("expected a transaction and a proposal account");
        };

        let transaction = transaction
            .as_ref()
            .with_context(|| format!("Squads transaction {index} doesn't exist"))?;
        let transaction = decode(&transaction.data, &VAULT_TRANSACTION_DISCRIMINATOR)
            .with_context(|| format!("Squads transaction {index} isn't a vault transaction"))?;
        let proposal = proposal
            .as_ref()
            .map(|proposal| {
                decode(&proposal.data, &PROPOSAL_DISCRIMINATOR)
                    .with_context(|| format!("malformed proposal for Squads transaction {index}"))
            })
            .transpose()?;

        Ok(Existing {
            index,
            transaction,
            proposal,
        })
    }

    /// The permission mask of `member`, which must be a member.
    fn member_permissions(&self, member: &Pubkey) -> anyhow::Result<u8> {
        self.account
            .members
            .iter()
            .find(|m| m.key == *member)
            .map(|m| m.permissions.mask)
            .with_context(|| format!("{member} is not a member of multisig {}", self.address))
    }

    pub fn vault(&self, vault_index: u8) -> Pubkey {
        find_pda(&[
            SEED_PREFIX,
            self.address.as_ref(),
            SEED_VAULT,
            &[vault_index],
        ])
    }

    fn transaction(&self, transaction_index: u64) -> Pubkey {
        find_pda(&[
            SEED_PREFIX,
            self.address.as_ref(),
            SEED_TRANSACTION,
            &transaction_index.to_le_bytes(),
        ])
    }

    fn proposal(&self, transaction_index: u64) -> Pubkey {
        find_pda(&[
            SEED_PREFIX,
            self.address.as_ref(),
            SEED_TRANSACTION,
            &transaction_index.to_le_bytes(),
            SEED_PROPOSAL,
        ])
    }

    /// The instructions that wrap `ixs` into the multisig's next vault
    /// transaction for `vault_index` and open a proposal for it, created and
    /// paid for by `member`. The proposal is also approved by `member` if it
    /// holds the vote permission.
    pub fn propose(
        &self,
        member: &Pubkey,
        vault_index: u8,
        ixs: &[Instruction],
    ) -> anyhow::Result<Proposal> {
        let permissions = self.member_permissions(member)?;
        anyhow::ensure!(
            permissions & PERMISSION_INITIATE != 0,
            "{member} lacks the initiate permission on multisig {}",
            self.address
        );

        let transaction_index = self
            .account
            .transaction_index
            .checked_add(1)
            .context("multisig transaction index overflow")?;
        let transaction = self.transaction(transaction_index);
        let approved = permissions & PERMISSION_VOTE != 0;
        let message = vault_message(&Message::new(ixs, Some(&self.vault(vault_index))))?;

        let mut instructions = vec![
            VaultTransactionCreate {
                multisig: self.address,
                transaction,
                creator: *member,
                rent_payer: *member,
                system_program: solana_system_interface::program::ID,
            }
            .instruction(VaultTransactionCreateInstructionArgs {
                vault_index,
                ephemeral_signers: 0,
                transaction_message: transaction_message(&message)?,
                memo: None,
            }),
            self.proposal_create_ix(member, transaction_index),
        ];
        if approved {
            instructions.push(self.proposal_approve_ix(member, transaction_index));
        }

        Ok(Proposal {
            instructions,
            transaction_index,
            transaction,
            proposal: self.proposal(transaction_index),
            created: true,
            approved,
        })
    }

    /// The instructions casting `member`'s approval of the `existing`
    /// transaction, opening its proposal first if nobody has. The transaction
    /// must hold exactly `ixs`, executed by the vault at `vault_index`.
    pub fn approve(
        &self,
        member: &Pubkey,
        vault_index: u8,
        ixs: &[Instruction],
        existing: &Existing,
    ) -> anyhow::Result<Proposal> {
        let index = existing.index;
        anyhow::ensure!(
            self.member_permissions(member)? & PERMISSION_VOTE != 0,
            "{member} lacks the vote permission on multisig {}",
            self.address
        );
        anyhow::ensure!(
            existing.transaction.vault_index == vault_index,
            "Squads transaction {index} is for vault {}, not vault {vault_index}",
            existing.transaction.vault_index
        );
        let message = vault_message(&Message::new(ixs, Some(&self.vault(vault_index))))?;
        anyhow::ensure!(
            existing.transaction.ephemeral_signer_bumps.is_empty()
                && existing.transaction.message == message,
            "Squads transaction {index} holds different instructions than this command builds"
        );
        anyhow::ensure!(
            index > self.account.stale_transaction_index,
            "Squads transaction {index} is stale: the multisig's members or threshold changed since"
        );

        let mut instructions = Vec::new();
        match &existing.proposal {
            None => instructions.push(self.proposal_create_ix(member, index)),
            Some(proposal) => match proposal.status {
                ProposalStatus::Active { .. } => anyhow::ensure!(
                    !proposal.approved.contains(member),
                    "{member} already approved Squads transaction {index}"
                ),
                ProposalStatus::Approved { .. } => anyhow::bail!(
                    "Squads transaction {index} is already approved and awaits execution"
                ),
                ProposalStatus::Draft { .. } => anyhow::bail!(
                    "Squads transaction {index} has a draft proposal, which must be activated before voting"
                ),
                ProposalStatus::Rejected { .. } => {
                    anyhow::bail!("Squads transaction {index} was rejected")
                }
                ProposalStatus::Executing | ProposalStatus::Executed { .. } => {
                    anyhow::bail!("Squads transaction {index} was already executed")
                }
                ProposalStatus::Cancelled { .. } => {
                    anyhow::bail!("Squads transaction {index} was cancelled")
                }
            },
        }
        instructions.push(self.proposal_approve_ix(member, index));

        Ok(Proposal {
            instructions,
            transaction_index: index,
            transaction: self.transaction(index),
            proposal: self.proposal(index),
            created: false,
            approved: true,
        })
    }

    fn proposal_create_ix(&self, member: &Pubkey, transaction_index: u64) -> Instruction {
        ProposalCreate {
            multisig: self.address,
            proposal: self.proposal(transaction_index),
            creator: *member,
            rent_payer: *member,
            system_program: solana_system_interface::program::ID,
        }
        .instruction(ProposalCreateInstructionArgs {
            transaction_index,
            draft: false,
        })
    }

    fn proposal_approve_ix(&self, member: &Pubkey, transaction_index: u64) -> Instruction {
        ProposalApprove {
            multisig: self.address,
            member: *member,
            proposal: self.proposal(transaction_index),
        }
        .instruction(ProposalApproveInstructionArgs { memo: None })
    }
}

fn find_pda(seeds: &[&[u8]]) -> Pubkey {
    Pubkey::find_program_address(seeds, &SQUADS_MULTISIG_PROGRAM_ID).0
}

/// Decode an Anchor account of the type `discriminator` identifies, or `None`
/// if `data` holds another type.
fn decode<T: BorshDeserialize>(data: &[u8], discriminator: &[u8; 8]) -> Option<T> {
    // Accounts may be allocated larger than their content, so trailing bytes
    // are ignored rather than rejected.
    data.starts_with(discriminator)
        .then(|| T::deserialize(&mut &data[..]).ok())
        .flatten()
}

/// A proposal ready to be sent, as built by [`Multisig::propose`] or
/// [`Multisig::approve`].
#[derive(Debug)]
pub struct Proposal {
    pub instructions: Vec<Instruction>,
    pub transaction_index: u64,
    /// The vault transaction account holding the proposed instructions.
    pub transaction: Pubkey,
    pub proposal: Pubkey,
    /// Whether `instructions` create a new vault transaction rather than
    /// approve an existing one.
    pub created: bool,
    /// Whether `instructions` also cast the member's approval.
    pub approved: bool,
}

/// A compiled legacy `message` as the Squads program stores it, which replaces
/// the header with signer and writable counts.
fn vault_message(message: &Message) -> anyhow::Result<VaultTransactionMessage> {
    let header = &message.header;
    let num_signers = header.num_required_signatures;
    let num_writable_non_signers = message
        .account_keys
        .len()
        .checked_sub(usize::from(num_signers))
        .and_then(|n| n.checked_sub(usize::from(header.num_readonly_unsigned_accounts)))
        .context("malformed message header")?;

    Ok(VaultTransactionMessage {
        num_signers,
        num_writable_signers: num_signers
            .checked_sub(header.num_readonly_signed_accounts)
            .context("malformed message header")?,
        num_writable_non_signers: u8::try_from(num_writable_non_signers)
            .context("too many writable accounts")?,
        account_keys: message.account_keys.clone(),
        instructions: message
            .instructions
            .iter()
            .map(|ix| MultisigCompiledInstruction {
                program_id_index: ix.program_id_index,
                account_indexes: ix.accounts.clone(),
                data: ix.data.clone(),
            })
            .collect(),
        address_table_lookups: Vec::new(),
    })
}

/// Encode `message` in the compact `TransactionMessage` layout that
/// `vault_transaction_create` takes, which shortens every length prefix to a
/// `u8` (`u16` for instruction data).
fn transaction_message(message: &VaultTransactionMessage) -> anyhow::Result<Vec<u8>> {
    let len = |len: usize| u8::try_from(len).context("too many items for a Squads transaction");

    let mut data = vec![
        message.num_signers,
        message.num_writable_signers,
        message.num_writable_non_signers,
        len(message.account_keys.len())?,
    ];
    for key in &message.account_keys {
        data.extend(key.as_ref());
    }

    data.push(len(message.instructions.len())?);
    for ix in &message.instructions {
        data.push(ix.program_id_index);
        data.push(len(ix.account_indexes.len())?);
        data.extend(&ix.account_indexes);
        data.extend(
            u16::try_from(ix.data.len())
                .context("instruction data too long")?
                .to_le_bytes(),
        );
        data.extend(&ix.data);
    }

    data.push(len(message.address_table_lookups.len())?);
    for lookup in &message.address_table_lookups {
        data.extend(lookup.account_key.as_ref());
        data.push(len(lookup.writable_indexes.len())?);
        data.extend(&lookup.writable_indexes);
        data.push(len(lookup.readonly_indexes.len())?);
        data.extend(&lookup.readonly_indexes);
    }

    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;
    use program::types::{Member, Permissions};
    use solana_instruction::AccountMeta;

    const VOTER: u8 = PERMISSION_INITIATE | PERMISSION_VOTE;

    fn member(key: Pubkey, mask: u8) -> Member {
        Member {
            key,
            permissions: Permissions { mask },
        }
    }

    fn multisig(members: Vec<Member>) -> Multisig {
        Multisig {
            address: Pubkey::new_unique(),
            account: MultisigAccount {
                discriminator: MULTISIG_DISCRIMINATOR,
                create_key: Pubkey::new_unique(),
                config_authority: Pubkey::default(),
                threshold: 2,
                time_lock: 0,
                transaction_index: 7,
                stale_transaction_index: 0,
                rent_collector: None,
                bump: 255,
                members,
            },
        }
    }

    fn inner_ix(multisig: &Multisig, vault_index: u8) -> Instruction {
        Instruction::new_with_bytes(
            Pubkey::new_unique(),
            &[9],
            vec![AccountMeta::new(multisig.vault(vault_index), true)],
        )
    }

    /// The vault transaction `index` holding `ixs`, with a proposal in
    /// `status` approved by `approved`, if any.
    fn existing(
        multisig: &Multisig,
        index: u64,
        vault_index: u8,
        ixs: &[Instruction],
        proposal: Option<(ProposalStatus, Vec<Pubkey>)>,
    ) -> Existing {
        Existing {
            index,
            transaction: VaultTransaction {
                discriminator: VAULT_TRANSACTION_DISCRIMINATOR,
                multisig: multisig.address,
                creator: Pubkey::new_unique(),
                index,
                bump: 255,
                vault_index,
                vault_bump: 255,
                ephemeral_signer_bumps: Vec::new(),
                message: vault_message(&Message::new(ixs, Some(&multisig.vault(vault_index))))
                    .unwrap(),
            },
            proposal: proposal.map(|(status, approved)| ProposalAccount {
                discriminator: PROPOSAL_DISCRIMINATOR,
                multisig: multisig.address,
                transaction_index: index,
                status,
                bump: 255,
                approved,
                rejected: Vec::new(),
                cancelled: Vec::new(),
            }),
        }
    }

    const ACTIVE: ProposalStatus = ProposalStatus::Active { timestamp: 0 };

    #[test]
    fn decodes_multisig_account() {
        let expected = multisig(vec![member(Pubkey::new_unique(), 7)]);
        let mut data = borsh::to_vec(&expected.account).unwrap();
        // Trailing space the account was allocated but doesn't use.
        data.extend([0; 32]);
        let decoded =
            Multisig::decode(expected.address, &SQUADS_MULTISIG_PROGRAM_ID, &data).unwrap();
        assert_eq!(decoded.address, expected.address);
        assert_eq!(decoded.account, expected.account);
    }

    #[test]
    fn rejects_non_multisig_accounts() {
        let multisig = multisig(vec![]);
        let data = borsh::to_vec(&multisig.account).unwrap();

        let err = Multisig::decode(multisig.address, &Pubkey::new_unique(), &data)
            .unwrap_err()
            .to_string();
        assert!(err.contains("rather than the Squads program"), "{err}");

        let mut wrong_discriminator = data.clone();
        wrong_discriminator[0] ^= 1;
        assert!(Multisig::decode(
            multisig.address,
            &SQUADS_MULTISIG_PROGRAM_ID,
            &wrong_discriminator,
        )
        .is_err());

        assert!(Multisig::decode(
            multisig.address,
            &SQUADS_MULTISIG_PROGRAM_ID,
            &data[..data.len() - 1]
        )
        .is_err());
    }

    #[test]
    fn encodes_transaction_message() {
        let vault = Pubkey::new_unique();
        let writable = Pubkey::new_unique();
        let readonly = Pubkey::new_unique();
        let program = Pubkey::new_unique();
        let message = Message::new(
            &[Instruction::new_with_bytes(
                program,
                &[1, 2, 3],
                vec![
                    AccountMeta::new_readonly(vault, true),
                    AccountMeta::new(writable, false),
                    AccountMeta::new_readonly(readonly, false),
                ],
            )],
            Some(&vault),
        );
        // `Message::new` orders keys as: writable signers, writable
        // non-signers, then read-only non-signers (the program last).
        assert_eq!(message.account_keys[..2], [vault, writable]);

        let mut expected = vec![1, 1, 1, 4];
        for key in &message.account_keys {
            expected.extend(key.as_ref());
        }
        let program_index = message.instructions[0].program_id_index;
        expected.extend([1, program_index, 3]);
        expected.extend(&message.instructions[0].accounts);
        expected.extend([3, 0, 1, 2, 3]);
        expected.push(0);

        let message = vault_message(&message).unwrap();
        assert_eq!(transaction_message(&message).unwrap(), expected);
    }

    #[test]
    fn proposes_and_approves_as_voting_member() {
        let member_key = Pubkey::new_unique();
        let multisig = multisig(vec![member(member_key, VOTER)]);
        let inner = inner_ix(&multisig, 3);

        let proposal = multisig
            .propose(&member_key, 3, std::slice::from_ref(&inner))
            .unwrap();
        assert_eq!(proposal.transaction_index, 8);
        assert_eq!(proposal.transaction, multisig.transaction(8));
        assert_eq!(proposal.proposal, multisig.proposal(8));
        assert!(proposal.created);
        assert!(proposal.approved);

        let [create, proposal_create, approve] = &proposal.instructions[..] else {
            panic!("expected create, proposal and approve instructions");
        };
        assert_eq!(create.accounts[1].pubkey, proposal.transaction);
        assert_eq!(proposal_create.accounts[1].pubkey, proposal.proposal);
        assert_eq!(approve.accounts[2].pubkey, proposal.proposal);

        // The vault transaction carries the inner instruction, executed by the
        // requested vault.
        let args =
            VaultTransactionCreateInstructionArgs::try_from_slice(&create.data[8..]).unwrap();
        assert_eq!(args.vault_index, 3);
        let message = vault_message(&Message::new(&[inner], Some(&multisig.vault(3)))).unwrap();
        assert_eq!(
            args.transaction_message,
            transaction_message(&message).unwrap()
        );

        let args =
            ProposalCreateInstructionArgs::try_from_slice(&proposal_create.data[8..]).unwrap();
        assert_eq!(args.transaction_index, 8);
        assert!(!args.draft);
    }

    #[test]
    fn proposes_without_approving_as_non_voting_member() {
        let member_key = Pubkey::new_unique();
        let proposal = multisig(vec![member(member_key, PERMISSION_INITIATE)])
            .propose(&member_key, 0, &[])
            .unwrap();
        assert!(proposal.created);
        assert!(!proposal.approved);
        assert_eq!(proposal.instructions.len(), 2);
    }

    #[test]
    fn rejects_proposals_from_non_initiators() {
        let voter = Pubkey::new_unique();
        let multisig = multisig(vec![member(voter, PERMISSION_VOTE)]);

        let err = multisig.propose(&voter, 0, &[]).unwrap_err().to_string();
        assert!(err.contains("initiate permission"), "{err}");

        let err = multisig
            .propose(&Pubkey::new_unique(), 0, &[])
            .unwrap_err()
            .to_string();
        assert!(err.contains("not a member"), "{err}");
    }

    /// A voting member of a multisig and the instructions they build.
    fn voter_setup() -> (Multisig, Pubkey, [Instruction; 1]) {
        let voter = Pubkey::new_unique();
        let multisig = multisig(vec![member(voter, PERMISSION_VOTE)]);
        let ixs = [inner_ix(&multisig, 0)];
        (multisig, voter, ixs)
    }

    #[test]
    fn approves_transaction_holding_the_same_instructions() {
        let creator = Pubkey::new_unique();
        let (mut multisig, voter, ixs) = voter_setup();
        multisig.account.members.push(member(creator, VOTER));
        let existing = existing(&multisig, 5, 0, &ixs, Some((ACTIVE, vec![creator])));

        let proposal = multisig.approve(&voter, 0, &ixs, &existing).unwrap();
        assert!(!proposal.created);
        assert!(proposal.approved);
        assert_eq!(proposal.transaction_index, 5);
        assert_eq!(proposal.transaction, multisig.transaction(5));
        assert_eq!(proposal.proposal, multisig.proposal(5));
        assert_eq!(
            proposal.instructions,
            [multisig.proposal_approve_ix(&voter, 5)]
        );
    }

    #[test]
    fn opens_missing_proposal_when_approving() {
        let (multisig, voter, ixs) = voter_setup();
        let existing = existing(&multisig, 5, 0, &ixs, None);

        let proposal = multisig.approve(&voter, 0, &ixs, &existing).unwrap();
        assert_eq!(
            proposal.instructions,
            [
                multisig.proposal_create_ix(&voter, 5),
                multisig.proposal_approve_ix(&voter, 5),
            ]
        );
    }

    /// Why `voter` can't approve `existing` with `ixs` for vault 0.
    fn approve_error(
        multisig: &Multisig,
        voter: &Pubkey,
        ixs: &[Instruction],
        existing: &Existing,
    ) -> String {
        multisig
            .approve(voter, 0, ixs, existing)
            .unwrap_err()
            .to_string()
    }

    #[test]
    fn rejects_approving_different_instructions() {
        let (multisig, voter, ixs) = voter_setup();
        let existing = existing(
            &multisig,
            5,
            0,
            &[inner_ix(&multisig, 0)],
            Some((ACTIVE, vec![])),
        );
        let err = approve_error(&multisig, &voter, &ixs, &existing);
        assert!(err.contains("holds different instructions"), "{err}");
    }

    #[test]
    fn rejects_approving_another_vaults_transaction() {
        let (multisig, voter, ixs) = voter_setup();
        let existing = existing(&multisig, 5, 1, &ixs, Some((ACTIVE, vec![])));
        let err = approve_error(&multisig, &voter, &ixs, &existing);
        assert!(err.contains("is for vault 1, not vault 0"), "{err}");
    }

    #[test]
    fn rejects_approving_stale_transaction() {
        let (mut multisig, voter, ixs) = voter_setup();
        multisig.account.stale_transaction_index = 5;
        let existing = existing(&multisig, 5, 0, &ixs, Some((ACTIVE, vec![])));
        let err = approve_error(&multisig, &voter, &ixs, &existing);
        assert!(err.contains("is stale"), "{err}");
    }

    #[test]
    fn rejects_approving_twice() {
        let (multisig, voter, ixs) = voter_setup();
        let existing = existing(&multisig, 5, 0, &ixs, Some((ACTIVE, vec![voter])));
        let err = approve_error(&multisig, &voter, &ixs, &existing);
        assert!(
            err.contains("already approved Squads transaction 5"),
            "{err}"
        );
    }

    #[test]
    fn rejects_approving_transaction_awaiting_execution() {
        let (multisig, voter, ixs) = voter_setup();
        let status = ProposalStatus::Approved { timestamp: 0 };
        let existing = existing(&multisig, 5, 0, &ixs, Some((status, vec![])));
        let err = approve_error(&multisig, &voter, &ixs, &existing);
        assert!(err.contains("awaits execution"), "{err}");
    }

    #[test]
    fn rejects_approving_draft_proposal() {
        let (multisig, voter, ixs) = voter_setup();
        let status = ProposalStatus::Draft { timestamp: 0 };
        let existing = existing(&multisig, 5, 0, &ixs, Some((status, vec![])));
        let err = approve_error(&multisig, &voter, &ixs, &existing);
        assert!(err.contains("draft proposal"), "{err}");
    }

    #[test]
    fn rejects_approving_closed_proposals() {
        let (multisig, voter, ixs) = voter_setup();
        for (status, expected) in [
            (ProposalStatus::Rejected { timestamp: 0 }, "was rejected"),
            (
                ProposalStatus::Executed { timestamp: 0 },
                "was already executed",
            ),
            (ProposalStatus::Cancelled { timestamp: 0 }, "was cancelled"),
        ] {
            let existing = existing(&multisig, 5, 0, &ixs, Some((status, vec![])));
            let err = approve_error(&multisig, &voter, &ixs, &existing);
            assert!(err.contains(expected), "{err}");
        }
    }

    #[test]
    fn rejects_approving_without_vote_permission() {
        let initiator = Pubkey::new_unique();
        let multisig = multisig(vec![member(initiator, PERMISSION_INITIATE)]);
        let ixs = [inner_ix(&multisig, 0)];
        let existing = existing(&multisig, 5, 0, &ixs, Some((ACTIVE, vec![])));
        let err = approve_error(&multisig, &initiator, &ixs, &existing);
        assert!(err.contains("lacks the vote permission"), "{err}");
    }
}
