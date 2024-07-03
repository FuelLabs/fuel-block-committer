const FUEL_CHAIN_STATE_JSON: &str = include_str!(concat!(env!("OUT_DIR"), "/FuelChainState.json"));

#[derive(Debug, Clone)]
pub struct CompilationArtifacts {
    pub bytecode: ethers_core::types::Bytes,
    pub abi: ethers_core::abi::Contract,
}

impl<'de> serde::Deserialize<'de> for CompilationArtifacts {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;

        let abi = {
            let json_reader = std::io::Cursor::new(
                serde_json::to_string(&value["abi"]).expect("foundry to generate valid JSON"),
            );
            // `ethers::abi::Contract` doesn't implement `serde::Deserialize` but it does have a
            // load method so we use that.
            ethers_core::abi::Contract::load(json_reader)
                .map_err(|e| serde::de::Error::custom(format!("could not load the ABI: {e}")))?
        };

        let bytecode = {
            let hex_bytecode = value["bytecode"]["object"]
                .as_str()
                .ok_or_else(|| serde::de::Error::custom("no bytecode"))?
                .strip_prefix("0x")
                .expect("foundry to generate bytecode with 0x prefix");

            hex::decode(hex_bytecode)
                .map_err(serde::de::Error::custom)?
                .into()
        };

        Ok(CompilationArtifacts { bytecode, abi })
    }
}

pub fn compilation_artifacts() -> CompilationArtifacts {
    serde_json::from_str(FUEL_CHAIN_STATE_JSON).expect("valid JSON")
}
