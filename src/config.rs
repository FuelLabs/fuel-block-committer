use std::path::PathBuf;

use clap::{command, Parser};

use crate::{errors::Result, setup::config::Config};

#[derive(Parser)]
#[command(
    name = "fuel-block-committer",
    version,
    about,
    propagate_version = true,
    arg_required_else_help(true)
)]
struct Cli {
    #[arg(value_name = "FILE", help = "Path to the configuration file")]
    config_path: PathBuf,
}

pub fn parse() -> Result<Config> {
    let cli = Cli::parse();

    let config = config::Config::builder()
        .add_source(config::File::from(cli.config_path))
        .add_source(config::Environment::default())
        .build()?
        .try_deserialize()?;

    Ok(config)
}
