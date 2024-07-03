use std::{
    fs::File,
    io::{self, Cursor},
    path::{Path, PathBuf},
};

use anyhow::bail;
use zip::{read::ZipFile, ZipArchive};

fn remove_first_component(path: &Path) -> PathBuf {
    // Split the path into components
    let mut components = path.components();
    // Skip the first component
    components.next();
    // Collect the remaining components into a new PathBuf
    components.collect()
}

pub struct Contracts {
    files: Vec<PathBuf>,
}

struct Zip {
    zip: ZipArchive<Cursor<Vec<u8>>>,
}

impl Zip {
    pub fn try_new(bytes: Vec<u8>) -> anyhow::Result<Self> {
        let cursor = Cursor::new(bytes);
        let zip = ZipArchive::new(cursor)?;
        Ok(Self { zip })
    }

    pub fn extract_files_with_prefix(
        &mut self,
        prefix: &str,
        dir: &Path,
        remove_prefix: &str,
    ) -> anyhow::Result<Vec<PathBuf>> {
        let mut extracted_files = vec![];

        for index in self.entries_with_prefix(prefix) {
            let entry = self.zip.by_index(index)?;

            if entry.is_file() {
                let extracted_file = extract_file(entry, dir, remove_prefix)?;
                extracted_files.push(extracted_file);
            }
        }

        Ok(extracted_files)
    }

    fn entries_with_prefix(&self, prefix: &str) -> Vec<usize> {
        self.zip
            .file_names()
            .enumerate()
            .filter_map(move |(index, file_name)| {
                remove_first_component(Path::new(file_name))
                    .starts_with(prefix)
                    .then_some(index)
            })
            .collect()
    }
}

fn extract_file(mut file: ZipFile, dir: &Path, remove_prefix: &str) -> anyhow::Result<PathBuf> {
    let original_path = remove_first_component(&file.enclosed_name().unwrap());

    let target_path = dir.join(original_path.strip_prefix(remove_prefix)?);

    if let Some(p) = target_path.parent() {
        if !p.exists() {
            std::fs::create_dir_all(p)?;
        }
    }

    let mut outfile = File::create(&target_path)?;
    io::copy(&mut file, &mut outfile)?;

    Ok(target_path)
}

impl Contracts {
    pub async fn download(revision: &str, dir: &Path) -> anyhow::Result<Self> {
        std::fs::create_dir_all(dir)?;

        let mut zip = download_fuel_bridge_zip(revision).await?;

        let chain_state_contract = extract_contracts(&mut zip, dir)?;
        let lib_files = extract_lib_contents(zip, dir)?;

        Ok(Self {
            files: [chain_state_contract, lib_files].concat(),
        })
    }
}

fn extract_lib_contents(mut zip: Zip, dir: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let lib_path_in_zip = "packages/solidity-contracts/contracts/lib";
    let remove_prefix = "packages/solidity-contracts/contracts/";
    zip.extract_files_with_prefix(lib_path_in_zip, dir, remove_prefix)
}

fn extract_contracts(zip: &mut Zip, dir: &Path) -> Result<Vec<PathBuf>, anyhow::Error> {
    let chain_state_in_zip = "packages/solidity-contracts/contracts/fuelchain/FuelChainState.sol";
    let remove_prefix = "packages/solidity-contracts/contracts/";

    let extracted_contracts =
        zip.extract_files_with_prefix(chain_state_in_zip, dir, remove_prefix)?;

    if extracted_contracts.is_empty() {
        bail!("Contract not found in the ZIP file");
    }

    if extracted_contracts.len() > 1 {
        bail!(
            "Multiple contracts found in the ZIP file: {:?}",
            extracted_contracts
        );
    }

    Ok(extracted_contracts)
}

async fn download_fuel_bridge_zip(revision: &str) -> Result<Zip, anyhow::Error> {
    let bytes =
        reqwest::get(&(format!("https://github.com/FuelLabs/fuel-bridge/archive/{revision}.zip")))
            .await?
            .bytes()
            .await?
            .to_vec();
    Zip::try_new(bytes)
}

#[cfg(test)]
mod tests {
    use std::fs::OpenOptions;

    use io::Write;
    use serde_json::Value;

    use super::*;

    #[tokio::test]
    async fn smt() {
        let project_dir = Path::new("./project");
        let _ = std::fs::remove_dir_all(project_dir);

        init_foundry_project(project_dir).unwrap();

        Contracts::download("26cfeac", &project_dir.join("src"))
            .await
            .unwrap();

        install_deps(project_dir).unwrap();

        dbg!(build(project_dir).unwrap());
    }

    fn init_foundry_project(dir: &Path) -> anyhow::Result<()> {
        std::fs::create_dir_all(dir).unwrap();

        let status = std::process::Command::new("forge")
            .arg("init")
            .arg("--no-git")
            .arg("--no-commit")
            .current_dir(dir)
            .status()
            .unwrap();

        if !status.success() {
            bail!("Failed to initialize the project");
        }

        for folder in ["src", "script", "test"] {
            std::fs::remove_dir_all(dir.join(folder))?;
        }

        Ok(())
    }

    fn install_deps(dir: &Path) -> anyhow::Result<()> {
        let status = std::process::Command::new("forge")
            .arg("install")
            .arg("--no-commit")
            .arg("--no-git")
            .arg("OpenZeppelin/openzeppelin-contracts-upgradeable@v4.8.3")
            .current_dir(dir)
            .status()?;

        if !status.success() {
            bail!("Failed to install dependencies");
        }

        let mut file = OpenOptions::new()
            .append(true) // Set the option to append
            .open(dir.join("foundry.toml"))?;

        let remapping = r#"remappings = ["@openzeppelin/contracts-upgradeable/=lib/openzeppelin-contracts-upgradeable/contracts/"]"#;
        write!(file, "{remapping}")?;

        Ok(())
    }

    #[derive(Debug, Clone)]
    struct CompilationArtifacts {
        bytecode: ethers::core::types::Bytes,
        abi: ethers::core::abi::Contract,
    }

    impl<'de> serde::Deserialize<'de> for CompilationArtifacts {
        fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
        where
            D: serde::Deserializer<'de>,
        {
            let value = serde_json::Value::deserialize(deserializer)?;
            let bytecode = value["bytecode"]["object"]
                .as_str()
                .ok_or_else(|| serde::de::Error::custom("no bytecode"))?;

            let abi = ethers::abi::Contract::load(std::io::Cursor::new(
                serde_json::to_string(&value["abi"]).unwrap(),
            ))
            .unwrap();

            Ok(CompilationArtifacts {
                bytecode: ethers::core::types::Bytes::from(
                    hex::decode(bytecode.strip_prefix("0x").unwrap()).unwrap(),
                ),
                abi,
            })
        }
    }

    fn build(dir: &Path) -> anyhow::Result<CompilationArtifacts> {
        let status = std::process::Command::new("forge")
            .arg("build")
            .current_dir(dir)
            .status()?;

        if !status.success() {
            bail!("Failed to build the project");
        }

        let generated_file = dir.join("out/FuelChainState.sol/FuelChainState.json");
        let file = std::fs::File::open(generated_file)?;

        Ok(serde_json::from_reader(file)?)
    }
}
