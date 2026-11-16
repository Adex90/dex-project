//! The KMS signer, through the production code path (alloy's AwsSigner over the KMS wire protocol): the notary's
//! digests and the relayer's EIP-1559 transactions, low-s whatever KMS returns, bound to one chain.

mod common;

use alloy::consensus::{SignableTransaction, TxEip1559};
use alloy::network::TxSigner;
use alloy::primitives::{Address, B256, Bytes, TxKind, U256, keccak256};
use alloy::signers::Signer;
use common::{FakeKms, address_of, test_key};
use zk_oracle::kms;
use zk_oracle::notary::{encode_signature, notary_digest, recover};

/// secp256k1's n / 2: a signature with a larger s is malleable, and ZkDecoOracle refuses it.
const HALF_N: U256 =
    U256::from_be_bytes(alloy::hex!("7FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF5D576E7357A4501DDFE92F46681B20A0"));

#[tokio::test]
async fn the_signer_reads_the_public_key_once_and_refuses_an_unknown_key() {
    let key = test_key("kms test: notary");
    let kms = FakeKms::start(&[("notary-key", key.clone())]).await;
    let signer = kms::signer(&kms.config("notary-key"), None).await.unwrap();
    assert_eq!(Signer::address(&signer), address_of(&key));
    assert_eq!(kms.public_key_reads(), 1);

    let error = kms::signer(&kms.config("no-such-key"), None).await.unwrap_err();
    assert!(error.contains("no-such-key"), "{error}");
}

#[tokio::test]
async fn notary_digests_are_signed_low_s_even_when_kms_answers_high_s() {
    let key = test_key("kms test: notary");
    let kms = FakeKms::start(&[("notary-key", key.clone())]).await;
    let signer = kms::signer(&kms.config("notary-key"), None).await.unwrap();
    let digest = notary_digest(common::provider_id(), B256::from(keccak256("a body commitment")));

    for high_s in [false, true] {
        kms.set_high_s(high_s);
        let signature = signer.sign_hash(&digest).await.unwrap();
        assert!(signature.s() <= HALF_N, "high_s={high_s}: s is normalized");
        let encoded = encode_signature(&signature);
        assert!(encoded[64] == 27 || encoded[64] == 28);
        assert_eq!(recover(&encoded, digest).unwrap(), address_of(&key), "high_s={high_s}");
    }
    let signed = kms.signed();
    assert_eq!(signed, vec![("notary-key".to_string(), digest), ("notary-key".to_string(), digest)]);
}

fn push_transaction(chain_id: u64) -> TxEip1559 {
    TxEip1559 {
        chain_id,
        nonce: 7,
        gas_limit: 400_000,
        max_fee_per_gas: 200_000_000,
        max_priority_fee_per_gas: 0,
        to: TxKind::Call(Address::repeat_byte(0xfe)),
        value: U256::ZERO,
        access_list: Default::default(),
        input: Bytes::from(vec![0xab; 4 + 32 * 9 + 353]),
    }
}

#[tokio::test]
async fn the_relayer_key_signs_transactions_for_its_chain_only() {
    let key = test_key("kms test: relayer");
    let kms = FakeKms::start(&[("relayer-key", key.clone())]).await;
    let signer = kms::signer(&kms.config("relayer-key"), Some(421_614)).await.unwrap();

    for high_s in [false, true] {
        kms.set_high_s(high_s);
        let mut tx = push_transaction(421_614);
        let signature = TxSigner::sign_transaction(&signer, &mut tx).await.unwrap();
        assert!(signature.s() <= HALF_N);
        let signed = tx.into_signed(signature);
        assert_eq!(signed.recover_signer().unwrap(), address_of(&key), "high_s={high_s}");
    }

    let mut elsewhere = push_transaction(1);
    assert!(
        TxSigner::sign_transaction(&signer, &mut elsewhere).await.is_err(),
        "a transaction for another chain is refused"
    );
    assert_eq!(kms.signed().len(), 2, "and never reaches KMS");
}
