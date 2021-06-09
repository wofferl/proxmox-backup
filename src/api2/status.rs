//! Datastote status

use proxmox::list_subdirs_api_method;

use anyhow::{Error};
use serde_json::{json, Value};

use proxmox::api::{
    api,
    ApiMethod,
    Permission,
    Router,
    RpcEnvironment,
    SubdirMap,
};

use crate::api2::types::{
    DATASTORE_SCHEMA,
    RRDMode,
    RRDTimeFrameResolution,
    Authid,
};

use crate::backup::DataStore;
use crate::config::datastore;
use crate::tools::statistics::{linear_regression};
use crate::config::cached_user_info::CachedUserInfo;
use crate::config::acl::{
    PRIV_DATASTORE_AUDIT,
    PRIV_DATASTORE_BACKUP,
};

#[api(
    returns: {
        description: "Lists the Status of the Datastores.",
        type: Array,
        items: {
            description: "Status of a Datastore",
            type: Object,
            properties: {
                store: {
                    schema: DATASTORE_SCHEMA,
                },
                total: {
                    type: Integer,
                    description: "The Size of the underlying storage in bytes",
                },
                used: {
                    type: Integer,
                    description: "The used bytes of the underlying storage",
                },
                avail: {
                    type: Integer,
                    description: "The available bytes of the underlying storage",
                },
                history: {
                    type: Array,
                    optional: true,
                    description: "A list of usages of the past (last Month).",
                    items: {
                        type: Number,
                        description: "The usage of a time in the past. Either null or between 0.0 and 1.0.",
                    }
                },
                "estimated-full-date": {
                    type: Integer,
                    optional: true,
                    description: "Estimation of the UNIX epoch when the storage will be full.\
                        This is calculated via a simple Linear Regression (Least Squares)\
                        of RRD data of the last Month. Missing if there are not enough data points yet.\
                        If the estimate lies in the past, the usage is decreasing.",
                },
                "error": {
                    type: String,
                    optional: true,
                    description: "An error description, for example, when the datastore could not be looked up.",
                },
            },
        },
    },
    access: {
        permission: &Permission::Anybody,
    },
)]
/// List Datastore usages and estimates
pub fn datastore_status(
    _param: Value,
    _info: &ApiMethod,
    rpcenv: &mut dyn RpcEnvironment,
    ) -> Result<Value, Error> {

    let (config, _digest) = datastore::config()?;

    let auth_id: Authid = rpcenv.get_auth_id().unwrap().parse()?;
    let user_info = CachedUserInfo::new()?;

    let mut list = Vec::new();

    for (store, (_, _)) in &config.sections {
        let user_privs = user_info.lookup_privs(&auth_id, &["datastore", &store]);
        let allowed = (user_privs & (PRIV_DATASTORE_AUDIT| PRIV_DATASTORE_BACKUP)) != 0;
        if !allowed {
            continue;
        }

        let datastore = match DataStore::lookup_datastore(&store) {
            Ok(datastore) => datastore,
            Err(err) => {
                list.push(json!({
                    "store": store,
                    "total": -1,
                    "used": -1,
                    "avail": -1,
                    "error": err.to_string()
                }));
                continue;
            }
        };
        let status = crate::tools::disks::disk_usage(&datastore.base_path())?;

        let mut entry = json!({
            "store": store,
            "total": status.total,
            "used": status.used,
            "avail": status.avail,
            "gc-status": datastore.last_gc_status(),
        });

        let rrd_dir = format!("datastore/{}", store);
        let now = proxmox::tools::time::epoch_f64();

        let get_rrd = |what: &str| crate::rrd::extract_cached_data(
            &rrd_dir,
            what,
            now,
            RRDTimeFrameResolution::Month,
            RRDMode::Average,
        );

        let total_res = get_rrd("total");
        let used_res = get_rrd("used");

        if let (Some((start, reso, total_list)), Some((_, _, used_list))) = (total_res, used_res) {
            let mut usage_list: Vec<f64> = Vec::new();
            let mut time_list: Vec<u64> = Vec::new();
            let mut history = Vec::new();

            for (idx, used) in used_list.iter().enumerate() {
                let total = if idx < total_list.len() {
                    total_list[idx]
                } else {
                    None
                };

                match (total, used) {
                    (Some(total), Some(used)) if total != 0.0 => {
                        time_list.push(start + (idx as u64)*reso);
                        let usage = used/total;
                        usage_list.push(usage);
                        history.push(json!(usage));
                    },
                    _ => {
                        history.push(json!(null))
                    }
                }
            }

            entry["history-start"] = start.into();
            entry["history-delta"] = reso.into();
            entry["history"] = history.into();

            // we skip the calculation for datastores with not enough data
            if usage_list.len() >= 7 {
                entry["estimated-full-date"] = match linear_regression(&time_list, &usage_list) {
                    Some((a, b)) if b != 0.0 => Value::from(((1.0 - a) / b).floor() as u64),
                    _ => Value::from(0),
                };
            }
        }

        list.push(entry);
    }

    Ok(list.into())
}

const SUBDIRS: SubdirMap = &[
    ("datastore-usage", &Router::new().get(&API_METHOD_DATASTORE_STATUS)),
];

pub const ROUTER: Router = Router::new()
    .get(&list_subdirs_api_method!(SUBDIRS))
    .subdirs(SUBDIRS);
