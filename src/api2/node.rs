//! Server/Node Configuration and Administration

use std::net::TcpListener;
use std::os::unix::io::AsRawFd;

use anyhow::{bail, format_err, Error};
use futures::future::{FutureExt, TryFutureExt};
use hyper::body::Body;
use hyper::http::request::Parts;
use hyper::upgrade::Upgraded;
use hyper::Request;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, BufReader};

use proxmox::api::router::{Router, SubdirMap};
use proxmox::api::{
    api, schema::*, ApiHandler, ApiMethod, ApiResponseFuture, Permission, RpcEnvironment,
};
use proxmox::list_subdirs_api_method;
use proxmox_http::websocket::WebSocket;
use proxmox::{identity, sortable};

use crate::api2::types::*;
use crate::config::acl::PRIV_SYS_CONSOLE;
use crate::server::WorkerTask;
use crate::tools;
use crate::tools::ticket::{self, Empty, Ticket};

pub mod apt;
pub mod certificates;
pub mod config;
pub mod disks;
pub mod dns;
pub mod network;
pub mod tasks;
pub mod subscription;

pub(crate) mod rrd;

mod journal;
pub(crate) mod services;
mod status;
mod syslog;
mod time;
mod report;

pub const SHELL_CMD_SCHEMA: Schema = StringSchema::new("The command to run.")
    .format(&ApiStringFormat::Enum(&[
        EnumEntry::new("login", "Login"),
        EnumEntry::new("upgrade", "Upgrade"),
    ]))
    .schema();

#[api(
    protected: true,
    input: {
        properties: {
            node: {
                schema: NODE_SCHEMA,
            },
            cmd: {
                schema: SHELL_CMD_SCHEMA,
                optional: true,
            },
        },
    },
    returns: {
        type: Object,
        description: "Object with the user, ticket, port and upid",
        properties: {
            user: {
                description: "",
                type: String,
            },
            ticket: {
                description: "",
                type: String,
            },
            port: {
                description: "",
                type: String,
            },
            upid: {
                description: "",
                type: String,
            },
        }
    },
    access: {
        description: "Restricted to users on realm 'pam'",
        permission: &Permission::Privilege(&["system"], PRIV_SYS_CONSOLE, false),
    }
)]
/// Call termproxy and return shell ticket
async fn termproxy(
    cmd: Option<String>,
    rpcenv: &mut dyn RpcEnvironment,
) -> Result<Value, Error> {
    // intentionally user only for now
    let auth_id: Authid = rpcenv
        .get_auth_id()
        .ok_or_else(|| format_err!("no authid available"))?
        .parse()?;

    if auth_id.is_token() {
        bail!("API tokens cannot access this API endpoint");
    }

    let userid = auth_id.user();

    if userid.realm() != "pam" {
        bail!("only pam users can use the console");
    }

    let path = "/system";

    // use port 0 and let the kernel decide which port is free
    let listener = TcpListener::bind("localhost:0")?;
    let port = listener.local_addr()?.port();

    let ticket = Ticket::new(ticket::TERM_PREFIX, &Empty)?
        .sign(
            crate::auth_helpers::private_auth_key(),
            Some(&ticket::term_aad(&userid, &path, port)),
        )?;

    let mut command = Vec::new();
    match cmd.as_deref() {
        Some("login") | None => {
            command.push("login");
            if userid == "root@pam" {
                command.push("-f");
                command.push("root");
            }
        }
        Some("upgrade") => {
            if userid != "root@pam" {
                bail!("only root@pam can upgrade");
            }
            // TODO: add nicer/safer wrapper like in PVE instead
            command.push("sh");
            command.push("-c");
            command.push("apt full-upgrade; bash -l");
        }
        _ => bail!("invalid command"),
    };

    let username = userid.name().to_owned();
    let upid = WorkerTask::spawn(
        "termproxy",
        None,
        auth_id,
        false,
        move |worker| async move {
            // move inside the worker so that it survives and does not close the port
            // remove CLOEXEC from listenere so that we can reuse it in termproxy
            tools::fd_change_cloexec(listener.as_raw_fd(), false)?;

            let mut arguments: Vec<&str> = Vec::new();
            let fd_string = listener.as_raw_fd().to_string();
            arguments.push(&fd_string);
            arguments.extend_from_slice(&[
                "--path",
                &path,
                "--perm",
                "Sys.Console",
                "--authport",
                "82",
                "--port-as-fd",
                "--",
            ]);
            arguments.extend_from_slice(&command);

            let mut cmd = tokio::process::Command::new("/usr/bin/termproxy");

            cmd.args(&arguments)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped());

            let mut child = cmd.spawn().expect("error executing termproxy");

            let stdout = child.stdout.take().expect("no child stdout handle");
            let stderr = child.stderr.take().expect("no child stderr handle");

            let worker_stdout = worker.clone();
            let stdout_fut = async move {
                let mut reader = BufReader::new(stdout).lines();
                while let Some(line) = reader.next_line().await? {
                    worker_stdout.log(line);
                }
                Ok::<(), Error>(())
            };

            let worker_stderr = worker.clone();
            let stderr_fut = async move {
                let mut reader = BufReader::new(stderr).lines();
                while let Some(line) = reader.next_line().await? {
                    worker_stderr.warn(line);
                }
                Ok::<(), Error>(())
            };

            let mut needs_kill = false;
            let res = tokio::select!{
                res = child.wait() => {
                    let exit_code = res?;
                    if !exit_code.success() {
                        match exit_code.code() {
                            Some(code) => bail!("termproxy exited with {}", code),
                            None => bail!("termproxy exited by signal"),
                        }
                    }
                    Ok(())
                },
                res = stdout_fut => res,
                res = stderr_fut => res,
                res = worker.abort_future() => {
                    needs_kill = true;
                    res.map_err(Error::from)
                }
            };

            if needs_kill {
                if res.is_ok() {
                    child.kill().await?;
                    return Ok(());
                }

                if let Err(err) = child.kill().await {
                    worker.warn(format!("error killing termproxy: {}", err));
                } else if let Err(err) = child.wait().await {
                    worker.warn(format!("error awaiting termproxy: {}", err));
                }
            }

            res
        },
    )?;

    // FIXME: We're returning the user NAME only?
    Ok(json!({
        "user": username,
        "ticket": ticket,
        "port": port,
        "upid": upid,
    }))
}

#[sortable]
pub const API_METHOD_WEBSOCKET: ApiMethod = ApiMethod::new(
    &ApiHandler::AsyncHttp(&upgrade_to_websocket),
    &ObjectSchema::new(
        "Upgraded to websocket",
        &sorted!([
            ("node", false, &NODE_SCHEMA),
            (
                "vncticket",
                false,
                &StringSchema::new("Terminal ticket").schema()
            ),
            ("port", false, &IntegerSchema::new("Terminal port").schema()),
        ]),
    ),
)
.access(
    Some("The user needs Sys.Console on /system."),
    &Permission::Privilege(&["system"], PRIV_SYS_CONSOLE, false),
);

fn upgrade_to_websocket(
    parts: Parts,
    req_body: Body,
    param: Value,
    _info: &ApiMethod,
    rpcenv: Box<dyn RpcEnvironment>,
) -> ApiResponseFuture {
    async move {
        // intentionally user only for now
        let auth_id: Authid = rpcenv
            .get_auth_id()
            .ok_or_else(|| format_err!("no authid available"))?
            .parse()?;

        if auth_id.is_token() {
            bail!("API tokens cannot access this API endpoint");
        }

        let userid = auth_id.user();
        let ticket = tools::required_string_param(&param, "vncticket")?;
        let port: u16 = tools::required_integer_param(&param, "port")? as u16;

        // will be checked again by termproxy
        Ticket::<Empty>::parse(ticket)?
            .verify(
                crate::auth_helpers::public_auth_key(),
                ticket::TERM_PREFIX,
                Some(&ticket::term_aad(&userid, "/system", port)),
            )?;

        let (ws, response) = WebSocket::new(parts.headers.clone())?;

        crate::server::spawn_internal_task(async move {
            let conn: Upgraded = match hyper::upgrade::on(Request::from_parts(parts, req_body)).map_err(Error::from).await {
                Ok(upgraded) => upgraded,
                _ => bail!("error"),
            };

            let local = tokio::net::TcpStream::connect(format!("localhost:{}", port)).await?;
            ws.serve_connection(conn, local).await
        });

        Ok(response)
    }
    .boxed()
}

pub const SUBDIRS: SubdirMap = &[
    ("apt", &apt::ROUTER),
    ("certificates", &certificates::ROUTER),
    ("config", &config::ROUTER),
    ("disks", &disks::ROUTER),
    ("dns", &dns::ROUTER),
    ("journal", &journal::ROUTER),
    ("network", &network::ROUTER),
    ("report", &report::ROUTER),
    ("rrd", &rrd::ROUTER),
    ("services", &services::ROUTER),
    ("status", &status::ROUTER),
    ("subscription", &subscription::ROUTER),
    ("syslog", &syslog::ROUTER),
    ("tasks", &tasks::ROUTER),
    ("termproxy", &Router::new().post(&API_METHOD_TERMPROXY)),
    ("time", &time::ROUTER),
    (
        "vncwebsocket",
        &Router::new().upgrade(&API_METHOD_WEBSOCKET),
    ),
];

pub const ROUTER: Router = Router::new()
    .get(&list_subdirs_api_method!(SUBDIRS))
    .subdirs(SUBDIRS);
