use anyhow::{Error};

use proxmox_backup::api2::types::Authid;
use proxmox_backup::client::*;

async fn upload_speed() -> Result<f64, Error> {

    let host = "localhost";
    let datastore = "store2";

    let auth_id = Authid::root_auth_id();

    let options = HttpClientOptions::default()
        .interactive(true)
        .ticket_cache(true);

    let client = HttpClient::new(host, 8007, auth_id, options)?;

    let backup_time = proxmox::tools::time::epoch_i64();

    let client = BackupWriter::start(client, None, datastore, "host", "speedtest", backup_time, false, true).await?;

    println!("start upload speed test");
    let res = client.upload_speedtest(true).await?;

    Ok(res)
}

fn main()  {
    match proxmox_backup::tools::runtime::main(upload_speed()) {
        Ok(mbs) => {
            println!("average upload speed: {} MB/s", mbs);
        }
        Err(err) => {
            eprintln!("ERROR: {}", err);
        }
    }
}
