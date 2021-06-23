//! Sync datastore from remote server

use anyhow::{bail, format_err, Error};
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::convert::TryFrom;
use std::io::{Seek, SeekFrom};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use crate::{
    api2::types::*,
    backup::*,
    client::*,
    server::WorkerTask,
    task_log,
    tools::{compute_file_csum, ParallelHandler},
};
use proxmox::api::error::{HttpError, StatusCode};

// fixme: implement filters
// fixme: delete vanished groups
// Todo: correctly lock backup groups

async fn pull_index_chunks<I: IndexFile>(
    worker: &WorkerTask,
    chunk_reader: RemoteChunkReader,
    target: Arc<DataStore>,
    index: I,
    downloaded_chunks: Arc<Mutex<HashSet<[u8; 32]>>>,
) -> Result<(), Error> {
    use futures::stream::{self, StreamExt, TryStreamExt};

    let start_time = SystemTime::now();

    let stream = stream::iter(
        (0..index.index_count())
            .map(|pos| index.chunk_info(pos).unwrap())
            .filter(|info| {
                let mut guard = downloaded_chunks.lock().unwrap();
                let done = guard.contains(&info.digest);
                if !done {
                    // Note: We mark a chunk as downloaded before its actually downloaded
                    // to avoid duplicate downloads.
                    guard.insert(info.digest);
                }
                !done
            }),
    );

    let target2 = target.clone();
    let verify_pool = ParallelHandler::new(
        "sync chunk writer",
        4,
        move |(chunk, digest, size): (DataBlob, [u8; 32], u64)| {
            // println!("verify and write {}", proxmox::tools::digest_to_hex(&digest));
            chunk.verify_unencrypted(size as usize, &digest)?;
            target2.insert_chunk(&chunk, &digest)?;
            Ok(())
        },
    );

    let verify_and_write_channel = verify_pool.channel();

    let bytes = Arc::new(AtomicUsize::new(0));

    stream
        .map(|info| {
            let target = Arc::clone(&target);
            let chunk_reader = chunk_reader.clone();
            let bytes = Arc::clone(&bytes);
            let verify_and_write_channel = verify_and_write_channel.clone();

            Ok::<_, Error>(async move {
                let chunk_exists = crate::tools::runtime::block_in_place(|| {
                    target.cond_touch_chunk(&info.digest, false)
                })?;
                if chunk_exists {
                    //worker.log(format!("chunk {} exists {}", pos, proxmox::tools::digest_to_hex(digest)));
                    return Ok::<_, Error>(());
                }
                //worker.log(format!("sync {} chunk {}", pos, proxmox::tools::digest_to_hex(digest)));
                let chunk = chunk_reader.read_raw_chunk(&info.digest).await?;
                let raw_size = chunk.raw_size() as usize;

                // decode, verify and write in a separate threads to maximize throughput
                crate::tools::runtime::block_in_place(|| {
                    verify_and_write_channel.send((chunk, info.digest, info.size()))
                })?;

                bytes.fetch_add(raw_size, Ordering::SeqCst);

                Ok(())
            })
        })
        .try_buffer_unordered(20)
        .try_for_each(|_res| futures::future::ok(()))
        .await?;

    drop(verify_and_write_channel);

    verify_pool.complete()?;

    let elapsed = start_time.elapsed()?.as_secs_f64();

    let bytes = bytes.load(Ordering::SeqCst);

    worker.log(format!(
        "downloaded {} bytes ({:.2} MiB/s)",
        bytes,
        (bytes as f64) / (1024.0 * 1024.0 * elapsed)
    ));

    Ok(())
}

async fn download_manifest(
    reader: &BackupReader,
    filename: &std::path::Path,
) -> Result<std::fs::File, Error> {
    let mut tmp_manifest_file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .read(true)
        .open(&filename)?;

    reader
        .download(MANIFEST_BLOB_NAME, &mut tmp_manifest_file)
        .await?;

    tmp_manifest_file.seek(SeekFrom::Start(0))?;

    Ok(tmp_manifest_file)
}

fn verify_archive(info: &FileInfo, csum: &[u8; 32], size: u64) -> Result<(), Error> {
    if size != info.size {
        bail!(
            "wrong size for file '{}' ({} != {})",
            info.filename,
            info.size,
            size
        );
    }

    if csum != &info.csum {
        bail!("wrong checksum for file '{}'", info.filename);
    }

    Ok(())
}

async fn pull_single_archive(
    worker: &WorkerTask,
    reader: &BackupReader,
    chunk_reader: &mut RemoteChunkReader,
    tgt_store: Arc<DataStore>,
    snapshot: &BackupDir,
    archive_info: &FileInfo,
    downloaded_chunks: Arc<Mutex<HashSet<[u8; 32]>>>,
) -> Result<(), Error> {
    let archive_name = &archive_info.filename;
    let mut path = tgt_store.base_path();
    path.push(snapshot.relative_path());
    path.push(archive_name);

    let mut tmp_path = path.clone();
    tmp_path.set_extension("tmp");

    worker.log(format!("sync archive {}", archive_name));
    let mut tmpfile = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .read(true)
        .open(&tmp_path)?;

    reader.download(archive_name, &mut tmpfile).await?;

    match archive_type(archive_name)? {
        ArchiveType::DynamicIndex => {
            let index = DynamicIndexReader::new(tmpfile).map_err(|err| {
                format_err!("unable to read dynamic index {:?} - {}", tmp_path, err)
            })?;
            let (csum, size) = index.compute_csum();
            verify_archive(archive_info, &csum, size)?;

            pull_index_chunks(
                worker,
                chunk_reader.clone(),
                tgt_store.clone(),
                index,
                downloaded_chunks,
            )
            .await?;
        }
        ArchiveType::FixedIndex => {
            let index = FixedIndexReader::new(tmpfile).map_err(|err| {
                format_err!("unable to read fixed index '{:?}' - {}", tmp_path, err)
            })?;
            let (csum, size) = index.compute_csum();
            verify_archive(archive_info, &csum, size)?;

            pull_index_chunks(
                worker,
                chunk_reader.clone(),
                tgt_store.clone(),
                index,
                downloaded_chunks,
            )
            .await?;
        }
        ArchiveType::Blob => {
            let (csum, size) = compute_file_csum(&mut tmpfile)?;
            verify_archive(archive_info, &csum, size)?;
        }
    }
    if let Err(err) = std::fs::rename(&tmp_path, &path) {
        bail!("Atomic rename file {:?} failed - {}", path, err);
    }
    Ok(())
}

// Note: The client.log.blob is uploaded after the backup, so it is
// not mentioned in the manifest.
async fn try_client_log_download(
    worker: &WorkerTask,
    reader: Arc<BackupReader>,
    path: &std::path::Path,
) -> Result<(), Error> {
    let mut tmp_path = path.to_owned();
    tmp_path.set_extension("tmp");

    let tmpfile = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .read(true)
        .open(&tmp_path)?;

    // Note: be silent if there is no log - only log successful download
    if let Ok(()) = reader.download(CLIENT_LOG_BLOB_NAME, tmpfile).await {
        if let Err(err) = std::fs::rename(&tmp_path, &path) {
            bail!("Atomic rename file {:?} failed - {}", path, err);
        }
        worker.log(format!("got backup log file {:?}", CLIENT_LOG_BLOB_NAME));
    }

    Ok(())
}

async fn pull_snapshot(
    worker: &WorkerTask,
    reader: Arc<BackupReader>,
    tgt_store: Arc<DataStore>,
    snapshot: &BackupDir,
    downloaded_chunks: Arc<Mutex<HashSet<[u8; 32]>>>,
) -> Result<(), Error> {
    let mut manifest_name = tgt_store.base_path();
    manifest_name.push(snapshot.relative_path());
    manifest_name.push(MANIFEST_BLOB_NAME);

    let mut client_log_name = tgt_store.base_path();
    client_log_name.push(snapshot.relative_path());
    client_log_name.push(CLIENT_LOG_BLOB_NAME);

    let mut tmp_manifest_name = manifest_name.clone();
    tmp_manifest_name.set_extension("tmp");

    let download_res = download_manifest(&reader, &tmp_manifest_name).await;
    let mut tmp_manifest_file = match download_res {
        Ok(manifest_file) => manifest_file,
        Err(err) => {
            match err.downcast_ref::<HttpError>() {
                Some(HttpError { code, message }) => match *code {
                    StatusCode::NOT_FOUND => {
                        worker.log(format!(
                            "skipping snapshot {} - vanished since start of sync",
                            snapshot
                        ));
                        return Ok(());
                    }
                    _ => {
                        bail!("HTTP error {} - {}", code, message);
                    }
                },
                None => {
                    return Err(err);
                }
            };
        }
    };
    let tmp_manifest_blob = DataBlob::load_from_reader(&mut tmp_manifest_file)?;

    if manifest_name.exists() {
        let manifest_blob = proxmox::try_block!({
            let mut manifest_file = std::fs::File::open(&manifest_name).map_err(|err| {
                format_err!(
                    "unable to open local manifest {:?} - {}",
                    manifest_name,
                    err
                )
            })?;

            let manifest_blob = DataBlob::load_from_reader(&mut manifest_file)?;
            Ok(manifest_blob)
        })
        .map_err(|err: Error| {
            format_err!(
                "unable to read local manifest {:?} - {}",
                manifest_name,
                err
            )
        })?;

        if manifest_blob.raw_data() == tmp_manifest_blob.raw_data() {
            if !client_log_name.exists() {
                try_client_log_download(worker, reader, &client_log_name).await?;
            }
            worker.log("no data changes");
            let _ = std::fs::remove_file(&tmp_manifest_name);
            return Ok(()); // nothing changed
        }
    }

    let manifest = BackupManifest::try_from(tmp_manifest_blob)?;

    for item in manifest.files() {
        let mut path = tgt_store.base_path();
        path.push(snapshot.relative_path());
        path.push(&item.filename);

        if path.exists() {
            match archive_type(&item.filename)? {
                ArchiveType::DynamicIndex => {
                    let index = DynamicIndexReader::open(&path)?;
                    let (csum, size) = index.compute_csum();
                    match manifest.verify_file(&item.filename, &csum, size) {
                        Ok(_) => continue,
                        Err(err) => {
                            worker.log(format!("detected changed file {:?} - {}", path, err));
                        }
                    }
                }
                ArchiveType::FixedIndex => {
                    let index = FixedIndexReader::open(&path)?;
                    let (csum, size) = index.compute_csum();
                    match manifest.verify_file(&item.filename, &csum, size) {
                        Ok(_) => continue,
                        Err(err) => {
                            worker.log(format!("detected changed file {:?} - {}", path, err));
                        }
                    }
                }
                ArchiveType::Blob => {
                    let mut tmpfile = std::fs::File::open(&path)?;
                    let (csum, size) = compute_file_csum(&mut tmpfile)?;
                    match manifest.verify_file(&item.filename, &csum, size) {
                        Ok(_) => continue,
                        Err(err) => {
                            worker.log(format!("detected changed file {:?} - {}", path, err));
                        }
                    }
                }
            }
        }

        let mut chunk_reader = RemoteChunkReader::new(
            reader.clone(),
            None,
            item.chunk_crypt_mode(),
            HashMap::new(),
        );

        pull_single_archive(
            worker,
            &reader,
            &mut chunk_reader,
            tgt_store.clone(),
            snapshot,
            &item,
            downloaded_chunks.clone(),
        )
        .await?;
    }

    if let Err(err) = std::fs::rename(&tmp_manifest_name, &manifest_name) {
        bail!("Atomic rename file {:?} failed - {}", manifest_name, err);
    }

    if !client_log_name.exists() {
        try_client_log_download(worker, reader, &client_log_name).await?;
    }

    // cleanup - remove stale files
    tgt_store.cleanup_backup_dir(snapshot, &manifest)?;

    Ok(())
}

pub async fn pull_snapshot_from(
    worker: &WorkerTask,
    reader: Arc<BackupReader>,
    tgt_store: Arc<DataStore>,
    snapshot: &BackupDir,
    downloaded_chunks: Arc<Mutex<HashSet<[u8; 32]>>>,
) -> Result<(), Error> {
    let (_path, is_new, _snap_lock) = tgt_store.create_locked_backup_dir(&snapshot)?;

    if is_new {
        worker.log(format!("sync snapshot {:?}", snapshot.relative_path()));

        if let Err(err) = pull_snapshot(
            worker,
            reader,
            tgt_store.clone(),
            &snapshot,
            downloaded_chunks,
        )
        .await
        {
            if let Err(cleanup_err) = tgt_store.remove_backup_dir(&snapshot, true) {
                worker.log(format!("cleanup error - {}", cleanup_err));
            }
            return Err(err);
        }
        worker.log(format!("sync snapshot {:?} done", snapshot.relative_path()));
    } else {
        worker.log(format!("re-sync snapshot {:?}", snapshot.relative_path()));
        pull_snapshot(
            worker,
            reader,
            tgt_store.clone(),
            &snapshot,
            downloaded_chunks,
        )
        .await?;
        worker.log(format!(
            "re-sync snapshot {:?} done",
            snapshot.relative_path()
        ));
    }

    Ok(())
}

struct SkipInfo {
    oldest: i64,
    newest: i64,
    count: u64,
}

impl SkipInfo {
    fn update(&mut self, backup_time: i64) {
        self.count += 1;

        if backup_time < self.oldest {
            self.oldest = backup_time;
        }

        if backup_time > self.newest {
            self.newest = backup_time;
        }
    }

    fn affected(&self) -> Result<String, Error> {
        match self.count {
            0 => Ok(String::new()),
            1 => proxmox::tools::time::epoch_to_rfc3339_utc(self.oldest),
            _ => {
                Ok(format!(
                    "{} .. {}",
                    proxmox::tools::time::epoch_to_rfc3339_utc(self.oldest)?,
                    proxmox::tools::time::epoch_to_rfc3339_utc(self.newest)?,
                ))
            }
        }
    }
}

impl std::fmt::Display for SkipInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "skipped: {} snapshot(s) ({}) older than the newest local snapshot",
            self.count,
            self.affected().map_err(|_| std::fmt::Error)?
        )
    }
}

pub async fn pull_group(
    worker: &WorkerTask,
    client: &HttpClient,
    src_repo: &BackupRepository,
    tgt_store: Arc<DataStore>,
    group: &BackupGroup,
    delete: bool,
    progress: &mut StoreProgress,
) -> Result<(), Error> {
    let path = format!("api2/json/admin/datastore/{}/snapshots", src_repo.store());

    let args = json!({
        "backup-type": group.backup_type(),
        "backup-id": group.backup_id(),
    });

    let mut result = client.get(&path, Some(args)).await?;
    let mut list: Vec<SnapshotListItem> = serde_json::from_value(result["data"].take())?;

    list.sort_unstable_by(|a, b| a.backup_time.cmp(&b.backup_time));

    client.login().await?; // make sure auth is complete

    let fingerprint = client.fingerprint();

    let last_sync = tgt_store.last_successful_backup(group)?;

    let mut remote_snapshots = std::collections::HashSet::new();

    // start with 16384 chunks (up to 65GB)
    let downloaded_chunks = Arc::new(Mutex::new(HashSet::with_capacity(1024 * 64)));

    progress.group_snapshots = list.len() as u64;

    let mut skip_info = SkipInfo {
        oldest: i64::MAX,
        newest: i64::MIN,
        count: 0,
    };

    for (pos, item) in list.into_iter().enumerate() {
        let snapshot = BackupDir::new(item.backup_type, item.backup_id, item.backup_time)?;

        // in-progress backups can't be synced
        if item.size.is_none() {
            worker.log(format!(
                "skipping snapshot {} - in-progress backup",
                snapshot
            ));
            continue;
        }

        let backup_time = snapshot.backup_time();

        remote_snapshots.insert(backup_time);

        if let Some(last_sync_time) = last_sync {
            if last_sync_time > backup_time {
                skip_info.update(backup_time);
                continue;
            }
        }

        // get updated auth_info (new tickets)
        let auth_info = client.login().await?;

        let options = HttpClientOptions::new_non_interactive(auth_info.ticket.clone(), fingerprint.clone());

        let new_client = HttpClient::new(
            src_repo.host(),
            src_repo.port(),
            src_repo.auth_id(),
            options,
        )?;

        let reader = BackupReader::start(
            new_client,
            None,
            src_repo.store(),
            snapshot.group().backup_type(),
            snapshot.group().backup_id(),
            backup_time,
            true,
        )
        .await?;

        let result = pull_snapshot_from(
            worker,
            reader,
            tgt_store.clone(),
            &snapshot,
            downloaded_chunks.clone(),
        )
        .await;

        progress.done_snapshots = pos as u64 + 1;
        worker.log(format!("percentage done: {}", progress));

        result?; // stop on error
    }

    if delete {
        let local_list = group.list_backups(&tgt_store.base_path())?;
        for info in local_list {
            let backup_time = info.backup_dir.backup_time();
            if remote_snapshots.contains(&backup_time) {
                continue;
            }
            worker.log(format!(
                "delete vanished snapshot {:?}",
                info.backup_dir.relative_path()
            ));
            tgt_store.remove_backup_dir(&info.backup_dir, false)?;
        }
    }

    if skip_info.count > 0 {
        task_log!(worker, "{}", skip_info);
    }

    Ok(())
}

pub async fn pull_store(
    worker: &WorkerTask,
    client: &HttpClient,
    src_repo: &BackupRepository,
    tgt_store: Arc<DataStore>,
    delete: bool,
    auth_id: Authid,
) -> Result<(), Error> {
    // explicit create shared lock to prevent GC on newly created chunks
    let _shared_store_lock = tgt_store.try_shared_chunk_store_lock()?;

    let path = format!("api2/json/admin/datastore/{}/groups", src_repo.store());

    let mut result = client
        .get(&path, None)
        .await
        .map_err(|err| format_err!("Failed to retrieve backup groups from remote - {}", err))?;

    let mut list: Vec<GroupListItem> = serde_json::from_value(result["data"].take())?;

    worker.log(format!("found {} groups to sync", list.len()));

    list.sort_unstable_by(|a, b| {
        let type_order = a.backup_type.cmp(&b.backup_type);
        if type_order == std::cmp::Ordering::Equal {
            a.backup_id.cmp(&b.backup_id)
        } else {
            type_order
        }
    });

    let mut errors = false;

    let mut new_groups = std::collections::HashSet::new();
    for item in list.iter() {
        new_groups.insert(BackupGroup::new(&item.backup_type, &item.backup_id));
    }

    let mut progress = StoreProgress::new(list.len() as u64);

    for (done, item) in list.into_iter().enumerate() {
        progress.done_groups = done as u64;
        progress.done_snapshots = 0;
        progress.group_snapshots = 0;

        let group = BackupGroup::new(&item.backup_type, &item.backup_id);

        let (owner, _lock_guard) = match tgt_store.create_locked_backup_group(&group, &auth_id) {
            Ok(result) => result,
            Err(err) => {
                worker.log(format!(
                    "sync group {}/{} failed - group lock failed: {}",
                    item.backup_type, item.backup_id, err
                ));
                errors = true; // do not stop here, instead continue
                continue;
            }
        };

        // permission check
        if auth_id != owner {
            // only the owner is allowed to create additional snapshots
            worker.log(format!(
                "sync group {}/{} failed - owner check failed ({} != {})",
                item.backup_type, item.backup_id, auth_id, owner
            ));
            errors = true; // do not stop here, instead continue
        } else if let Err(err) = pull_group(
            worker,
            client,
            src_repo,
            tgt_store.clone(),
            &group,
            delete,
            &mut progress,
        )
        .await
        {
            worker.log(format!(
                "sync group {}/{} failed - {}",
                item.backup_type, item.backup_id, err,
            ));
            errors = true; // do not stop here, instead continue
        }
    }

    if delete {
        let result: Result<(), Error> = proxmox::try_block!({
            let local_groups = BackupInfo::list_backup_groups(&tgt_store.base_path())?;
            for local_group in local_groups {
                if new_groups.contains(&local_group) {
                    continue;
                }
                worker.log(format!(
                    "delete vanished group '{}/{}'",
                    local_group.backup_type(),
                    local_group.backup_id()
                ));
                if let Err(err) = tgt_store.remove_backup_group(&local_group) {
                    worker.log(err.to_string());
                    errors = true;
                }
            }
            Ok(())
        });
        if let Err(err) = result {
            worker.log(format!("error during cleanup: {}", err));
            errors = true;
        };
    }

    if errors {
        bail!("sync failed with some errors.");
    }

    Ok(())
}
