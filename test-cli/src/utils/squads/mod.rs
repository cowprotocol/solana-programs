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

use program::accounts::{Multisig as MultisigAccount, MULTISIG_DISCRIMINATOR};
use program::instructions::{
    ProposalApprove, ProposalApproveInstructionArgs, ProposalCreate, ProposalCreateInstructionArgs,
    VaultTransactionCreate, VaultTransactionCreateInstructionArgs,
};
pub use program::programs::SQUADS_MULTISIG_PROGRAM_ID;

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
        let message = Message::new(ixs, Some(&self.vault(vault_index)));

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
            approved,
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

/// A proposal ready to be sent, as built by [`Multisig::propose`].
#[derive(Debug)]
pub struct Proposal {
    pub instructions: Vec<Instruction>,
    pub transaction_index: u64,
    /// The vault transaction account holding the proposed instructions.
    pub transaction: Pubkey,
    pub proposal: Pubkey,
    /// Whether `instructions` also cast the member's approval.
    pub approved: bool,
}

/// Encode a compiled legacy `message` in the compact `TransactionMessage`
/// layout that `vault_transaction_create` takes, which replaces the header with
/// signer and writable counts and shortens every length prefix to a `u8` (`u16`
/// for instruction data).
fn transaction_message(message: &Message) -> anyhow::Result<Vec<u8>> {
    let len = |len: usize| u8::try_from(len).context("too many items for a Squads transaction");

    let header = &message.header;
    let num_signers = header.num_required_signatures;
    let num_writable_non_signers = message
        .account_keys
        .len()
        .checked_sub(usize::from(num_signers))
        .and_then(|n| n.checked_sub(usize::from(header.num_readonly_unsigned_accounts)))
        .context("malformed message header")?;

    let mut data = vec![
        num_signers,
        num_signers
            .checked_sub(header.num_readonly_signed_accounts)
            .context("malformed message header")?,
        u8::try_from(num_writable_non_signers).context("too many writable accounts")?,
        len(message.account_keys.len())?,
    ];
    for key in &message.account_keys {
        data.extend(key.as_ref());
    }

    data.push(len(message.instructions.len())?);
    for ix in &message.instructions {
        data.push(ix.program_id_index);
        data.push(len(ix.accounts.len())?);
        data.extend(&ix.accounts);
        data.extend(
            u16::try_from(ix.data.len())
                .context("instruction data too long")?
                .to_le_bytes(),
        );
        data.extend(&ix.data);
    }

    // Legacy messages have no address table lookups.
    data.push(0);

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
        let message = Message::new(&[inner], Some(&multisig.vault(3)));
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
}
