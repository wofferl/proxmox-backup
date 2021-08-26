use std::time::SystemTime;
use std::fs::{File, OpenOptions};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;
use std::path::Path;
use std::convert::TryFrom;

use anyhow::{bail, format_err, Error};
use endian_trait::Endian;
use nix::fcntl::{fcntl, FcntlArg, OFlag};

mod encryption;
pub use encryption::*;

mod volume_statistics;
pub use volume_statistics::*;

mod tape_alert_flags;
pub use tape_alert_flags::*;

mod mam;
pub use mam::*;

mod report_density;
pub use report_density::*;

use proxmox::{
    sys::error::SysResult,
    tools::io::{ReadExt, WriteExt},
};

use crate::{
    api2::types::{
        MamAttribute,
        Lp17VolumeStatistics,
    },
    tape::{
        BlockRead,
        BlockReadError,
        BlockWrite,
        file_formats::{
            BlockedWriter,
            BlockedReader,
        },
    },
    tools::sgutils2::{
        SgRaw,
        SenseInfo,
        ScsiError,
        InquiryInfo,
        ModeParameterHeader,
        ModeBlockDescriptor,
        alloc_page_aligned_buffer,
        scsi_inquiry,
        scsi_mode_sense,
        scsi_request_sense,
    },
};

#[repr(C, packed)]
#[derive(Endian, Debug, Copy, Clone)]
pub struct ReadPositionLongPage {
    flags: u8,
    reserved: [u8;3],
    partition_number: u32,
    pub logical_object_number: u64,
    pub logical_file_id: u64,
    obsolete: [u8;8],
}

#[repr(C, packed)]
#[derive(Endian, Debug, Copy, Clone)]
struct DataCompressionModePage {
    page_code: u8,   // 0x0f
    page_length: u8,  // 0x0e
    flags2: u8,
    flags3: u8,
    compression_algorithm: u32,
    decompression_algorithm: u32,
    reserved: [u8;4],
}

impl DataCompressionModePage {

    pub fn set_compression(&mut self, enable: bool) {
        if enable {
            self.flags2 |= 128;
        } else {
            self.flags2 = self.flags2 & 127;
        }
    }

    pub fn compression_enabled(&self) -> bool {
        (self.flags2 & 0b1000_0000) != 0
    }
}

#[repr(C, packed)]
#[derive(Endian)]
struct MediumConfigurationModePage {
    page_code: u8,   // 0x1d
    page_length: u8,  // 0x1e
    flags2: u8,
    reserved: [u8;29],
}

impl MediumConfigurationModePage {

    pub fn is_worm(&self) -> bool {
        (self.flags2 & 1) == 1
    }

}

#[derive(Debug)]
pub struct LtoTapeStatus {
    pub block_length: u32,
    pub density_code: u8,
    pub buffer_mode: u8,
    pub write_protect: bool,
    pub compression: bool,
}

pub struct SgTape {
    file: File,
    locate_offset: Option<i64>,
    info: InquiryInfo,
    encryption_key_loaded: bool,
}

impl SgTape {

    const SCSI_TAPE_DEFAULT_TIMEOUT: usize = 60*2; // 2 minutes

    /// Create a new instance
    ///
    /// Uses scsi_inquiry to check the device type.
    pub fn new(mut file: File) -> Result<Self, Error> {

        let info = scsi_inquiry(&mut file)?;

        if info.peripheral_type != 1 {
            bail!("not a tape device (peripheral_type = {})", info.peripheral_type);
        }

        Ok(Self {
            file,
            info,
            encryption_key_loaded: false,
            locate_offset: None,
        })
    }

    /// Access to file descriptor - useful for testing
    pub fn file_mut(&mut self) -> &mut File {
        &mut self.file
    }

    pub fn info(&self) -> &InquiryInfo {
        &self.info
    }

    /// Return the maximum supported density code
    ///
    /// This can be used to detect the drive generation.
    pub fn max_density_code(&mut self) -> Result<u8, Error> {
        report_density(&mut self.file)
    }

    pub fn open<P: AsRef<Path>>(path: P) -> Result<SgTape, Error> {
        // do not wait for media, use O_NONBLOCK
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(path)?;

        // then clear O_NONBLOCK
        let flags = fcntl(file.as_raw_fd(), FcntlArg::F_GETFL)
            .into_io_result()?;

        let mut flags = OFlag::from_bits_truncate(flags);
        flags.remove(OFlag::O_NONBLOCK);

        fcntl(file.as_raw_fd(), FcntlArg::F_SETFL(flags))
            .into_io_result()?;

        Self::new(file)
    }

    pub fn inquiry(&mut self) -> Result<InquiryInfo, Error> {
        scsi_inquiry(&mut self.file)
    }

    /// Erase medium.
    ///
    /// EOD is written at the current position, which marks it as end
    /// of data. After the command is successfully completed, the
    /// drive is positioned immediately before End Of Data (not End Of
    /// Tape).
    pub fn erase_media(&mut self, fast: bool) -> Result<(), Error> {
        let mut sg_raw = SgRaw::new(&mut self.file, 16)?;
        sg_raw.set_timeout(Self::SCSI_TAPE_DEFAULT_TIMEOUT);
        let mut cmd = Vec::new();
        cmd.push(0x19);
        if fast {
            cmd.push(0); // LONG=0
        } else {
            cmd.push(1); // LONG=1
        }
        cmd.extend(&[0, 0, 0, 0]);

        sg_raw.do_command(&cmd)
            .map_err(|err| format_err!("erase failed - {}", err))?;

        Ok(())
    }

    /// Format media, single partition
    pub fn format_media(&mut self, fast: bool) -> Result<(), Error> {

        // try to get info about loaded media first
        let (has_format, is_worm) = match self.read_medium_configuration_page() {
            Ok((_head, block_descriptor, page)) => {
                // FORMAT requires LTO5 or newer
                let has_format = block_descriptor.density_code >= 0x58;
                let is_worm = page.is_worm();
                (has_format, is_worm)
            }
            Err(_) => {
                // LTO3 and older do not supprt medium configuration mode page
                (false, false)
            }
        };

        if is_worm {
            // We cannot FORMAT WORM media! Instead we check if its empty.

            self.move_to_eom(false)?;
            let pos = self.position()?;
            if pos.logical_object_number != 0 {
                bail!("format failed - detected WORM media with data.");
            }

            Ok(())

        } else {
            self.rewind()?;

            let mut sg_raw = SgRaw::new(&mut self.file, 16)?;
            sg_raw.set_timeout(Self::SCSI_TAPE_DEFAULT_TIMEOUT);
            let mut cmd = Vec::new();

            if has_format {
                cmd.extend(&[0x04, 0, 0, 0, 0, 0]); // FORMAT
                sg_raw.do_command(&cmd)?;
                if !fast {
                    self.erase_media(false)?; // overwrite everything
                }
            } else {
                // try rewind/erase instead
                self.erase_media(fast)?
            }

            Ok(())
        }
    }

    /// Lock/Unlock drive door
    pub fn set_medium_removal(&mut self, allow: bool) -> Result<(), ScsiError> {

        let mut sg_raw = SgRaw::new(&mut self.file, 16)?;
        sg_raw.set_timeout(Self::SCSI_TAPE_DEFAULT_TIMEOUT);
        let mut cmd = Vec::new();
        cmd.extend(&[0x1E, 0, 0, 0]);
        if allow {
            cmd.push(0);
        } else {
            cmd.push(1);
        }
        cmd.push(0); // control

        sg_raw.do_command(&cmd)?;

        Ok(())
    }

    pub fn rewind(&mut self) -> Result<(), Error> {

        let mut sg_raw = SgRaw::new(&mut self.file, 16)?;
        sg_raw.set_timeout(Self::SCSI_TAPE_DEFAULT_TIMEOUT);
        let mut cmd = Vec::new();
        cmd.extend(&[0x01, 0, 0, 0, 0, 0]); // REWIND

        sg_raw.do_command(&cmd)
            .map_err(|err| format_err!("rewind failed - {}", err))?;

        Ok(())
    }

    pub fn locate_file(&mut self, position: u64) ->  Result<(), Error> {
        if position == 0 {
            return self.rewind();
        }

        const SPACE_ONE_FILEMARK: &[u8] = &[0x11, 0x01, 0, 0, 1, 0];

        // Special case for position 1, because LOCATE 0 does not work
        if position == 1 {
            self.rewind()?;
            let mut sg_raw = SgRaw::new(&mut self.file, 16)?;
            sg_raw.set_timeout(Self::SCSI_TAPE_DEFAULT_TIMEOUT);
            sg_raw.do_command(SPACE_ONE_FILEMARK)
                .map_err(|err| format_err!("locate file {} (space) failed - {}", position, err))?;
            return Ok(());
        }

        let mut sg_raw = SgRaw::new(&mut self.file, 16)?;
        sg_raw.set_timeout(Self::SCSI_TAPE_DEFAULT_TIMEOUT);

        // Note: LOCATE(16) works for LTO4 or newer
        //
        // It seems the LOCATE command behaves slightly different across vendors
        // e.g. for IBM drives, LOCATE 1 moves to File #2, but
        // for HP drives, LOCATE 1 move to File #1

        let fixed_position = if let Some(locate_offset) = self.locate_offset {
            if locate_offset < 0 {
                position.saturating_sub((-locate_offset) as u64)
            } else {
                position.saturating_add(locate_offset as u64)
            }
        } else {
            position
        };
        // always sub(1), so that it works for IBM drives without locate_offset
        let fixed_position = fixed_position.saturating_sub(1);

        let mut cmd = Vec::new();
        cmd.extend(&[0x92, 0b000_01_000, 0, 0]); // LOCATE(16) filemarks
        cmd.extend(&fixed_position.to_be_bytes());
        cmd.extend(&[0, 0, 0, 0]);

        sg_raw.do_command(&cmd)
            .map_err(|err| format_err!("locate file {} failed - {}", position, err))?;

        // LOCATE always position at the BOT side of the filemark, so
        // we need to move to other side of filemark
        sg_raw.do_command(SPACE_ONE_FILEMARK)
            .map_err(|err| format_err!("locate file {} (space) failed - {}", position, err))?;

        if self.locate_offset.is_none() {
            // check if we landed at correct position
            let current_file = self.current_file_number()?;
            if current_file != position {
                let offset: i64 =
                    i64::try_from((position as i128) - (current_file as i128)).map_err(|err| {
                        format_err!(
                            "locate_file: offset between {} and {} invalid: {}",
                            position,
                            current_file,
                            err
                        )
                    })?;
                self.locate_offset = Some(offset);
                self.locate_file(position)?;
                let current_file = self.current_file_number()?;
                if current_file != position {
                    bail!("locate_file: compensating offset did not work, aborting...");
                }
            } else {
                self.locate_offset = Some(0);
            }
        }

        Ok(())
    }

    pub fn position(&mut self) -> Result<ReadPositionLongPage, Error> {

        let expected_size = std::mem::size_of::<ReadPositionLongPage>();

        let mut sg_raw = SgRaw::new(&mut self.file, 32)?;
        sg_raw.set_timeout(30); // use short timeout
        let mut cmd = Vec::new();
        // READ POSITION LONG FORM works on LTO4 or newer (with recent
        // firmware), although it is missing in the IBM LTO4 SSCI
        // reference manual.
        cmd.extend(&[0x34, 0x06, 0, 0, 0, 0, 0, 0, 0, 0]); // READ POSITION LONG FORM

        let data = sg_raw.do_command(&cmd)
            .map_err(|err| format_err!("read position failed - {}", err))?;

        let page = proxmox::try_block!({
            if data.len() != expected_size {
                bail!("got unexpected data len ({} != {}", data.len(), expected_size);
            }

            let mut reader = &data[..];

            let page: ReadPositionLongPage = unsafe { reader.read_be_value()? };

            Ok(page)
        }).map_err(|err: Error| format_err!("decode position page failed - {}", err))?;

        if page.partition_number != 0 {
            bail!("detecthed partitioned tape - not supported");
        }

        Ok(page)
    }

    pub fn current_file_number(&mut self) -> Result<u64, Error> {
        let position = self.position()?;
        Ok(position.logical_file_id)
    }

    /// Check if we are positioned after a filemark (or BOT)
    pub fn check_filemark(&mut self) -> Result<bool, Error> {

        let pos = self.position()?;
        if pos.logical_object_number == 0 {
            // at BOT, Ok (no filemark required)
            return Ok(true);
        }

        // Note: SPACE blocks returns Err at filemark
        match self.space(-1, true) {
            Ok(_) => {
                self.space(1, true) // move back to end
                    .map_err(|err| format_err!("check_filemark failed (space forward) - {}", err))?;
                Ok(false)
            }
            Err(ScsiError::Sense(SenseInfo { sense_key: 0, asc: 0, ascq: 1 })) => {
                // Filemark detected - good
                self.space(1, false) // move to EOT side of filemark
                    .map_err(|err| format_err!("check_filemark failed (move to EOT side of filemark) - {}", err))?;
                Ok(true)
            }
            Err(err) => {
                bail!("check_filemark failed - {:?}", err);
            }
        }
    }

    pub fn move_to_eom(&mut self, write_missing_eof: bool) ->  Result<(), Error> {
        let mut sg_raw = SgRaw::new(&mut self.file, 16)?;
        sg_raw.set_timeout(Self::SCSI_TAPE_DEFAULT_TIMEOUT);
        let mut cmd = Vec::new();
        cmd.extend(&[0x11, 0x03, 0, 0, 0, 0]); // SPACE(6) move to EOD

        sg_raw.do_command(&cmd)
            .map_err(|err| format_err!("move to EOD failed - {}", err))?;

        if write_missing_eof {
            if !self.check_filemark()? {
                self.write_filemarks(1, false)?;
            }
        }

        Ok(())
    }

    fn space(&mut self, count: isize, blocks: bool) ->  Result<(), ScsiError> {
        let mut sg_raw = SgRaw::new(&mut self.file, 16)?;
        sg_raw.set_timeout(Self::SCSI_TAPE_DEFAULT_TIMEOUT);
        let mut cmd = Vec::new();

        // Use short command if possible (supported by all drives)
        if (count <= 0x7fffff) && (count > -0x7fffff) {
            cmd.push(0x11); // SPACE(6)
            if blocks {
                cmd.push(0); // blocks
            } else {
                cmd.push(1); // filemarks
            }
            cmd.push(((count >> 16) & 0xff) as u8);
            cmd.push(((count >> 8) & 0xff) as u8);
            cmd.push((count & 0xff) as u8);
            cmd.push(0); //control byte
        } else {
            cmd.push(0x91); // SPACE(16)
            if blocks {
                cmd.push(0); // blocks
            } else {
                cmd.push(1); // filemarks
            }
            cmd.extend(&[0, 0]); // reserved
            let count: i64 = count as i64;
            cmd.extend(&count.to_be_bytes());
            cmd.extend(&[0, 0, 0, 0]); // reserved
        }

        sg_raw.do_command(&cmd)?;

        Ok(())
    }

    pub fn space_filemarks(&mut self, count: isize) ->  Result<(), Error> {
        self.space(count, false)
            .map_err(|err| format_err!("space filemarks failed - {}", err))
    }

    pub fn space_blocks(&mut self, count: isize) ->  Result<(), Error> {
        self.space(count, true)
            .map_err(|err| format_err!("space blocks failed - {}", err))
    }

    pub fn eject(&mut self) ->  Result<(), Error> {
        let mut sg_raw = SgRaw::new(&mut self.file, 16)?;
        sg_raw.set_timeout(Self::SCSI_TAPE_DEFAULT_TIMEOUT);
        let mut cmd = Vec::new();
        cmd.extend(&[0x1B, 0, 0, 0, 0, 0]); // LODA/UNLOAD HOLD=0, LOAD=0

        sg_raw.do_command(&cmd)
            .map_err(|err| format_err!("eject failed - {}", err))?;

        Ok(())
    }

    pub fn load(&mut self) ->  Result<(), Error> {
        let mut sg_raw = SgRaw::new(&mut self.file, 16)?;
        sg_raw.set_timeout(Self::SCSI_TAPE_DEFAULT_TIMEOUT);
        let mut cmd = Vec::new();
        cmd.extend(&[0x1B, 0, 0, 0, 0b0000_0001, 0]); // LODA/UNLOAD HOLD=0, LOAD=1

        sg_raw.do_command(&cmd)
            .map_err(|err| format_err!("load media failed - {}", err))?;

        Ok(())
    }

    pub fn write_filemarks(
        &mut self,
        count: usize,
        immediate: bool,
    ) ->  Result<(), std::io::Error> {

        if count > 255 {
            proxmox::io_bail!("write_filemarks failed: got strange count '{}'", count);
        }

        let mut sg_raw = SgRaw::new(&mut self.file, 16)
            .map_err(|err| proxmox::io_format_err!("write_filemarks failed (alloc) - {}", err))?;

        sg_raw.set_timeout(Self::SCSI_TAPE_DEFAULT_TIMEOUT);
        let mut cmd = Vec::new();
        cmd.push(0x10);
        if immediate {
            cmd.push(1); // IMMED=1
        } else {
            cmd.push(0); // IMMED=0
        }
        cmd.extend(&[0, 0, count as u8]); // COUNT
        cmd.push(0); // control byte

        match sg_raw.do_command(&cmd) {
            Ok(_) => { /* OK */ }
            Err(ScsiError::Sense(SenseInfo { sense_key: 0, asc: 0, ascq: 2 })) => {
                /* LEOM - ignore */
            }
            Err(err) => {
                proxmox::io_bail!("write filemark  failed - {}", err);
            }
        }

        Ok(())
    }

    // Flush tape buffers (WEOF with count 0 => flush)
    pub fn sync(&mut self) -> Result<(), std::io::Error> {
        self.write_filemarks(0, false)?;
        Ok(())
    }

    pub fn test_unit_ready(&mut self) -> Result<(), Error> {

        let mut sg_raw = SgRaw::new(&mut self.file, 16)?;
        sg_raw.set_timeout(30); // use short timeout
        let mut cmd = Vec::new();
        cmd.extend(&[0x00, 0, 0, 0, 0, 0]); // TEST UNIT READY

        match sg_raw.do_command(&cmd) {
            Ok(_) => Ok(()),
            Err(err) => {
                bail!("test_unit_ready failed - {}", err);
            }
        }
    }

    pub fn wait_until_ready(&mut self) -> Result<(), Error> {

        let start = SystemTime::now();
        let max_wait = std::time::Duration::new(Self::SCSI_TAPE_DEFAULT_TIMEOUT as u64, 0);

        loop {
            match self.test_unit_ready() {
                Ok(()) => return Ok(()),
                _ => {
                    std::thread::sleep(std::time::Duration::new(1, 0));
                    if start.elapsed()? > max_wait {
                        bail!("wait_until_ready failed - got timeout");
                    }
                }
            }
        }
    }

    /// Read Tape Alert Flags
    pub fn tape_alert_flags(&mut self) -> Result<TapeAlertFlags, Error> {
        read_tape_alert_flags(&mut self.file)
    }

    /// Read Cartridge Memory (MAM Attributes)
    pub fn cartridge_memory(&mut self) -> Result<Vec<MamAttribute>, Error> {
        read_mam_attributes(&mut self.file)
    }

    /// Read Volume Statistics
    pub fn volume_statistics(&mut self) -> Result<Lp17VolumeStatistics, Error> {
        return read_volume_statistics(&mut self.file);
    }

    pub fn set_encryption(
        &mut self,
        key: Option<[u8; 32]>,
    ) -> Result<(), Error> {

        self.encryption_key_loaded = key.is_some();

        set_encryption(&mut self.file, key)
    }

    // Note: use alloc_page_aligned_buffer to alloc data transfer buffer
    //
    // Returns true if the drive reached the Logical End Of Media (early warning)
    fn write_block(&mut self, data: &[u8]) -> Result<bool, std::io::Error> {

        let transfer_len = data.len();

        if transfer_len > 0x800000 {
           proxmox::io_bail!("write failed - data too large");
        }

        let mut sg_raw = SgRaw::new(&mut self.file, 0)
            .unwrap(); // cannot fail with size 0

        sg_raw.set_timeout(Self::SCSI_TAPE_DEFAULT_TIMEOUT);
        let mut cmd = Vec::new();
        cmd.push(0x0A);  // WRITE
        cmd.push(0x00); // VARIABLE SIZED BLOCKS
        cmd.push(((transfer_len >> 16) & 0xff) as u8);
        cmd.push(((transfer_len >> 8) & 0xff) as u8);
        cmd.push((transfer_len & 0xff) as u8);
        cmd.push(0); // control byte

        //println!("WRITE {:?}", cmd);
        //println!("WRITE {:?}", data);

        match sg_raw.do_out_command(&cmd, data) {
            Ok(()) => { return Ok(false) }
            Err(ScsiError::Sense(SenseInfo { sense_key: 0, asc: 0, ascq: 2 })) => {
                return Ok(true); // LEOM
            }
            Err(err) => {
                proxmox::io_bail!("write failed - {}", err);
            }
        }
    }

    fn read_block(&mut self, buffer: &mut [u8]) -> Result<usize, BlockReadError> {
        let transfer_len = buffer.len();

        if transfer_len > 0xFFFFFF {
            return Err(BlockReadError::Error(
                proxmox::io_format_err!("read failed - buffer too large")
            ));
        }

        let mut sg_raw = SgRaw::new(&mut self.file, 0)
            .unwrap(); // cannot fail with size 0

        sg_raw.set_timeout(Self::SCSI_TAPE_DEFAULT_TIMEOUT);
        let mut cmd = Vec::new();
        cmd.push(0x08); // READ
        cmd.push(0x02); // VARIABLE SIZED BLOCKS, SILI=1
        //cmd.push(0x00); // VARIABLE SIZED BLOCKS, SILI=0
        cmd.push(((transfer_len >> 16) & 0xff) as u8);
        cmd.push(((transfer_len >> 8) & 0xff) as u8);
        cmd.push((transfer_len & 0xff) as u8);
        cmd.push(0); // control byte

        let data = match sg_raw.do_in_command(&cmd, buffer) {
            Ok(data) => data,
            Err(ScsiError::Sense(SenseInfo { sense_key: 0, asc: 0, ascq: 1 })) => {
                return Err(BlockReadError::EndOfFile);
            }
            Err(ScsiError::Sense(SenseInfo { sense_key: 8, asc: 0, ascq: 5 })) => {
                return Err(BlockReadError::EndOfStream);
            }
            Err(err) => {
                return Err(BlockReadError::Error(
                    proxmox::io_format_err!("read failed - {}", err)
                ));
            }
        };

        if data.len() != transfer_len {
            return Err(BlockReadError::Error(
                proxmox::io_format_err!("read failed - unexpected block len ({} != {})", data.len(), buffer.len())
            ));
        }

        Ok(transfer_len)
    }

    pub fn open_writer(&mut self) -> BlockedWriter<SgTapeWriter> {
        let writer = SgTapeWriter::new(self);
        BlockedWriter::new(writer)
    }

    pub fn open_reader(&mut self) -> Result<BlockedReader<SgTapeReader>, BlockReadError> {
        let reader = SgTapeReader::new(self);
        BlockedReader::open(reader)
    }

    /// Set important drive options
    pub fn set_drive_options(
        &mut self,
        compression: Option<bool>,
        block_length: Option<u32>,
        buffer_mode: Option<bool>,
    ) -> Result<(), Error> {

        // Note: Read/Modify/Write

        let (mut head, mut block_descriptor, mut page) = self.read_compression_page()?;

        let mut sg_raw = SgRaw::new(&mut self.file, 0)?;
        sg_raw.set_timeout(Self::SCSI_TAPE_DEFAULT_TIMEOUT);

        head.mode_data_len = 0; // need to b e zero

        if let Some(compression) = compression {
            page.set_compression(compression);
        }

        if let Some(block_length) = block_length {
            block_descriptor.set_block_length(block_length)?;
        }

        if let Some(buffer_mode) = buffer_mode {
            head.set_buffer_mode(buffer_mode);
        }

        let mut data = Vec::new();
        unsafe {
            data.write_be_value(head)?;
            data.write_be_value(block_descriptor)?;
            data.write_be_value(page)?;
        }

        let mut cmd = Vec::new();
        cmd.push(0x55); // MODE SELECT(10)
        cmd.push(0b0001_0000); // PF=1
        cmd.extend(&[0,0,0,0,0]); //reserved

        let param_list_len: u16 = data.len() as u16;
        cmd.extend(&param_list_len.to_be_bytes());
        cmd.push(0); // control

        let mut buffer = alloc_page_aligned_buffer(4096)?;

        buffer[..data.len()].copy_from_slice(&data[..]);

        sg_raw.do_out_command(&cmd, &buffer[..data.len()])
            .map_err(|err| format_err!("set drive options failed - {}", err))?;

        Ok(())
    }

    fn read_medium_configuration_page(
        &mut self,
    ) -> Result<(ModeParameterHeader, ModeBlockDescriptor, MediumConfigurationModePage), Error> {

        let (head, block_descriptor, page): (_,_, MediumConfigurationModePage)
            = scsi_mode_sense(&mut self.file, false, 0x1d, 0)?;

        proxmox::try_block!({
            if (page.page_code & 0b0011_1111) != 0x1d {
                bail!("wrong page code {}", page.page_code);
            }
            if page.page_length != 0x1e {
                bail!("wrong page length {}", page.page_length);
            }

            let block_descriptor = match block_descriptor {
                Some(block_descriptor) => block_descriptor,
                None => bail!("missing block descriptor"),
            };

            Ok((head, block_descriptor, page))
        }).map_err(|err| format_err!("read_medium_configuration failed - {}", err))
    }

    fn read_compression_page(
        &mut self,
    ) -> Result<(ModeParameterHeader, ModeBlockDescriptor, DataCompressionModePage), Error> {

        let (head, block_descriptor, page): (_,_, DataCompressionModePage)
            = scsi_mode_sense(&mut self.file, false, 0x0f, 0)?;

        proxmox::try_block!({
            if (page.page_code & 0b0011_1111) != 0x0f {
                bail!("wrong page code {}", page.page_code);
            }
            if page.page_length != 0x0e {
                bail!("wrong page length {}", page.page_length);
            }

            let block_descriptor = match block_descriptor {
                Some(block_descriptor) => block_descriptor,
                None => bail!("missing block descriptor"),
            };

            Ok((head, block_descriptor, page))
        }).map_err(|err| format_err!("read_compression_page failed: {}", err))
    }

    /// Read drive options/status
    ///
    /// We read the drive compression page, including the
    /// block_descriptor. This is all information we need for now.
    pub fn read_drive_status(&mut self) -> Result<LtoTapeStatus, Error> {

        // We do a Request Sense, but ignore the result.
        // This clears deferred error or media changed events.
        let _ = scsi_request_sense(&mut self.file);

        let (head, block_descriptor, page) = self.read_compression_page()?;

        Ok(LtoTapeStatus {
            block_length: block_descriptor.block_length(),
            write_protect: head.write_protect(),
            buffer_mode: head.buffer_mode(),
            compression: page.compression_enabled(),
            density_code: block_descriptor.density_code,
        })
    }
}

impl Drop for SgTape {
    fn drop(&mut self) {
        // For security reasons, clear the encryption key
        if self.encryption_key_loaded {
            let _ = self.set_encryption(None);
        }
    }
}


pub struct SgTapeReader<'a> {
    sg_tape: &'a mut SgTape,
    end_of_file: bool,
}

impl <'a> SgTapeReader<'a> {

    pub fn new(sg_tape: &'a mut SgTape) -> Self {
        Self { sg_tape, end_of_file: false, }
    }
}

impl <'a> BlockRead for SgTapeReader<'a> {

    fn read_block(&mut self, buffer: &mut [u8]) -> Result<usize, BlockReadError> {
        if self.end_of_file {
            return Err(BlockReadError::Error(proxmox::io_format_err!("detected read after EOF!")));
        }
        match self.sg_tape.read_block(buffer) {
            Ok(usize) => Ok(usize),
            Err(BlockReadError::EndOfFile) => {
                self.end_of_file = true;
                Err(BlockReadError::EndOfFile)
            },
            Err(err) => Err(err),
        }
    }
}

pub struct SgTapeWriter<'a> {
    sg_tape: &'a mut SgTape,
    _leom_sent: bool,
}

impl <'a> SgTapeWriter<'a> {

    pub fn new(sg_tape: &'a mut SgTape) -> Self {
        Self { sg_tape, _leom_sent: false }
    }
}

impl <'a> BlockWrite for SgTapeWriter<'a> {

    fn write_block(&mut self, buffer: &[u8]) -> Result<bool, std::io::Error> {
        self.sg_tape.write_block(buffer)
    }

    fn write_filemark(&mut self) -> Result<(), std::io::Error> {
        self.sg_tape.write_filemarks(1, true)
    }
}
