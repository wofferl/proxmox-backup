//! *pxar* format encoder.
//!
//! This module contain the code to generate *pxar* archive files.
use std::collections::{HashMap, HashSet};
use std::ffi::{CStr, CString};
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::io::AsRawFd;
use std::os::unix::io::RawFd;
use std::path::{Path, PathBuf};

use endian_trait::Endian;
use failure::*;
use nix::errno::Errno;
use nix::fcntl::OFlag;
use nix::sys::stat::FileStat;
use nix::sys::stat::Mode;
use nix::NixPath;

use proxmox::tools::vec;

use super::binary_search_tree::*;
use super::catalog::BackupCatalogWriter;
use super::flags;
use super::format_definition::*;
use super::helper::*;
use super::match_pattern::{MatchPattern, MatchPatternSlice, MatchType};
use crate::tools::acl;
use crate::tools::fs;
use crate::tools::xattr;

/// The format requires to build sorted directory lookup tables in
/// memory, so we restrict the number of allowed entries to limit
/// maximum memory usage.
pub const MAX_DIRECTORY_ENTRIES: usize = 256 * 1024;

#[derive(Eq, PartialEq, Hash)]
struct HardLinkInfo {
    st_dev: u64,
    st_ino: u64,
}

pub struct Encoder<'a, W: Write, C: BackupCatalogWriter> {
    base_path: PathBuf,
    relative_path: PathBuf,
    writer: &'a mut W,
    writer_pos: usize,
    catalog: Option<&'a mut C>,
    _size: usize,
    file_copy_buffer: Vec<u8>,
    device_set: Option<HashSet<u64>>,
    verbose: bool,
    // Flags set by the user
    feature_flags: u64,
    // Flags signaling features supported by the filesystem
    fs_feature_flags: u64,
    hardlinks: HashMap<HardLinkInfo, (PathBuf, u64)>,
}

impl<'a, W: Write, C: BackupCatalogWriter> Encoder<'a, W, C> {
    // used for error reporting
    fn full_path(&self) -> PathBuf {
        self.base_path.join(&self.relative_path)
    }

    /// Create archive, write result data to ``writer``.
    ///
    /// The ``device_set`` can be use used to limit included mount points.
    ///
    /// - ``None``: include all mount points
    /// - ``Some(set)``: only include devices listed in this set (the
    ///   root path device is automathically added to this list, so
    ///   you can pass an empty set if you want to archive a single
    ///   mount point.)
    pub fn encode(
        path: PathBuf,
        dir: &mut nix::dir::Dir,
        writer: &'a mut W,
        catalog: Option<&'a mut C>,
        device_set: Option<HashSet<u64>>,
        verbose: bool,
        skip_lost_and_found: bool, // fixme: should be a feature flag ??
        feature_flags: u64,
        mut excludes: Vec<MatchPattern>,
    ) -> Result<(), Error> {
        const FILE_COPY_BUFFER_SIZE: usize = 1024 * 1024;

        let mut file_copy_buffer = Vec::with_capacity(FILE_COPY_BUFFER_SIZE);
        unsafe {
            file_copy_buffer.set_len(FILE_COPY_BUFFER_SIZE);
        }

        // todo: use scandirat??

        let dir_fd = dir.as_raw_fd();
        let stat = nix::sys::stat::fstat(dir_fd)
            .map_err(|err| format_err!("fstat {:?} failed - {}", path, err))?;

        if !is_directory(&stat) {
            bail!("got unexpected file type {:?} (not a directory)", path);
        }

        let mut device_set = device_set.clone();
        if let Some(ref mut set) = device_set {
            set.insert(stat.st_dev);
        }

        let magic = detect_fs_type(dir_fd)?;

        if is_virtual_file_system(magic) {
            bail!("backup virtual file systems is disabled!");
        }

        let fs_feature_flags = flags::feature_flags_from_magic(magic);

        let mut me = Self {
            base_path: path,
            relative_path: PathBuf::new(),
            writer,
            writer_pos: 0,
            catalog,
            _size: 0,
            file_copy_buffer,
            device_set,
            verbose,
            feature_flags,
            fs_feature_flags,
            hardlinks: HashMap::new(),
        };

        if verbose {
            println!("{:?}", me.full_path());
        }

        if skip_lost_and_found {
            excludes.push(MatchPattern::from_line(b"**/lost+found").unwrap().unwrap());
        }
        let mut exclude_slices = Vec::new();
        for excl in &excludes {
            exclude_slices.push(excl.as_slice());
        }

        me.encode_dir(dir, &stat, magic, exclude_slices)?;

        Ok(())
    }

    fn write(&mut self, buf: &[u8]) -> Result<(), Error> {
        self.writer.write_all(buf)?;
        self.writer_pos += buf.len();
        Ok(())
    }

    fn write_item<T: Endian>(&mut self, item: T) -> Result<(), Error> {
        let data = item.to_le();

        let buffer = unsafe {
            std::slice::from_raw_parts(&data as *const T as *const u8, std::mem::size_of::<T>())
        };

        self.write(buffer)?;

        Ok(())
    }

    fn flush_copy_buffer(&mut self, size: usize) -> Result<(), Error> {
        self.writer.write_all(&self.file_copy_buffer[..size])?;
        self.writer_pos += size;
        Ok(())
    }

    fn write_header(&mut self, htype: u64, size: u64) -> Result<(), Error> {
        let size = size + (std::mem::size_of::<PxarHeader>() as u64);
        self.write_item(PxarHeader { size, htype })?;

        Ok(())
    }

    fn write_filename(&mut self, name: &CStr) -> Result<(), Error> {
        let buffer = name.to_bytes_with_nul();
        self.write_header(PXAR_FILENAME, buffer.len() as u64)?;
        self.write(buffer)?;

        Ok(())
    }

    fn create_entry(&self, stat: &FileStat) -> Result<PxarEntry, Error> {
        let mode = if is_symlink(&stat) {
            (libc::S_IFLNK | 0o777) as u64
        } else {
            (stat.st_mode & (libc::S_IFMT | 0o7777)) as u64
        };

        let mtime = stat.st_mtime * 1_000_000_000 + stat.st_mtime_nsec;
        if mtime < 0 {
            bail!("got strange mtime ({}) from fstat for {:?}.", mtime, self.full_path());
        }

        let entry = PxarEntry {
            mode,
            flags: 0,
            uid: stat.st_uid,
            gid: stat.st_gid,
            mtime: mtime as u64,
        };

        Ok(entry)
    }

    fn read_chattr(&self, fd: RawFd, entry: &mut PxarEntry) -> Result<(), Error> {
        let mut attr: usize = 0;

        let res = unsafe { fs::read_attr_fd(fd, &mut attr) };
        if let Err(err) = res {
            if let nix::Error::Sys(errno) = err {
                if errno_is_unsupported(errno) {
                    return Ok(());
                };
            }
            bail!("read_attr_fd failed for {:?} - {}", self.full_path(), err);
        }

        let flags = flags::feature_flags_from_chattr(attr as u32);
        entry.flags |= flags;

        Ok(())
    }

    fn read_fat_attr(&self, fd: RawFd, magic: i64, entry: &mut PxarEntry) -> Result<(), Error> {
        use proxmox::sys::linux::magic::*;

        if magic != MSDOS_SUPER_MAGIC && magic != FUSE_SUPER_MAGIC {
            return Ok(());
        }

        let mut attr: u32 = 0;

        let res = unsafe { fs::read_fat_attr_fd(fd, &mut attr) };
        if let Err(err) = res {
            if let nix::Error::Sys(errno) = err {
                if errno_is_unsupported(errno) {
                    return Ok(());
                };
            }
            bail!("read_fat_attr_fd failed for {:?} - {}", self.full_path(), err);
        }

        let flags = flags::feature_flags_from_fat_attr(attr);
        entry.flags |= flags;

        Ok(())
    }

    /// True if all of the given feature flags are set in the Encoder, false otherwise
    fn has_features(&self, feature_flags: u64) -> bool {
        (self.feature_flags & self.fs_feature_flags & feature_flags) == feature_flags
    }

    /// True if at least one of the given feature flags is set in the Encoder, false otherwise
    fn has_some_features(&self, feature_flags: u64) -> bool {
        (self.feature_flags & self.fs_feature_flags & feature_flags) != 0
    }

    fn read_xattrs(
        &self,
        fd: RawFd,
        stat: &FileStat,
    ) -> Result<(Vec<PxarXAttr>, Option<PxarFCaps>), Error> {
        let mut xattrs = Vec::new();
        let mut fcaps = None;

        let flags = flags::WITH_XATTRS | flags::WITH_FCAPS;
        if !self.has_some_features(flags) {
            return Ok((xattrs, fcaps));
        }
        // Should never be called on symlinks, just in case check anyway
        if is_symlink(&stat) {
            return Ok((xattrs, fcaps));
        }

        let xattr_names = match xattr::flistxattr(fd) {
            Ok(names) => names,
            // Do not bail if the underlying endpoint does not supports xattrs
            Err(Errno::EOPNOTSUPP) => return Ok((xattrs, fcaps)),
            // Do not bail if the endpoint cannot carry xattrs (such as symlinks)
            Err(Errno::EBADF) => return Ok((xattrs, fcaps)),
            Err(err) => bail!("read_xattrs failed for {:?} - {}", self.full_path(), err),
        };

        for name in xattr_names.split(|c| *c == b'\0') {
            // Only extract the relevant extended attributes
            if !xattr::is_valid_xattr_name(&name) {
                continue;
            }

            let value = match xattr::fgetxattr(fd, name) {
                Ok(value) => value,
                // Vanished between flistattr and getxattr, this is ok, silently ignore
                Err(Errno::ENODATA) => continue,
                Err(err) => bail!("read_xattrs failed for {:?} - {}", self.full_path(), err),
            };

            if xattr::is_security_capability(&name) {
                if self.has_features(flags::WITH_FCAPS) {
                    // fcaps are stored in own format within the archive
                    fcaps = Some(PxarFCaps { data: value });
                }
            } else if self.has_features(flags::WITH_XATTRS) {
                xattrs.push(PxarXAttr {
                    name: name.to_vec(),
                    value,
                });
            }
        }
        xattrs.sort();

        Ok((xattrs, fcaps))
    }

    fn read_acl(
        &self,
        fd: RawFd,
        stat: &FileStat,
        acl_type: acl::ACLType,
    ) -> Result<PxarACL, Error> {
        let ret = PxarACL {
            users: Vec::new(),
            groups: Vec::new(),
            group_obj: None,
            default: None,
        };

        if !self.has_features(flags::WITH_ACL) {
            return Ok(ret);
        }
        if is_symlink(&stat) {
            return Ok(ret);
        }
        if acl_type == acl::ACL_TYPE_DEFAULT && !is_directory(&stat) {
            bail!("ACL_TYPE_DEFAULT only defined for directories.");
        }

        // In order to be able to get ACLs with type ACL_TYPE_DEFAULT, we have
        // to create a path for acl_get_file(). acl_get_fd() only allows to get
        // ACL_TYPE_ACCESS attributes.
        let proc_path = Path::new("/proc/self/fd/").join(fd.to_string());
        let acl = match acl::ACL::get_file(&proc_path, acl_type) {
            Ok(acl) => acl,
            // Don't bail if underlying endpoint does not support acls
            Err(Errno::EOPNOTSUPP) => return Ok(ret),
            // Don't bail if the endpoint cannot carry acls
            Err(Errno::EBADF) => return Ok(ret),
            // Don't bail if there is no data
            Err(Errno::ENODATA) => return Ok(ret),
            Err(err) => bail!("error while reading ACL - {}", err),
        };

        self.process_acl(acl, acl_type)
    }

    fn process_acl(&self, acl: acl::ACL, acl_type: acl::ACLType) -> Result<PxarACL, Error> {
        let mut acl_user = Vec::new();
        let mut acl_group = Vec::new();
        let mut acl_group_obj = None;
        let mut acl_default = None;
        let mut user_obj_permissions = None;
        let mut group_obj_permissions = None;
        let mut other_permissions = None;
        let mut mask_permissions = None;

        for entry in &mut acl.entries() {
            let tag = entry.get_tag_type()?;
            let permissions = entry.get_permissions()?;
            match tag {
                acl::ACL_USER_OBJ => user_obj_permissions = Some(permissions),
                acl::ACL_GROUP_OBJ => group_obj_permissions = Some(permissions),
                acl::ACL_OTHER => other_permissions = Some(permissions),
                acl::ACL_MASK => mask_permissions = Some(permissions),
                acl::ACL_USER => {
                    acl_user.push(PxarACLUser {
                        uid: entry.get_qualifier()?,
                        permissions,
                    });
                }
                acl::ACL_GROUP => {
                    acl_group.push(PxarACLGroup {
                        gid: entry.get_qualifier()?,
                        permissions,
                    });
                }
                _ => bail!("Unexpected ACL tag encountered!"),
            }
        }

        acl_user.sort();
        acl_group.sort();

        match acl_type {
            acl::ACL_TYPE_ACCESS => {
                // The mask permissions are mapped to the stat group permissions
                // in case that the ACL group permissions were set.
                // Only in that case we need to store the group permissions,
                // in the other cases they are identical to the stat group permissions.
                if let (Some(gop), Some(_)) = (group_obj_permissions, mask_permissions) {
                    acl_group_obj = Some(PxarACLGroupObj { permissions: gop });
                }
            }
            acl::ACL_TYPE_DEFAULT => {
                if user_obj_permissions != None
                    || group_obj_permissions != None
                    || other_permissions != None
                    || mask_permissions != None
                {
                    acl_default = Some(PxarACLDefault {
                        // The value is set to UINT64_MAX as placeholder if one
                        // of the permissions is not set
                        user_obj_permissions: user_obj_permissions.unwrap_or(std::u64::MAX),
                        group_obj_permissions: group_obj_permissions.unwrap_or(std::u64::MAX),
                        other_permissions: other_permissions.unwrap_or(std::u64::MAX),
                        mask_permissions: mask_permissions.unwrap_or(std::u64::MAX),
                    });
                }
            }
            _ => bail!("Unexpected ACL type encountered"),
        }

        Ok(PxarACL {
            users: acl_user,
            groups: acl_group,
            group_obj: acl_group_obj,
            default: acl_default,
        })
    }

    /// Read the quota project id for an inode, supported on ext4/XFS/FUSE/ZFS filesystems
    fn read_quota_project_id(
        &self,
        fd: RawFd,
        magic: i64,
        stat: &FileStat,
    ) -> Result<Option<PxarQuotaProjID>, Error> {
        if !(is_directory(&stat) || is_reg_file(&stat)) {
            return Ok(None);
        }
        if !self.has_features(flags::WITH_QUOTA_PROJID) {
            return Ok(None);
        }

        use proxmox::sys::linux::magic::*;

        match magic {
            EXT4_SUPER_MAGIC | XFS_SUPER_MAGIC | FUSE_SUPER_MAGIC | ZFS_SUPER_MAGIC => {
                let mut fsxattr = fs::FSXAttr::default();
                let res = unsafe { fs::fs_ioc_fsgetxattr(fd, &mut fsxattr) };

                // On some FUSE filesystems it can happen that ioctl is not supported.
                // For these cases projid is set to 0 while the error is ignored.
                if let Err(err) = res {
                    let errno = err.as_errno().ok_or_else(|| {
                        format_err!(
                            "error while reading quota project id for {:#?}",
                            self.full_path()
                        )
                    })?;
                    if errno_is_unsupported(errno) {
                        return Ok(None);
                    } else {
                        bail!(
                            "error while reading quota project id for {:#?} - {}",
                            self.full_path(),
                            errno
                        );
                    }
                }

                let projid = fsxattr.fsx_projid as u64;
                if projid == 0 {
                    Ok(None)
                } else {
                    Ok(Some(PxarQuotaProjID { projid }))
                }
            }
            _ => Ok(None),
        }
    }

    fn write_entry(&mut self, entry: PxarEntry) -> Result<(), Error> {
        self.write_header(PXAR_ENTRY, std::mem::size_of::<PxarEntry>() as u64)?;
        self.write_item(entry)?;

        Ok(())
    }

    fn write_xattr(&mut self, xattr: PxarXAttr) -> Result<(), Error> {
        let size = xattr.name.len() + xattr.value.len() + 1; // +1 for '\0' separating name and value
        self.write_header(PXAR_XATTR, size as u64)?;
        self.write(xattr.name.as_slice())?;
        self.write(&[0])?;
        self.write(xattr.value.as_slice())?;

        Ok(())
    }

    fn write_fcaps(&mut self, fcaps: Option<PxarFCaps>) -> Result<(), Error> {
        if let Some(fcaps) = fcaps {
            let size = fcaps.data.len();
            self.write_header(PXAR_FCAPS, size as u64)?;
            self.write(fcaps.data.as_slice())?;
        }

        Ok(())
    }

    fn write_acl_user(&mut self, acl_user: PxarACLUser) -> Result<(), Error> {
        self.write_header(PXAR_ACL_USER, std::mem::size_of::<PxarACLUser>() as u64)?;
        self.write_item(acl_user)?;

        Ok(())
    }

    fn write_acl_group(&mut self, acl_group: PxarACLGroup) -> Result<(), Error> {
        self.write_header(PXAR_ACL_GROUP, std::mem::size_of::<PxarACLGroup>() as u64)?;
        self.write_item(acl_group)?;

        Ok(())
    }

    fn write_acl_group_obj(&mut self, acl_group_obj: PxarACLGroupObj) -> Result<(), Error> {
        self.write_header(
            PXAR_ACL_GROUP_OBJ,
            std::mem::size_of::<PxarACLGroupObj>() as u64,
        )?;
        self.write_item(acl_group_obj)?;

        Ok(())
    }

    fn write_acl_default(&mut self, acl_default: PxarACLDefault) -> Result<(), Error> {
        self.write_header(
            PXAR_ACL_DEFAULT,
            std::mem::size_of::<PxarACLDefault>() as u64,
        )?;
        self.write_item(acl_default)?;

        Ok(())
    }

    fn write_acl_default_user(&mut self, acl_default_user: PxarACLUser) -> Result<(), Error> {
        self.write_header(
            PXAR_ACL_DEFAULT_USER,
            std::mem::size_of::<PxarACLUser>() as u64,
        )?;
        self.write_item(acl_default_user)?;

        Ok(())
    }

    fn write_acl_default_group(&mut self, acl_default_group: PxarACLGroup) -> Result<(), Error> {
        self.write_header(
            PXAR_ACL_DEFAULT_GROUP,
            std::mem::size_of::<PxarACLGroup>() as u64,
        )?;
        self.write_item(acl_default_group)?;

        Ok(())
    }

    fn write_quota_project_id(&mut self, projid: PxarQuotaProjID) -> Result<(), Error> {
        self.write_header(
            PXAR_QUOTA_PROJID,
            std::mem::size_of::<PxarQuotaProjID>() as u64,
        )?;
        self.write_item(projid)?;

        Ok(())
    }

    fn write_goodbye_table(
        &mut self,
        goodbye_offset: usize,
        goodbye_items: &mut [PxarGoodbyeItem],
    ) -> Result<(), Error> {
        goodbye_items.sort_unstable_by(|a, b| a.hash.cmp(&b.hash));

        let item_count = goodbye_items.len();

        let goodbye_table_size = (item_count + 1) * std::mem::size_of::<PxarGoodbyeItem>();

        self.write_header(PXAR_GOODBYE, goodbye_table_size as u64)?;

        if self.file_copy_buffer.len() < goodbye_table_size {
            let need = goodbye_table_size - self.file_copy_buffer.len();
            self.file_copy_buffer.reserve(need);
            unsafe {
                self.file_copy_buffer
                    .set_len(self.file_copy_buffer.capacity());
            }
        }

        let buffer = &mut self.file_copy_buffer;

        copy_binary_search_tree(item_count, |s, d| {
            let item = &goodbye_items[s];
            let offset = d * std::mem::size_of::<PxarGoodbyeItem>();
            let dest =
                crate::tools::map_struct_mut::<PxarGoodbyeItem>(&mut buffer[offset..]).unwrap();
            dest.offset = u64::to_le(item.offset);
            dest.size = u64::to_le(item.size);
            dest.hash = u64::to_le(item.hash);
        });

        // append PxarGoodbyeTail as last item
        let offset = item_count * std::mem::size_of::<PxarGoodbyeItem>();
        let dest = crate::tools::map_struct_mut::<PxarGoodbyeItem>(&mut buffer[offset..]).unwrap();
        dest.offset = u64::to_le(goodbye_offset as u64);
        dest.size = u64::to_le((goodbye_table_size + std::mem::size_of::<PxarHeader>()) as u64);
        dest.hash = u64::to_le(PXAR_GOODBYE_TAIL_MARKER);

        self.flush_copy_buffer(goodbye_table_size)?;

        Ok(())
    }

    fn encode_dir(
        &mut self,
        dir: &mut nix::dir::Dir,
        dir_stat: &FileStat,
        magic: i64,
        match_pattern: Vec<MatchPatternSlice>,
    ) -> Result<(), Error> {
        //println!("encode_dir: {:?} start {}", self.full_path(), self.writer_pos);

        let mut name_list = Vec::new();

        let rawfd = dir.as_raw_fd();

        let dir_start_pos = self.writer_pos;

        let is_root = dir_start_pos == 0;

        let mut dir_entry = self.create_entry(&dir_stat)?;

        self.read_chattr(rawfd, &mut dir_entry)?;
        self.read_fat_attr(rawfd, magic, &mut dir_entry)?;

        // for each node in the directory tree, the filesystem features are
        // checked based on the fs magic number.
        self.fs_feature_flags = flags::feature_flags_from_magic(magic);

        let (xattrs, fcaps) = self.read_xattrs(rawfd, &dir_stat)?;
        let acl_access = self.read_acl(rawfd, &dir_stat, acl::ACL_TYPE_ACCESS)?;
        let acl_default = self.read_acl(rawfd, &dir_stat, acl::ACL_TYPE_DEFAULT)?;
        let projid = self.read_quota_project_id(rawfd, magic, &dir_stat)?;

        self.write_entry(dir_entry)?;
        for xattr in xattrs {
            self.write_xattr(xattr)?;
        }
        self.write_fcaps(fcaps)?;

        for user in acl_access.users {
            self.write_acl_user(user)?;
        }
        for group in acl_access.groups {
            self.write_acl_group(group)?;
        }
        if let Some(group_obj) = acl_access.group_obj {
            self.write_acl_group_obj(group_obj)?;
        }

        for default_user in acl_default.users {
            self.write_acl_default_user(default_user)?;
        }
        for default_group in acl_default.groups {
            self.write_acl_default_group(default_group)?;
        }
        if let Some(default) = acl_default.default {
            self.write_acl_default(default)?;
        }
        if let Some(projid) = projid {
            self.write_quota_project_id(projid)?;
        }

        let include_children;
        if is_virtual_file_system(magic) {
            include_children = false;
        } else if let Some(set) = &self.device_set {
            include_children = set.contains(&dir_stat.st_dev);
        } else {
            include_children = true;
        }

        // Expand the exclude match pattern inherited from the parent by local entries, if present
        let mut local_match_pattern = match_pattern.clone();
        let (pxar_exclude, excludes) = match MatchPattern::from_file(rawfd, ".pxarexclude") {
            Ok(Some((excludes, buffer, stat))) => {
                (Some((buffer, stat)), excludes)
            }
            Ok(None) => (None, Vec::new()),
            Err(err) => bail!("error while reading exclude file - {}", err),
        };
        for excl in &excludes {
            local_match_pattern.push(excl.as_slice());
        }

        if include_children {
            // Exclude patterns passed via the CLI are stored as '.pxarexclude-cli'
            // in the root directory of the archive.
            if is_root && !match_pattern.is_empty() {
                let filename = CString::new(".pxarexclude-cli")?;
                name_list.push((filename, *dir_stat, match_pattern.clone()));
            }

            for entry in dir.iter() {
                let entry = entry
                    .map_err(|err| format_err!("readir {:?} failed - {}", self.full_path(), err))?;
                let filename = entry.file_name().to_owned();

                let name = filename.to_bytes_with_nul();
                if name == b".\0" || name == b"..\0" {
                    continue;
                }
                // Do not store a ".pxarexclude-cli" file found in the archive root,
                // as this would confilict with new cli passed exclude patterns,
                // if present.
                if is_root && name == b".pxarexclude-cli\0" {
                    eprintln!("skip existing '.pxarexclude-cli' in archive root.");
                    continue;
                }

                let stat = match nix::sys::stat::fstatat(
                    rawfd,
                    filename.as_ref(),
                    nix::fcntl::AtFlags::AT_SYMLINK_NOFOLLOW,
                ) {
                    Ok(stat) => stat,
                    Err(nix::Error::Sys(Errno::ENOENT)) => {
                        let filename_osstr = std::ffi::OsStr::from_bytes(filename.to_bytes());
                        self.report_vanished_file(&self.full_path().join(filename_osstr))?;
                        continue;
                    }
                    Err(err) => bail!("fstat {:?} failed - {}", self.full_path(), err),
                };

                match MatchPatternSlice::match_filename_exclude(
                    &filename,
                    is_directory(&stat),
                    &local_match_pattern,
                )? {
                    (MatchType::Positive, _) => {
                        let filename_osstr = std::ffi::OsStr::from_bytes(filename.to_bytes());
                        eprintln!(
                            "matched by .pxarexclude entry - skipping: {:?}",
                            self.full_path().join(filename_osstr)
                        );
                    }
                    (_, child_pattern) => name_list.push((filename, stat, child_pattern)),
                }

                if name_list.len() > MAX_DIRECTORY_ENTRIES {
                    bail!(
                        "too many directory items in {:?} (> {})",
                        self.full_path(),
                        MAX_DIRECTORY_ENTRIES
                    );
                }
            }
        } else {
            eprintln!("skip mount point: {:?}", self.full_path());
        }

        name_list.sort_unstable_by(|a, b| a.0.cmp(&b.0));

        let mut goodbye_items = Vec::with_capacity(name_list.len());

        for (filename, stat, exclude_list) in name_list {
            let start_pos = self.writer_pos;

            if filename.as_bytes() == b".pxarexclude" {
                if let Some((ref content, ref stat)) = pxar_exclude {
                    let filefd = match nix::fcntl::openat(
                        rawfd,
                        filename.as_ref(),
                        OFlag::O_NOFOLLOW,
                        Mode::empty(),
                    ) {
                        Ok(filefd) => filefd,
                        Err(nix::Error::Sys(Errno::ENOENT)) => {
                            self.report_vanished_file(&self.full_path())?;
                            continue;
                        }
                        Err(err) => {
                            let filename_osstr = std::ffi::OsStr::from_bytes(filename.to_bytes());
                            bail!(
                                "open file {:?} failed - {}",
                                self.full_path().join(filename_osstr),
                                err
                            );
                        }
                    };

                    let child_magic = if dir_stat.st_dev != stat.st_dev {
                        detect_fs_type(filefd)?
                    } else {
                        magic
                    };

                    self.write_filename(&filename)?;
                    if let Some(ref mut catalog) = self.catalog {
                        catalog.add_file(&filename, stat.st_size as u64, stat.st_mtime as u64)?;
                    }
                    self.encode_pxar_exclude(filefd, stat, child_magic, content)?;
                    continue;
                }
            }

            if is_root && filename.as_bytes() == b".pxarexclude-cli" {
                // '.pxarexclude-cli' is used to store the exclude MatchPatterns
                // passed via the cli in the root directory of the archive.
                self.write_filename(&filename)?;
                let content = MatchPatternSlice::to_bytes(&exclude_list);
                if let Some(ref mut catalog) = self.catalog {
                    catalog.add_file(&filename, content.len() as u64, 0)?;
                }
                self.encode_pxar_exclude_cli(stat.st_uid, stat.st_gid, 0, &content)?;
                continue;
            }

            self.relative_path
                .push(std::ffi::OsStr::from_bytes(filename.as_bytes()));

            if self.verbose {
                println!("{:?}", self.full_path());
            }

            if is_directory(&stat) {
                let mut dir = match nix::dir::Dir::openat(
                    rawfd,
                    filename.as_ref(),
                    OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW,
                    Mode::empty(),
                ) {
                    Ok(dir) => dir,
                    Err(nix::Error::Sys(Errno::ENOENT)) => {
                        self.report_vanished_file(&self.full_path())?;
                        self.relative_path.pop();
                        continue;
                    }
                    Err(err) => bail!("open dir {:?} failed - {}", self.full_path(), err),
                };

                let child_magic = if dir_stat.st_dev != stat.st_dev {
                    detect_fs_type(dir.as_raw_fd())?
                } else {
                    magic
                };

                self.write_filename(&filename)?;
                if let Some(ref mut catalog) = self.catalog {
                    catalog.start_directory(&filename)?;
                }
                self.encode_dir(&mut dir, &stat, child_magic, exclude_list)?;
                if let Some(ref mut catalog) = self.catalog {
                    catalog.end_directory()?;
                }
            } else if is_reg_file(&stat) {
                let mut hardlink_target = None;

                if stat.st_nlink > 1 {
                    let link_info = HardLinkInfo {
                        st_dev: stat.st_dev,
                        st_ino: stat.st_ino,
                    };
                    hardlink_target = self.hardlinks.get(&link_info).map(|(v, offset)| {
                        let mut target = v.clone().into_os_string();
                        target.push("\0"); // add Nul byte
                        (target, (start_pos as u64) - offset)
                    });
                    if hardlink_target == None {
                        self.hardlinks
                            .insert(link_info, (self.relative_path.clone(), start_pos as u64));
                    }
                }

                if let Some((target, offset)) = hardlink_target {
                    if let Some(ref mut catalog) = self.catalog {
                        catalog.add_hardlink(&filename)?;
                    }
                    self.write_filename(&filename)?;
                    self.encode_hardlink(target.as_bytes(), offset)?;
                } else {
                    let filefd = match nix::fcntl::openat(
                        rawfd,
                        filename.as_ref(),
                        OFlag::O_NOFOLLOW,
                        Mode::empty(),
                    ) {
                        Ok(filefd) => filefd,
                        Err(nix::Error::Sys(Errno::ENOENT)) => {
                            self.report_vanished_file(&self.full_path())?;
                            self.relative_path.pop();
                            continue;
                        }
                        Err(err) => bail!("open file {:?} failed - {}", self.full_path(), err),
                    };

                    if let Some(ref mut catalog) = self.catalog {
                        catalog.add_file(&filename, stat.st_size as u64, stat.st_mtime as u64)?;
                    }
                    let child_magic = if dir_stat.st_dev != stat.st_dev {
                        detect_fs_type(filefd)?
                    } else {
                        magic
                    };

                    self.write_filename(&filename)?;
                    let res = self.encode_file(filefd, &stat, child_magic);
                    let _ = nix::unistd::close(filefd); // ignore close errors
                    res?;
                }
            } else if is_symlink(&stat) {
                let mut buffer = vec::undefined(libc::PATH_MAX as usize);

                let res = filename.with_nix_path(|cstr| unsafe {
                    libc::readlinkat(
                        rawfd,
                        cstr.as_ptr(),
                        buffer.as_mut_ptr() as *mut libc::c_char,
                        buffer.len() - 1,
                    )
                })?;

                match Errno::result(res) {
                    Ok(len) => {
                        if let Some(ref mut catalog) = self.catalog {
                            catalog.add_symlink(&filename)?;
                        }
                        buffer[len as usize] = 0u8; // add Nul byte
                        self.write_filename(&filename)?;
                        self.encode_symlink(&buffer[..((len + 1) as usize)], &stat)?
                    }
                    Err(nix::Error::Sys(Errno::ENOENT)) => {
                        self.report_vanished_file(&self.full_path())?;
                        self.relative_path.pop();
                        continue;
                    }
                    Err(err) => bail!("readlink {:?} failed - {}", self.full_path(), err),
                }
            } else if is_block_dev(&stat) || is_char_dev(&stat) {
                if self.has_features(flags::WITH_DEVICE_NODES) {
                    if let Some(ref mut catalog) = self.catalog {
                        if is_block_dev(&stat) {
                            catalog.add_block_device(&filename)?;
                        } else {
                            catalog.add_char_device(&filename)?;
                        }
                    }
                    self.write_filename(&filename)?;
                    self.encode_device(&stat)?;
                } else {
                    eprintln!("skip device node: {:?}", self.full_path());
                    self.relative_path.pop();
                    continue;
                }
            } else if is_fifo(&stat) {
                if self.has_features(flags::WITH_FIFOS) {
                    if let Some(ref mut catalog) = self.catalog {
                        catalog.add_fifo(&filename)?;
                    }
                    self.write_filename(&filename)?;
                    self.encode_special(&stat)?;
                } else {
                    eprintln!("skip fifo: {:?}", self.full_path());
                    self.relative_path.pop();
                    continue;
                }
            } else if is_socket(&stat) {
                if self.has_features(flags::WITH_SOCKETS) {
                    if let Some(ref mut catalog) = self.catalog {
                        catalog.add_socket(&filename)?;
                    }
                    self.write_filename(&filename)?;
                    self.encode_special(&stat)?;
                } else {
                    eprintln!("skip socket: {:?}", self.full_path());
                    self.relative_path.pop();
                    continue;
                }
            } else {
                bail!(
                    "unsupported file type (mode {:o} {:?})",
                    stat.st_mode,
                    self.full_path()
                );
            }

            let end_pos = self.writer_pos;

            goodbye_items.push(PxarGoodbyeItem {
                offset: start_pos as u64,
                size: (end_pos - start_pos) as u64,
                hash: compute_goodbye_hash(filename.to_bytes()),
            });

            self.relative_path.pop();
        }

        //println!("encode_dir: {:?} end {}", self.full_path(), self.writer_pos);

        // fixup goodby item offsets
        let goodbye_start = self.writer_pos as u64;
        for item in &mut goodbye_items {
            item.offset = goodbye_start - item.offset;
        }

        let goodbye_offset = self.writer_pos - dir_start_pos;

        self.write_goodbye_table(goodbye_offset, &mut goodbye_items)?;

        //println!("encode_dir: {:?} end1 {}", self.full_path(), self.writer_pos);
        Ok(())
    }

    fn encode_file(&mut self, filefd: RawFd, stat: &FileStat, magic: i64) -> Result<(), Error> {
        //println!("encode_file: {:?}", self.full_path());

        let mut entry = self.create_entry(&stat)?;

        self.read_chattr(filefd, &mut entry)?;
        self.read_fat_attr(filefd, magic, &mut entry)?;
        let (xattrs, fcaps) = self.read_xattrs(filefd, &stat)?;
        let acl_access = self.read_acl(filefd, &stat, acl::ACL_TYPE_ACCESS)?;
        let projid = self.read_quota_project_id(filefd, magic, &stat)?;

        self.write_entry(entry)?;
        for xattr in xattrs {
            self.write_xattr(xattr)?;
        }
        self.write_fcaps(fcaps)?;
        for user in acl_access.users {
            self.write_acl_user(user)?;
        }
        for group in acl_access.groups {
            self.write_acl_group(group)?;
        }
        if let Some(group_obj) = acl_access.group_obj {
            self.write_acl_group_obj(group_obj)?;
        }
        if let Some(projid) = projid {
            self.write_quota_project_id(projid)?;
        }

        let include_payload;
        if is_virtual_file_system(magic) {
            include_payload = false;
        } else if let Some(ref set) = &self.device_set {
            include_payload = set.contains(&stat.st_dev);
        } else {
            include_payload = true;
        }

        if !include_payload {
            eprintln!("skip content: {:?}", self.full_path());
            self.write_header(PXAR_PAYLOAD, 0)?;
            return Ok(());
        }

        let size = stat.st_size as u64;

        self.write_header(PXAR_PAYLOAD, size)?;

        let mut pos: u64 = 0;
        loop {
            let n = match nix::unistd::read(filefd, &mut self.file_copy_buffer) {
                Ok(n) => n,
                Err(nix::Error::Sys(Errno::EINTR)) => continue, /* try again */
                Err(err) => bail!("read {:?} failed - {}", self.full_path(), err),
            };
            if n == 0 { // EOF
                if pos != size {
                    // Note:: casync format cannot handle that
                    bail!(
                        "detected shrinked file {:?} ({} < {})",
                        self.full_path(),
                        pos,
                        size
                    );
                }
                break;
            }

            let mut next = pos + (n as u64);

            if next > size {
                next = size;
            }

            let count = (next - pos) as usize;

            self.flush_copy_buffer(count)?;

            pos = next;

            if pos >= size {
                break;
            }
        }

        Ok(())
    }

    fn encode_device(&mut self, stat: &FileStat) -> Result<(), Error> {
        let entry = self.create_entry(&stat)?;

        self.write_entry(entry)?;

        let major = unsafe { libc::major(stat.st_rdev) } as u64;
        let minor = unsafe { libc::minor(stat.st_rdev) } as u64;

        //println!("encode_device: {:?} {} {} {}", self.full_path(), stat.st_rdev, major, minor);

        self.write_header(PXAR_DEVICE, std::mem::size_of::<PxarDevice>() as u64)?;
        self.write_item(PxarDevice { major, minor })?;

        Ok(())
    }

    // FIFO or Socket
    fn encode_special(&mut self, stat: &FileStat) -> Result<(), Error> {
        let entry = self.create_entry(&stat)?;

        self.write_entry(entry)?;

        Ok(())
    }

    fn encode_symlink(&mut self, target: &[u8], stat: &FileStat) -> Result<(), Error> {
        //println!("encode_symlink: {:?} -> {:?}", self.full_path(), target);

        let entry = self.create_entry(&stat)?;
        self.write_entry(entry)?;

        self.write_header(PXAR_SYMLINK, target.len() as u64)?;
        self.write(target)?;

        Ok(())
    }

    fn encode_hardlink(&mut self, target: &[u8], offset: u64) -> Result<(), Error> {
        //println!("encode_hardlink: {:?} -> {:?}", self.full_path(), target);

        // Note: HARDLINK replaces an ENTRY.
        self.write_header(PXAR_FORMAT_HARDLINK, (target.len() as u64) + 8)?;
        self.write_item(offset)?;
        self.write(target)?;

        Ok(())
    }

    fn encode_pxar_exclude(
        &mut self,
        filefd: RawFd,
        stat: &FileStat,
        magic: i64,
        content: &[u8],
    ) -> Result<(), Error> {
        let mut entry = self.create_entry(&stat)?;

        self.read_chattr(filefd, &mut entry)?;
        self.read_fat_attr(filefd, magic, &mut entry)?;
        let (xattrs, fcaps) = self.read_xattrs(filefd, &stat)?;
        let acl_access = self.read_acl(filefd, &stat, acl::ACL_TYPE_ACCESS)?;
        let projid = self.read_quota_project_id(filefd, magic, &stat)?;

        self.write_entry(entry)?;
        for xattr in xattrs {
            self.write_xattr(xattr)?;
        }
        self.write_fcaps(fcaps)?;
        for user in acl_access.users {
            self.write_acl_user(user)?;
        }
        for group in acl_access.groups {
            self.write_acl_group(group)?;
        }
        if let Some(group_obj) = acl_access.group_obj {
            self.write_acl_group_obj(group_obj)?;
        }
        if let Some(projid) = projid {
            self.write_quota_project_id(projid)?;
        }

        let include_payload;
        if is_virtual_file_system(magic) {
            include_payload = false;
        } else if let Some(set) = &self.device_set {
            include_payload = set.contains(&stat.st_dev);
        } else {
            include_payload = true;
        }

        if !include_payload {
            eprintln!("skip content: {:?}", self.full_path());
            self.write_header(PXAR_PAYLOAD, 0)?;
            return Ok(());
        }

        let size = content.len();
        self.write_header(PXAR_PAYLOAD, size as u64)?;
        self.writer.write_all(content)?;
        self.writer_pos += size;

        Ok(())
    }

    /// Encodes the excude match patterns passed via cli as file in the archive.
    fn encode_pxar_exclude_cli(
        &mut self,
        uid: u32,
        gid: u32,
        mtime: u64,
        content: &[u8],
    ) -> Result<(), Error> {
        let entry = PxarEntry {
            mode: (libc::S_IFREG | 0o600) as u64,
            flags: 0,
            uid,
            gid,
            mtime,
        };
        self.write_entry(entry)?;
        let size = content.len();
        self.write_header(PXAR_PAYLOAD, size as u64)?;
        self.writer.write_all(content)?;
        self.writer_pos += size;

        Ok(())
    }

    // the report_XXX method may raise and error - depending on encoder configuration

    fn report_vanished_file(&self, path: &Path) -> Result<(), Error> {
        eprintln!("WARNING: detected vanished file {:?}", path);

        Ok(())
    }
}

fn errno_is_unsupported(errno: Errno) -> bool {
    match errno {
        Errno::ENOTTY | Errno::ENOSYS | Errno::EBADF | Errno::EOPNOTSUPP | Errno::EINVAL => true,
        _ => false,
    }
}

fn detect_fs_type(fd: RawFd) -> Result<i64, Error> {
    let mut fs_stat = std::mem::MaybeUninit::uninit();
    let res = unsafe { libc::fstatfs(fd, fs_stat.as_mut_ptr()) };
    Errno::result(res)?;
    let fs_stat = unsafe { fs_stat.assume_init() };

    Ok(fs_stat.f_type)
}

#[inline(always)]
pub fn is_temporary_file_system(magic: i64) -> bool {
    use proxmox::sys::linux::magic::*;
    magic == RAMFS_MAGIC || magic == TMPFS_MAGIC
}

pub fn is_virtual_file_system(magic: i64) -> bool {
    use proxmox::sys::linux::magic::*;

    match magic {
        BINFMTFS_MAGIC |
        CGROUP2_SUPER_MAGIC |
        CGROUP_SUPER_MAGIC |
        CONFIGFS_MAGIC |
        DEBUGFS_MAGIC |
        DEVPTS_SUPER_MAGIC |
        EFIVARFS_MAGIC |
        FUSE_CTL_SUPER_MAGIC |
        HUGETLBFS_MAGIC |
        MQUEUE_MAGIC |
        NFSD_MAGIC |
        PROC_SUPER_MAGIC |
        PSTOREFS_MAGIC |
        RPCAUTH_GSSMAGIC |
        SECURITYFS_MAGIC |
        SELINUX_MAGIC |
        SMACK_MAGIC |
        SYSFS_MAGIC => true,
        _ => false
    }
}
