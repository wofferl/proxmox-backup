use anyhow::{bail, Error};
use ::serde::{Deserialize, Serialize};
use serde_json::Value;

use proxmox::api::{api, Router, RpcEnvironment, Permission};

use crate::{
    config::{
        self,
        cached_user_info::CachedUserInfo,
        acl::{
            PRIV_TAPE_AUDIT,
            PRIV_TAPE_MODIFY,
        },
    },
    api2::types::{
        Authid,
        PROXMOX_CONFIG_DIGEST_SCHEMA,
        DRIVE_NAME_SCHEMA,
        CHANGER_NAME_SCHEMA,
        CHANGER_DRIVENUM_SCHEMA,
        LTO_DRIVE_PATH_SCHEMA,
        LtoTapeDrive,
        ScsiTapeChanger,
    },
    tape::{
        lto_tape_device_list,
        check_drive_path,
    },
};

#[api(
    protected: true,
    input: {
        properties: {
            name: {
                schema: DRIVE_NAME_SCHEMA,
            },
            path: {
                schema: LTO_DRIVE_PATH_SCHEMA,
            },
            changer: {
                schema: CHANGER_NAME_SCHEMA,
                optional: true,
            },
            "changer-drivenum": {
                schema: CHANGER_DRIVENUM_SCHEMA,
                optional: true,
            },
        },
    },
    access: {
        permission: &Permission::Privilege(&["tape", "device"], PRIV_TAPE_MODIFY, false),
    },
)]
/// Create a new drive
pub fn create_drive(param: Value) -> Result<(), Error> {

    let _lock = config::drive::lock()?;

    let (mut config, _digest) = config::drive::config()?;

    let item: LtoTapeDrive = serde_json::from_value(param)?;

    let lto_drives = lto_tape_device_list();

    check_drive_path(&lto_drives, &item.path)?;

    let existing: Vec<LtoTapeDrive> = config.convert_to_typed_array("lto")?;

    for drive in existing {
        if drive.name == item.name {
            bail!("Entry '{}' already exists", item.name);
        }
        if drive.path == item.path {
            bail!("Path '{}' already used in drive '{}'", item.path, drive.name);
        }
    }

    config.set_data(&item.name, "lto", &item)?;

    config::drive::save_config(&config)?;

    Ok(())
}

#[api(
    input: {
        properties: {
            name: {
                schema: DRIVE_NAME_SCHEMA,
            },
        },
    },
    returns: {
        type: LtoTapeDrive,
    },
    access: {
        permission: &Permission::Privilege(&["tape", "device", "{name}"], PRIV_TAPE_AUDIT, false),
    },
)]
/// Get drive configuration
pub fn get_config(
    name: String,
    _param: Value,
    mut rpcenv: &mut dyn RpcEnvironment,
) -> Result<LtoTapeDrive, Error> {

    let (config, digest) = config::drive::config()?;

    let data: LtoTapeDrive = config.lookup("lto", &name)?;

    rpcenv["digest"] = proxmox::tools::digest_to_hex(&digest).into();

    Ok(data)
}

#[api(
    input: {
        properties: {},
    },
    returns: {
        description: "The list of configured drives (with config digest).",
        type: Array,
        items: {
            type: LtoTapeDrive,
        },
    },
    access: {
        description: "List configured tape drives filtered by Tape.Audit privileges",
        permission: &Permission::Anybody,
    },
)]
/// List drives
pub fn list_drives(
    _param: Value,
    mut rpcenv: &mut dyn RpcEnvironment,
) -> Result<Vec<LtoTapeDrive>, Error> {
    let auth_id: Authid = rpcenv.get_auth_id().unwrap().parse()?;
    let user_info = CachedUserInfo::new()?;

    let (config, digest) = config::drive::config()?;

    let drive_list: Vec<LtoTapeDrive> = config.convert_to_typed_array("lto")?;

    let drive_list = drive_list
        .into_iter()
        .filter(|drive| {
            let privs = user_info.lookup_privs(&auth_id, &["tape", "device", &drive.name]);
            privs & PRIV_TAPE_AUDIT != 0
        })
        .collect();

    rpcenv["digest"] = proxmox::tools::digest_to_hex(&digest).into();

    Ok(drive_list)
}

#[api()]
#[derive(Serialize, Deserialize)]
#[allow(non_camel_case_types)]
#[serde(rename_all = "kebab-case")]
/// Deletable property name
pub enum DeletableProperty {
    /// Delete the changer property.
    changer,
    /// Delete the changer-drivenum property.
    changer_drivenum,
}

#[api(
    protected: true,
    input: {
        properties: {
            name: {
                schema: DRIVE_NAME_SCHEMA,
            },
            path: {
                schema: LTO_DRIVE_PATH_SCHEMA,
                optional: true,
            },
            changer: {
                schema: CHANGER_NAME_SCHEMA,
                optional: true,
            },
            "changer-drivenum": {
                schema: CHANGER_DRIVENUM_SCHEMA,
                optional: true,
            },
            delete: {
                description: "List of properties to delete.",
                type: Array,
                optional: true,
                items: {
                    type: DeletableProperty,
                }
            },
            digest: {
                schema: PROXMOX_CONFIG_DIGEST_SCHEMA,
                optional: true,
            },
       },
    },
    access: {
        permission: &Permission::Privilege(&["tape", "device", "{name}"], PRIV_TAPE_MODIFY, false),
    },
)]
/// Update a drive configuration
pub fn update_drive(
    name: String,
    path: Option<String>,
    changer: Option<String>,
    changer_drivenum: Option<u64>,
    delete: Option<Vec<DeletableProperty>>,
    digest: Option<String>,
   _param: Value,
) -> Result<(), Error> {

    let _lock = config::drive::lock()?;

    let (mut config, expected_digest) = config::drive::config()?;

    if let Some(ref digest) = digest {
        let digest = proxmox::tools::hex_to_digest(digest)?;
        crate::tools::detect_modified_configuration_file(&digest, &expected_digest)?;
    }

    let mut data: LtoTapeDrive = config.lookup("lto", &name)?;

    if let Some(delete) = delete {
        for delete_prop in delete {
            match delete_prop {
                DeletableProperty::changer => {
                    data.changer = None;
                    data.changer_drivenum = None;
                },
                DeletableProperty::changer_drivenum => { data.changer_drivenum = None; },
            }
        }
    }

    if let Some(path) = path {
        let lto_drives = lto_tape_device_list();
        check_drive_path(&lto_drives, &path)?;
        data.path = path;
    }

    if let Some(changer) = changer {
        let _: ScsiTapeChanger = config.lookup("changer", &changer)?;
        data.changer = Some(changer);
    }

    if let Some(changer_drivenum) = changer_drivenum {
        if changer_drivenum == 0 {
            data.changer_drivenum = None;
        } else {
            if data.changer.is_none() {
                bail!("Option 'changer-drivenum' requires option 'changer'.");
            }
            data.changer_drivenum = Some(changer_drivenum);
        }
    }

    config.set_data(&name, "lto", &data)?;

    config::drive::save_config(&config)?;

    Ok(())
}

#[api(
    protected: true,
    input: {
        properties: {
            name: {
                schema: DRIVE_NAME_SCHEMA,
            },
        },
    },
    access: {
        permission: &Permission::Privilege(&["tape", "device", "{name}"], PRIV_TAPE_MODIFY, false),
    },
)]
/// Delete a drive configuration
pub fn delete_drive(name: String, _param: Value) -> Result<(), Error> {

    let _lock = config::drive::lock()?;

    let (mut config, _digest) = config::drive::config()?;

    match config.sections.get(&name) {
        Some((section_type, _)) => {
            if section_type != "lto" {
                bail!("Entry '{}' exists, but is not a lto tape drive", name);
            }
            config.sections.remove(&name);
        },
        None => bail!("Delete drive '{}' failed - no such drive", name),
    }

    config::drive::save_config(&config)?;

    Ok(())
}

const ITEM_ROUTER: Router = Router::new()
    .get(&API_METHOD_GET_CONFIG)
    .put(&API_METHOD_UPDATE_DRIVE)
    .delete(&API_METHOD_DELETE_DRIVE);


pub const ROUTER: Router = Router::new()
    .get(&API_METHOD_LIST_DRIVES)
    .post(&API_METHOD_CREATE_DRIVE)
    .match_all("name", &ITEM_ROUTER);
