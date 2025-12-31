use std::path::Path;
use std::fs::File;
use memmap2::MmapOptions;
use nom::Parser;
use crate::mdict::recordblock::record_block_parser;

pub struct MdxReader {
    mmap: memmap2::Mmap,
}

impl MdxReader {
    pub fn new(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let file = File::open(path)?;
        let mmap = unsafe { MmapOptions::new().map(&file)? };
        Ok(Self { mmap })
    }

    pub fn read_record(
        &self,
        block_offset: usize,
        block_csize: usize,
        block_dsize: usize,
        record_offset: usize,
        record_length: usize,
    ) -> anyhow::Result<Vec<u8>> {
        if block_offset + block_csize > self.mmap.len() {
            return Err(anyhow::anyhow!(
                "Block out of bounds: offset {} + size {} > file size {}",
                block_offset, block_csize, self.mmap.len()
            ));
        }
        let block_buf = &self.mmap[block_offset..block_offset + block_csize];

        let (_, block_decompressed) = record_block_parser(block_csize, block_dsize)
            .parse(block_buf)
            .map_err(|e| anyhow::anyhow!("Parse error: {}", e))?;

        // 边界检查
        if record_offset + record_length > block_decompressed.len() {
             return Err(anyhow::anyhow!(
                "Record out of bounds: offset {} + length {} > block size {}",
                record_offset, record_length, block_decompressed.len()
            ));
        }

        let record_slice = &block_decompressed[record_offset..record_offset + record_length];
        Ok(record_slice.to_vec())
    }
}
