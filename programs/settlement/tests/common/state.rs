use cow_settlement_client::instruction::Initialize;
use solana_sdk::signature::Keypair;

/// Send `ix` as the settlement's `Initialize`, signed by `payer`.
pub fn initialize(svm: &mut litesvm::LiteSVM, payer: &Keypair, ix: Initialize) {
    let tx = super::signed_tx(svm, payer, payer, ix);
    svm.send_transaction(tx).expect("initialize should succeed");
}
