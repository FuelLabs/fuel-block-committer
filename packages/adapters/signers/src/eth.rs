use alloy::{
    consensus::SignableTransaction,
    network::TxSigner,
    primitives::{B256, ChainId},
    signers::Signature,
};
use eth::Address;

pub mod kms {
    pub use alloy::signers::aws::AwsSigner as Signer;
    use alloy::{
        consensus::SignableTransaction,
        network::TxSigner,
        primitives::{B256, ChainId},
        signers::Signature,
    };
    use eth::Address;

    #[cfg(feature = "test-helpers")]
    #[derive(Clone)]
    pub struct TestEthKmsSigner {
        pub key_id: String,
        pub url: String,
        pub signer: Signer,
    }

    #[async_trait::async_trait]
    impl TxSigner<Signature> for TestEthKmsSigner {
        fn address(&self) -> Address {
            self.signer.address()
        }

        async fn sign_transaction(
            &self,
            tx: &mut dyn SignableTransaction<Signature>,
        ) -> alloy::signers::Result<Signature> {
            self.signer.sign_transaction(tx).await
        }
    }

    #[async_trait::async_trait]
    impl alloy::signers::Signer<Signature> for TestEthKmsSigner {
        async fn sign_hash(&self, hash: &B256) -> alloy::signers::Result<Signature> {
            self.signer.sign_hash(hash).await
        }

        fn address(&self) -> Address {
            alloy::signers::Signer::<Signature>::address(&self.signer)
        }

        fn chain_id(&self) -> Option<ChainId> {
            self.signer.chain_id()
        }

        fn set_chain_id(&mut self, chain_id: Option<ChainId>) {
            self.signer.set_chain_id(chain_id)
        }
    }
}
pub mod private_key {
    pub use alloy::signers::local::PrivateKeySigner as Signer;
}

#[derive(Clone)]
pub enum Signer {
    Private(private_key::Signer),
    Kms(kms::Signer),
}

#[async_trait::async_trait]
impl TxSigner<Signature> for Signer {
    fn address(&self) -> Address {
        match self {
            Signer::Private(local_signer) => local_signer.address(),
            Signer::Kms(aws_signer) => aws_signer.address(),
        }
    }

    async fn sign_transaction(
        &self,
        tx: &mut dyn SignableTransaction<Signature>,
    ) -> alloy::signers::Result<Signature> {
        match self {
            Signer::Private(local_signer) => local_signer.sign_transaction(tx).await,
            Signer::Kms(aws_signer) => aws_signer.sign_transaction(tx).await,
        }
    }
}

#[async_trait::async_trait]
impl alloy::signers::Signer<Signature> for Signer {
    async fn sign_hash(&self, hash: &B256) -> alloy::signers::Result<Signature> {
        match self {
            Signer::Private(local_signer) => local_signer.sign_hash(hash).await,
            Signer::Kms(aws_signer) => aws_signer.sign_hash(hash).await,
        }
    }

    fn address(&self) -> Address {
        match self {
            Signer::Private(local_signer) => local_signer.address(),
            Signer::Kms(aws_signer) => alloy::signers::Signer::<Signature>::address(aws_signer),
        }
    }

    fn chain_id(&self) -> Option<ChainId> {
        match self {
            Signer::Private(local_signer) => local_signer.chain_id(),
            Signer::Kms(aws_signer) => aws_signer.chain_id(),
        }
    }

    fn set_chain_id(&mut self, chain_id: Option<ChainId>) {
        match self {
            Signer::Private(local_signer) => local_signer.set_chain_id(chain_id),
            Signer::Kms(aws_signer) => aws_signer.set_chain_id(chain_id),
        }
    }
}
