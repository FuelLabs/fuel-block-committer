use async_trait::async_trait;
use rust_eigenda_signers::{PublicKey, RecoverableSignature, secp256k1::Message};

pub mod private_key {
    pub use rust_eigenda_signers::signers::private_key::Signer;
}
pub mod kms;

#[derive(Debug, Clone)]
pub enum Signer {
    Private(private_key::Signer),
    Kms(kms::Signer),
}

#[async_trait]
impl eigenda::Sign for Signer {
    type Error = kms::Error;

    /// Signs a digest using the signer's key.
    async fn sign_digest(&self, message: &Message) -> Result<RecoverableSignature, Self::Error> {
        match self {
            Signer::Private(signer) => {
                // private_key.sign_digest cannot fail
                let Ok(sig) = signer.sign_digest(message).await;
                Ok(sig)
            }
            Signer::Kms(signer) => signer.sign_digest(message).await,
        }
    }

    /// Returns the public key associated with this signer.
    fn public_key(&self) -> PublicKey {
        match self {
            Signer::Private(signer) => signer.public_key(),
            Signer::Kms(signer) => signer.public_key(),
        }
    }
}
