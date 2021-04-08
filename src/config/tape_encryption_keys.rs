//! Store Tape encryptions keys
//!
//! This module can store 256bit encryption keys for tape backups,
//! indexed by key fingerprint.
//!
//! We store the plain key (unencrypted), as well as a encrypted
//! version protected by password (see struct `KeyConfig`)
//!
//! Tape backups store the password protected version on tape, so that
//! it is possible to restore the key from tape if you know the
//! password.

use std::collections::HashMap;

use anyhow::{bail, Error};
use serde::{Deserialize, Serialize};

use proxmox::tools::fs::{
    file_read_optional_string,
    replace_file,
    open_file_locked,
    CreateOptions,
};

use crate::{
    backup::{
        Fingerprint,
        KeyConfig,
    },
};

mod hex_key {
    use serde::{self, Deserialize, Serializer, Deserializer};

    pub fn serialize<S>(
        csum: &[u8; 32],
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let s = proxmox::tools::digest_to_hex(csum);
        serializer.serialize_str(&s)
    }

    pub fn deserialize<'de, D>(
        deserializer: D,
    ) -> Result<[u8; 32], D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        proxmox::tools::hex_to_digest(&s).map_err(serde::de::Error::custom)
    }
}

/// Store Hardware Encryption keys (plain, unprotected keys)
#[derive(Deserialize, Serialize)]
pub struct EncryptionKeyInfo {
    /// Key fingerprint (we verify the fingerprint on load)
    pub fingerprint: Fingerprint,
    /// The plain encryption key
    #[serde(with = "hex_key")]
    pub key: [u8; 32],
}

impl EncryptionKeyInfo {
    pub fn new(key: [u8; 32], fingerprint: Fingerprint) -> Self {
        Self { fingerprint, key }
    }
}

pub const TAPE_KEYS_FILENAME: &str = "/etc/proxmox-backup/tape-encryption-keys.json";
pub const TAPE_KEY_CONFIG_FILENAME: &str = "/etc/proxmox-backup/tape-encryption-key-config.json";
pub const TAPE_KEYS_LOCKFILE: &str = "/etc/proxmox-backup/.tape-encryption-keys.lck";

/// Load tape encryption keys (plain, unprotected keys)
pub fn load_keys() -> Result<(HashMap<Fingerprint, EncryptionKeyInfo>,  [u8;32]), Error> {

    let content = file_read_optional_string(TAPE_KEYS_FILENAME)?;
    let content = content.unwrap_or_else(|| String::from("[]"));

    let digest = openssl::sha::sha256(content.as_bytes());

    let key_list: Vec<EncryptionKeyInfo> = serde_json::from_str(&content)?;

    let mut map = HashMap::new();

    for item in key_list {
        let key_config = KeyConfig::without_password(item.key)?; // to compute fingerprint
        let expected_fingerprint = key_config.fingerprint.unwrap();
        if item.fingerprint != expected_fingerprint {
            bail!(
                "inconsistent fingerprint ({} != {})",
                item.fingerprint,
                expected_fingerprint,
            );
        }

        if map.insert(item.fingerprint.clone(), item).is_some() {
            bail!("found duplicate fingerprint");
        }
    }

    Ok((map, digest))
}

/// Load tape encryption key configurations (password protected keys)
pub fn load_key_configs() -> Result<(HashMap<Fingerprint, KeyConfig>,  [u8;32]), Error> {

    let content = file_read_optional_string(TAPE_KEY_CONFIG_FILENAME)?;
    let content = content.unwrap_or_else(|| String::from("[]"));

    let digest = openssl::sha::sha256(content.as_bytes());

    let key_list: Vec<KeyConfig> = serde_json::from_str(&content)?;

    let mut map = HashMap::new();

    for key_config in key_list {
        match key_config.fingerprint {
            Some(ref fingerprint) => {
                if map.insert(fingerprint.clone(), key_config).is_some() {
                    bail!("found duplicate fingerprint");
                }
            }
            None => bail!("missing fingerprint"),
        }
    }

    Ok((map, digest))
}

/// Store tape encryption keys (plain, unprotected keys)
///
/// The file is only accessible by user root (mode 0600).
pub fn save_keys(map: HashMap<Fingerprint, EncryptionKeyInfo>) -> Result<(), Error> {

    let mut list = Vec::new();

    for (_fp, item) in map {
        list.push(item);
    }

    let raw = serde_json::to_string_pretty(&list)?;

    let mode = nix::sys::stat::Mode::from_bits_truncate(0o0600);
    // set the correct owner/group/permissions while saving file
    // owner(rw) = root, group(r)= root
    let options = CreateOptions::new()
        .perm(mode)
        .owner(nix::unistd::ROOT)
        .group(nix::unistd::Gid::from_raw(0));

    replace_file(TAPE_KEYS_FILENAME, raw.as_bytes(), options)?;

    Ok(())
}

/// Store tape encryption key configurations (password protected keys)
pub fn save_key_configs(map: HashMap<Fingerprint, KeyConfig>) -> Result<(), Error> {

    let mut list = Vec::new();

    for (_fp, item) in map {
        list.push(item);
    }

    let raw = serde_json::to_string_pretty(&list)?;

    let backup_user = crate::backup::backup_user()?;
    let mode = nix::sys::stat::Mode::from_bits_truncate(0o0640);
    // set the correct owner/group/permissions while saving file
    // owner(rw) = root, group(r)= backup
    let options = CreateOptions::new()
        .perm(mode)
        .owner(nix::unistd::ROOT)
        .group(backup_user.gid);

    replace_file(TAPE_KEY_CONFIG_FILENAME, raw.as_bytes(), options)?;

    Ok(())
}

/// Insert a new key
///
/// Get the lock, load both files, insert the new key, store files.
pub fn insert_key(key: [u8;32], key_config: KeyConfig, force: bool) -> Result<(), Error> {

    let _lock = open_file_locked(
        TAPE_KEYS_LOCKFILE,
        std::time::Duration::new(10, 0),
        true,
    )?;

    let (mut key_map, _) = load_keys()?;
    let (mut config_map, _) = load_key_configs()?;

    let fingerprint = match key_config.fingerprint.clone() {
        Some(fingerprint) => fingerprint,
        None => bail!("missing encryption key fingerprint - internal error"),
    };

    if !force {
        if config_map.get(&fingerprint).is_some() {
            bail!("encryption key '{}' already exists.", fingerprint);
        }
    }

    let item = EncryptionKeyInfo::new(key, fingerprint.clone());
    key_map.insert(fingerprint.clone(), item);
    save_keys(key_map)?;

    config_map.insert(fingerprint.clone(), key_config);
    save_key_configs(config_map)?;

    Ok(())
}

// shell completion helper
/// Complete tape encryption key fingerprints
pub fn complete_key_fingerprint(_arg: &str, _param: &HashMap<String, String>) -> Vec<String> {
    let data = match load_key_configs() {
        Ok((data, _digest)) => data,
        Err(_) => return Vec::new(),
    };

    data.keys().map(|fp| crate::tools::format::as_fingerprint(fp.bytes())).collect()
}
