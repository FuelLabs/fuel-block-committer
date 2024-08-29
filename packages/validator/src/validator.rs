use fuel_core_client::client::types::{
    block::{
        Block as FuelBlock, Consensus as FuelConsensus, Header as FuelHeader,
        PoAConsensus as FuelPoAConsensus,
    },
    primitives::{BlockId as FuelBlockId, Bytes32 as FuelBytes32},
};
use fuel_crypto::{Hasher, Message};

use crate::{block::ValidatedFuelBlock, Error, Result, Validator};

#[derive(Debug)]
pub struct BlockValidator {
    producer_addr: [u8; 32],
}

impl Validator for BlockValidator {
    fn validate(&self, fuel_block: &FuelBlock) -> Result<ValidatedFuelBlock> {
        self._validate(fuel_block)
    }
}

impl BlockValidator {
    pub fn new(producer_addr: [u8; 32]) -> Self {
        Self { producer_addr }
    }

    fn _validate(&self, fuel_block: &FuelBlock) -> Result<ValidatedFuelBlock> {
        self.validate_producer_addr(fuel_block)?;
        Self::validate_block_id(fuel_block)?;
        self.validate_block_signature(fuel_block)?;

        Ok(ValidatedFuelBlock {
            hash: *fuel_block.id,
            height: fuel_block.header.height,
        })
    }

    fn validate_producer_addr(&self, fuel_block: &FuelBlock) -> Result<()> {
        let Some(producer_addr) = fuel_block.block_producer().map(|key| key.hash()) else {
            return Err(Error::BlockValidation(
                "producer public key not found in fuel block".to_string(),
            ));
        };

        if *producer_addr != self.producer_addr {
            return Err(Error::BlockValidation(format!(
                "producer addr '{}' does not match expected addr '{}'.",
                hex::encode(producer_addr),
                hex::encode(self.producer_addr)
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

    fn validate_block_signature(&self, fuel_block: &FuelBlock) -> Result<()> {
        let FuelConsensus::PoAConsensus(FuelPoAConsensus { signature }) = fuel_block.consensus
        else {
            return Err(Error::BlockValidation(
                "PoAConsensus signature not found in fuel block".to_string(),
            ));
        };

        let recovered_producer_addr = *signature
            .recover(&Message::from_bytes(*fuel_block.id))
            .map_err(|e| {
                Error::BlockValidation(format!(
                    "failed to recover public key from PoAConsensus signature: {e:?}",
                ))
            })?
            .hash();

        if recovered_producer_addr != self.producer_addr {
            return Err(Error::BlockValidation(format!(
                "recovered producer addr `{}` does not match \
             expected addr`{}`.",
                hex::encode(recovered_producer_addr),
                hex::encode(self.producer_addr)
            )));
        }

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
}

#[cfg(test)]
mod tests {
    use fuel_crypto::{SecretKey, Signature};
    use rand::{rngs::StdRng, SeedableRng};

    use super::*;

    #[test]
    #[should_panic(expected = "producer public key not found in fuel block")]
    fn validate_public_key_missing() {
        let fuel_block = given_a_block(None);
        let validator = BlockValidator::new([0; 32]);

        validator.validate(&fuel_block).unwrap();
    }

    #[test]
    #[should_panic(expected = "does not match expected addr")]
    fn validate_public_key_mistmach() {
        let secret_key = given_secret_key();
        let fuel_block = given_a_block(Some(secret_key));
        let validator = BlockValidator::new([0; 32]);

        validator.validate(&fuel_block).unwrap();
    }

    #[test]
    #[should_panic(expected = "does not match calculated block id")]
    fn validate_block_id_mismatch() {
        let secret_key = given_secret_key();
        let mut fuel_block = given_a_block(Some(secret_key));
        fuel_block.header.height = 42; // Change a value to get a different block id
        let validator = BlockValidator::new(*secret_key.public_key().hash());

        validator.validate(&fuel_block).unwrap();
    }

    #[test]
    #[should_panic(expected = "PoAConsensus signature not found in fuel block")]
    fn validate_block_consensus_not_poa() {
        let secret_key = given_secret_key();
        let mut fuel_block = given_a_block(Some(secret_key));
        fuel_block.consensus = FuelConsensus::Unknown;
        let validator = BlockValidator::new(*secret_key.public_key().hash());

        validator.validate(&fuel_block).unwrap();
    }

    #[test]
    #[should_panic(
        expected = "recovered producer addr `286b769a36b01cebc43cd9820ba709b438b14566e16a287c36881194eacc45c6` does not match expected addr`f95112e76de29dca6ed315c5a5be7855e62dee55478077cf209554d5bfb7cd85`."
    )]
    fn validate_block_consensus_invalid_signature() {
        let correct_secret_key = given_secret_key();

        let mut fuel_block = given_a_block(Some(correct_secret_key));
        let invalid_signature = {
            let different_secret_key = SecretKey::random(&mut StdRng::seed_from_u64(43));
            let id_message = Message::from_bytes(*fuel_block.id);
            Signature::sign(&different_secret_key, &id_message)
        };

        fuel_block.consensus = FuelConsensus::PoAConsensus(FuelPoAConsensus {
            signature: invalid_signature,
        });
        let validator = BlockValidator::new(*correct_secret_key.public_key().hash());

        validator.validate(&fuel_block).unwrap();
    }

    #[test]
    fn validate_fuel_block() {
        let secret_key = given_secret_key();
        let fuel_block = given_a_block(Some(secret_key));
        let validator = BlockValidator::new(*secret_key.public_key().hash());

        validator.validate(&fuel_block).unwrap();
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
