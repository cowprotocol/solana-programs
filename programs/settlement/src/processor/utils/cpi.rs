//! Detection of cross-program invocation, shared across instruction handlers.

use solana_instruction::{syscalls::get_stack_height, TRANSACTION_LEVEL_STACK_HEIGHT};

pub fn is_cpi_call() -> bool {
    get_stack_height() > TRANSACTION_LEVEL_STACK_HEIGHT
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_cpi_false_outside_solana_lib() {
        assert!(!is_cpi_call());
    }
}
