use std::fs::File;
use std::num::NonZeroUsize;
use std::path::Path;
use std::sync::Mutex;

use crate::error::MdictError;
use crate::mdict::recordblock::record_block_parser;
use bytes::Bytes;
use lru::LruCache;
use memmap2::MmapOptions;
use nom::Parser;

/// 解压后记录块缓存的总字节预算上限。
/// 采用**字节预算**而非固定条数：历史上是固定 64 条，而单个 record
/// block 解压后可达 256 MiB，理论上 64 条可吃满约 16 GiB 内存；改为字节
/// 预算后峰值被钉死在 `BLOCK_CACHE_BUDGET_BYTES`，与 onedict 的 64 MiB
/// 对齐。
const BLOCK_CACHE_BUDGET_BYTES: usize = 64 * 1024 * 1024;

/// 条数兜底上限。
///
/// 字节预算是真正的约束；这里再给一个有限的条数上限，以防退化场景
/// （大量极小块）导致条数无界膨胀。常态下字节预算先于条数生效。注意
/// 一旦真的触达条数上限，`lru` 会自行按 LRU 驱逐（不经我们记账），只会
/// 令 `used` 低估（偏多驱逐），内存不会超预算——安全退化。
const BLOCK_CACHE_MAX_ENTRIES: usize = 4096;

#[derive(Debug, Hash, PartialEq, Eq)]
struct BlockKey {
    offset: usize,
    csize: usize,
    dsize: usize,
}

/// 按字节预算驱逐的 record block 缓存。
///
/// `lru::LruCache` 只懂条数；这里在它之上叠一层 `used` 字节计数：每次
/// 插入前先把最少使用的条目驱逐到"新块能放下"为止。单个块大于预算的
/// 永不入缓存（入也只会把整缓存驱逐光且仍超预算），但仍会返回给调用方。
struct BlockCache {
    budget: usize,
    used: usize,
    cache: LruCache<BlockKey, Bytes>,
}

impl BlockCache {
    fn new(budget: usize) -> Self {
        Self {
            budget,
            used: 0,
            // 条数容量放至一个宽裕的有限上限；驱逐主要由字节预算驱动。
            cache: LruCache::new(NonZeroUsize::new(BLOCK_CACHE_MAX_ENTRIES).unwrap()),
        }
    }

    fn get(&mut self, key: &BlockKey) -> Option<Bytes> {
        self.cache.get(key).cloned()
    }

    fn put(&mut self, key: BlockKey, val: Bytes) {
        let size = val.len();
        // 若已存在同 key 旧条目，先移除并扣回它占的字节，避免重复计数。
        if let Some(old) = self.cache.pop(&key) {
            self.used = self.used.saturating_sub(old.len());
        }
        // 单块就超预算：不缓存（缓存它会把整缓存驱逐光且仍超），直接放行返回。
        if size > self.budget {
            return;
        }
        // 逐出最久未用条目，直到新块放得下。
        while self
            .used
            .checked_add(size)
            .is_none_or(|total| total > self.budget)
            && !self.cache.is_empty()
        {
            match self.cache.pop_lru() {
                Some((_, evicted)) => self.used = self.used.saturating_sub(evicted.len()),
                None => break,
            }
        }
        self.used += size;
        // 上方已 pop 掉同 key，put 必返回 None。
        let _ = self.cache.put(key, val);
    }

    #[cfg(test)]
    fn used(&self) -> usize {
        self.used
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.cache.len()
    }
}

pub struct MdxReader {
    mmap: memmap2::Mmap,
    block_cache: Mutex<BlockCache>,
}

impl MdxReader {
    pub fn new(path: impl AsRef<Path>) -> Result<Self, MdictError> {
        let file = File::open(path)?;
        let mmap = unsafe { MmapOptions::new().map(&file)? };
        Ok(Self {
            mmap,
            block_cache: Mutex::new(BlockCache::new(BLOCK_CACHE_BUDGET_BYTES)),
        })
    }

    /// 读取一条 record（必要时解压所在 record block 并按字节预算缓存）。
    ///
    /// 返回 [`MdictError`]：块越界、不支持加密、解压失败等都能被上游精确
    /// 识别，而非一段 `nom::Err` Debug。
    pub fn read_record(
        &self,
        block_offset: usize,
        block_csize: usize,
        block_dsize: usize,
        record_offset: usize,
        record_length: usize,
    ) -> Result<Bytes, MdictError> {
        let block_end = block_offset.checked_add(block_csize).ok_or_else(|| {
            MdictError::CorruptInput(format!(
                "block end overflow: offset {block_offset} + size {block_csize}"
            ))
        })?;
        if block_end > self.mmap.len() {
            return Err(MdictError::BlockOutOfBounds {
                offset: block_offset,
                csize: block_csize,
                file_size: self.mmap.len(),
            });
        }
        let key = BlockKey {
            offset: block_offset,
            csize: block_csize,
            dsize: block_dsize,
        };

        let block_decompressed = {
            let mut cache = self.block_cache.lock().expect("block cache mutex poisoned");
            if let Some(cached) = cache.get(&key) {
                cached
            } else {
                let block_buf = &self.mmap[block_offset..block_end];
                // 前置拒绝不支持的加密方法（enc_method == 2，即
                // RegCode/商业加密）：与其让 LZO/zlib/Ripemd128 走一遭注定
                // 失败的路径，不如直接给出可读错误。
                if block_buf.len() >= 4 {
                    let enc = u32::from_le_bytes([
                        block_buf[0],
                        block_buf[1],
                        block_buf[2],
                        block_buf[3],
                    ]);
                    let enc_method = (enc >> 4) & 0xf;
                    if enc_method == 2 {
                        return Err(MdictError::UnsupportedEncryption { method: 2 });
                    }
                }
                let (_, decompressed) = record_block_parser(block_csize, block_dsize)
                    .parse(block_buf)
                    .map_err(|e| {
                        MdictError::CorruptInput(format!("record block parse error: {e:?}"))
                    })?;
                let decompressed = Bytes::from(decompressed);
                cache.put(key, decompressed.clone());
                decompressed
            }
        };

        // 边界检查
        let record_end = record_offset.checked_add(record_length).ok_or_else(|| {
            MdictError::CorruptInput(format!(
                "record end overflow: offset {record_offset} + length {record_length}"
            ))
        })?;
        if record_end > block_decompressed.len() {
            return Err(MdictError::CorruptInput(format!(
                "record out of bounds: offset {record_offset} + length {record_length} > block size {}",
                block_decompressed.len()
            )));
        }

        Ok(block_decompressed.slice(record_offset..record_end))
    }
}

#[cfg(test)]
mod tests {
    use super::BlockCache;
    use bytes::Bytes;

    fn key(i: usize) -> super::BlockKey {
        super::BlockKey {
            offset: i * 1000,
            csize: 100,
            dsize: 100,
        }
    }

    #[test]
    fn byte_budget_evicts_lru_entries() {
        // 200-byte budget; insert three 80-byte blocks: total 240 > 200,
        // so the least-recently-used one must be evicted.
        let mut cache = BlockCache::new(200);
        cache.put(key(0), Bytes::from(vec![0u8; 80]));
        cache.put(key(1), Bytes::from(vec![0u8; 80]));
        // touch key(0) so key(1) becomes LRU
        let _ = cache.get(&key(0));
        cache.put(key(2), Bytes::from(vec![0u8; 80]));

        // 80 + 80 = 160 fits two entries; the third evicted the LRU (key(1)).
        assert_eq!(cache.len(), 2);
        assert!(cache.used() <= 200);
        // key(0) and key(2) should still be present.
        assert!(cache.get(&key(0)).is_some());
        assert!(cache.get(&key(2)).is_some());
        // key(1) was evicted.
        assert!(cache.get(&key(1)).is_none());
    }

    #[test]
    fn oversized_block_is_not_cached_but_returned() {
        // 100-byte budget; a 150-byte block is bigger than the whole budget.
        let mut cache = BlockCache::new(100);
        cache.put(key(0), Bytes::from(vec![0u8; 150]));
        assert_eq!(cache.len(), 0);
        assert_eq!(cache.used(), 0);
    }
}
