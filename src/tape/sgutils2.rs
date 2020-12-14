/// Bindings for libsgutils2
///
/// Incomplete, but we currently do not need more.

use std::os::unix::io::AsRawFd;

use anyhow::{bail, Error};
use libc::{c_char, c_int};

#[repr(C)]
pub struct SgPtBase { _private: [u8; 0] }

impl Drop for SgPtBase  {
    fn drop(&mut self) {
        unsafe { destruct_scsi_pt_obj(self as *mut SgPtBase) };
    }
}

#[link(name = "sgutils2")]
extern {

    pub fn scsi_pt_open_device(
        device_name: * const c_char,
        read_only: bool,
        verbose: c_int,
    ) -> c_int;

    pub fn sg_is_scsi_cdb(
        cdbp: *const u8,
        clen: c_int,
    ) -> bool;

    pub fn construct_scsi_pt_obj() -> *mut SgPtBase;
    pub fn destruct_scsi_pt_obj(objp: *mut SgPtBase);

    pub fn set_scsi_pt_data_in(
        objp: *mut SgPtBase,
        dxferp: *const u8,
        dxfer_ilen: c_int,
    );

    pub fn set_scsi_pt_cdb(
        objp: *mut SgPtBase,
        cdb: *const u8,
        cdb_len: c_int,
    );

    pub fn set_scsi_pt_sense(
        objp: *mut SgPtBase,
        sense: *const u8,
        max_sense_len: c_int,
    );

    pub fn do_scsi_pt(
        objp: *mut SgPtBase,
        fd: c_int,
        timeout_secs: c_int,
        verbose: c_int,
    ) -> c_int;

    pub fn get_scsi_pt_resid(objp: *const SgPtBase) -> c_int;

    pub fn get_scsi_pt_sense_len(objp: *const SgPtBase) -> c_int;

    pub fn get_scsi_pt_status_response(objp: *const SgPtBase) -> c_int;
}

/// Creates a Box<SgPtBase>
///
/// Which get automatically dropped, so you do not need to call
/// destruct_scsi_pt_obj yourself.
pub fn boxed_scsi_pt_obj() -> Result<Box<SgPtBase>, Error> {
    let objp = unsafe {
        construct_scsi_pt_obj()
    };
    if objp.is_null() {
        bail!("construct_scsi_pt_ob failed");
    }

    Ok(unsafe { std::mem::transmute(objp)})
}

/// Safe interface to run RAW SCSI commands
pub struct SgRaw<'a, F> {
    file: &'a mut F,
    buffer: Box<[u8]>,
    sense_buffer: [u8; 32],
}

impl <'a, F: AsRawFd> SgRaw<'a, F> {

    /// Create a new instance to run commands
    ///
    /// The file must be a handle to a SCSI device.
    pub fn new(file: &'a mut F, buffer_size: usize) -> Result<Self, Error> {

        let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
        let layout = std::alloc::Layout::from_size_align(buffer_size, page_size)?;
        let dinp = unsafe { std::alloc::alloc(layout) };
        if dinp.is_null() {
            bail!("alloc SCSI output buffer failed");
        }

        let buffer = unsafe { std::slice::from_raw_parts_mut(dinp, buffer_size)};
        let buffer = unsafe { Box::from_raw(buffer) };

        let sense_buffer = [0u8; 32];

        Ok(Self { file, buffer, sense_buffer })
    }

    // create new object with initialized data_in and sense buffer
    fn create_boxed_scsi_pt_obj(&mut self) -> Result<Box<SgPtBase>, Error> {

        let mut ptvp = boxed_scsi_pt_obj()?;

        unsafe {
            set_scsi_pt_data_in(
                &mut *ptvp,
                self.buffer.as_ptr(),
                self.buffer.len() as c_int,
            )
        };

        unsafe {
            set_scsi_pt_sense(
                &mut *ptvp,
                self.sense_buffer.as_ptr(),
                self.sense_buffer.len() as c_int,
            )
        };

        Ok(ptvp)
    }

    /// Run the specified RAW SCSI command
    pub fn do_command(&mut self, cmd: &[u8]) -> Result<&[u8], Error> {

        if !unsafe { sg_is_scsi_cdb(cmd.as_ptr(), cmd.len() as c_int) } {
            bail!("no valid SCSI command");
        }

        let mut ptvp = self.create_boxed_scsi_pt_obj()?;

        unsafe {
            set_scsi_pt_cdb(
                &mut *ptvp,
                cmd.as_ptr(),
                cmd.len() as c_int,
            )
        };

        let res = unsafe { do_scsi_pt(&mut *ptvp, self.file.as_raw_fd(), 0, 0) };
        if res < 0 {
            let err = nix::Error::last();
            bail!("do_scsi_pt failed  - {}", err);
        }
        if res != 0 {
            bail!("do_scsi_pt failed {}", res);
        }

        // todo: what about sense data?
        let _sense_len = unsafe { get_scsi_pt_sense_len(&mut *ptvp) };

        let status = unsafe { get_scsi_pt_status_response(&mut *ptvp) };
        if status != 0 {
            // toto: improve error reporting
            bail!("unknown scsi error - status response {}", status);
        }

        let data_len = self.buffer.len() -
            (unsafe { get_scsi_pt_resid(&mut *ptvp) } as usize);
        if data_len <= 0 {
            bail!("do_scsi_pt failed - no data received");
        }

        Ok(&self.buffer[..data_len])
    }
}