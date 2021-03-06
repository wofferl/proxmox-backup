//! Media Pool configuration (Tape backup)
//!
//! This configuration module is based on [`SectionConfig`], and
//! provides a type safe interface to store [`MediaPoolConfig`],
//!
//! [MediaPoolConfig]: crate::api2::types::MediaPoolConfig
//! [SectionConfig]: proxmox::api::section_config::SectionConfig

use std::collections::HashMap;

use anyhow::Error;
use lazy_static::lazy_static;

use proxmox::{
    api::{
        schema::*,
        section_config::{
            SectionConfig,
            SectionConfigData,
            SectionConfigPlugin,
        }
    },
    tools::fs::{
        open_file_locked,
        replace_file,
        CreateOptions,
    },
};

use crate::{
    api2::types::{
        MEDIA_POOL_NAME_SCHEMA,
        MediaPoolConfig,
    },
};

lazy_static! {
    /// Static [`SectionConfig`] to access parser/writer functions.
    pub static ref CONFIG: SectionConfig = init();
}

fn init() -> SectionConfig {
    let mut config = SectionConfig::new(&MEDIA_POOL_NAME_SCHEMA);

    let obj_schema = match MediaPoolConfig::API_SCHEMA {
        Schema::Object(ref obj_schema) => obj_schema,
        _ => unreachable!(),
    };
    let plugin = SectionConfigPlugin::new("pool".to_string(), Some("name".to_string()), obj_schema);
    config.register_plugin(plugin);

    config
}

/// Configuration file name
pub const MEDIA_POOL_CFG_FILENAME: &str = "/etc/proxmox-backup/media-pool.cfg";
/// Lock file name (used to prevent concurrent access)
pub const MEDIA_POOL_CFG_LOCKFILE: &str = "/etc/proxmox-backup/.media-pool.lck";


/// Get exclusive lock
pub fn lock() -> Result<std::fs::File, Error> {
    open_file_locked(MEDIA_POOL_CFG_LOCKFILE, std::time::Duration::new(10, 0), true)
}

/// Read and parse the configuration file
pub fn config() -> Result<(SectionConfigData, [u8;32]), Error> {

    let content = proxmox::tools::fs::file_read_optional_string(MEDIA_POOL_CFG_FILENAME)?
        .unwrap_or_else(|| "".to_string());

    let digest = openssl::sha::sha256(content.as_bytes());
    let data = CONFIG.parse(MEDIA_POOL_CFG_FILENAME, &content)?;
    Ok((data, digest))
}

/// Save the configuration file
pub fn save_config(config: &SectionConfigData) -> Result<(), Error> {
    let raw = CONFIG.write(MEDIA_POOL_CFG_FILENAME, &config)?;

    let backup_user = crate::backup::backup_user()?;
    let mode = nix::sys::stat::Mode::from_bits_truncate(0o0640);
    // set the correct owner/group/permissions while saving file
    // owner(rw) = root, group(r)= backup
    let options = CreateOptions::new()
        .perm(mode)
        .owner(nix::unistd::ROOT)
        .group(backup_user.gid);

    replace_file(MEDIA_POOL_CFG_FILENAME, raw.as_bytes(), options)?;

    Ok(())
}

// shell completion helper

/// List existing pool names
pub fn complete_pool_name(_arg: &str, _param: &HashMap<String, String>) -> Vec<String> {
    match config() {
        Ok((data, _digest)) => data.sections.iter().map(|(id, _)| id.to_string()).collect(),
        Err(_) => return vec![],
    }
}
