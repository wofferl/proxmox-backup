use anyhow::{bail, Error};
use serde_json::{json, Value};

use proxmox::api::{api, Permission, RpcEnvironment, RpcEnvironmentType};
use proxmox::api::router::{Router, SubdirMap};
use proxmox::{sortable, identity};
use proxmox::{list_subdirs_api_method};

use crate::config::acl::{PRIV_SYS_AUDIT, PRIV_SYS_MODIFY};
use crate::tools::disks::{
    DiskUsageInfo, DiskUsageType, DiskManage, SmartData,
    get_disks, get_smart_data, get_disk_usage_info, inititialize_gpt_disk,
};
use crate::server::WorkerTask;

use crate::api2::types::{Authid, UPID_SCHEMA, NODE_SCHEMA, BLOCKDEVICE_NAME_SCHEMA};

pub mod directory;
pub mod zfs;

#[api(
    protected: true,
    input: {
        properties: {
            node: {
                schema: NODE_SCHEMA,
            },
            skipsmart: {
                description: "Skip smart checks.",
                type: bool,
                optional: true,
                default: false,
            },
            "usage-type": {
                type: DiskUsageType,
                optional: true,
            },
        },
    },
    returns: {
        description: "Local disk list.",
        type: Array,
        items: {
            type: DiskUsageInfo,
        },
    },
    access: {
        permission: &Permission::Privilege(&["system", "disks"], PRIV_SYS_AUDIT, false),
    },
)]
/// List local disks
pub fn list_disks(
    skipsmart: bool,
    usage_type: Option<DiskUsageType>,
) -> Result<Vec<DiskUsageInfo>, Error> {

    let mut list = Vec::new();

    for (_, info) in get_disks(None, skipsmart)? {
        if let Some(ref usage_type) = usage_type {
            if info.used == *usage_type {
                list.push(info);
            }
        } else {
            list.push(info);
        }
    }

    list.sort_by(|a, b| a.name.cmp(&b.name));

    Ok(list)
}

#[api(
    protected: true,
    input: {
        properties: {
            node: {
                schema: NODE_SCHEMA,
            },
            disk: {
                schema: BLOCKDEVICE_NAME_SCHEMA,
            },
            healthonly: {
                description: "If true returns only the health status.",
                type: bool,
                optional: true,
            },
        },
    },
    returns: {
        type: SmartData,
    },
    access: {
        permission: &Permission::Privilege(&["system", "disks"], PRIV_SYS_AUDIT, false),
    },
)]
/// Get SMART attributes and health of a disk.
pub fn smart_status(
    disk: String,
    healthonly: Option<bool>,
) -> Result<SmartData, Error> {

    let healthonly = healthonly.unwrap_or(false);

    let manager = DiskManage::new();
    let disk = manager.disk_by_name(&disk)?;
    get_smart_data(&disk, healthonly)
}

#[api(
    protected: true,
    input: {
        properties: {
            node: {
                schema: NODE_SCHEMA,
            },
            disk: {
                schema: BLOCKDEVICE_NAME_SCHEMA,
            },
            uuid: {
                description: "UUID for the GPT table.",
                type: String,
                optional: true,
                max_length: 36,
            },
        },
    },
    returns: {
        schema: UPID_SCHEMA,
    },
    access: {
        permission: &Permission::Privilege(&["system", "disks"], PRIV_SYS_MODIFY, false),
    },
)]
/// Initialize empty Disk with GPT
pub fn initialize_disk(
    disk: String,
    uuid: Option<String>,
    rpcenv: &mut dyn RpcEnvironment,
) -> Result<Value, Error> {

    let to_stdout = rpcenv.env_type() == RpcEnvironmentType::CLI;

    let auth_id: Authid = rpcenv.get_auth_id().unwrap().parse()?;

    let info = get_disk_usage_info(&disk, true)?;

    if info.used != DiskUsageType::Unused {
        bail!("disk '{}' is already in use.", disk);
    }

    let upid_str = WorkerTask::new_thread(
        "diskinit", Some(disk.clone()), auth_id, to_stdout, move |worker|
        {
            worker.log(format!("initialize disk {}", disk));

            let disk_manager = DiskManage::new();
            let disk_info = disk_manager.disk_by_name(&disk)?;

            inititialize_gpt_disk(&disk_info, uuid.as_deref())?;

            Ok(())
        })?;

    Ok(json!(upid_str))
}

#[sortable]
const SUBDIRS: SubdirMap = &sorted!([
    //    ("lvm", &lvm::ROUTER),
    ("directory", &directory::ROUTER),
    ("zfs", &zfs::ROUTER),
    (
        "initgpt", &Router::new()
            .post(&API_METHOD_INITIALIZE_DISK)
    ),
    (
        "list", &Router::new()
            .get(&API_METHOD_LIST_DISKS)
    ),
    (
        "smart", &Router::new()
            .get(&API_METHOD_SMART_STATUS)
    ),
]);

pub const ROUTER: Router = Router::new()
    .get(&list_subdirs_api_method!(SUBDIRS))
    .subdirs(SUBDIRS);
