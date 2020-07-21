use apt_pkg_native::Cache;
use anyhow::{Error, bail};
use serde_json::{json, Value};

use proxmox::{list_subdirs_api_method, const_regex};
use proxmox::api::{api, Router, Permission, SubdirMap};

use crate::config::acl::PRIV_SYS_AUDIT;
use crate::api2::types::{APTUpdateInfo, NODE_SCHEMA};

const_regex! {
    VERSION_EPOCH_REGEX = r"^\d+:";
    FILENAME_EXTRACT_REGEX = r"^.*/.*?_(.*)_Packages$";
}

// FIXME: Replace with call to 'apt changelog <pkg> --print-uris'. Currently
// not possible as our packages do not have a URI set in their Release file
fn get_changelog_url(
    package: &str,
    filename: &str,
    source_pkg: &str,
    version: &str,
    source_version: &str,
    origin: &str,
    component: &str,
) -> Result<String, Error> {
    if origin == "" {
        bail!("no origin available for package {}", package);
    }

    if origin == "Debian" {
        let source_version = (VERSION_EPOCH_REGEX.regex_obj)().replace_all(source_version, "");

        let prefix = if source_pkg.starts_with("lib") {
            source_pkg.get(0..4)
        } else {
            source_pkg.get(0..1)
        };

        let prefix = match prefix {
            Some(p) => p,
            None => bail!("cannot get starting characters of package name '{}'", package)
        };

        // note: security updates seem to not always upload a changelog for
        // their package version, so this only works *most* of the time
        return Ok(format!("https://metadata.ftp-master.debian.org/changelogs/main/{}/{}/{}_{}_changelog",
                          prefix, source_pkg, source_pkg, source_version));

    } else if origin == "Proxmox" {
        let version = (VERSION_EPOCH_REGEX.regex_obj)().replace_all(version, "");

        let base = match (FILENAME_EXTRACT_REGEX.regex_obj)().captures(filename) {
            Some(captures) => {
                let base_capture = captures.get(1);
                match base_capture {
                    Some(base_underscore) => base_underscore.as_str().replace("_", "/"),
                    None => bail!("incompatible filename, cannot find regex group")
                }
            },
            None => bail!("incompatible filename, doesn't match regex")
        };

        return Ok(format!("http://download.proxmox.com/{}/{}_{}.changelog",
                          base, package, version));
    }

    bail!("unknown origin ({}) or component ({})", origin, component)
}

fn list_installed_apt_packages<F: Fn(&str, &str, &str) -> bool>(filter: F)
    -> Vec<APTUpdateInfo> {

    let mut ret = Vec::new();

    // note: this is not an 'apt update', it just re-reads the cache from disk
    let mut cache = Cache::get_singleton();
    cache.reload();

    let mut cache_iter = cache.iter();

    loop {
        let view = match cache_iter.next() {
            Some(view) => view,
            None => break
        };

        let current_version = match view.current_version() {
            Some(vers) => vers,
            None => continue
        };
        let candidate_version = match view.candidate_version() {
            Some(vers) => vers,
            // if there's no candidate (i.e. no update) get info of currently
            // installed version instead
            None => current_version.clone()
        };

        let package = view.name();
        if filter(&package, &current_version, &candidate_version) {
            let mut origin_res = "unknown".to_owned();
            let mut section_res = "unknown".to_owned();
            let mut priority_res = "unknown".to_owned();
            let mut change_log_url = "".to_owned();
            let mut short_desc = package.clone();
            let mut long_desc = "".to_owned();

            // get additional information via nested APT 'iterators'
            let mut view_iter = view.versions();
            while let Some(ver) = view_iter.next() {
                if ver.version() == candidate_version {
                    if let Some(section) = ver.section() {
                        section_res = section;
                    }

                    if let Some(prio) = ver.priority_type() {
                        priority_res = prio;
                    }

                    // assume every package has only one origin file (not
                    // origin, but origin *file*, for some reason those seem to
                    // be different concepts in APT)
                    let mut origin_iter = ver.origin_iter();
                    let origin = origin_iter.next();
                    if let Some(origin) = origin {

                        if let Some(sd) = origin.short_desc() {
                            short_desc = sd;
                        }

                        if let Some(ld) = origin.long_desc() {
                            long_desc = ld;
                        }

                        // the package files appear in priority order, meaning
                        // the one for the candidate version is first
                        let mut pkg_iter = origin.file();
                        let pkg_file = pkg_iter.next();
                        if let Some(pkg_file) = pkg_file {
                            if let Some(origin_name) = pkg_file.origin() {
                                origin_res = origin_name;
                            }

                            let filename = pkg_file.file_name();
                            let source_pkg = ver.source_package();
                            let source_ver = ver.source_version();
                            let component = pkg_file.component();

                            // build changelog URL from gathered information
                            // ignore errors, use empty changelog instead
                            let url = get_changelog_url(&package, &filename, &source_pkg,
                                &candidate_version, &source_ver, &origin_res, &component);
                            if let Ok(url) = url {
                                change_log_url = url;
                            }
                        }
                    }

                    break;
                }
            }

            let info = APTUpdateInfo {
                package,
                title: short_desc,
                arch: view.arch(),
                description: long_desc,
                change_log_url,
                origin: origin_res,
                version: candidate_version,
                old_version: current_version,
                priority: priority_res,
                section: section_res,
            };
            ret.push(info);
        }
    }

    return ret;
}

#[api(
    input: {
        properties: {
            node: {
                schema: NODE_SCHEMA,
            },
        },
    },
    returns: {
        description: "A list of packages with available updates.",
        type: Array,
        items: { type: APTUpdateInfo },
    },
    access: {
        permission: &Permission::Privilege(&[], PRIV_SYS_AUDIT, false),
    },
)]
/// List available APT updates
fn apt_update_available(_param: Value) -> Result<Value, Error> {
    let ret = list_installed_apt_packages(|_pkg, cur_ver, can_ver| cur_ver != can_ver);
    Ok(json!(ret))
}

const SUBDIRS: SubdirMap = &[
    ("update", &Router::new().get(&API_METHOD_APT_UPDATE_AVAILABLE)),
];

pub const ROUTER: Router = Router::new()
    .get(&list_subdirs_api_method!(SUBDIRS))
    .subdirs(SUBDIRS);