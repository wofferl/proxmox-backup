use anyhow::{bail, Error};
use serde_json::json;
use ::serde::{Deserialize, Serialize};

use proxmox::api::{
    api, Permission, RpcEnvironment, RpcEnvironmentType,
    schema::{
        Schema,
        StringSchema,
        ArraySchema,
        IntegerSchema,
        ApiStringFormat,
        parse_property_string,
    },
};
use proxmox::api::router::Router;

use crate::config::acl::{PRIV_SYS_AUDIT, PRIV_SYS_MODIFY};
use crate::tools::disks::{
    DiskUsageType,
};

use crate::server::WorkerTask;

use crate::api2::types::*;

pub const DISK_ARRAY_SCHEMA: Schema = ArraySchema::new(
    "Disk name list.", &BLOCKDEVICE_NAME_SCHEMA)
    .schema();

pub const DISK_LIST_SCHEMA: Schema = StringSchema::new(
    "A list of disk names, comma separated.")
    .format(&ApiStringFormat::PropertyString(&DISK_ARRAY_SCHEMA))
    .schema();

pub const ZFS_ASHIFT_SCHEMA: Schema = IntegerSchema::new(
    "Pool sector size exponent.")
    .minimum(9)
    .maximum(16)
    .default(12)
    .schema();


#[api(
    default: "On",
)]
#[derive(Debug, Copy, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
/// The ZFS compression algorithm to use.
pub enum ZfsCompressionType {
    /// Gnu Zip
    Gzip,
    /// LZ4
    Lz4,
    /// LZJB
    Lzjb,
    /// ZLE
    Zle,
    /// Enable compression using the default algorithm.
    On,
    /// Disable compression.
    Off,
}

#[api()]
#[derive(Debug, Copy, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
/// The ZFS RAID level to use.
pub enum ZfsRaidLevel {
    /// Single Disk
    Single,
    /// Mirror
    Mirror,
    /// Raid10
    Raid10,
    /// RaidZ
    RaidZ,
    /// RaidZ2
    RaidZ2,
    /// RaidZ3
    RaidZ3,
}


#[api()]
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all="kebab-case")]
/// zpool list item
pub struct ZpoolListItem {
    /// zpool name
    pub name: String,
    /// Health
    pub health: String,
    /// Total size
    pub size: u64,
    /// Used size
    pub alloc: u64,
    /// Free space
    pub free: u64,
    /// ZFS fragnentation level
    pub frag: u64,
    /// ZFS deduplication ratio
    pub dedup: f64,
}


#[api(
    protected: true,
    input: {
        properties: {
            node: {
                schema: NODE_SCHEMA,
            },
        },
    },
    returns: {
        description: "List of zpools.",
        type: Array,
        items: {
            type: ZpoolListItem,
        },
    },
    access: {
        permission: &Permission::Privilege(&["system", "disks"], PRIV_SYS_AUDIT, false),
    },
)]
/// List zfs pools.
pub fn list_zpools() -> Result<Vec<ZpoolListItem>, Error> {

    let mut command = std::process::Command::new("/sbin/zpool");
    command.args(&["list", "-H", "-p", "-P"]);

    let output = crate::tools::run_command(command, None)?;

    let data = crate::tools::disks::parse_zfs_list(&output)?;

    let mut list = Vec::new();

    for item in data {
        if let Some(usage) = item.usage {
            list.push(ZpoolListItem {
                name: item.name,
                health: item.health,
                size: usage.size,
                alloc: usage.alloc,
                free: usage.free,
                frag: usage.frag,
                dedup: usage.dedup,
            });
        }
    }

    Ok(list)
}

#[api(
    protected: true,
    input: {
        properties: {
            node: {
                schema: NODE_SCHEMA,
            },
            name: {
                schema: DATASTORE_SCHEMA,
            },
            devices: {
                schema: DISK_LIST_SCHEMA,
            },
            raidlevel: {
                type: ZfsRaidLevel,
            },
            ashift: {
                schema: ZFS_ASHIFT_SCHEMA,
                optional: true,
            },
            compression: {
                type: ZfsCompressionType,
                optional: true,
            },
            "add-datastore": {
                description: "Configure a datastore using the zpool.",
                type: bool,
                optional: true,
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
/// Create a new ZFS pool.
pub fn create_zpool(
    name: String,
    devices: String,
    raidlevel: ZfsRaidLevel,
    compression: Option<String>,
    ashift: Option<usize>,
    add_datastore: Option<bool>,
    rpcenv: &mut dyn RpcEnvironment,
) -> Result<String, Error> {

    let to_stdout = if rpcenv.env_type() == RpcEnvironmentType::CLI { true } else { false };

    let username = rpcenv.get_user().unwrap();

    let add_datastore = add_datastore.unwrap_or(false);

    let ashift = ashift.unwrap_or(12);

    let devices_text = devices.clone();
    let devices = parse_property_string(&devices, &DISK_ARRAY_SCHEMA)?;
    let devices: Vec<String> = devices.as_array().unwrap().iter()
        .map(|v| v.as_str().unwrap().to_string()).collect();

    let disk_map = crate::tools::disks::get_disks(None, true)?;
    for disk in devices.iter() {
        match disk_map.get(disk) {
            Some(info) => {
                if info.used != DiskUsageType::Unused {
                    bail!("disk '{}' is already in use.", disk);
                }
            }
            None => {
                bail!("no such disk '{}'", disk);
            }
        }
    }

    let min_disks = match raidlevel {
        ZfsRaidLevel::Single => 1,
        ZfsRaidLevel::Mirror => 2,
        ZfsRaidLevel::Raid10 => 4,
        ZfsRaidLevel::RaidZ => 3,
        ZfsRaidLevel::RaidZ2 => 4,
        ZfsRaidLevel::RaidZ3 => 5,
    };

    // Sanity checks
    if raidlevel == ZfsRaidLevel::Raid10 && devices.len() % 2 != 0 {
        bail!("Raid10 needs an even number of disks.");
    }

    if raidlevel == ZfsRaidLevel::Single && devices.len() > 1 {
        bail!("Please give only one disk for single disk mode.");
    }

    if devices.len() < min_disks {
        bail!("{:?} needs at least {} disks.", raidlevel, min_disks);
    }

     let upid_str = WorkerTask::new_thread(
        "zfscreate", Some(name.clone()), &username.clone(), to_stdout, move |worker|
        {
            worker.log(format!("create {:?} zpool '{}' on devices '{}'", raidlevel, name, devices_text));


            let mut command = std::process::Command::new("zpool");
            command.args(&["create", "-o", &format!("ashift={}", ashift), &name]);

            match raidlevel {
                ZfsRaidLevel::Single => {
                    command.arg(&devices[0]);
                }
                ZfsRaidLevel::Mirror => {
                    command.arg("mirror");
                    command.args(devices);
                }
                ZfsRaidLevel::Raid10 => {
                     devices.chunks(2).for_each(|pair| {
                         command.arg("mirror");
                         command.args(pair);
                     });
                }
                ZfsRaidLevel::RaidZ => {
                    command.arg("raidz");
                    command.args(devices);
                }
                ZfsRaidLevel::RaidZ2 => {
                    command.arg("raidz2");
                    command.args(devices);
                }
                ZfsRaidLevel::RaidZ3 => {
                    command.arg("raidz3");
                    command.args(devices);
                }
            }

            worker.log(format!("# {:?}", command));

            let output = crate::tools::run_command(command, None)?;
            worker.log(output);

            if let Some(compression) = compression {
                let mut command = std::process::Command::new("zfs");
                command.args(&["set", &format!("compression={}", compression), &name]);
                worker.log(format!("# {:?}", command));
                let output = crate::tools::run_command(command, None)?;
                worker.log(output);
            }

            if add_datastore {
                let mount_point = format!("/{}", name);
                crate::api2::config::datastore::create_datastore(json!({ "name": name, "path": mount_point }))?
            }

            Ok(())
        })?;

    Ok(upid_str)
}

pub const ROUTER: Router = Router::new()
    .get(&API_METHOD_LIST_ZPOOLS)
    .post(&API_METHOD_CREATE_ZPOOL);