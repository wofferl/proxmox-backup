//! Tape drive/changer configuration
//!
//! This configuration module is based on [`SectionConfig`], and
//! provides a type safe interface to store [`LtoTapeDrive`],
//! [`VirtualTapeDrive`] and [`ScsiTapeChanger`] configurations.
//!
//! Drive type [`VirtualTapeDrive`] is only useful for debugging.
//!
//! [LtoTapeDrive]: crate::api2::types::LtoTapeDrive
//! [VirtualTapeDrive]: crate::api2::types::VirtualTapeDrive
//! [ScsiTapeChanger]: crate::api2::types::ScsiTapeChanger
//! [SectionConfig]: proxmox::api::section_config::SectionConfig

use std::collections::HashMap;

use anyhow::{bail, Error};
use lazy_static::lazy_static;

use proxmox::{
    api::{
        schema::*,
        section_config::{
            SectionConfig,
            SectionConfigData,
            SectionConfigPlugin,
        },
    },
    tools::fs::{
        open_file_locked,
        replace_file,
        CreateOptions,
    },
};

use crate::{
    api2::types::{
        DRIVE_NAME_SCHEMA,
        VirtualTapeDrive,
        LtoTapeDrive,
        ScsiTapeChanger,
    },
};

lazy_static! {
    /// Static [`SectionConfig`] to access parser/writer functions.
    pub static ref CONFIG: SectionConfig = init();
}


fn init() -> SectionConfig {
    let mut config = SectionConfig::new(&DRIVE_NAME_SCHEMA);

    let obj_schema = match VirtualTapeDrive::API_SCHEMA {
        Schema::Object(ref obj_schema) => obj_schema,
        _ => unreachable!(),
    };
    let plugin = SectionConfigPlugin::new("virtual".to_string(), Some("name".to_string()), obj_schema);
    config.register_plugin(plugin);

    let obj_schema = match LtoTapeDrive::API_SCHEMA {
        Schema::Object(ref obj_schema) => obj_schema,
        _ => unreachable!(),
    };
    let plugin = SectionConfigPlugin::new("lto".to_string(), Some("name".to_string()), obj_schema);
    config.register_plugin(plugin);

    let obj_schema = match ScsiTapeChanger::API_SCHEMA {
        Schema::Object(ref obj_schema) => obj_schema,
        _ => unreachable!(),
    };
    let plugin = SectionConfigPlugin::new("changer".to_string(), Some("name".to_string()), obj_schema);
    config.register_plugin(plugin);
    config
}

/// Configuration file name
pub const DRIVE_CFG_FILENAME: &str = "/etc/proxmox-backup/tape.cfg";
/// Lock file name (used to prevent concurrent access)
pub const DRIVE_CFG_LOCKFILE: &str = "/etc/proxmox-backup/.tape.lck";

/// Get exclusive lock
pub fn lock() -> Result<std::fs::File, Error> {
    open_file_locked(DRIVE_CFG_LOCKFILE, std::time::Duration::new(10, 0), true)
}

/// Read and parse the configuration file
pub fn config() -> Result<(SectionConfigData, [u8;32]), Error> {

    let content = proxmox::tools::fs::file_read_optional_string(DRIVE_CFG_FILENAME)?
        .unwrap_or_else(|| "".to_string());

    let digest = openssl::sha::sha256(content.as_bytes());
    let data = CONFIG.parse(DRIVE_CFG_FILENAME, &content)?;
    Ok((data, digest))
}

/// Save the configuration file
pub fn save_config(config: &SectionConfigData) -> Result<(), Error> {
    let raw = CONFIG.write(DRIVE_CFG_FILENAME, &config)?;

    let backup_user = crate::backup::backup_user()?;
    let mode = nix::sys::stat::Mode::from_bits_truncate(0o0640);
    // set the correct owner/group/permissions while saving file
    // owner(rw) = root, group(r)= backup
    let options = CreateOptions::new()
        .perm(mode)
        .owner(nix::unistd::ROOT)
        .group(backup_user.gid);

    replace_file(DRIVE_CFG_FILENAME, raw.as_bytes(), options)?;

    Ok(())
}

/// Check if the specified drive name exists in the config.
pub fn check_drive_exists(config: &SectionConfigData, drive: &str) -> Result<(), Error> {
    match config.sections.get(drive) {
        Some((section_type, _)) => {
            if !(section_type == "lto" || section_type == "virtual") {
                bail!("Entry '{}' exists, but is not a tape drive", drive);
            }
        }
        None => bail!("Drive '{}' does not exist", drive),
    }
    Ok(())
}


// shell completion helper

/// List all drive names
pub fn complete_drive_name(_arg: &str, _param: &HashMap<String, String>) -> Vec<String> {
    match config() {
        Ok((data, _digest)) => data.sections.iter()
            .map(|(id, _)| id.to_string())
            .collect(),
        Err(_) => return vec![],
    }
}

/// List Lto tape drives
pub fn complete_lto_drive_name(_arg: &str, _param: &HashMap<String, String>) -> Vec<String> {
    match config() {
        Ok((data, _digest)) => data.sections.iter()
            .filter(|(_id, (section_type, _))| {
                section_type == "lto"
            })
            .map(|(id, _)| id.to_string())
            .collect(),
        Err(_) => return vec![],
    }
}

/// List Scsi tape changer names
pub fn complete_changer_name(_arg: &str, _param: &HashMap<String, String>) -> Vec<String> {
    match config() {
        Ok((data, _digest)) => data.sections.iter()
            .filter(|(_id, (section_type, _))| {
                section_type == "changer"
            })
            .map(|(id, _)| id.to_string())
            .collect(),
        Err(_) => return vec![],
    }
}
