use std::fs::File;
use std::io::{BufRead, BufReader};

use anyhow::{bail, Error};
use serde_json::{json, Value};

use proxmox::api::{api, Router, RpcEnvironment, Permission};
use proxmox::api::router::SubdirMap;
use proxmox::{identity, list_subdirs_api_method, sortable};

use crate::tools;

use crate::api2::types::*;
use crate::api2::pull::check_pull_privs;

use crate::server::{self, UPID, TaskState, TaskListInfoIterator};
use crate::config::acl::{
    PRIV_DATASTORE_MODIFY,
    PRIV_DATASTORE_VERIFY,
    PRIV_SYS_AUDIT,
    PRIV_SYS_MODIFY,
};
use crate::config::cached_user_info::CachedUserInfo;

// matches respective job execution privileges
fn check_job_privs(auth_id: &Authid, user_info: &CachedUserInfo, upid: &UPID) -> Result<(), Error> {
    match (upid.worker_type.as_str(), &upid.worker_id) {
        ("verificationjob", Some(workerid)) => {
            if let Some(captures) = VERIFICATION_JOB_WORKER_ID_REGEX.captures(&workerid) {
                if let Some(store) = captures.get(1) {
                    return user_info.check_privs(&auth_id,
                                                 &["datastore", store.as_str()],
                                                 PRIV_DATASTORE_VERIFY,
                                                 true);
                }
            }
        },
        ("syncjob", Some(workerid)) => {
            if let Some(captures) = SYNC_JOB_WORKER_ID_REGEX.captures(&workerid) {
                let remote = captures.get(1);
                let remote_store = captures.get(2);
                let local_store = captures.get(3);

                if let (Some(remote), Some(remote_store), Some(local_store)) =
                    (remote, remote_store, local_store) {

                    return check_pull_privs(&auth_id,
                                            local_store.as_str(),
                                            remote.as_str(),
                                            remote_store.as_str(),
                                            false);
                }
            }
        },
        ("garbage_collection", Some(workerid)) => {
            return user_info.check_privs(&auth_id,
                                         &["datastore", &workerid],
                                         PRIV_DATASTORE_MODIFY,
                                         true)
        },
        ("prune", Some(workerid)) => {
            return user_info.check_privs(&auth_id,
                                         &["datastore",
                                         &workerid],
                                         PRIV_DATASTORE_MODIFY,
                                         true);
        },
        _ => bail!("not a scheduled job task"),
    };

    bail!("not a scheduled job task");
}

// get the store out of the worker_id
fn check_job_store(upid: &UPID, store: &str) -> bool {
    match (upid.worker_type.as_str(), &upid.worker_id) {
        (workertype, Some(workerid)) if workertype.starts_with("verif") => {
            if let Some(captures) = VERIFICATION_JOB_WORKER_ID_REGEX.captures(&workerid) {
                if let Some(jobstore) = captures.get(1) {
                    return store == jobstore.as_str();
                }
            } else {
                return workerid == store;
            }
        }
        ("syncjob", Some(workerid)) => {
            if let Some(captures) = SYNC_JOB_WORKER_ID_REGEX.captures(&workerid) {
                if let Some(local_store) = captures.get(3) {
                    return store == local_store.as_str();
                }
            }
        }
        ("prune", Some(workerid))
        | ("backup", Some(workerid))
        | ("garbage_collection", Some(workerid)) => {
            return workerid == store || workerid.starts_with(&format!("{}:", store));
        }
        _ => {}
    };

    false
}

fn check_task_access(auth_id: &Authid, upid: &UPID) -> Result<(), Error> {
    let task_auth_id = &upid.auth_id;
    if auth_id == task_auth_id
        || (task_auth_id.is_token() && &Authid::from(task_auth_id.user().clone()) == auth_id) {
        // task owner can always read
        Ok(())
    } else {
        let user_info = CachedUserInfo::new()?;

        // access to all tasks
        // or task == job which the user/token could have configured/manually executed

        user_info.check_privs(auth_id, &["system", "tasks"], PRIV_SYS_AUDIT, false)
            .or_else(|_| check_job_privs(&auth_id, &user_info, upid))
            .or_else(|_| bail!("task access not allowed"))
    }
}

#[api(
    input: {
        properties: {
            node: {
                schema: NODE_SCHEMA,
            },
            upid: {
                schema: UPID_SCHEMA,
            },
        },
    },
    returns: {
        description: "Task status information.",
        properties: {
            node: {
                schema: NODE_SCHEMA,
            },
            upid: {
                schema: UPID_SCHEMA,
            },
            pid: {
                type: i64,
                description: "The Unix PID.",
            },
            pstart: {
                type: u64,
                description: "The Unix process start time from `/proc/pid/stat`",
            },
            starttime: {
                type: i64,
                description: "The task start time (Epoch)",
            },
            "type": {
                type: String,
                description: "Worker type (arbitrary ASCII string)",
            },
            id: {
                type: String,
                optional: true,
                description: "Worker ID (arbitrary ASCII string)",
            },
            user: {
                type: Userid,
            },
            tokenid: {
                type: Tokenname,
                optional: true,
            },
            status: {
                type: String,
                description: "'running' or 'stopped'",
            },
            exitstatus: {
                type: String,
                optional: true,
                description: "'OK', 'Error: <msg>', or 'unkwown'.",
            },
        },
    },
    access: {
        description: "Users can access their own tasks, or need Sys.Audit on /system/tasks.",
        permission: &Permission::Anybody,
    },
)]
/// Get task status.
async fn get_task_status(
    param: Value,
    rpcenv: &mut dyn RpcEnvironment,
) -> Result<Value, Error> {

    let upid = extract_upid(&param)?;

    let auth_id: Authid = rpcenv.get_auth_id().unwrap().parse()?;
    check_task_access(&auth_id, &upid)?;

    let mut result = json!({
        "upid": param["upid"],
        "node": upid.node,
        "pid": upid.pid,
        "pstart": upid.pstart,
        "starttime": upid.starttime,
        "type": upid.worker_type,
        "id": upid.worker_id,
        "user": upid.auth_id.user(),
    });

    if upid.auth_id.is_token() {
        result["tokenid"] = Value::from(upid.auth_id.tokenname().unwrap().as_str());
    }

    if crate::server::worker_is_active(&upid).await? {
        result["status"] = Value::from("running");
    } else {
        let exitstatus = crate::server::upid_read_status(&upid).unwrap_or(TaskState::Unknown { endtime: 0 });
        result["status"] = Value::from("stopped");
        result["exitstatus"] = Value::from(exitstatus.to_string());
    };

    Ok(result)
}

fn extract_upid(param: &Value) -> Result<UPID, Error> {

    let upid_str = tools::required_string_param(&param, "upid")?;

    upid_str.parse::<UPID>()
}

#[api(
    input: {
        properties: {
            node: {
                schema: NODE_SCHEMA,
            },
            upid: {
                schema: UPID_SCHEMA,
            },
            "test-status": {
                type: bool,
                optional: true,
                description: "Test task status, and set result attribute \"active\" accordingly.",
            },
            start: {
                type: u64,
                optional: true,
                description: "Start at this line.",
                default: 0,
            },
            limit: {
                type: u64,
                optional: true,
                description: "Only list this amount of lines.",
                default: 50,
            },
        },
    },
    access: {
        description: "Users can access their own tasks, or need Sys.Audit on /system/tasks.",
        permission: &Permission::Anybody,
    },
)]
/// Read task log.
async fn read_task_log(
    param: Value,
    mut rpcenv: &mut dyn RpcEnvironment,
) -> Result<Value, Error> {

    let upid = extract_upid(&param)?;

    let auth_id: Authid = rpcenv.get_auth_id().unwrap().parse()?;

    check_task_access(&auth_id, &upid)?;

    let test_status = param["test-status"].as_bool().unwrap_or(false);

    let start = param["start"].as_u64().unwrap_or(0);
    let mut limit = param["limit"].as_u64().unwrap_or(50);

    let mut count: u64 = 0;

    let path = upid.log_path();

    let file = File::open(path)?;

    let mut lines: Vec<Value> = vec![];

    for line in BufReader::new(file).lines() {
        match line {
            Ok(line) => {
                count += 1;
                if count < start { continue };
	        if limit == 0 { continue };

                lines.push(json!({ "n": count, "t": line }));

                limit -= 1;
            }
            Err(err) => {
                log::error!("reading task log failed: {}", err);
                break;
            }
        }
    }

    rpcenv["total"] = Value::from(count);

    if test_status {
        let active = crate::server::worker_is_active(&upid).await?;
        rpcenv["active"] = Value::from(active);
    }

    Ok(json!(lines))
}

#[api(
    protected: true,
    input: {
        properties: {
            node: {
                schema: NODE_SCHEMA,
            },
            upid: {
                schema: UPID_SCHEMA,
            },
        },
    },
    access: {
        description: "Users can stop their own tasks, or need Sys.Modify on /system/tasks.",
        permission: &Permission::Anybody,
    },
)]
/// Try to stop a task.
fn stop_task(
    param: Value,
    rpcenv: &mut dyn RpcEnvironment,
) -> Result<Value, Error> {

    let upid = extract_upid(&param)?;

    let auth_id: Authid = rpcenv.get_auth_id().unwrap().parse()?;

    if auth_id != upid.auth_id {
        let user_info = CachedUserInfo::new()?;
        user_info.check_privs(&auth_id, &["system", "tasks"], PRIV_SYS_MODIFY, false)?;
    }

    server::abort_worker_async(upid);

    Ok(Value::Null)
}

#[api(
    input: {
        properties: {
            node: {
                schema: NODE_SCHEMA
            },
            start: {
                type: u64,
                description: "List tasks beginning from this offset.",
                default: 0,
                optional: true,
            },
            limit: {
                type: u64,
                description: "Only list this amount of tasks. (0 means no limit)",
                default: 50,
                optional: true,
            },
            store: {
                schema: DATASTORE_SCHEMA,
                optional: true,
            },
            running: {
                type: bool,
                description: "Only list running tasks.",
                optional: true,
                default: false,
            },
            errors: {
                type: bool,
                description: "Only list erroneous tasks.",
                optional:true,
                default: false,
            },
            userfilter: {
                optional: true,
                type: String,
                description: "Only list tasks from this user.",
            },
            since: {
                type: i64,
                description: "Only list tasks since this UNIX epoch.",
                optional: true,
            },
            until: {
                type: i64,
                description: "Only list tasks until this UNIX epoch.",
                optional: true,
            },
            typefilter: {
                optional: true,
                type: String,
                description: "Only list tasks whose type contains this.",
            },
            statusfilter: {
                optional: true,
                type: Array,
                description: "Only list tasks which have any one of the listed status.",
                items: {
                    type: TaskStateType,
                },
            },
        },
    },
    returns: {
        description: "A list of tasks.",
        type: Array,
        items: { type: TaskListItem },
    },
    access: {
        description: "Users can only see their own tasks, unless they have Sys.Audit on /system/tasks.",
        permission: &Permission::Anybody,
    },
)]
/// List tasks.
#[allow(clippy::too_many_arguments)]
pub fn list_tasks(
    start: u64,
    limit: u64,
    errors: bool,
    running: bool,
    userfilter: Option<String>,
    since: Option<i64>,
    until: Option<i64>,
    typefilter: Option<String>,
    statusfilter: Option<Vec<TaskStateType>>,
    param: Value,
    mut rpcenv: &mut dyn RpcEnvironment,
) -> Result<Vec<TaskListItem>, Error> {

    let auth_id: Authid = rpcenv.get_auth_id().unwrap().parse()?;
    let user_info = CachedUserInfo::new()?;
    let user_privs = user_info.lookup_privs(&auth_id, &["system", "tasks"]);

    let list_all = (user_privs & PRIV_SYS_AUDIT) != 0;

    let store = param["store"].as_str();

    let list = TaskListInfoIterator::new(running)?;
    let limit = if limit > 0 { limit as usize } else { usize::MAX };

    let result: Vec<TaskListItem> = list
        .skip_while(|info| {
            match (info, until) {
                (Ok(info), Some(until)) => info.upid.starttime > until,
                (Ok(_), None) => false,
                (Err(_), _) => false,
            }
        })
        .take_while(|info| {
            match (info, since) {
                (Ok(info), Some(since)) => info.upid.starttime > since,
                (Ok(_), None) => true,
                (Err(_), _) => false,
            }
        })
        .filter_map(|info| {
        let info = match info {
            Ok(info) => info,
            Err(_) => return None,
        };

        if !list_all && check_task_access(&auth_id, &info.upid).is_err() {
            return None;
        }

        if let Some(needle) = &userfilter {
            if !info.upid.auth_id.to_string().contains(needle) { return None; }
        }

        if let Some(store) = store {
            if !check_job_store(&info.upid, store) {
                return None;
            }
        }

        if let Some(typefilter) = &typefilter {
            if !info.upid.worker_type.contains(typefilter) {
                return None;
            }
        }

        match (&info.state, &statusfilter) {
            (Some(_), _) if running => return None,
            (Some(crate::server::TaskState::OK { .. }), _) if errors => return None,
            (Some(state), Some(filters)) => {
                if !filters.contains(&state.tasktype()) {
                    return None;
                }
            },
            (None, Some(_)) => return None,
            _ => {},
        }

        Some(info.into())
    }).skip(start as usize)
        .take(limit)
        .collect();

    let mut count = result.len() + start as usize;
    if !result.is_empty() && result.len() >= limit { // we have a 'virtual' entry as long as we have any new
        count += 1;
    }

    rpcenv["total"] = Value::from(count);

    Ok(result)
}

#[sortable]
const UPID_API_SUBDIRS: SubdirMap = &sorted!([
    (
        "log", &Router::new()
            .get(&API_METHOD_READ_TASK_LOG)
    ),
    (
        "status", &Router::new()
            .get(&API_METHOD_GET_TASK_STATUS)
    )
]);

pub const UPID_API_ROUTER: Router = Router::new()
    .get(&list_subdirs_api_method!(UPID_API_SUBDIRS))
    .delete(&API_METHOD_STOP_TASK)
    .subdirs(&UPID_API_SUBDIRS);

pub const ROUTER: Router = Router::new()
    .get(&API_METHOD_LIST_TASKS)
    .match_all("upid", &UPID_API_ROUTER);
