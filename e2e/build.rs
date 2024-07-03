use std::path::PathBuf;

const BRIDGE_REVISION: &str = "26cfeac";

#[tokio::main]
async fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    let current_revision =
        PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("contracts_revision.txt");

    if current_revision.exists() {
        let current_revision = std::fs::read_to_string(&current_revision).unwrap();
        if current_revision == BRIDGE_REVISION {
            return;
        }
    }

    let compile_json_artifact = download_and_compile(BRIDGE_REVISION).await.unwrap();
    if let Some(parent) = current_revision.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }

    let save_location =
        PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("FuelChainState.json");

    std::fs::write(save_location, compile_json_artifact).unwrap();
    std::fs::write(current_revision, BRIDGE_REVISION).unwrap();
}

async fn download_and_compile(revision: &str) -> anyhow::Result<String> {
    let tempdir = tempfile::tempdir()?;

    let dir = tempdir.path();

    foundry::init(dir)?;

    bridge::download_contract(revision, &dir.join("src")).await?;

    foundry::install_deps(dir)?;

    foundry::compile(dir)
}

mod bridge {
    use std::fs::File;
    use std::io::Cursor;

    use anyhow::bail;
    use zip::read::ZipFile;
    use zip::ZipArchive;

    use std::path::PathBuf;

    use std::path::Path;

    pub(crate) async fn download_contract(revision: &str, dir: &Path) -> anyhow::Result<()> {
        std::fs::create_dir_all(dir)?;

        let mut zip = download_fuel_bridge_zip(revision).await?;

        extract_contract(&mut zip, dir)?;
        extract_lib_contents(zip, dir)?;

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

        fn extract_files_with_prefix(
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
        let zip_file_name = file
            .enclosed_name()
            .ok_or_else(|| anyhow::anyhow!("Could not get the name of a file in the ZIP"))?;

        let file_path = remove_first_component(&zip_file_name);

        let target_path = dir.join(file_path.strip_prefix(remove_prefix)?);

        if let Some(parent) = target_path.parent() {
            if !parent.exists() {
                std::fs::create_dir_all(parent)?;
            }
        }

        let mut outfile = File::create(&target_path)?;
        std::io::copy(&mut file, &mut outfile)?;

        Ok(target_path)
    }

    fn extract_lib_contents(mut zip: Zip, dir: &Path) -> anyhow::Result<()> {
        let lib_path_in_zip = "packages/solidity-contracts/contracts/lib";
        let remove_prefix = "packages/solidity-contracts/contracts/";
        zip.extract_files_with_prefix(lib_path_in_zip, dir, remove_prefix)?;
        Ok(())
    }

    fn extract_contract(zip: &mut Zip, dir: &Path) -> anyhow::Result<()> {
        let chain_state_in_zip =
            "packages/solidity-contracts/contracts/fuelchain/FuelChainState.sol";
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

        Ok(())
    }
}

mod foundry {
    use std::fs::OpenOptions;
    use std::io::Write;

    use std::path::Path;

    use anyhow::{bail, Context};

    pub(crate) fn init(dir: &Path) -> anyhow::Result<()> {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("could not create the project directory: {:?}", dir))?;

        let output = std::process::Command::new("forge")
            .arg("init")
            .arg("--no-git")
            .arg("--no-commit")
            .stdin(std::process::Stdio::null())
            .current_dir(dir)
            .output()
            .map_err(|e| anyhow::anyhow!("could not start `forge`, err: {e}"))?;

        if !output.status.success() {
            let err = String::from_utf8_lossy(&output.stderr);
            bail!("failed to initialize the project. stderr: {err}");
        }

        for folder in ["src", "script", "test"] {
            let to_remove = dir.join(folder);
            std::fs::remove_dir_all(&to_remove)
                .with_context(|| format!("could not remove the folder: {to_remove:?}"))?;
        }

        Ok(())
    }

    pub(crate) fn install_deps(dir: &Path) -> anyhow::Result<()> {
        let output = std::process::Command::new("forge")
            .arg("install")
            .arg("--no-commit")
            .arg("--no-git")
            .arg("OpenZeppelin/openzeppelin-contracts-upgradeable@v4.8.3")
            .current_dir(dir)
            .stdin(std::process::Stdio::null())
            .output()?;

        if !output.status.success() {
            let err = String::from_utf8_lossy(&output.stderr);
            bail!("Failed to install dependencies: {err}")
        }

        let mut file = OpenOptions::new()
            .append(true) // Set the option to append
            .open(dir.join("foundry.toml"))?;

        let remapping = r#"remappings = ["@openzeppelin/contracts-upgradeable/=lib/openzeppelin-contracts-upgradeable/contracts/"]"#;
        write!(file, "{remapping}")?;

        Ok(())
    }

    pub(crate) fn compile(dir: &Path) -> anyhow::Result<String> {
        let output = std::process::Command::new("forge")
            .arg("build")
            .stdin(std::process::Stdio::null())
            .current_dir(dir)
            .output()?;

        if !output.status.success() {
            let err = String::from_utf8_lossy(&output.stderr);
            bail!("Failed to build the project, stderr: {err}");
        }

        Ok(std::fs::read_to_string(
            dir.join("out/FuelChainState.sol/FuelChainState.json"),
        )?)
    }
}
