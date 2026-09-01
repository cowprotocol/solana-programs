//! Transferable authority roles stored in the settlement state PDA.

use solana_program_error::ProgramError;

/// A transferable authority stored in the state PDA.
///
/// The discriminant is the wire value carried by the authority-transfer
/// instruction (see [`transfer_authority`](crate::instruction::transfer_authority)).
#[derive(Clone, Copy, Debug, Eq, PartialEq, num_enum::TryFromPrimitive)]
#[repr(u8)]
#[num_enum(error_type(name = ProgramError, constructor = Role::unknown_role))]
pub enum Role {
    /// The account authorized to add and remove solvers and to transfer roles.
    /// It is the highest authority: it may transfer any role.
    Manager = 0,
    /// The account authorized to close buffer accounts and reclaim their rent,
    /// choosing where that rent goes.
    ReclaimAuthority,
}

impl Role {
    /// Every [`Role`] variant, in discriminant order.
    pub const ALL: [Self; 2] = [Role::Manager, Role::ReclaimAuthority];

    /// The single wire byte that selects this role in the authority-transfer
    /// instruction.
    pub fn discriminator(self) -> u8 {
        self as u8
    }

    fn unknown_role(_: u8) -> ProgramError {
        ProgramError::InvalidInstructionData
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_try_from_partitions_all_bytes() {
        for i in u8::MIN..=u8::MAX {
            match Role::try_from(i) {
                Ok(role) => assert_eq!(role as u8, i),
                Err(err) => assert_eq!(err, ProgramError::InvalidInstructionData),
            }
        }
    }

    #[test]
    fn role_try_from_matches_manager() {
        assert_eq!(Role::try_from(0), Ok(Role::Manager));
    }

    #[test]
    fn all_roles_lists_every_role_in_discriminator_order() {
        // The roles `try_from` accepts, discovered independently of `Role::ALL`.
        // The scan runs over ascending bytes, so this is every role that exists,
        // in discriminant order.
        let every_role: Vec<Role> = (u8::MIN..=u8::MAX)
            .filter_map(|byte| Role::try_from(byte).ok())
            .collect();

        assert_eq!(Role::ALL.as_slice(), every_role.as_slice());
    }
}
