use std::path::PathBuf;

const BRIDGE_REVISION: &str = "26cfeac";

#[tokio::main]
async fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    let current_revision =
        PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("contracts_revision.txt");

    if current_revision.exists() {
        let current_revision = tokio::fs::read_to_string(&current_revision).await.unwrap();
        if current_revision == BRIDGE_REVISION {
            return;
        }
    }

    let project_path = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("foundry");
    if project_path.exists() {
        tokio::fs::remove_dir_all(&project_path).await.unwrap();
    }

    init_and_build(BRIDGE_REVISION, &project_path)
        .await
        .unwrap();

    tokio::fs::write(current_revision, BRIDGE_REVISION)
        .await
        .unwrap();
}

use std::path::Path;

async fn init_and_build(revision: &str, path: &Path) -> anyhow::Result<()> {
    foundry::init(path).await?;

    let source_files = path.join("src");
    bridge::download_contract(revision, &source_files).await?;
    bridge::make_solidity_version_more_flexible(&source_files).await?;

    foundry::install_deps(path).await?;
    foundry::build(path).await?;

    foundry::add_deploy_script(path).await?;
    Ok(())
}

mod bridge {
    use anyhow::bail;
    use std::io::Cursor;
    use std::path::Path;
    use std::path::PathBuf;
    use walkdir::WalkDir;
    use zip::{read::ZipFile, ZipArchive};

    pub async fn download_contract(revision: &str, dir: &Path) -> anyhow::Result<()> {
        tokio::fs::create_dir_all(dir).await?;

        let mut zip = download_fuel_bridge_zip(revision).await?;

        extract_contract(&mut zip, dir).await?;
        extract_lib_contents(zip, dir).await?;

        Ok(())
    }

    async fn download_fuel_bridge_zip(revision: &str) -> Result<Zip, anyhow::Error> {
        let bytes = reqwest::get(
            &(format!("https://github.com/FuelLabs/fuel-bridge/archive/{revision}.zip")),
        )
        .await?
        .bytes()
        .await?
        .to_vec();
        Zip::try_new(bytes)
    }

    fn remove_first_component(path: &Path) -> PathBuf {
        // Split the path into components
        let mut components = path.components();
        // Skip the first component
        components.next();
        // Collect the remaining components into a new PathBuf
        components.collect()
    }

    struct Zip {
        zip: ZipArchive<Cursor<Vec<u8>>>,
    }

    impl Zip {
        fn try_new(bytes: Vec<u8>) -> anyhow::Result<Self> {
            let cursor = Cursor::new(bytes);
            let zip = ZipArchive::new(cursor)?;
            Ok(Self { zip })
        }

        async fn extract_files_with_prefix(
            &mut self,
            prefix: &str,
            dir: &Path,
            remove_prefix: &str,
        ) -> anyhow::Result<Vec<PathBuf>> {
            let mut extracted_files = vec![];

            for index in self.entries_with_prefix(prefix) {
                let entry = self.zip.by_index(index)?;

                if entry.is_file() {
                    let extracted_file = extract_file(entry, dir, remove_prefix).await?;
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

    async fn extract_file<'a>(
        mut file: ZipFile<'a>,
        dir: &Path,
        remove_prefix: &str,
    ) -> anyhow::Result<PathBuf> {
        let zip_file_name = file
            .enclosed_name()
            .ok_or_else(|| anyhow::anyhow!("Could not get the name of a file in the ZIP"))?;

        let file_path = remove_first_component(&zip_file_name);

        let target_path = dir.join(file_path.strip_prefix(remove_prefix)?);

        if let Some(parent) = target_path.parent() {
            if !parent.exists() {
                tokio::fs::create_dir_all(parent).await?;
            }
        }

        tokio::task::block_in_place(|| -> anyhow::Result<()> {
            let mut outfile = std::fs::File::create(&target_path)?;
            std::io::copy(&mut file, &mut outfile)?;
            Ok(())
        })?;

        Ok(target_path)
    }

    async fn extract_lib_contents(mut zip: Zip, dir: &Path) -> anyhow::Result<()> {
        let lib_path_in_zip = "packages/solidity-contracts/contracts/lib";
        let remove_prefix = "packages/solidity-contracts/contracts/";
        zip.extract_files_with_prefix(lib_path_in_zip, dir, remove_prefix)
            .await?;
        Ok(())
    }

    async fn extract_contract(zip: &mut Zip, dir: &Path) -> anyhow::Result<()> {
        let chain_state_in_zip =
            "packages/solidity-contracts/contracts/fuelchain/FuelChainState.sol";
        let remove_prefix = "packages/solidity-contracts/contracts/";

        let extracted_contracts = zip
            .extract_files_with_prefix(chain_state_in_zip, dir, remove_prefix)
            .await?;

        if extracted_contracts.is_empty() {
            bail!("Contract not found in the ZIP file");
        }

        if extracted_contracts.len() > 1 {
            bail!(
                "Multiple contracts found in the ZIP file: {:?}",
                extracted_contracts
            );
        }

        Ok(())
    }

    pub async fn make_solidity_version_more_flexible(dir: &Path) -> anyhow::Result<()> {
        let sol_files: Vec<PathBuf> = WalkDir::new(dir)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().and_then(|ext| ext.to_str()) == Some("sol"))
            .map(|e| e.into_path())
            .collect();

        for sol_file in sol_files {
            let contents = tokio::fs::read_to_string(&sol_file).await?;
            let updated_contents = update_solidity_pragma(&contents)?;
            tokio::fs::write(&sol_file, updated_contents).await?;
        }

        Ok(())
    }

    fn update_solidity_pragma(contents: &str) -> anyhow::Result<String> {
        // Define the new pragma statement
        let new_pragma = "pragma solidity >=0.8.9;";

        // Replace the old pragma statement with the new one
        let updated_contents = contents
            .lines()
            .map(|line| {
                if line.starts_with("pragma solidity") {
                    new_pragma.to_string()
                } else {
                    line.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("\n");

        Ok(updated_contents)
    }
}

mod foundry {
    use itertools::Itertools;
    use tokio::{fs::OpenOptions, io::AsyncWriteExt};

    use std::path::Path;

    use anyhow::{bail, Context};

    struct Dep {
        git: String,
        tag: String,
        remap: Option<(String, String)>,
    }

    pub async fn init(dir: &Path) -> anyhow::Result<()> {
        tokio::fs::create_dir_all(dir)
            .await
            .with_context(|| format!("could not create the project directory: {dir:?}"))?;

        let output = tokio::process::Command::new("forge")
            .arg("init")
            .arg("--no-git")
            .arg("--no-commit")
            .stdin(std::process::Stdio::null())
            .current_dir(dir)
            .kill_on_drop(true)
            .output()
            .await
            .map_err(|e| anyhow::anyhow!("could not start `forge`, err: {e}"))?;

        if !output.status.success() {
            let err = String::from_utf8_lossy(&output.stderr);
            bail!("failed to initialize the project. stderr: {err}");
        }

        for folder in ["src", "script", "test"] {
            let to_remove = dir.join(folder);
            tokio::fs::remove_dir_all(&to_remove)
                .await
                .with_context(|| format!("could not remove the folder: {to_remove:?}"))?;
        }

        Ok(())
    }

    pub async fn install_deps(dir: &Path) -> anyhow::Result<()> {
        let deps = [
            Dep {
                git: "OpenZeppelin/openzeppelin-foundry-upgrades".to_string(),
                tag: "v0.3.1".to_string(),
                remap: None,
            },
            Dep {
                git: "OpenZeppelin/openzeppelin-contracts".to_string(),
                tag: "v5.0.2".to_string(),
                remap: Some((
                    "openzeppelin/contracts".to_string(),
                    "openzeppelin-contracts/contracts".to_string(),
                )),
            },
            Dep {
                git: "OpenZeppelin/openzeppelin-contracts-upgradeable".to_string(),
                tag: "v4.8.3".to_string(),
                remap: Some((
                    "openzeppelin/contracts-upgradeable".to_string(),
                    "openzeppelin-contracts-upgradeable/contracts".to_string(),
                )),
            },
        ];

        for Dep { git, tag, .. } in &deps {
            let output = tokio::process::Command::new("forge")
                .arg("install")
                .arg("--no-commit")
                .arg("--no-git")
                .arg(format!("{git}@{tag}"))
                .current_dir(dir)
                .stdin(std::process::Stdio::null())
                .kill_on_drop(true)
                .output()
                .await?;

            if !output.status.success() {
                let err = String::from_utf8_lossy(&output.stderr);
                bail!("Failed to install dependencies: {err}")
            }
        }

        let mut file = OpenOptions::new()
            .append(true) // Set the option to append
            .open(dir.join("foundry.toml"))
            .await?;

        let remappings = deps
            .iter()
            .filter_map(|dep| dep.remap.as_ref())
            .map(|(from, to)| format!("\"@{from}/=lib/{to}\""))
            .join(",");

        file.write_all((format!("remappings = [{remappings}]")).as_bytes())
            .await?;

        Ok(())
    }

    pub async fn add_deploy_script(path: &Path) -> anyhow::Result<()> {
        let script_path = path.join("script/deploy.sol");
        if let Some(parent) = script_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        let script = r#"
pragma solidity ^0.8.9;

import {UnsafeUpgrades} from "openzeppelin-foundry-upgrades/Upgrades.sol";
import "forge-std/Script.sol";
import {FuelChainState} from "../src/fuelchain/FuelChainState.sol";

contract MyScript is Script {
    function run() external {
        uint256 deployerPrivateKey = vm.envUint("PRIVATE_KEY");
        vm.startBroadcast(deployerPrivateKey);

        uint256 timeToFinalize = vm.envUint("TIME_TO_FINALIZE");
        uint256 blocksPerCommitInterval = vm.envUint("BLOCKS_PER_COMMIT_INTERVAL");
        uint32 commitCooldown = uint32(vm.envUint("COMMIT_COOLDOWN"));
        FuelChainState implementation = new FuelChainState(timeToFinalize,blocksPerCommitInterval, commitCooldown);

        address proxy = UnsafeUpgrades.deployUUPSProxy(
            address(implementation),
            abi.encodeCall(FuelChainState.initialize, ())
        );

        console.log("PROXY:", proxy);
        console.log("IMPL:", address(implementation));

        vm.stopBroadcast();
    }
}
        "#;

        tokio::fs::write(script_path, script.trim_start()).await?;

        Ok(())
    }

    pub async fn build(path: &Path) -> anyhow::Result<()> {
        let output = tokio::process::Command::new("forge")
            .arg("build")
            .stdin(std::process::Stdio::null())
            .current_dir(path)
            .kill_on_drop(true)
            .output()
            .await
            .map_err(|e| anyhow::anyhow!("could not start `forge`, err: {e}"))?;

        if !output.status.success() {
            let err = String::from_utf8_lossy(&output.stderr);
            bail!("failed to initialize the project. stderr: {err}");
        }

        Ok(())
    }
}
