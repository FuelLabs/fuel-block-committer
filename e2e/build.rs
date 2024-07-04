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

    let compile_json_artifact = download_and_compile(BRIDGE_REVISION).await.unwrap();
    if let Some(parent) = current_revision.parent() {
        tokio::fs::create_dir_all(parent).await.unwrap();
    }

    let save_location =
        PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("FuelChainState.json");

    tokio::fs::write(save_location, compile_json_artifact)
        .await
        .unwrap();
    tokio::fs::write(current_revision, BRIDGE_REVISION)
        .await
        .unwrap();
}

async fn download_and_compile(revision: &str) -> anyhow::Result<String> {
    let tempdir = tempfile::tempdir()?;

    let dir = tempdir.path();

    foundry::init(dir).await?;

    bridge::download_contract(revision, &dir.join("src")).await?;

    foundry::install_deps(dir).await?;

    foundry::compile(dir).await
}

mod bridge {
    use std::io::Cursor;

    use anyhow::bail;
    use zip::read::ZipFile;
    use zip::ZipArchive;

    use std::path::PathBuf;

    use std::path::Path;

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
}

mod foundry {
    use tokio::{fs::OpenOptions, io::AsyncWriteExt};

    use std::path::Path;

    use anyhow::{bail, Context};

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
        let output = tokio::process::Command::new("forge")
            .arg("install")
            .arg("--no-commit")
            .arg("--no-git")
            .arg("OpenZeppelin/openzeppelin-contracts-upgradeable@v4.8.3")
            .current_dir(dir)
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true)
            .output()
            .await?;

        if !output.status.success() {
            let err = String::from_utf8_lossy(&output.stderr);
            bail!("Failed to install dependencies: {err}")
        }

        let mut file = OpenOptions::new()
            .append(true) // Set the option to append
            .open(dir.join("foundry.toml"))
            .await?;

        let remapping = r#"remappings = ["@openzeppelin/contracts-upgradeable/=lib/openzeppelin-contracts-upgradeable/contracts/"]"#;
        file.write_all(remapping.as_bytes()).await?;

        Ok(())
    }

    pub async fn compile(dir: &Path) -> anyhow::Result<String> {
        let output = tokio::process::Command::new("forge")
            .arg("build")
            .stdin(std::process::Stdio::null())
            .current_dir(dir)
            .kill_on_drop(true)
            .output()
            .await?;

        if !output.status.success() {
            let err = String::from_utf8_lossy(&output.stderr);
            bail!("Failed to build the project, stderr: {err}");
        }

        Ok(
            tokio::fs::read_to_string(dir.join("out/FuelChainState.sol/FuelChainState.json"))
                .await?,
        )
    }
}
