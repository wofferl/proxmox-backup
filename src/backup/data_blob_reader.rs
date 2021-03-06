use anyhow::{bail, format_err, Error};
use std::sync::Arc;
use std::io::{Read, BufReader};
use proxmox::tools::io::ReadExt;

use super::*;

enum BlobReaderState<R: Read> {
    Uncompressed { expected_crc: u32, csum_reader: ChecksumReader<R> },
    Compressed { expected_crc: u32, decompr: zstd::stream::read::Decoder<BufReader<ChecksumReader<R>>> },
    Encrypted { expected_crc: u32, decrypt_reader: CryptReader<BufReader<ChecksumReader<R>>> },
    EncryptedCompressed { expected_crc: u32, decompr: zstd::stream::read::Decoder<BufReader<CryptReader<BufReader<ChecksumReader<R>>>>> },
}

/// Read data blobs
pub struct DataBlobReader<R: Read> {
    state: BlobReaderState<R>,
}

// zstd_safe::DCtx is not sync but we are, since
// the only public interface is on mutable reference
unsafe impl<R: Read> Sync for DataBlobReader<R> {}

impl <R: Read> DataBlobReader<R> {

    pub fn new(mut reader: R, config: Option<Arc<CryptConfig>>) -> Result<Self, Error> {

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
            ENCRYPTED_BLOB_MAGIC_1_0 => {
                let config = config.ok_or_else(|| format_err!("unable to read encrypted blob without key"))?;
                let expected_crc = u32::from_le_bytes(head.crc);
                let mut iv = [0u8; 16];
                let mut expected_tag = [0u8; 16];
                reader.read_exact(&mut iv)?;
                reader.read_exact(&mut expected_tag)?;
                let csum_reader = ChecksumReader::new(reader, None);
                let decrypt_reader = CryptReader::new(BufReader::with_capacity(64*1024, csum_reader), iv, expected_tag, config)?;
                Ok(Self { state: BlobReaderState::Encrypted { expected_crc, decrypt_reader }})
            }
            ENCR_COMPR_BLOB_MAGIC_1_0 => {
                let config = config.ok_or_else(|| format_err!("unable to read encrypted blob without key"))?;
                let expected_crc = u32::from_le_bytes(head.crc);
                let mut iv = [0u8; 16];
                let mut expected_tag = [0u8; 16];
                reader.read_exact(&mut iv)?;
                reader.read_exact(&mut expected_tag)?;
                let csum_reader = ChecksumReader::new(reader, None);
                let decrypt_reader = CryptReader::new(BufReader::with_capacity(64*1024, csum_reader), iv, expected_tag, config)?;
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

impl <R: Read> Read for DataBlobReader<R> {

    fn read(&mut self, buf: &mut [u8]) -> Result<usize, std::io::Error> {
        match &mut self.state {
            BlobReaderState::Uncompressed { csum_reader, .. } => {
                csum_reader.read(buf)
            }
            BlobReaderState::Compressed { decompr, .. } => {
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
