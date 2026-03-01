use crate::mdict::header::parse_header;
use crate::mdict::keyblock::{
    RecordDeBufOffset, parse_key_block_header, parse_key_block_info, parse_key_blocks,
};
use crate::mdict::recordblock::{RecordBlockSize, parse_record_blocks};
use anyhow::{Context, anyhow};

/// 一个record的定位信息：在buf(buf表示所有record_block的bytes)中的offset和在block解压后的offset
/// draw with: https://asciiflow.com/#/
//                   ◄──block_csize───►
//                   ┌────────────────┐
//            block  │                │
//                   └────────────────┘
//                   ▲
//           block_start_in_buf
//
//                   ◄──── block_dsize ───────►
//                   ┌───┬────────────┬───────┐
//     block_decomp  │   │   record   │       │
//                   └───┴────────────┴───────┘
//                       ▲
//           record_start_in_de_block
//
#[derive(Debug)]
pub struct RecordOffsetInfo {
    pub(crate) text: String,
    // record所在block在buf的offset 截取block使用
    pub block_offset_in_buf: usize,
    // 解析block使用
    pub block_csize: usize,
    pub block_dsize: usize,
    // record在解压后的block的offset 和 end
    pub record_start_in_de_block: usize,
    pub record_end_in_de_block: usize,
}

// todo: why can not be String?
#[derive(Debug)]
#[allow(dead_code)]
pub struct Record<'a> {
    pub(crate) text: &'a str,
    pub(crate) definition: String,
}

/// MDX 详细结构见 https://bitbucket.org/xwang/mdict-analysis/src/master/MDX.svg
/// MDX file 结构
/// header: 得到 version encoding encrypted
/// key block header: entry number and checksum
/// key block size info: every key block compressed and decompressed size, for parse key block bytes
/// key block bytes: 根据上面的key block info得到的（csize,dsize）解析得到 Entry list
/// record header: record block size, entry number, record block info size, record block size
/// record block size info: every record block compressed and decompressed size, 用于解析下面的record block
/// record block bytes: entry and definition bytes, parsed by RecordEntry and RecordBlockSize
/// record: 是一条释义
pub struct Mdx {
    pub records_offset: Vec<RecordOffsetInfo>,
    #[allow(unused)]
    pub encoding: String,
    #[allow(unused)]
    pub encrypted: String,
}

impl Mdx {
    /// Parse an MDX file from bytes.
    ///
    /// # Arguments
    /// * `data` - The raw bytes of the MDX file
    ///
    /// # Returns
    /// * `Ok(Mdx)` - Successfully parsed MDX structure
    /// * `Err` - Parse error with context
    ///
    /// # Example
    /// ```ignore
    /// let data = include_bytes!("/file.mdx");
    /// let mdx = Mdx::new(&data)?;
    /// ```
    pub fn new(data: &[u8]) -> anyhow::Result<Mdx> {
        let input_len = data.len();

        let (data, header) = parse_header(data).context("Failed to parse MDX header")?;

        let (data, kbh) = parse_key_block_header(data, &header)
            .map_err(|e| anyhow::anyhow!("Failed to parse key block header: {:?}", e))?;

        let (data, key_blocks_size) = parse_key_block_info(
            data,
            kbh.key_block_info_len,
            kbh.key_block_info_decompressed_len,
            &header,
        )
        .map_err(|e| anyhow::anyhow!("Failed to parse key block info: {:?}", e))?;

        let (data, mut entries) =
            parse_key_blocks(data, kbh.key_blocks_len, &header, &key_blocks_size)
                .map_err(|e| anyhow::anyhow!("Failed to parse key blocks: {:?}", e))?;

        let (rest, record_blocks_size) = parse_record_blocks(data, &header)
            .map_err(|e| anyhow::anyhow!("Failed to parse record blocks: {:?}", e))?;

        let base_offset = input_len - rest.len();

        // 计算position耗时，一次计算就保存下来
        let offset = records_offset(entries.as_mut_slice(), &record_blocks_size, base_offset)
            .context("Failed to calculate record offsets")?;

        Ok(Mdx {
            records_offset: offset,
            encoding: header.encoding,
            encrypted: header.encrypted,
        })
    }

    #[allow(unused)]
    pub fn entries(&self) -> impl Iterator<Item = &RecordOffsetInfo> {
        self.records_offset.iter()
    }
}

/// bytes structure: buf -> block -> record(entry)
fn records_offset(
    records_debuf_index: &mut [RecordDeBufOffset],
    record_blocks_size: &[RecordBlockSize],
    base_offset: usize,
) -> anyhow::Result<Vec<RecordOffsetInfo>> {
    // Pre-allocate capacity for better performance
    let mut positions: Vec<RecordOffsetInfo> = Vec::with_capacity(records_debuf_index.len());
    let mut i: usize = 0;
    let mut pre_blocks_dsize_sum: usize = 0;
    let mut pre_blocks_csize_sum: usize = 0;

    // 同时开始遍历record_blocks_size和entries，每个block包含0或n个entry，
    // 当entry的buf_decompressed_offset > pre_blocks_dsize_sum时 说明当前block已经遍历结束
    for block in record_blocks_size {
        while i < records_debuf_index.len() {
            let record = &records_debuf_index[i];
            let record_offset_in_debuf = record.record_offset_in_debuf;
            let block_end = pre_blocks_dsize_sum
                .checked_add(block.dsize)
                .ok_or_else(|| anyhow!("record block end overflow"))?;

            // 当前entry已经属于下一个block，注意等于号
            if record_offset_in_debuf >= block_end {
                break;
            }

            let record_start_in_de_block = record_offset_in_debuf
                .checked_sub(pre_blocks_dsize_sum)
                .ok_or_else(|| anyhow!("record start offset underflow"))?;
            let record_end_in_de_block = if i < records_debuf_index.len() - 1 {
                let next_entry = &records_debuf_index[i + 1];
                next_entry
                    .record_offset_in_debuf
                    .checked_sub(pre_blocks_dsize_sum)
                    .ok_or_else(|| anyhow!("record end offset underflow"))?
            } else {
                // last entry
                block.dsize
            };
            if record_end_in_de_block < record_start_in_de_block
                || record_end_in_de_block > block.dsize
            {
                return Err(anyhow!("invalid record range in decompressed block"));
            }
            let block_offset_in_buf = base_offset
                .checked_add(pre_blocks_csize_sum)
                .ok_or_else(|| anyhow!("record block offset overflow"))?;

            positions.push(RecordOffsetInfo {
                text: std::mem::take(&mut records_debuf_index[i].text),
                block_offset_in_buf,
                block_csize: block.csize,
                block_dsize: block.dsize,
                record_start_in_de_block,
                record_end_in_de_block,
            });
            i += 1;
        }
        pre_blocks_dsize_sum = pre_blocks_dsize_sum
            .checked_add(block.dsize)
            .ok_or_else(|| anyhow!("accumulate dsize overflow"))?;
        pre_blocks_csize_sum = pre_blocks_csize_sum
            .checked_add(block.csize)
            .ok_or_else(|| anyhow!("accumulate csize overflow"))?;
    }
    Ok(positions)
}
