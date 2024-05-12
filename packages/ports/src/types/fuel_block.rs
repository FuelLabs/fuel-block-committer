use crate::ports::fuel::{
    Error, FuelBlockId, FuelBytes32, FuelConsensus, FuelHeader, FuelPoAConsensus, FuelPublicKey,
    Result,
};
use fuel_core_client::client::types::Block as FuelBlock;
use fuel_crypto::{Hasher, Message};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
pub struct ValidatedFuelBlock {
    hash: [u8; 32],
    height: u32,
}

impl ValidatedFuelBlock {
    /// Create `ValidatedFuelBlock` from `FuelBlock` validating
    /// public key, block id and signature.
    pub fn from(fuel_block: &FuelBlock, expected_pub_key: &FuelPublicKey) -> Result<Self> {
        Self::validate_public_key(fuel_block, expected_pub_key)?;
        Self::validate_block_id(fuel_block)?;
        Self::validate_block_signature(fuel_block, expected_pub_key)?;

        Ok(Self {
            hash: *fuel_block.id,
            height: fuel_block.header.height,
        })
    }

    fn validate_public_key(fuel_block: &FuelBlock, expected_pub_key: &FuelPublicKey) -> Result<()> {
        let Some(producer_pub_key) = fuel_block.block_producer() else {
            return Err(Error::BlockValidation(
                "producer public key not found in fuel block".to_string(),
            ));
        };

        if producer_pub_key != expected_pub_key {
            return Err(Error::BlockValidation(format!(
                "producer public key `{producer_pub_key:x}` does not match \
                 expected public key `{expected_pub_key:x}`."
            )));
        }

        Ok(())
    }

    fn validate_block_id(fuel_block: &FuelBlock) -> Result<()> {
        let calculated_block_id = Self::calculate_block_id(fuel_block);
        if fuel_block.id != calculated_block_id {
            return Err(Error::BlockValidation(format!(
                "fuel block id `{:x}` does not match \
                 calculated block id `{calculated_block_id:x}`.",
                fuel_block.id,
            )));
        }

        Ok(())
    }

    fn validate_block_signature(
        fuel_block: &FuelBlock,
        expected_pub_key: &FuelPublicKey,
    ) -> Result<()> {
        let FuelConsensus::PoAConsensus(FuelPoAConsensus { signature }) = fuel_block.consensus
        else {
            return Err(Error::BlockValidation(
                "PoAConsensus signature not found in fuel block".to_string(),
            ));
        };

        let block_id_message = Message::from_bytes(*fuel_block.id);

        signature
            .verify(expected_pub_key, &block_id_message)
            .map_err(|_| {
                Error::BlockValidation(format!(
                    "signature validation failed for fuel block with id: `{:x}`",
                    fuel_block.id
                ))
            })?;

        Ok(())
    }

    fn calculate_block_id(fuel_block: &FuelBlock) -> FuelBlockId {
        let application_hash = Self::application_hash(&fuel_block.header);

        let mut hasher = Hasher::default();
        let FuelHeader {
            prev_root,
            height,
            time,
            ..
        } = &fuel_block.header;

        hasher.input(prev_root.as_ref());
        hasher.input(height.to_be_bytes());
        hasher.input(time.0.to_be_bytes());
        hasher.input(application_hash.as_ref());

        FuelBlockId::from(hasher.digest())
    }

    fn application_hash(header: &FuelHeader) -> FuelBytes32 {
        // Order matters and is the same as the spec.
        let mut hasher = Hasher::default();
        let FuelHeader {
            da_height,
            consensus_parameters_version,
            state_transition_bytecode_version,
            transactions_count,
            message_receipt_count,
            transactions_root,
            message_outbox_root,
            event_inbox_root,
            ..
        } = header;

        hasher.input(da_height.to_be_bytes());
        hasher.input(consensus_parameters_version.to_be_bytes());
        hasher.input(state_transition_bytecode_version.to_be_bytes());
        hasher.input(transactions_count.to_be_bytes());
        hasher.input(message_receipt_count.to_be_bytes());
        hasher.input(transactions_root.as_ref());
        hasher.input(message_outbox_root.as_ref());
        hasher.input(event_inbox_root.as_ref());

        hasher.digest()
    }

    /// # Safety
    ///
    /// This function should only be called when the caller can guarantee that
    /// `hash` and `height` meet all the requirements normally enforced by `validate`.
    pub unsafe fn new_unchecked(hash: [u8; 32], height: u32) -> Self {
        Self { hash, height }
    }

    pub fn hash(&self) -> [u8; 32] {
        self.hash
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    #[cfg(feature = "test-helpers")]
    pub fn new(hash: [u8; 32], height: u32) -> Self {
        Self { hash, height }
    }

    #[cfg(feature = "test-helpers")]
    pub fn set_height(&mut self, height: u32) {
        self.height = height;
    }
}

#[cfg(feature = "test-helpers")]
impl From<FuelBlock> for ValidatedFuelBlock {
    fn from(block: FuelBlock) -> Self {
        Self {
            hash: *block.id,
            height: block.header.height,
        }
    }
}

#[cfg(feature = "test-helpers")]
impl rand::distributions::Distribution<ValidatedFuelBlock> for rand::distributions::Standard {
    fn sample<R: rand::prelude::Rng + ?Sized>(&self, rng: &mut R) -> ValidatedFuelBlock {
        ValidatedFuelBlock {
            hash: rng.gen(),
            height: rng.gen(),
        }
    }
}

impl std::fmt::Debug for ValidatedFuelBlock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let hash = self.hash.map(|byte| format!("{byte:02x?}")).join("");
        f.debug_struct("FuelBlock")
            .field("hash", &hash)
            .field("height", &self.height)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fuel_crypto::{SecretKey, Signature};
    use rand::{rngs::StdRng, SeedableRng};

    #[test]
    #[should_panic(expected = "producer public key not found in fuel block")]
    fn validate_public_key_missing() {
        let fuel_block = given_a_block(None);

        ValidatedFuelBlock::from(&fuel_block, &FuelPublicKey::default()).unwrap();
    }

    #[test]
    #[should_panic(expected = "does not match expected public key")]
    fn validate_public_key_mistmach() {
        let secret_key = given_secret_key();
        let fuel_block = given_a_block(Some(secret_key));

        ValidatedFuelBlock::from(&fuel_block, &FuelPublicKey::default()).unwrap();
    }

    #[test]
    #[should_panic(expected = "does not match calculated block id")]
    fn validate_block_id_mistmach() {
        let secret_key = given_secret_key();
        let mut fuel_block = given_a_block(Some(secret_key));
        fuel_block.header.height = 42; // Change a value to get a different block id

        ValidatedFuelBlock::from(&fuel_block, &secret_key.public_key()).unwrap();
    }

    #[test]
    #[should_panic(expected = "PoAConsensus signature not found in fuel block")]
    fn validate_block_consensus_not_poa() {
        let secret_key = given_secret_key();
        let mut fuel_block = given_a_block(Some(secret_key));
        fuel_block.consensus = FuelConsensus::Unknown;

        ValidatedFuelBlock::from(&fuel_block, &secret_key.public_key()).unwrap();
    }

    #[test]
    #[should_panic(expected = "signature validation failed for fuel block with id:")]
    fn validate_block_consensus_invalid_signature() {
        let secret_key = given_secret_key();
        let mut fuel_block = given_a_block(Some(secret_key));
        fuel_block.consensus = FuelConsensus::PoAConsensus(FuelPoAConsensus {
            signature: Signature::default(),
        });

        ValidatedFuelBlock::from(&fuel_block, &secret_key.public_key()).unwrap();
    }

    #[test]
    fn validate_fuel_block() {
        let secret_key = given_secret_key();
        let fuel_block = given_a_block(Some(secret_key));

        ValidatedFuelBlock::from(&fuel_block, &secret_key.public_key()).unwrap();
    }

    fn given_secret_key() -> SecretKey {
        let mut rng = StdRng::seed_from_u64(42);

        SecretKey::random(&mut rng)
    }

    fn given_a_block(secret_key: Option<SecretKey>) -> FuelBlock {
        let header = given_header();
        let id: FuelBytes32 = "0x57131ec6e99caafc08803aa946093e02c4303a305e5cc959ad84b775e668a5c3"
            .parse()
            .unwrap();

        if let Some(secret_key) = secret_key {
            let id_message = Message::from_bytes(*id);
            let signature = Signature::sign(&secret_key, &id_message);

            FuelBlock {
                id,
                header,
                consensus: FuelConsensus::PoAConsensus(FuelPoAConsensus { signature }),
                transactions: vec![],
                block_producer: Some(secret_key.public_key()),
            }
        } else {
            FuelBlock {
                id,
                header,
                consensus: FuelConsensus::Unknown,
                transactions: vec![],
                block_producer: None,
            }
        }
    }

    fn given_header() -> FuelHeader {
        let application_hash = "0x017ab4b70ea129c29e932d44baddc185ad136bf719c4ada63a10b5bf796af91e"
            .parse()
            .unwrap();

        FuelHeader {
            id: Default::default(),
            da_height: Default::default(),
            consensus_parameters_version: Default::default(),
            state_transition_bytecode_version: Default::default(),
            transactions_count: Default::default(),
            message_receipt_count: Default::default(),
            transactions_root: Default::default(),
            message_outbox_root: Default::default(),
            event_inbox_root: Default::default(),
            height: Default::default(),
            prev_root: Default::default(),
            time: tai64::Tai64(0),
            application_hash,
        }
    }
}
