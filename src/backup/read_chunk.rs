use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use anyhow::Error;

use super::crypt_config::CryptConfig;
use super::data_blob::DataBlob;
use super::datastore::DataStore;

/// The ReadChunk trait allows reading backup data chunks (local or remote)
pub trait ReadChunk {
    /// Returns the encoded chunk data
    fn read_raw_chunk(&self, digest: &[u8; 32]) -> Result<DataBlob, Error>;

    /// Returns the decoded chunk data
    fn read_chunk(&self, digest: &[u8; 32]) -> Result<Vec<u8>, Error>;
}

#[derive(Clone)]
pub struct LocalChunkReader {
    store: Arc<DataStore>,
    crypt_config: Option<Arc<CryptConfig>>,
}

impl LocalChunkReader {
    pub fn new(store: Arc<DataStore>, crypt_config: Option<Arc<CryptConfig>>) -> Self {
        Self {
            store,
            crypt_config,
        }
    }
}

impl ReadChunk for LocalChunkReader {
    fn read_raw_chunk(&self, digest: &[u8; 32]) -> Result<DataBlob, Error> {
        self.store.load_chunk(digest)
    }

    fn read_chunk(&self, digest: &[u8; 32]) -> Result<Vec<u8>, Error> {
        let chunk = ReadChunk::read_raw_chunk(self, digest)?;

        let raw_data = chunk.decode(self.crypt_config.as_ref().map(Arc::as_ref))?;

        // fixme: verify digest?

        Ok(raw_data)
    }
}

pub trait AsyncReadChunk: Send {
    /// Returns the encoded chunk data
    fn read_raw_chunk<'a>(
        &'a self,
        digest: &'a [u8; 32],
    ) -> Pin<Box<dyn Future<Output = Result<DataBlob, Error>> + Send + 'a>>;

    /// Returns the decoded chunk data
    fn read_chunk<'a>(
        &'a self,
        digest: &'a [u8; 32],
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, Error>> + Send + 'a>>;
}

impl AsyncReadChunk for LocalChunkReader {
    fn read_raw_chunk<'a>(
        &'a self,
        digest: &'a [u8; 32],
    ) -> Pin<Box<dyn Future<Output = Result<DataBlob, Error>> + Send + 'a>> {
        Box::pin(async move{
            let (path, _) = self.store.chunk_path(digest);

            let raw_data = tokio::fs::read(&path).await?;

            let chunk = DataBlob::load_from_reader(&mut &raw_data[..])?;
           
            Ok(chunk)
        })
    }

    fn read_chunk<'a>(
        &'a self,
        digest: &'a [u8; 32],
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, Error>> + Send + 'a>> {
        Box::pin(async move {
            let chunk = AsyncReadChunk::read_raw_chunk(self, digest).await?;

            let raw_data = chunk.decode(self.crypt_config.as_ref().map(Arc::as_ref))?;

            // fixme: verify digest?

            Ok(raw_data)
        })
    }
}
