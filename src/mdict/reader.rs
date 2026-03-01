use std::fs::File;
use std::num::NonZeroUsize;
use std::path::Path;
use std::sync::Mutex;

use crate::mdict::recordblock::record_block_parser;
use axum::body::Bytes;
use lru::LruCache;
use memmap2::MmapOptions;
use nom::Parser;

const BLOCK_CACHE_SIZE: usize = 64;

#[derive(Debug, Hash, PartialEq, Eq)]
struct BlockKey {
    offset: usize,
    csize: usize,
    dsize: usize,
}

pub struct MdxReader {
    mmap: memmap2::Mmap,
    block_cache: Mutex<LruCache<BlockKey, Bytes>>,
}

impl MdxReader {
    pub fn new(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let file = File::open(path)?;
        let mmap = unsafe { MmapOptions::new().map(&file)? };
        let cache = LruCache::new(NonZeroUsize::new(BLOCK_CACHE_SIZE).unwrap());
        Ok(Self {
            mmap,
            block_cache: Mutex::new(cache),
        })
    }

    pub fn read_record(
        &self,
        block_offset: usize,
        block_csize: usize,
        block_dsize: usize,
        record_offset: usize,
        record_length: usize,
    ) -> anyhow::Result<Bytes> {
        let block_end = block_offset.checked_add(block_csize).ok_or_else(|| {
            anyhow::anyhow!(
                "Block end overflow: offset {} + size {}",
                block_offset,
                block_csize
            )
        })?;
        if block_end > self.mmap.len() {
            return Err(anyhow::anyhow!(
                "Block out of bounds: offset {} + size {} > file size {}",
                block_offset,
                block_csize,
                self.mmap.len()
            ));
        }
        let key = BlockKey {
            offset: block_offset,
            csize: block_csize,
            dsize: block_dsize,
        };

        let block_decompressed = if let Some(cached) = self
            .block_cache
            .lock()
            .expect("block cache mutex poisoned")
            .get(&key)
            .cloned()
        {
            cached
        } else {
            let block_buf = &self.mmap[block_offset..block_end];
            let (_, decompressed) = record_block_parser(block_csize, block_dsize)
                .parse(block_buf)
                .map_err(|e| anyhow::anyhow!("Parse error: {}", e))?;
            let decompressed = Bytes::from(decompressed);
            self.block_cache
                .lock()
                .expect("block cache mutex poisoned")
                .put(key, decompressed.clone());
            decompressed
        };

        // 边界检查
        let record_end = record_offset.checked_add(record_length).ok_or_else(|| {
            anyhow::anyhow!(
                "Record end overflow: offset {} + length {}",
                record_offset,
                record_length
            )
        })?;
        if record_end > block_decompressed.len() {
            return Err(anyhow::anyhow!(
                "Record out of bounds: offset {} + length {} > block size {}",
                record_offset,
                record_length,
                block_decompressed.len()
            ));
        }

        Ok(block_decompressed.slice(record_offset..record_end))
    }
}
