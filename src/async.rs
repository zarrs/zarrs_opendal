use futures::stream::TryStreamExt;
use futures::{future, StreamExt};
use opendal::Operator;

use zarrs_storage::byte_range::ByteRangeIterator;
use zarrs_storage::{
    byte_range::InvalidByteRangeError, AsyncListableStorageTraits, AsyncReadableStorageTraits,
    AsyncWritableStorageTraits, StorageError, StoreKey, StoreKeys, StoreKeysPrefixes, StorePrefix,
};
use zarrs_storage::{AsyncMaybeBytesIterator, Bytes, MaybeBytes, OffsetBytesIterator};

use crate::{handle_result, handle_result_notfound};

/// An asynchronous store backed by an [`opendal::Operator`].
pub struct AsyncOpendalStore {
    operator: Operator,
    concurrent_stat_requests: usize,
}

impl AsyncOpendalStore {
    /// Create a new [`AsyncOpendalStore`].
    #[must_use]
    pub fn new(operator: Operator) -> Self {
        Self {
            operator,
            concurrent_stat_requests: 32,
        }
    }

    /// Set the number of concurrent stat requests.
    #[must_use]
    pub fn with_concurrent_stat_requests(mut self, concurrent_stat_requests: usize) -> Self {
        self.concurrent_stat_requests = concurrent_stat_requests;
        self
    }
}

#[async_trait::async_trait]
impl AsyncReadableStorageTraits for AsyncOpendalStore {
    async fn get(&self, key: &StoreKey) -> Result<MaybeBytes, StorageError> {
        handle_result_notfound(
            self.operator
                .read(key.as_str())
                .await
                .map(|buf| buf.to_bytes()),
        )
    }

    async fn get_partial_many<'a>(
        &'a self,
        key: &StoreKey,
        byte_ranges: ByteRangeIterator<'a>,
    ) -> Result<AsyncMaybeBytesIterator<'a>, StorageError> {
        let (size, reader) = futures::join!(self.size_key(key), self.operator.reader(key.as_str()));
        if let (Some(size), Some(reader)) = (size?, handle_result_notfound(reader)?) {
            let byte_ranges_fetch = byte_ranges
                .map(|byte_range| {
                    let byte_range_opendal = byte_range.to_range(size);
                    if byte_range_opendal.end > size {
                        Err(InvalidByteRangeError::new(byte_range, size).into())
                    } else {
                        Ok(byte_range_opendal)
                    }
                })
                .collect::<Result<Vec<_>, StorageError>>()?;

            let result = handle_result(reader.fetch(byte_ranges_fetch).await)?;
            Ok(Some(
                futures::stream::iter(result.into_iter().map(|b| Ok(b.to_bytes()))).boxed(),
            ))
        } else {
            Ok(None)
        }
    }

    async fn size_key(&self, key: &StoreKey) -> Result<Option<u64>, StorageError> {
        Ok(
            handle_result_notfound(self.operator.stat(key.as_str()).await)?
                .map(|metadata| metadata.content_length()),
        )
    }
}

#[async_trait::async_trait]
impl AsyncWritableStorageTraits for AsyncOpendalStore {
    async fn set(&self, key: &StoreKey, value: Bytes) -> Result<(), StorageError> {
        handle_result(self.operator.write(key.as_str(), value).await)?;
        Ok(())
    }

    async fn set_partial_many<'a>(
        &'a self,
        key: &StoreKey,
        offset_values: OffsetBytesIterator<'a>,
    ) -> Result<(), StorageError> {
        zarrs_storage::async_store_set_partial_many(self, key, offset_values).await
    }

    async fn erase(&self, key: &StoreKey) -> Result<(), StorageError> {
        handle_result(self.operator.delete(key.as_str()).await)
    }

    async fn erase_prefix(&self, prefix: &StorePrefix) -> Result<(), StorageError> {
        handle_result(self.operator.remove_all(prefix.as_str()).await)
    }
}

#[async_trait::async_trait]
impl AsyncListableStorageTraits for AsyncOpendalStore {
    async fn list(&self) -> Result<StoreKeys, StorageError> {
        self.list_prefix(&StorePrefix::root()).await
    }

    async fn list_prefix(&self, prefix: &StorePrefix) -> Result<StoreKeys, StorageError> {
        handle_result_notfound(
            self.operator
                .list_with(prefix.as_str())
                .recursive(true)
                .await,
        )?
        .map_or_else(
            || Ok(vec![]),
            |list_with_prefix| {
                let mut list = list_with_prefix
                    .into_iter()
                    .filter_map(|entry| {
                        if entry.metadata().mode() == opendal::EntryMode::FILE {
                            Some(StoreKey::try_from(entry.path()))
                        } else {
                            None
                        }
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                list.sort();
                Ok(list)
            },
        )
    }

    async fn list_dir(&self, prefix: &StorePrefix) -> Result<StoreKeysPrefixes, StorageError> {
        handle_result_notfound(
            self.operator
                .list_with(prefix.as_str())
                .recursive(false)
                .await,
        )?
        .map_or_else(
            || Ok(StoreKeysPrefixes::new(vec![], vec![])),
            |entries| {
                let mut prefixes = Vec::<StorePrefix>::with_capacity(entries.len());
                let mut keys = Vec::<StoreKey>::with_capacity(entries.len());
                for entry in entries {
                    match entry.metadata().mode() {
                        opendal::EntryMode::FILE => {
                            keys.push(StoreKey::try_from(entry.path())?);
                        }
                        opendal::EntryMode::DIR => {
                            let prefix_entry = StorePrefix::try_from(entry.path())?;
                            if &prefix_entry != prefix {
                                prefixes.push(StorePrefix::try_from(entry.path())?);
                            }
                        }
                        opendal::EntryMode::Unknown => {}
                    }
                }
                keys.sort();
                prefixes.sort();
                Ok(StoreKeysPrefixes::new(keys, prefixes))
            },
        )
    }

    async fn size_prefix(&self, prefix: &StorePrefix) -> Result<u64, StorageError> {
        let lister = handle_result(
            self.operator
                .lister_with(prefix.as_str())
                .recursive(true)
                .await,
        )?;

        let total_size = lister
            .try_filter(|entry| future::ready(entry.metadata().mode().is_file()))
            .map(move |entry_result| async move {
                let entry = handle_result(entry_result)?;
                let metadata = handle_result(self.operator.stat(entry.path()).await)?;
                Ok::<_, StorageError>(metadata.content_length())
            })
            .buffer_unordered(self.concurrent_stat_requests)
            .try_fold(0, |acc, size| future::ready(Ok(acc + size)))
            .await?;

        Ok(total_size)
    }
}
