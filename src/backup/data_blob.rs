use failure::*;
use std::convert::TryInto;

use proxmox::tools::io::{ReadExt, WriteExt};

const MAX_BLOB_SIZE: usize = 128*1024*1024;

use super::*;

/// Data blob binary storage format
///
/// Data blobs store arbitrary binary data (< 128MB), and can be
/// compressed and encrypted. A simply binary format is used to store
/// them on disk or transfer them over the network. Please use index
/// files to store large data files (".fidx" of ".didx").
///
pub struct DataBlob {
    raw_data: Vec<u8>, // tagged, compressed, encryped data
}

impl DataBlob {

    pub fn header_size(magic: &[u8; 8]) -> usize {
        match magic {
            &UNCOMPRESSED_CHUNK_MAGIC_1_0 => std::mem::size_of::<DataChunkHeader>(),
            &COMPRESSED_CHUNK_MAGIC_1_0 => std::mem::size_of::<DataChunkHeader>(),
            &ENCRYPTED_CHUNK_MAGIC_1_0 => std::mem::size_of::<EncryptedDataChunkHeader>(),
            &ENCR_COMPR_CHUNK_MAGIC_1_0 => std::mem::size_of::<EncryptedDataChunkHeader>(),

            &UNCOMPRESSED_BLOB_MAGIC_1_0 => std::mem::size_of::<DataBlobHeader>(),
            &COMPRESSED_BLOB_MAGIC_1_0 => std::mem::size_of::<DataBlobHeader>(),
            &ENCRYPTED_BLOB_MAGIC_1_0 => std::mem::size_of::<EncryptedDataBlobHeader>(),
            &ENCR_COMPR_BLOB_MAGIC_1_0 => std::mem::size_of::<EncryptedDataBlobHeader>(),
            &AUTHENTICATED_BLOB_MAGIC_1_0 => std::mem::size_of::<AuthenticatedDataBlobHeader>(),
            &AUTH_COMPR_BLOB_MAGIC_1_0 => std::mem::size_of::<AuthenticatedDataBlobHeader>(),
            _ => panic!("unknown blob magic"),
        }
    }

    /// accessor to raw_data field
    pub fn raw_data(&self) -> &[u8]  {
        &self.raw_data
    }

    /// Consume self and returns raw_data
    pub fn into_inner(self) -> Vec<u8> {
        self.raw_data
    }

    /// accessor to chunk type (magic number)
    pub fn magic(&self) -> &[u8; 8] {
        self.raw_data[0..8].try_into().unwrap()
    }

    /// accessor to crc32 checksum
    pub fn crc(&self) -> u32 {
        let crc_o = proxmox::tools::offsetof!(DataBlobHeader, crc);
        u32::from_le_bytes(self.raw_data[crc_o..crc_o+4].try_into().unwrap())
    }

    // set the CRC checksum field
    pub fn set_crc(&mut self, crc: u32) {
        let crc_o = proxmox::tools::offsetof!(DataBlobHeader, crc);
        self.raw_data[crc_o..crc_o+4].copy_from_slice(&crc.to_le_bytes());
    }

    /// compute the CRC32 checksum
    pub fn compute_crc(&self) -> u32 {
        let mut hasher = crc32fast::Hasher::new();
        let start = Self::header_size(self.magic()); // start after HEAD
        hasher.update(&self.raw_data[start..]);
        hasher.finalize()
    }

    /// verify the CRC32 checksum
    pub fn verify_crc(&self) -> Result<(), Error> {
        let expected_crc = self.compute_crc();
        if expected_crc != self.crc() {
            bail!("Data blob has wrong CRC checksum.");
        }
        Ok(())
    }

    /// Create a DataBlob, optionally compressed and/or encrypted
    pub fn encode(
        data: &[u8],
        config: Option<&CryptConfig>,
        compress: bool,
    ) -> Result<Self, Error> {

        if data.len() > MAX_BLOB_SIZE {
            bail!("data blob too large ({} bytes).", data.len());
        }

        let mut blob = if let Some(config) = config {

            let compr_data;
            let (_compress, data, magic) = if compress {
                compr_data = zstd::block::compress(data, 1)?;
                // Note: We only use compression if result is shorter
                if compr_data.len() < data.len() {
                    (true, &compr_data[..], ENCR_COMPR_BLOB_MAGIC_1_0)
                } else {
                    (false, data, ENCRYPTED_BLOB_MAGIC_1_0)
                }
            } else {
                (false, data, ENCRYPTED_BLOB_MAGIC_1_0)
            };

            let header_len = std::mem::size_of::<EncryptedDataBlobHeader>();
            let mut raw_data = Vec::with_capacity(data.len() + header_len);

            let dummy_head = EncryptedDataBlobHeader {
                head: DataBlobHeader { magic: [0u8; 8], crc: [0; 4] },
                iv: [0u8; 16],
                tag: [0u8; 16],
            };
            unsafe {
                raw_data.write_le_value(dummy_head)?;
            }

            let (iv, tag) = config.encrypt_to(data, &mut raw_data)?;

            let head = EncryptedDataBlobHeader {
                head: DataBlobHeader { magic, crc: [0; 4] }, iv, tag,
            };

            unsafe {
                (&mut raw_data[0..header_len]).write_le_value(head)?;
            }

            DataBlob { raw_data }
        } else {

            let max_data_len = data.len() + std::mem::size_of::<DataBlobHeader>();
            if compress {
                let mut comp_data = Vec::with_capacity(max_data_len);

                let head =  DataBlobHeader {
                    magic: COMPRESSED_BLOB_MAGIC_1_0,
                    crc: [0; 4],
                };
                unsafe {
                    comp_data.write_le_value(head)?;
                }

                zstd::stream::copy_encode(data, &mut comp_data, 1)?;

                if comp_data.len() < max_data_len {
                    let mut blob = DataBlob { raw_data: comp_data };
                    blob.set_crc(blob.compute_crc());
                    return Ok(blob);
                }
            }

            let mut raw_data = Vec::with_capacity(max_data_len);

            let head =  DataBlobHeader {
                magic: UNCOMPRESSED_BLOB_MAGIC_1_0,
                crc: [0; 4],
            };
            unsafe {
                raw_data.write_le_value(head)?;
            }
            raw_data.extend_from_slice(data);

            DataBlob { raw_data }
        };

        blob.set_crc(blob.compute_crc());

        Ok(blob)
    }

    /// Decode blob data
    pub fn decode(self, config: Option<&CryptConfig>) -> Result<Vec<u8>, Error> {

        let magic = self.magic();

        if magic == &UNCOMPRESSED_BLOB_MAGIC_1_0 {
            let data_start = std::mem::size_of::<DataBlobHeader>();
            return Ok(self.raw_data[data_start..].to_vec());
        } else if magic == &COMPRESSED_BLOB_MAGIC_1_0 {
            let data_start = std::mem::size_of::<DataBlobHeader>();
            let data = zstd::block::decompress(&self.raw_data[data_start..], MAX_BLOB_SIZE)?;
            return Ok(data);
        } else if magic == &ENCR_COMPR_BLOB_MAGIC_1_0 || magic == &ENCRYPTED_BLOB_MAGIC_1_0 {
            let header_len = std::mem::size_of::<EncryptedDataBlobHeader>();
            let head = unsafe {
                (&self.raw_data[..header_len]).read_le_value::<EncryptedDataBlobHeader>()?
            };

            if let Some(config) = config  {
                let data = if magic == &ENCR_COMPR_BLOB_MAGIC_1_0 {
                    config.decode_compressed_chunk(&self.raw_data[header_len..], &head.iv, &head.tag)?
                } else {
                    config.decode_uncompressed_chunk(&self.raw_data[header_len..], &head.iv, &head.tag)?
                };
                return Ok(data);
            } else {
                bail!("unable to decrypt blob - missing CryptConfig");
            }
        } else if magic == &AUTH_COMPR_BLOB_MAGIC_1_0 || magic == &AUTHENTICATED_BLOB_MAGIC_1_0 {
            let header_len = std::mem::size_of::<AuthenticatedDataBlobHeader>();
            let head = unsafe {
                (&self.raw_data[..header_len]).read_le_value::<AuthenticatedDataBlobHeader>()?
            };

            let data_start = std::mem::size_of::<AuthenticatedDataBlobHeader>();

            // Note: only verify if we have a crypt config
            if let Some(config) = config  {
                let signature = config.compute_auth_tag(&self.raw_data[data_start..]);
                if signature != head.tag {
                    bail!("verifying blob signature failed");
                }
            }

            if magic == &AUTH_COMPR_BLOB_MAGIC_1_0 {
                let data = zstd::block::decompress(&self.raw_data[data_start..], 16*1024*1024)?;
                return Ok(data);
            } else {
                return Ok(self.raw_data[data_start..].to_vec());
            }
        } else {
            bail!("Invalid blob magic number.");
        }
    }

    /// Create a signed DataBlob, optionally compressed
    pub fn create_signed(
        data: &[u8],
        config: &CryptConfig,
        compress: bool,
    ) -> Result<Self, Error> {

        if data.len() > MAX_BLOB_SIZE {
            bail!("data blob too large ({} bytes).", data.len());
        }

        let compr_data;
        let (_compress, data, magic) = if compress {
            compr_data = zstd::block::compress(data, 1)?;
            // Note: We only use compression if result is shorter
            if compr_data.len() < data.len() {
                (true, &compr_data[..], AUTH_COMPR_BLOB_MAGIC_1_0)
            } else {
                (false, data, AUTHENTICATED_BLOB_MAGIC_1_0)
            }
        } else {
            (false, data, AUTHENTICATED_BLOB_MAGIC_1_0)
        };

        let header_len = std::mem::size_of::<AuthenticatedDataBlobHeader>();
        let mut raw_data = Vec::with_capacity(data.len() + header_len);

        let head = AuthenticatedDataBlobHeader {
            head: DataBlobHeader { magic, crc: [0; 4] },
            tag: config.compute_auth_tag(data),
        };
        unsafe {
            raw_data.write_le_value(head)?;
        }
        raw_data.extend_from_slice(data);

        let mut blob = DataBlob { raw_data };
        blob.set_crc(blob.compute_crc());

        return Ok(blob);
    }

    /// Create Instance from raw data
    pub fn from_raw(data: Vec<u8>) -> Result<Self, Error> {

        if data.len() < std::mem::size_of::<DataBlobHeader>() {
            bail!("blob too small ({} bytes).", data.len());
        }

        let magic = &data[0..8];

        if magic == ENCR_COMPR_BLOB_MAGIC_1_0 || magic == ENCRYPTED_BLOB_MAGIC_1_0 {

            if data.len() < std::mem::size_of::<EncryptedDataBlobHeader>() {
                bail!("encrypted blob too small ({} bytes).", data.len());
            }

            let blob = DataBlob { raw_data: data };

            Ok(blob)
        } else if magic == COMPRESSED_BLOB_MAGIC_1_0 || magic == UNCOMPRESSED_BLOB_MAGIC_1_0 {

            let blob = DataBlob { raw_data: data };

            Ok(blob)
        } else if magic == AUTH_COMPR_BLOB_MAGIC_1_0 || magic == AUTHENTICATED_BLOB_MAGIC_1_0 {
            if data.len() < std::mem::size_of::<AuthenticatedDataBlobHeader>() {
                bail!("authenticated blob too small ({} bytes).", data.len());
            }

            let blob = DataBlob { raw_data: data };

            Ok(blob)
        } else {
            bail!("unable to parse raw blob - wrong magic");
        }
    }

}

use std::io::{Read, BufRead, BufReader, Write, Seek, SeekFrom};

struct CryptReader<R> {
    reader: R,
    small_read_buf: Vec<u8>,
    block_size: usize,
    crypter: openssl::symm::Crypter,
    finalized: bool,
}

impl <R: BufRead> CryptReader<R> {

    fn new(reader: R, iv: [u8; 16], tag: [u8; 16], config: &CryptConfig) -> Result<Self, Error> {
        let block_size = config.cipher().block_size(); // Note: block size is normally 1 byte for stream ciphers
        if block_size.count_ones() != 1 || block_size > 512 {
            bail!("unexpected Cipher block size {}", block_size);
        }
        let mut crypter = config.data_crypter(&iv, openssl::symm::Mode::Decrypt)?;
        crypter.set_tag(&tag)?;

        Ok(Self { reader, crypter, block_size, finalized: false, small_read_buf: Vec::new() })
    }

    fn finish(self) -> Result<R, Error> {
        if !self.finalized {
            bail!("CryptReader not successfully finalized.");
        }
        Ok(self.reader)
    }
}

impl <R: BufRead> Read for CryptReader<R> {

    fn read(&mut self, buf: &mut [u8]) -> Result<usize, std::io::Error> {
        if self.small_read_buf.len() > 0 {
            let max = if self.small_read_buf.len() > buf.len() {  buf.len() } else { self.small_read_buf.len() };
            let rest = self.small_read_buf.split_off(max);
            buf[..max].copy_from_slice(&self.small_read_buf);
            self.small_read_buf = rest;
            return Ok(max);
        }

        let data = self.reader.fill_buf()?;

        // handle small read buffers
        if buf.len() <= 2*self.block_size {
            let mut outbuf = [0u8; 1024];

            let count = if data.len() == 0 { // EOF
                let written = self.crypter.finalize(&mut outbuf)?;
                self.finalized = true;
                written
            } else {
                let mut read_size = outbuf.len() - self.block_size;
                if read_size > data.len() {
                    read_size = data.len();
                }
                let written = self.crypter.update(&data[..read_size], &mut outbuf)?;
                self.reader.consume(read_size);
                written
            };

            if count > buf.len() {
                buf.copy_from_slice(&outbuf[..buf.len()]);
                self.small_read_buf = outbuf[buf.len()..count].to_vec();
                return Ok(buf.len());
            } else {
                buf[..count].copy_from_slice(&outbuf[..count]);
                return Ok(count);
            }
        } else {
            if data.len() == 0 { // EOF
                let rest = self.crypter.finalize(buf)?;
                self.finalized = true;
                return Ok(rest)
            } else {
                let mut read_size = buf.len() - self.block_size;
                if read_size > data.len() {
                    read_size = data.len();
                }
                let count = self.crypter.update(&data[..read_size], buf)?;
                self.reader.consume(read_size);
                return Ok(count)
            }
        }
    }
}

struct CryptWriter<W> {
    writer: W,
    block_size: usize,
    encr_buf: [u8; 64*1024],
    iv: [u8; 16],
    crypter: openssl::symm::Crypter,
}

impl <W: Write> CryptWriter<W> {

    fn new(writer: W, config: &CryptConfig) -> Result<Self, Error> {
        let mut iv = [0u8; 16];
        proxmox::sys::linux::fill_with_random_data(&mut iv)?;
        let block_size = config.cipher().block_size();

        let crypter = config.data_crypter(&iv, openssl::symm::Mode::Encrypt)?;

        Ok(Self { writer, iv, crypter, block_size, encr_buf: [0u8; 64*1024] })
    }

    fn finish(mut self) ->  Result<(W, [u8; 16], [u8; 16]), Error> {
        let rest = self.crypter.finalize(&mut self.encr_buf)?;
        if rest > 0 {
            self.writer.write_all(&self.encr_buf[..rest])?;
        }

        self.writer.flush()?;

        let mut tag = [0u8; 16];
        self.crypter.get_tag(&mut tag)?;

        Ok((self.writer, self.iv, tag))
    }
}

impl <W: Write> Write for CryptWriter<W> {

    fn write(&mut self, buf: &[u8]) -> Result<usize, std::io::Error> {
        let mut write_size = buf.len();
        if write_size > (self.encr_buf.len() - self.block_size) {
            write_size = self.encr_buf.len() - self.block_size;
        }
        let count = self.crypter.update(&buf[..write_size], &mut self.encr_buf)
            .map_err(|err| {
                std::io::Error::new(
                    std::io::ErrorKind::Other,
                    format!("crypter update failed - {}", err))
            })?;

        self.writer.write_all(&self.encr_buf[..count])?;

        Ok(write_size)
    }

    fn flush(&mut self) -> Result<(), std::io::Error> {
        Ok(())
    }
}

struct ChecksumWriter<'a, W> {
    writer: W,
    hasher: crc32fast::Hasher,
    signer: Option<openssl::sign::Signer<'a>>,
}

impl <'a, W: Write> ChecksumWriter<'a, W> {

    fn new(writer: W, signer: Option<openssl::sign::Signer<'a>>) -> Self {
        let hasher = crc32fast::Hasher::new();
        Self { writer, hasher, signer }
    }

    pub fn finish(mut self) -> Result<(W, u32, Option<[u8; 32]>), Error> {
        let crc = self.hasher.finalize();

        if let Some(ref mut signer) = self.signer {
            let mut tag = [0u8; 32];
            signer.sign(&mut tag)?;
            Ok((self.writer, crc, Some(tag)))
        } else {
            Ok((self.writer, crc, None))
        }
    }
}

impl <'a, W: Write> Write for ChecksumWriter<'a, W> {

    fn write(&mut self, buf: &[u8]) -> Result<usize, std::io::Error> {
        self.hasher.update(buf);
        if let Some(ref mut signer) = self.signer {
            signer.update(buf)
                .map_err(|err| {
                    std::io::Error::new(
                        std::io::ErrorKind::Other,
                        format!("hmac update failed - {}", err))
                })?;
        }
        self.writer.write(buf)
    }

    fn flush(&mut self) -> Result<(), std::io::Error> {
        self.writer.flush()
    }
}

enum BlobWriterState<'a, W: Write> {
    Uncompressed { csum_writer: ChecksumWriter<'a, W> },
    Compressed { compr: zstd::stream::write::Encoder<ChecksumWriter<'a, W>> },
    Signed { csum_writer: ChecksumWriter<'a, W> },
    SignedCompressed { compr: zstd::stream::write::Encoder<ChecksumWriter<'a, W>> },
    Encrypted { crypt_writer: CryptWriter<ChecksumWriter<'a, W>> },
    EncryptedCompressed { compr: zstd::stream::write::Encoder<CryptWriter<ChecksumWriter<'a, W>>> },
}

/// Write compressed data blobs
pub struct DataBlobWriter<'a, W: Write> {
    state: BlobWriterState<'a, W>,
}

impl <'a, W: Write + Seek> DataBlobWriter<'a, W> {

    pub fn new_uncompressed(mut writer: W) -> Result<Self, Error> {
        writer.seek(SeekFrom::Start(0))?;
        let head = DataBlobHeader { magic: UNCOMPRESSED_BLOB_MAGIC_1_0, crc: [0; 4] };
        unsafe {
            writer.write_le_value(head)?;
        }
        let csum_writer = ChecksumWriter::new(writer, None);
        Ok(Self { state: BlobWriterState::Uncompressed { csum_writer }})
    }

    pub fn new_compressed(mut writer: W) -> Result<Self, Error> {
         writer.seek(SeekFrom::Start(0))?;
        let head = DataBlobHeader { magic: COMPRESSED_BLOB_MAGIC_1_0, crc: [0; 4] };
        unsafe {
            writer.write_le_value(head)?;
        }
        let csum_writer = ChecksumWriter::new(writer, None);
        let compr = zstd::stream::write::Encoder::new(csum_writer, 1)?;
        Ok(Self { state: BlobWriterState::Compressed { compr }})
    }

    pub fn new_signed(mut writer: W, config: &'a CryptConfig) -> Result<Self, Error> {
        writer.seek(SeekFrom::Start(0))?;
        let head = AuthenticatedDataBlobHeader {
            head: DataBlobHeader { magic: AUTHENTICATED_BLOB_MAGIC_1_0, crc: [0; 4] },
            tag: [0u8; 32],
        };
        unsafe {
            writer.write_le_value(head)?;
        }
        let signer = config.data_signer();
        let csum_writer = ChecksumWriter::new(writer, Some(signer));
        Ok(Self { state:  BlobWriterState::Signed { csum_writer }})
    }

    pub fn new_signed_compressed(mut writer: W, config: &'a CryptConfig) -> Result<Self, Error> {
        writer.seek(SeekFrom::Start(0))?;
        let head = AuthenticatedDataBlobHeader {
            head: DataBlobHeader { magic: AUTH_COMPR_BLOB_MAGIC_1_0, crc: [0; 4] },
            tag: [0u8; 32],
        };
        unsafe {
            writer.write_le_value(head)?;
        }
        let signer = config.data_signer();
        let csum_writer = ChecksumWriter::new(writer, Some(signer));
        let compr = zstd::stream::write::Encoder::new(csum_writer, 1)?;
        Ok(Self { state: BlobWriterState::SignedCompressed { compr }})
    }

    pub fn new_encrypted(mut writer: W, config: &'a CryptConfig) -> Result<Self, Error> {
        writer.seek(SeekFrom::Start(0))?;
        let head = EncryptedDataBlobHeader {
            head: DataBlobHeader { magic: ENCRYPTED_BLOB_MAGIC_1_0, crc: [0; 4] },
            iv: [0u8; 16],
            tag: [0u8; 16],
        };
        unsafe {
            writer.write_le_value(head)?;
        }

        let csum_writer = ChecksumWriter::new(writer, None);
        let crypt_writer =  CryptWriter::new(csum_writer, config)?;
        Ok(Self { state: BlobWriterState::Encrypted { crypt_writer }})
    }

    pub fn new_encrypted_compressed(mut writer: W, config: &'a CryptConfig) -> Result<Self, Error> {
        writer.seek(SeekFrom::Start(0))?;
        let head = EncryptedDataBlobHeader {
            head: DataBlobHeader { magic: ENCR_COMPR_BLOB_MAGIC_1_0, crc: [0; 4] },
            iv: [0u8; 16],
            tag: [0u8; 16],
        };
        unsafe {
            writer.write_le_value(head)?;
        }

        let csum_writer = ChecksumWriter::new(writer, None);
        let crypt_writer =  CryptWriter::new(csum_writer, config)?;
        let compr = zstd::stream::write::Encoder::new(crypt_writer, 1)?;
        Ok(Self { state: BlobWriterState::EncryptedCompressed { compr }})
    }

    pub fn finish(self) -> Result<W, Error> {
        match self.state {
            BlobWriterState::Uncompressed { csum_writer } => {
                // write CRC
                let (mut writer, crc, _) = csum_writer.finish()?;
                let head = DataBlobHeader { magic: UNCOMPRESSED_BLOB_MAGIC_1_0, crc: crc.to_le_bytes() };

                writer.seek(SeekFrom::Start(0))?;
                unsafe {
                    writer.write_le_value(head)?;
                }

                return Ok(writer)
            }
            BlobWriterState::Compressed { compr } => {
                let csum_writer = compr.finish()?;
                let (mut writer, crc, _) = csum_writer.finish()?;

                let head = DataBlobHeader { magic: COMPRESSED_BLOB_MAGIC_1_0, crc: crc.to_le_bytes() };

                writer.seek(SeekFrom::Start(0))?;
                unsafe {
                    writer.write_le_value(head)?;
                }

                return Ok(writer)
            }
            BlobWriterState::Signed { csum_writer } => {
                let (mut writer, crc, tag) = csum_writer.finish()?;

                let head = AuthenticatedDataBlobHeader {
                    head: DataBlobHeader { magic: AUTHENTICATED_BLOB_MAGIC_1_0, crc: crc.to_le_bytes() },
                    tag: tag.unwrap(),
                };

                writer.seek(SeekFrom::Start(0))?;
                unsafe {
                    writer.write_le_value(head)?;
                }

                return Ok(writer)
            }
            BlobWriterState::SignedCompressed { compr } => {
                let csum_writer = compr.finish()?;
                let (mut writer, crc, tag) = csum_writer.finish()?;

                let head = AuthenticatedDataBlobHeader {
                    head: DataBlobHeader { magic: AUTH_COMPR_BLOB_MAGIC_1_0, crc: crc.to_le_bytes() },
                    tag: tag.unwrap(),
                };

                writer.seek(SeekFrom::Start(0))?;
                unsafe {
                    writer.write_le_value(head)?;
                }

                return Ok(writer)
            }
            BlobWriterState::Encrypted { crypt_writer } => {
                let (csum_writer, iv, tag) = crypt_writer.finish()?;
                let (mut writer, crc, _) = csum_writer.finish()?;

                let head = EncryptedDataBlobHeader {
                    head: DataBlobHeader { magic: ENCRYPTED_BLOB_MAGIC_1_0, crc: crc.to_le_bytes() },
                    iv, tag,
                };
                writer.seek(SeekFrom::Start(0))?;
                unsafe {
                    writer.write_le_value(head)?;
                }
                return Ok(writer)
            }
            BlobWriterState::EncryptedCompressed { compr } => {
                let crypt_writer = compr.finish()?;
                let (csum_writer, iv, tag) = crypt_writer.finish()?;
                let (mut writer, crc, _) = csum_writer.finish()?;

                let head = EncryptedDataBlobHeader {
                    head: DataBlobHeader { magic: ENCR_COMPR_BLOB_MAGIC_1_0, crc: crc.to_le_bytes() },
                    iv, tag,
                };
                writer.seek(SeekFrom::Start(0))?;
                unsafe {
                    writer.write_le_value(head)?;
                }
                return Ok(writer)
            }
        }
    }
}

impl <'a, W: Write + Seek> Write for DataBlobWriter<'a, W> {

    fn write(&mut self, buf: &[u8]) -> Result<usize, std::io::Error> {
        match self.state {
            BlobWriterState::Uncompressed { ref mut csum_writer } => {
                csum_writer.write(buf)
            }
            BlobWriterState::Compressed { ref mut compr } => {
                compr.write(buf)
            }
            BlobWriterState::Signed { ref mut csum_writer } => {
                csum_writer.write(buf)
            }
            BlobWriterState::SignedCompressed { ref mut compr } => {
               compr.write(buf)
            }
            BlobWriterState::Encrypted { ref mut crypt_writer } => {
                crypt_writer.write(buf)
            }
            BlobWriterState::EncryptedCompressed { ref mut compr } => {
                compr.write(buf)
            }
        }
    }

    fn flush(&mut self) -> Result<(), std::io::Error> {
        match self.state {
            BlobWriterState::Uncompressed { ref mut csum_writer } => {
                csum_writer.flush()
            }
            BlobWriterState::Compressed { ref mut compr } => {
                compr.flush()
            }
            BlobWriterState::Signed { ref mut csum_writer } => {
                csum_writer.flush()
            }
            BlobWriterState::SignedCompressed { ref mut compr } => {
                compr.flush()
            }
            BlobWriterState::Encrypted { ref mut crypt_writer } => {
               crypt_writer.flush()
            }
            BlobWriterState::EncryptedCompressed { ref mut compr } => {
                compr.flush()
            }
        }
    }
}

struct ChecksumReader<'a, R> {
    reader: R,
    hasher: crc32fast::Hasher,
    signer: Option<openssl::sign::Signer<'a>>,
}

impl <'a, R: Read> ChecksumReader<'a, R> {

    fn new(reader: R, signer: Option<openssl::sign::Signer<'a>>) -> Self {
        let hasher = crc32fast::Hasher::new();
        Self { reader, hasher, signer }
    }

    pub fn finish(mut self) -> Result<(R, u32, Option<[u8; 32]>), Error> {
        let crc = self.hasher.finalize();

        if let Some(ref mut signer) = self.signer {
            let mut tag = [0u8; 32];
            signer.sign(&mut tag)?;
            Ok((self.reader, crc, Some(tag)))
        } else {
            Ok((self.reader, crc, None))
        }
    }
}

impl <'a, R: Read> Read for ChecksumReader<'a, R> {

    fn read(&mut self, buf: &mut [u8]) -> Result<usize, std::io::Error> {
        let count = self.reader.read(buf)?;
        if count > 0 {
            self.hasher.update(&buf[..count]);
            if let Some(ref mut signer) = self.signer {
                signer.update(&buf[..count])
                    .map_err(|err| {
                        std::io::Error::new(
                            std::io::ErrorKind::Other,
                            format!("hmac update failed - {}", err))
                    })?;
            }
        }
        Ok(count)
    }
}

enum BlobReaderState<'a, R: Read> {
    Uncompressed { expected_crc: u32, csum_reader: ChecksumReader<'a, R> },
    Compressed { expected_crc: u32, decompr: zstd::stream::read::Decoder<BufReader<ChecksumReader<'a, R>>> },
    Signed { expected_crc: u32, expected_hmac: [u8; 32], csum_reader: ChecksumReader<'a, R> },
    SignedCompressed { expected_crc: u32, expected_hmac: [u8; 32], decompr: zstd::stream::read::Decoder<BufReader<ChecksumReader<'a, R>>> },
    Encrypted { expected_crc: u32, decrypt_reader: CryptReader<BufReader<ChecksumReader<'a, R>>> },
    EncryptedCompressed { expected_crc: u32, decompr: zstd::stream::read::Decoder<BufReader<CryptReader<BufReader<ChecksumReader<'a, R>>>>> },
}

/// Read data blobs
pub struct DataBlobReader<'a, R: Read> {
    state: BlobReaderState<'a, R>,
}

impl <'a, R: Read> DataBlobReader<'a, R> {

    pub fn new(mut reader: R, config: Option<&'a CryptConfig>) -> Result<Self, Error> {

        let head: DataBlobHeader = unsafe { reader.read_le_value()? };
        match head.magic {
            UNCOMPRESSED_BLOB_MAGIC_1_0 => {
                let expected_crc = u32::from_le_bytes(head.crc);
                let csum_reader =  ChecksumReader::new(reader, None);
                Ok(Self { state: BlobReaderState::Uncompressed { expected_crc, csum_reader }})
            }
            COMPRESSED_BLOB_MAGIC_1_0 => {
                let expected_crc = u32::from_le_bytes(head.crc);
                let csum_reader =  ChecksumReader::new(reader, None);

                let decompr = zstd::stream::read::Decoder::new(csum_reader)?;
                Ok(Self { state: BlobReaderState::Compressed { expected_crc, decompr }})
            }
            AUTHENTICATED_BLOB_MAGIC_1_0 => {
                let expected_crc = u32::from_le_bytes(head.crc);
                let mut expected_hmac = [0u8; 32];
                reader.read_exact(&mut expected_hmac)?;
                let signer = config.map(|c| c.data_signer());
                let csum_reader = ChecksumReader::new(reader, signer);
                Ok(Self { state: BlobReaderState::Signed { expected_crc, expected_hmac, csum_reader }})
            }
            AUTH_COMPR_BLOB_MAGIC_1_0 => {
                let expected_crc = u32::from_le_bytes(head.crc);
                let mut expected_hmac = [0u8; 32];
                reader.read_exact(&mut expected_hmac)?;
                let signer = config.map(|c| c.data_signer());
                let csum_reader = ChecksumReader::new(reader, signer);

                let decompr = zstd::stream::read::Decoder::new(csum_reader)?;
                Ok(Self { state: BlobReaderState::SignedCompressed { expected_crc, expected_hmac, decompr }})
            }
            ENCRYPTED_BLOB_MAGIC_1_0 => {
                let expected_crc = u32::from_le_bytes(head.crc);
                let mut iv = [0u8; 16];
                let mut expected_tag = [0u8; 16];
                reader.read_exact(&mut iv)?;
                reader.read_exact(&mut expected_tag)?;
                let csum_reader = ChecksumReader::new(reader, None);
                let decrypt_reader = CryptReader::new(BufReader::with_capacity(64*1024, csum_reader), iv, expected_tag, config.unwrap())?;
                Ok(Self { state: BlobReaderState::Encrypted { expected_crc, decrypt_reader }})
            }
            ENCR_COMPR_BLOB_MAGIC_1_0 => {
                let expected_crc = u32::from_le_bytes(head.crc);
                let mut iv = [0u8; 16];
                let mut expected_tag = [0u8; 16];
                reader.read_exact(&mut iv)?;
                reader.read_exact(&mut expected_tag)?;
                let csum_reader = ChecksumReader::new(reader, None);
                let decrypt_reader = CryptReader::new(BufReader::with_capacity(64*1024, csum_reader), iv, expected_tag, config.unwrap())?;
                let decompr = zstd::stream::read::Decoder::new(decrypt_reader)?;
                Ok(Self { state: BlobReaderState::EncryptedCompressed { expected_crc, decompr }})
            }
            _ => bail!("got wrong magic number {:?}", head.magic)
        }
    }

    pub fn finish(self) -> Result<R, Error> {
        match self.state {
            BlobReaderState::Uncompressed { csum_reader, expected_crc } => {
                let (reader, crc, _) = csum_reader.finish()?;
                if crc != expected_crc {
                    bail!("blob crc check failed");
                }
                Ok(reader)
            }
            BlobReaderState::Compressed { expected_crc, decompr } => {
                let csum_reader = decompr.finish().into_inner();
                let (reader, crc, _) = csum_reader.finish()?;
                if crc != expected_crc {
                    bail!("blob crc check failed");
                }
                Ok(reader)
            }
            BlobReaderState::Signed { csum_reader, expected_crc, expected_hmac } => {
                let (reader, crc, hmac) = csum_reader.finish()?;
                if crc != expected_crc {
                    bail!("blob crc check failed");
                }
                if let Some(hmac) = hmac {
                    if hmac != expected_hmac {
                        bail!("blob signature check failed");
                    }
                }
                Ok(reader)
            }
            BlobReaderState::SignedCompressed { expected_crc, expected_hmac, decompr } => {
                let csum_reader = decompr.finish().into_inner();
                let (reader, crc, hmac) = csum_reader.finish()?;
                if crc != expected_crc {
                    bail!("blob crc check failed");
                }
                if let Some(hmac) = hmac {
                    if hmac != expected_hmac {
                        bail!("blob signature check failed");
                    }
                }
                Ok(reader)
            }
            BlobReaderState::Encrypted { expected_crc, decrypt_reader } =>  {
                let csum_reader = decrypt_reader.finish()?.into_inner();
                let (reader, crc, _) = csum_reader.finish()?;
                if crc != expected_crc {
                    bail!("blob crc check failed");
                }
                Ok(reader)
            }
            BlobReaderState::EncryptedCompressed { expected_crc, decompr } => {
                let decrypt_reader = decompr.finish().into_inner();
                let csum_reader = decrypt_reader.finish()?.into_inner();
                let (reader, crc, _) = csum_reader.finish()?;
                if crc != expected_crc {
                    bail!("blob crc check failed");
                }
                Ok(reader)
            }
        }
    }
}

impl <'a, R: BufRead> Read for DataBlobReader<'a, R> {

    fn read(&mut self, buf: &mut [u8]) -> Result<usize, std::io::Error> {
        match &mut self.state {
            BlobReaderState::Uncompressed { csum_reader, .. } => {
                csum_reader.read(buf)
            }
            BlobReaderState::Compressed { decompr, .. } => {
                decompr.read(buf)
            }
            BlobReaderState::Signed { csum_reader, .. } => {
                csum_reader.read(buf)
            }
            BlobReaderState::SignedCompressed { decompr, .. } => {
                decompr.read(buf)
            }
            BlobReaderState::Encrypted { decrypt_reader, .. } =>  {
                decrypt_reader.read(buf)
            }
            BlobReaderState::EncryptedCompressed { decompr, .. } => {
                decompr.read(buf)
            }
        }
    }
}
