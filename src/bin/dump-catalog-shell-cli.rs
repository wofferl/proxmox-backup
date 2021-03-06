use anyhow::{Error};

use proxmox::api::format::*;
use proxmox::api::cli::*;

use proxmox_backup::backup::catalog_shell_cli;

fn main() -> Result<(), Error> {

    match catalog_shell_cli() {
        CommandLineInterface::Nested(map) => {
            let usage = generate_nested_usage("", &map, DocumentationFormat::ReST);
            println!("{}", usage);

        }
        _ => unreachable!(),
    }

    Ok(())
}
