use anyhow::Error;
use serde_json::Value;

use proxmox::api::{api, cli::*, RpcEnvironment, ApiHandler};

use proxmox_backup::{
    config,
    client::{
        connect_to_localhost,
        view_task_result,
    },
    api2::{
        self,
        types::*,
    },
};

#[api(
    input: {
        properties: {
            "output-format": {
                schema: OUTPUT_FORMAT,
                optional: true,
            },
        }
    }
)]
/// Tape backup job list.
fn list_tape_backup_jobs(param: Value, rpcenv: &mut dyn RpcEnvironment) -> Result<Value, Error> {

    let output_format = get_output_format(&param);

    let info = &api2::config::tape_backup_job::API_METHOD_LIST_TAPE_BACKUP_JOBS;
    let mut data = match info.handler {
        ApiHandler::Sync(handler) => (handler)(param, info, rpcenv)?,
        _ => unreachable!(),
    };

    let options = default_table_format_options()
        .column(ColumnConfig::new("id"))
        .column(ColumnConfig::new("store"))
        .column(ColumnConfig::new("pool"))
        .column(ColumnConfig::new("drive"))
        .column(ColumnConfig::new("schedule"))
        .column(ColumnConfig::new("comment"));

    format_and_print_result_full(&mut data, &info.returns, &output_format, &options);

    Ok(Value::Null)
}

#[api(
    input: {
        properties: {
            id: {
                schema: JOB_ID_SCHEMA,
            },
            "output-format": {
                schema: OUTPUT_FORMAT,
                optional: true,
            },
        }
    }
)]
/// Show tape backup job configuration
fn show_tape_backup_job(param: Value, rpcenv: &mut dyn RpcEnvironment) -> Result<Value, Error> {

    let output_format = get_output_format(&param);

    let info = &api2::config::tape_backup_job::API_METHOD_READ_TAPE_BACKUP_JOB;
    let mut data = match info.handler {
        ApiHandler::Sync(handler) => (handler)(param, info, rpcenv)?,
        _ => unreachable!(),
    };

    let options = default_table_format_options();
    format_and_print_result_full(&mut data, &info.returns, &output_format, &options);

    Ok(Value::Null)
}

#[api(
    input: {
        properties: {
            id: {
                schema: JOB_ID_SCHEMA,
            },
        },
    },
)]
/// Run THape Backup Job
async fn run_tape_backup_job(mut param: Value) -> Result<(), Error> {

    let output_format = get_output_format(&param);

    let id = param["id"].take().as_str().unwrap().to_string();

    let mut client = connect_to_localhost()?;

    let result = client.post(&format!("api2/json/tape/backup/{}", id), Some(param)).await?;

    view_task_result(&mut client, result, &output_format).await?;

    Ok(())
}

pub fn backup_job_commands() -> CommandLineInterface {

    let cmd_def = CliCommandMap::new()
        .insert("list", CliCommand::new(&API_METHOD_LIST_TAPE_BACKUP_JOBS))
        .insert("show",
                CliCommand::new(&API_METHOD_SHOW_TAPE_BACKUP_JOB)
                .arg_param(&["id"])
                .completion_cb("id", config::tape_job::complete_tape_job_id)
        )
        .insert("run",
                CliCommand::new(&API_METHOD_RUN_TAPE_BACKUP_JOB)
                .arg_param(&["id"])
                .completion_cb("id", config::tape_job::complete_tape_job_id)
        )
        .insert("create",
                CliCommand::new(&api2::config::tape_backup_job::API_METHOD_CREATE_TAPE_BACKUP_JOB)
                .arg_param(&["id"])
                .completion_cb("id", config::tape_job::complete_tape_job_id)
                .completion_cb("schedule", config::datastore::complete_calendar_event)
                .completion_cb("store", config::datastore::complete_datastore_name)
                .completion_cb("pool", config::media_pool::complete_pool_name)
                .completion_cb("drive", crate::complete_drive_name)
        )
        .insert("update",
                CliCommand::new(&api2::config::tape_backup_job::API_METHOD_UPDATE_TAPE_BACKUP_JOB)
                .arg_param(&["id"])
                .completion_cb("id", config::tape_job::complete_tape_job_id)
                .completion_cb("schedule", config::datastore::complete_calendar_event)
                .completion_cb("store", config::datastore::complete_datastore_name)
                .completion_cb("pool", config::media_pool::complete_pool_name)
                .completion_cb("drive", crate::complete_drive_name)
        )
        .insert("remove",
                CliCommand::new(&api2::config::tape_backup_job::API_METHOD_DELETE_TAPE_BACKUP_JOB)
                .arg_param(&["id"])
                .completion_cb("id", config::tape_job::complete_tape_job_id)
        );

    cmd_def.into()
}
