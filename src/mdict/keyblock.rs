use crate::mdict::header::{Header, Version};
use crate::util::fast_decrypt;
use crate::util::text_len_parser_v1;
use crate::util::text_len_parser_v2;
use crate::util::text_len_parser_v2_utf16;
use adler32::adler32;
use encoding::Encoding;
use encoding::all::{UTF_8, UTF_16LE};
use encoding::label::encoding_from_whatwg_label;
use flate2::read::ZlibDecoder;
use nom::{
    IResult, Parser,
    bytes::complete::{take, take_till},
    combinator::map,
    multi::{length_data, many0},
    number::complete::{be_u32, be_u64},
};
use ripemd::{Digest, Ripemd128};
use std::io::Read;
use tracing::warn;

pub struct KeyBlockHeader {
    #[allow(unused)]
    pub block_num: usize,
    #[allow(unused)]
    pub entry_num: usize,
    // only version >= 2
    #[allow(unused)]
    pub key_block_info_decompressed_len: usize,
    pub key_block_info_len: usize,
    pub key_blocks_len: usize,
}

/// every key block compressed size and decompressed size
/// 用于解析出 RecordEntry list
pub struct KeyBlockSize {
    pub csize: usize,
    pub dsize: usize,
}

/// 词典索引信息, 和实体词典的索引一样，一个text以及一个页码，不过这个页码是整个RecordBlock解压后(叫debuf)的偏移量
#[derive(Debug)]
pub struct RecordDeBufOffset {
    pub text: String,
    // record在所有RecordBlock解压后的起始位置
    pub record_offset_in_debuf: usize,
}

pub fn parse_key_block_header<'a>(
    data: &'a [u8],
    header: &Header,
) -> IResult<&'a [u8], KeyBlockHeader> {
    return match header.version {
        Version::V1 => parse_key_block_header_v1(data),
        Version::V2 => parse_key_block_header_v2(data),
    };

    fn parse_key_block_header_v1(data: &[u8]) -> IResult<&[u8], KeyBlockHeader> {
        let (data, info_buf) = take(16_usize)(data)?;
        // map 接收一个parser和一个匿名fn, 将parser的结果传递给fn后得到返回值
        let (_, kbh) = map(
            (be_u32, be_u32, be_u32, be_u32),
            |(block_num, entry_num, info_len, blocks_len)| KeyBlockHeader {
                block_num: block_num as usize,
                entry_num: entry_num as usize,
                key_block_info_decompressed_len: info_len as usize, // 没有压缩则相等
                key_block_info_len: info_len as usize,
                key_blocks_len: blocks_len as usize,
            },
        )
        .parse(info_buf)?;
        Ok((data, kbh))
    }

    fn parse_key_block_header_v2(data: &[u8]) -> IResult<&[u8], KeyBlockHeader> {
        // 5个元信息 和 v1相比多了一个key_block_info_decompressed_size 和一个 adler32 checksum
        let (data, info_buf) = take(40_usize)(data)?;
        let (data, checksum) = be_u32(data)?;

        // checksum info_buf
        let computed = adler32(info_buf).map_err(|_| {
            nom::Err::Failure(nom::error::Error::new(
                info_buf,
                nom::error::ErrorKind::Verify,
            ))
        })?;
        if computed != checksum {
            return Err(nom::Err::Failure(nom::error::Error::new(
                info_buf,
                nom::error::ErrorKind::Verify,
            )));
        }
        let (_, kbh) = map(
            (be_u64, be_u64, be_u64, be_u64, be_u64),
            |(
                block_num,
                entry_num,
                key_block_info_decompressed_len,
                key_block_info_len,
                key_blocks_len,
            )| KeyBlockHeader {
                block_num: block_num as usize,
                entry_num: entry_num as usize,
                key_block_info_decompressed_len: key_block_info_decompressed_len as usize,
                key_block_info_len: key_block_info_len as usize,
                key_blocks_len: key_blocks_len as usize,
            },
        )
        .parse(info_buf)?;
        Ok((data, kbh))
    }
}

/// Vec<(usize,usize)>: every key block compressed and decompressed size
pub fn parse_key_block_info<'a>(
    data: &'a [u8],
    block_info_len: usize,
    header: &Header,
) -> IResult<&'a [u8], Vec<KeyBlockSize>> {
    return match &header.version {
        Version::V1 => v1(data, block_info_len),
        Version::V2 => v2(data, block_info_len, &header.encrypted, &header.encoding),
    };

    fn v1(data: &[u8], block_info_len: usize) -> IResult<&[u8], Vec<KeyBlockSize>> {
        let (data, block_info) = take(block_info_len)(data)?;
        let (_, key_blocks_size) = decode_key_blocks_size_v1(block_info)?;
        Ok((data, key_blocks_size))
    }

    fn v2<'a>(
        data: &'a [u8],
        block_info_len: usize,
        encrypted: &str,
        encoding: &str,
    ) -> IResult<&'a [u8], Vec<KeyBlockSize>> {
        let (data, block_info) = take(block_info_len)(data)?;
        if block_info.len() < 8 {
            return Err(nom::Err::Failure(nom::error::Error::new(
                block_info,
                nom::error::ErrorKind::LengthValue,
            )));
        }
        if &block_info[0..4] != b"\x02\x00\x00\x00" {
            return Err(nom::Err::Failure(nom::error::Error::new(
                block_info,
                nom::error::ErrorKind::Verify,
            )));
        }

        let mut key_block_info = vec![];

        // encrypted 可能是 "0", "No", "" 等表示未加密
        if encrypted == "0" || encrypted.eq_ignore_ascii_case("no") || encrypted.is_empty() {
            ZlibDecoder::new(&block_info[8..])
                .read_to_end(&mut key_block_info)
                .map_err(|_| {
                    nom::Err::Failure(nom::error::Error::new(
                        block_info,
                        nom::error::ErrorKind::Fail,
                    ))
                })?;
        }
        // decrypt: encrypted 为 "2" 或 "3" 表示加密
        else if encrypted == "2" || encrypted == "3" {
            let mut md = Ripemd128::new();
            let mut v = Vec::from(&block_info[4..8]);
            let value: u32 = 0x3695;
            v.extend_from_slice(&value.to_le_bytes());
            md.update(v);
            let key = md.finalize();
            let mut d = Vec::from(&block_info[0..8]);
            let decrypted = fast_decrypt(&block_info[8..], key.as_slice());
            d.extend(decrypted);
            ZlibDecoder::new(&d[8..])
                .read_to_end(&mut key_block_info)
                .map_err(|_| {
                    nom::Err::Failure(nom::error::Error::new(
                        block_info,
                        nom::error::ErrorKind::Fail,
                    ))
                })?;
        } else {
            // Unknown encryption flag
            return Err(nom::Err::Failure(nom::error::Error::new(
                block_info,
                nom::error::ErrorKind::Verify,
            )));
        }

        // MDD 文件的 encoding 为空字符串，使用 UTF-16LE 编码
        // 对于 UTF-16，text length 字段是字符数，需要 * 2 得到字节数
        let is_utf16 = encoding.is_empty() || encoding.to_ascii_lowercase().contains("utf-16");
        let key_blocks_size = if is_utf16 {
            let (_, res) = decode_key_blocks_size_v2_utf16(&key_block_info[..]).map_err(|_| {
                nom::Err::Failure(nom::error::Error::new(
                    block_info,
                    nom::error::ErrorKind::Fail,
                ))
            })?;
            res
        } else {
            let (_, res) = decode_key_blocks_size_v2(&key_block_info[..]).map_err(|_| {
                nom::Err::Failure(nom::error::Error::new(
                    block_info,
                    nom::error::ErrorKind::Fail,
                ))
            })?;
            res
        };
        Ok((data, key_blocks_size))
    }

    /// number of entries, num of bytes, first, num of bytes, last?
    fn decode_key_blocks_size_v1(block_info: &[u8]) -> IResult<&[u8], Vec<KeyBlockSize>> {
        let mut parser = many0(map(
            (
                be_u32,
                length_data(text_len_parser_v1),
                length_data(text_len_parser_v1),
                be_u32,
                be_u32,
            ),
            |(_, _, _, csize, dsize)| KeyBlockSize {
                csize: csize as usize,
                dsize: dsize as usize,
            },
        ));
        let (remain, res) = parser.parse(block_info)?;
        if !remain.is_empty() {
            return Err(nom::Err::Failure(nom::error::Error::new(
                remain,
                nom::error::ErrorKind::Eof,
            )));
        }
        Ok((remain, res))
    }

    fn decode_key_blocks_size_v2(block_info: &[u8]) -> IResult<&[u8], Vec<KeyBlockSize>> {
        use tracing::debug;
        debug!(
            "decode_key_blocks_size_v2: block_info len = {}",
            block_info.len()
        );
        if block_info.len() < 10 {
            debug!("block_info too short, returning empty");
            return Err(nom::Err::Failure(nom::error::Error::new(
                block_info,
                nom::error::ErrorKind::LengthValue,
            )));
        }
        debug!(
            "block_info first 16 bytes: {:02x?}",
            &block_info[..std::cmp::min(16, block_info.len())]
        );

        let mut parser = many0(map(
            (
                be_u64,
                length_data(text_len_parser_v2),
                length_data(text_len_parser_v2),
                be_u64,
                be_u64,
            ),
            |(_, _, _, csize, dsize)| KeyBlockSize {
                csize: csize as usize,
                dsize: dsize as usize,
            },
        ));

        let (remain, res) = parser.parse(block_info)?;
        debug!(
            "parsed {} key blocks, remain len = {}",
            res.len(),
            remain.len()
        );
        if !remain.is_empty() {
            debug!(
                "warning: remain bytes = {:02x?}",
                &remain[..std::cmp::min(32, remain.len())]
            );
            return Err(nom::Err::Failure(nom::error::Error::new(
                remain,
                nom::error::ErrorKind::Eof,
            )));
        }
        Ok((remain, res))
    }

    /// UTF-16 版本的 key block info 解析器，用于 MDD 文件
    /// 文本长度字段是字符数，需要 * 2 得到字节数
    fn decode_key_blocks_size_v2_utf16(block_info: &[u8]) -> IResult<&[u8], Vec<KeyBlockSize>> {
        use tracing::debug;
        debug!(
            "decode_key_blocks_size_v2_utf16: block_info len = {}",
            block_info.len()
        );
        if block_info.len() < 10 {
            debug!("block_info too short, returning empty");
            return Err(nom::Err::Failure(nom::error::Error::new(
                block_info,
                nom::error::ErrorKind::LengthValue,
            )));
        }
        debug!(
            "block_info first 16 bytes: {:02x?}",
            &block_info[..std::cmp::min(16, block_info.len())]
        );

        let mut parser = many0(map(
            (
                be_u64,
                length_data(text_len_parser_v2_utf16),
                length_data(text_len_parser_v2_utf16),
                be_u64,
                be_u64,
            ),
            |(_, _, _, csize, dsize)| KeyBlockSize {
                csize: csize as usize,
                dsize: dsize as usize,
            },
        ));

        let (remain, res) = parser.parse(block_info)?;
        debug!(
            "parsed {} key blocks (utf16), remain len = {}",
            res.len(),
            remain.len()
        );
        if !remain.is_empty() {
            debug!(
                "warning: remain bytes = {:02x?}",
                &remain[..std::cmp::min(32, remain.len())]
            );
            return Err(nom::Err::Failure(nom::error::Error::new(
                remain,
                nom::error::ErrorKind::Eof,
            )));
        }
        Ok((remain, res))
    }
}

/// 解析 key blocks
pub fn parse_key_blocks<'a>(
    data: &'a [u8],
    key_blocks_len: usize,
    header: &Header,
    key_blocks_size: &Vec<KeyBlockSize>,
) -> IResult<&'a [u8], Vec<RecordDeBufOffset>> {
    let (data, buf) = take(key_blocks_len)(data)?;
    let mut buf = buf;

    let mut key_entries: Vec<RecordDeBufOffset> = vec![];

    for block_size in key_blocks_size.iter() {
        let (remain, decompressed) = key_block_parser(buf, block_size.csize, block_size.dsize)?;
        let (unconsumed, mut one_block_entries) = match &header.version {
            Version::V1 => {
                parse_block_items_v1(&decompressed[..], &header.encoding).map_err(|_| {
                    nom::Err::Failure(nom::error::Error::new(buf, nom::error::ErrorKind::Fail))
                })?
            }
            Version::V2 => {
                parse_block_items_v2(&decompressed[..], &header.encoding).map_err(|_| {
                    nom::Err::Failure(nom::error::Error::new(buf, nom::error::ErrorKind::Fail))
                })?
            }
        };
        if !unconsumed.is_empty() {
            warn!(
                "key block items parser left {} bytes unconsumed",
                unconsumed.len()
            );
        }

        buf = remain;
        key_entries.append(&mut one_block_entries);
    }

    Ok((data, key_entries))
}

// TODO 可以合并
fn parse_block_items_v1<'a>(
    data: &'a [u8],
    encoding: &str,
) -> IResult<&'a [u8], Vec<RecordDeBufOffset>> {
    // MDD 文件的 encoding 为空，使用 UTF-16LE
    let actual_encoding = if encoding.is_empty() {
        "utf-16le"
    } else {
        encoding
    };

    let (remain, entries) = many0(map(
        (be_u32, take_till(|x| x == 0), take(1_usize)),
        |(offset, buf, _)| {
            let decoder = encoding_from_whatwg_label(actual_encoding).unwrap_or(UTF_8);
            let text = decoder
                .decode(buf, encoding::DecoderTrap::Ignore)
                .unwrap_or_default();
            RecordDeBufOffset {
                record_offset_in_debuf: offset as usize,
                text,
            }
        },
    ))
    .parse(data)?;

    Ok((remain, entries))
}

fn parse_block_items_v2<'a>(
    data: &'a [u8],
    encoding: &str,
) -> IResult<&'a [u8], Vec<RecordDeBufOffset>> {
    // MDD 文件的 encoding 为空，使用 UTF-16LE
    let actual_encoding = if encoding.is_empty() {
        "utf-16le"
    } else {
        encoding
    };
    let is_utf16 = actual_encoding.to_lowercase().contains("utf-16");

    // UTF-16 编码使用 2 字节的 null terminator (\x00\x00)
    // 但由于 take_till 一个字节一个字节检查，当检测到第一个 \x00 时会停止
    // 对于 UTF-16LE，字符如 'A' 是 [0x41, 0x00]，所以我们不能简单用 take_till(|x| x == 0)
    // 需要手动解析 UTF-16 编码的字符串

    if is_utf16 {
        // 手动解析 UTF-16 编码的 entries
        let mut entries = vec![];
        let mut remaining = data;

        while remaining.len() >= 8 {
            // 读取 8 字节的 offset
            let offset = u64::from_be_bytes([
                remaining[0],
                remaining[1],
                remaining[2],
                remaining[3],
                remaining[4],
                remaining[5],
                remaining[6],
                remaining[7],
            ]) as usize;
            remaining = &remaining[8..];

            // 查找 UTF-16LE null terminator (\x00\x00)
            // 注意：需要在偶数位置查找
            let mut end_pos = 0;
            while end_pos + 1 < remaining.len() {
                if remaining[end_pos] == 0 && remaining[end_pos + 1] == 0 {
                    break;
                }
                end_pos += 2;
            }

            if end_pos + 1 >= remaining.len() {
                // 没有找到 null terminator，可能是最后一个条目
                break;
            }

            let text_buf = &remaining[..end_pos];
            remaining = &remaining[end_pos + 2..]; // 跳过 2 字节的 null terminator

            // 解码 UTF-16LE
            let text = UTF_16LE
                .decode(text_buf, encoding::DecoderTrap::Ignore)
                .unwrap_or_default();

            entries.push(RecordDeBufOffset {
                record_offset_in_debuf: offset,
                text,
            });
        }

        Ok((remaining, entries))
    } else {
        // UTF-8 等单字节 null terminator 的编码
        let (remain, sep) = many0(map(
            (be_u64, take_till(|x| x == 0), take(1_usize)),
            |(offset, buf, _end_zero)| {
                let decoder = encoding_from_whatwg_label(actual_encoding).unwrap_or(UTF_8);
                let text = decoder
                    .decode(buf, encoding::DecoderTrap::Ignore)
                    .unwrap_or_default();
                RecordDeBufOffset {
                    record_offset_in_debuf: offset as usize,
                    text,
                }
            },
        ))
        .parse(data)?;

        Ok((remain, sep))
    }
}

/// 解析一个 key block 得到的是bytes
fn key_block_parser<'a>(input: &'a [u8], csize: usize, dsize: usize) -> IResult<&'a [u8], Vec<u8>> {
    if csize < 8 {
        return Err(nom::Err::Failure(nom::error::Error::new(
            input,
            nom::error::ErrorKind::LengthValue,
        )));
    }

    if input.len() < csize {
        return Err(nom::Err::Incomplete(nom::Needed::new(csize - input.len())));
    }

    let (block, remain) = input.split_at(csize);
    let enc = u32::from_le_bytes([block[0], block[1], block[2], block[3]]);
    let checksum = &block[4..8];
    let encrypted_buf = &block[8..];

    let enc_method = (enc >> 4) & 0xf;
    let comp_method = enc & 0xf;

    let mut md = Ripemd128::new();
    md.update(checksum);
    let key = md.finalize();

    let data: Vec<u8> = match enc_method {
        0 => encrypted_buf.to_vec(),
        1 => fast_decrypt(encrypted_buf, key.as_slice()),
        2 => {
            return Err(nom::Err::Failure(nom::error::Error::new(
                input,
                nom::error::ErrorKind::Verify,
            )));
        }
        _ => {
            return Err(nom::Err::Failure(nom::error::Error::new(
                input,
                nom::error::ErrorKind::Verify,
            )));
        }
    };

    let decompressed = match comp_method {
        0 => data,
        1 => {
            let lzo = minilzo_rs::LZO::init().map_err(|_| {
                nom::Err::Failure(nom::error::Error::new(input, nom::error::ErrorKind::Fail))
            })?;
            lzo.decompress(&data[..], dsize).map_err(|_| {
                nom::Err::Failure(nom::error::Error::new(input, nom::error::ErrorKind::Fail))
            })?
        }
        2 => {
            let mut v = Vec::with_capacity(dsize);
            ZlibDecoder::new(&data[..])
                .read_to_end(&mut v)
                .map_err(|_| {
                    nom::Err::Failure(nom::error::Error::new(input, nom::error::ErrorKind::Fail))
                })?;
            v
        }
        _ => {
            return Err(nom::Err::Failure(nom::error::Error::new(
                input,
                nom::error::ErrorKind::Verify,
            )));
        }
    };

    Ok((remain, decompressed))
}
