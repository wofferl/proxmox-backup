use anyhow::Error;
use serde_json::Value;

use proxmox::api::{api, cli::*, RpcEnvironment, ApiHandler};

use proxmox_backup::config;
use proxmox_backup::api2::{self, types::* };
use proxmox_backup::client::{
    connect_to_localhost,
    view_task_result,
};
use proxmox_backup::config::datastore::DIR_NAME_SCHEMA;

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
/// Datastore list.
fn list_datastores(param: Value, rpcenv: &mut dyn RpcEnvironment) -> Result<Value, Error> {

    let output_format = get_output_format(&param);

    let info = &api2::config::datastore::API_METHOD_LIST_DATASTORES;
    let mut data = match info.handler {
        ApiHandler::Sync(handler) => (handler)(param, info, rpcenv)?,
        _ => unreachable!(),
    };

    let options = default_table_format_options()
        .column(ColumnConfig::new("name"))
        .column(ColumnConfig::new("path"))
        .column(ColumnConfig::new("comment"));

    format_and_print_result_full(&mut data, &info.returns, &output_format, &options);

    Ok(Value::Null)
}

#[api(
    input: {
        properties: {
            name: {
                schema: DATASTORE_SCHEMA,
            },
            "output-format": {
                schema: OUTPUT_FORMAT,
                optional: true,
            },
        }
    }
)]
/// Show datastore configuration
fn show_datastore(param: Value, rpcenv: &mut dyn RpcEnvironment) -> Result<Value, Error> {

    let output_format = get_output_format(&param);

    let info = &api2::config::datastore::API_METHOD_READ_DATASTORE;
    let mut data = match info.handler {
        ApiHandler::Sync(handler) => (handler)(param, info, rpcenv)?,
        _ => unreachable!(),
    };

    let options = default_table_format_options();
    format_and_print_result_full(&mut data, &info.returns, &output_format, &options);

    Ok(Value::Null)
}

#[api(
    protected: true,
    input: {
        properties: {
            name: {
                schema: DATASTORE_SCHEMA,
            },
            path: {
                schema: DIR_NAME_SCHEMA,
            },
            comment: {
                optional: true,
                schema: SINGLE_LINE_COMMENT_SCHEMA,
            },
            "notify-user": {
                optional: true,
                type: Userid,
            },
            "notify": {
                optional: true,
                schema: DATASTORE_NOTIFY_STRING_SCHEMA,
            },
            "gc-schedule": {
                optional: true,
                schema: GC_SCHEDULE_SCHEMA,
            },
            "prune-schedule": {
                optional: true,
                schema: PRUNE_SCHEDULE_SCHEMA,
            },
            "keep-last": {
                optional: true,
                schema: PRUNE_SCHEMA_KEEP_LAST,
            },
            "keep-hourly": {
                optional: true,
                schema: PRUNE_SCHEMA_KEEP_HOURLY,
            },
            "keep-daily": {
                optional: true,
                schema: PRUNE_SCHEMA_KEEP_DAILY,
            },
            "keep-weekly": {
                optional: true,
                schema: PRUNE_SCHEMA_KEEP_WEEKLY,
            },
            "keep-monthly": {
                optional: true,
                schema: PRUNE_SCHEMA_KEEP_MONTHLY,
            },
            "keep-yearly": {
                optional: true,
                schema: PRUNE_SCHEMA_KEEP_YEARLY,
            },
            "output-format": {
                schema: OUTPUT_FORMAT,
                optional: true,
            },
        },
    },
)]
/// Create new datastore config.
async fn create_datastore(mut param: Value) -> Result<Value, Error> {

    let output_format = extract_output_format(&mut param);

    let mut client = connect_to_localhost()?;

    let result = client.post(&"api2/json/config/datastore", Some(param)).await?;

    view_task_result(&mut client, result, &output_format).await?;

    Ok(Value::Null)
}

pub fn datastore_commands() -> CommandLineInterface {

    let cmd_def = CliCommandMap::new()
        .insert("list", CliCommand::new(&API_METHOD_LIST_DATASTORES))
        .insert("show",
                CliCommand::new(&API_METHOD_SHOW_DATASTORE)
                .arg_param(&["name"])
                .completion_cb("name", config::datastore::complete_datastore_name)
        )
        .insert("create",
                CliCommand::new(&API_METHOD_CREATE_DATASTORE)
                .arg_param(&["name", "path"])
        )
        .insert("update",
                CliCommand::new(&api2::config::datastore::API_METHOD_UPDATE_DATASTORE)
                .arg_param(&["name"])
                .completion_cb("name", config::datastore::complete_datastore_name)
                .completion_cb("gc-schedule", config::datastore::complete_calendar_event)
                .completion_cb("prune-schedule", config::datastore::complete_calendar_event)
        )
        .insert("remove",
                CliCommand::new(&api2::config::datastore::API_METHOD_DELETE_DATASTORE)
                .arg_param(&["name"])
                .completion_cb("name", config::datastore::complete_datastore_name)
        );

    cmd_def.into()
}
