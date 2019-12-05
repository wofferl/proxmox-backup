use failure::*;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use chrono::{DateTime, Datelike, Local};

use super::{BackupDir, BackupInfo};

enum PruneMark { Keep, KeepPartial, Remove }

fn mark_selections<F: Fn(DateTime<Local>, &BackupInfo) -> String> (
    mark: &mut HashMap<PathBuf, PruneMark>,
    list: &Vec<BackupInfo>,
    keep: usize,
    select_id: F,
) {

    let mut include_hash = HashSet::new();

    let mut already_included = HashSet::new();
    for info in list {
        let backup_id = info.backup_dir.relative_path();
        if let Some(PruneMark::Keep) = mark.get(&backup_id) {
            let local_time = info.backup_dir.backup_time().with_timezone(&Local);
            let sel_id: String = select_id(local_time, &info);
            already_included.insert(sel_id);
        }
    }

    for info in list {
        let backup_id = info.backup_dir.relative_path();
        if let Some(_) = mark.get(&backup_id) { continue; }
        let local_time = info.backup_dir.backup_time().with_timezone(&Local);
        let sel_id: String = select_id(local_time, &info);

        if already_included.contains(&sel_id) { continue; }

        if !include_hash.contains(&sel_id) {
            if include_hash.len() >= keep { break; }
            include_hash.insert(sel_id);
            mark.insert(backup_id, PruneMark::Keep);
        } else {
            mark.insert(backup_id, PruneMark::Remove);
        }
    }
}

fn remove_incomplete_snapshots(
    mark: &mut HashMap<PathBuf, PruneMark>,
    list: &Vec<BackupInfo>,
) {

    let mut keep_unfinished = true;
    for info in list.iter() {
        // backup is considered unfinished if there is no manifest
        if info.files.iter().any(|name| name == super::MANIFEST_BLOB_NAME) {
            // There is a new finished backup, so there is no need
            // to keep older unfinished backups.
            keep_unfinished = false;
        } else {
            let backup_id = info.backup_dir.relative_path();
            if keep_unfinished { // keep first unfinished
                mark.insert(backup_id, PruneMark::KeepPartial);
            } else {
                mark.insert(backup_id, PruneMark::Remove);
            }
            keep_unfinished = false;
        }
    }
}

pub struct PruneOptions {
    pub keep_last: Option<u64>,
    pub keep_daily: Option<u64>,
    pub keep_weekly: Option<u64>,
    pub keep_monthly: Option<u64>,
    pub keep_yearly: Option<u64>,
}

impl PruneOptions {

    pub fn new() -> Self {
        Self {
            keep_last: None,
            keep_daily: None,
            keep_weekly: None,
            keep_monthly: None,
            keep_yearly: None,
        }
    }

    pub fn keep_last(mut self, value: Option<u64>) -> Self {
        self.keep_last = value;
        self
    }

    pub fn keep_daily(mut self, value: Option<u64>) -> Self {
        self.keep_daily = value;
        self
    }

    pub fn keep_weekly(mut self, value: Option<u64>) -> Self {
        self.keep_weekly = value;
        self
    }

    pub fn keep_monthly(mut self, value: Option<u64>) -> Self {
        self.keep_monthly = value;
        self
    }

    pub fn keep_yearly(mut self, value: Option<u64>) -> Self {
        self.keep_yearly = value;
        self
    }
}

pub fn compute_prune_info(
    mut list: Vec<BackupInfo>,
    options: &PruneOptions,
) -> Result<Vec<(BackupInfo, bool)>, Error> {

    let mut mark = HashMap::new();

    BackupInfo::sort_list(&mut list, false);

    remove_incomplete_snapshots(&mut mark, &list);

    if let Some(keep_last) = options.keep_last {
        mark_selections(&mut mark, &list, keep_last as usize, |_local_time, info| {
            BackupDir::backup_time_to_string(info.backup_dir.backup_time())
        });
    }

    if let Some(keep_daily) = options.keep_daily {
        mark_selections(&mut mark, &list, keep_daily as usize, |local_time, _info| {
            format!("{}/{}/{}", local_time.year(), local_time.month(), local_time.day())
        });
    }

    if let Some(keep_weekly) = options.keep_weekly {
        mark_selections(&mut mark, &list, keep_weekly as usize, |local_time, _info| {
            format!("{}/{}", local_time.year(), local_time.iso_week().week())
        });
    }

    if let Some(keep_monthly) = options.keep_monthly {
        mark_selections(&mut mark, &list, keep_monthly as usize, |local_time, _info| {
            format!("{}/{}", local_time.year(), local_time.month())
        });
    }

    if let Some(keep_yearly) = options.keep_yearly {
        mark_selections(&mut mark, &list, keep_yearly as usize, |local_time, _info| {
            format!("{}/{}", local_time.year(), local_time.year())
        });
    }

    let prune_info: Vec<(BackupInfo, bool)> = list.into_iter()
        .map(|info| {
            let backup_id = info.backup_dir.relative_path();
            let keep = match mark.get(&backup_id) {
                Some(PruneMark::Keep) => true,
                Some(PruneMark::KeepPartial) => true,
               _ => false,
            };
            (info, keep)
        })
        .collect();

    Ok(prune_info)
}