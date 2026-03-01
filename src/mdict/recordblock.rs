use std::io::Read;

use flate2::read::ZlibDecoder;
use nom::bytes::complete::take;
use nom::combinator::map;
use nom::multi::count;
use nom::number::complete::{be_u32, be_u64, le_u32};
use nom::{IResult, Parser};
use ripemd::{Digest, Ripemd128};

use crate::mdict::header::{Header, Version};
use crate::util::fast_decrypt;

const MAX_RECORD_BLOCK_DSIZE: usize = 256 * 1024 * 1024;

/// every record block compressed size and decompressed size
#[derive(Debug)]
pub struct RecordBlockSize {
    pub csize: usize,
    pub dsize: usize,
}

pub fn parse_record_blocks<'a>(
    data: &'a [u8],
    header: &Header,
) -> IResult<&'a [u8], Vec<RecordBlockSize>> {
    match &header.version {
        Version::V1 => parse_record_blocks_v1(data),
        Version::V2 => parse_record_blocks_v2(data),
    }
}

fn parse_record_blocks_v1(data: &[u8]) -> IResult<&[u8], Vec<RecordBlockSize>> {
    let (data, (records_num, _entries_num, record_info_len, _record_buf_len)) =
        (be_u32, be_u32, be_u32, be_u32).parse(data)?;

    // Validate record info length
    let Some(expected_info_len) = records_num.checked_mul(8) else {
        tracing::error!("V1 records_num overflow: {}", records_num);
        return Err(nom::Err::Failure(nom::error::Error::new(
            data,
            nom::error::ErrorKind::Verify,
        )));
    };
    if expected_info_len != record_info_len {
        tracing::error!(
            "V1 record info length mismatch: {} * 8 != {}",
            records_num,
            record_info_len
        );
        return Err(nom::Err::Failure(nom::error::Error::new(
            data,
            nom::error::ErrorKind::Verify,
        )));
    }

    let records_num = usize::try_from(records_num).map_err(|_| {
        nom::Err::Failure(nom::error::Error::new(data, nom::error::ErrorKind::Verify))
    })?;

    count(
        map((be_u32, be_u32), |(csize, dsize)| RecordBlockSize {
            csize: csize as usize,
            dsize: dsize as usize,
        }),
        records_num,
    )
    .parse(data)
}

fn parse_record_blocks_v2(data: &[u8]) -> IResult<&[u8], Vec<RecordBlockSize>> {
    let (data, (records_num, _entries_num, record_info_len, _record_buf_len)) =
        (be_u64, be_u64, be_u64, be_u64).parse(data)?;

    // Validate record info length
    let Some(expected_info_len) = records_num.checked_mul(16) else {
        tracing::error!("V2 records_num overflow: {}", records_num);
        return Err(nom::Err::Failure(nom::error::Error::new(
            data,
            nom::error::ErrorKind::Verify,
        )));
    };
    if expected_info_len != record_info_len {
        tracing::error!(
            "V2 record info length mismatch: {} * 16 != {}",
            records_num,
            record_info_len
        );
        return Err(nom::Err::Failure(nom::error::Error::new(
            data,
            nom::error::ErrorKind::Verify,
        )));
    }

    let records_num = usize::try_from(records_num).map_err(|_| {
        nom::Err::Failure(nom::error::Error::new(data, nom::error::ErrorKind::Verify))
    })?;
    let (data, raw_sizes) = count((be_u64, be_u64), records_num).parse(data)?;
    let mut record_blocks = Vec::with_capacity(raw_sizes.len());
    for (csize, dsize) in raw_sizes {
        let csize = usize::try_from(csize).map_err(|_| {
            nom::Err::Failure(nom::error::Error::new(data, nom::error::ErrorKind::Verify))
        })?;
        let dsize = usize::try_from(dsize).map_err(|_| {
            nom::Err::Failure(nom::error::Error::new(data, nom::error::ErrorKind::Verify))
        })?;
        record_blocks.push(RecordBlockSize { csize, dsize });
    }
    Ok((data, record_blocks))
}

pub(crate) fn record_block_parser<'a>(
    size: usize,
    dsize: usize,
) -> impl Parser<&'a [u8], Output = Vec<u8>, Error = nom::error::Error<&'a [u8]>> {
    move |input: &'a [u8]| {
        if dsize > MAX_RECORD_BLOCK_DSIZE {
            tracing::error!(
                "record block dsize {} exceeds safety limit {}",
                dsize,
                MAX_RECORD_BLOCK_DSIZE
            );
            return Err(nom::Err::Failure(nom::error::Error::new(
                input,
                nom::error::ErrorKind::Verify,
            )));
        }

        let payload_len = size.checked_sub(8).ok_or_else(|| {
            nom::Err::Failure(nom::error::Error::new(
                input,
                nom::error::ErrorKind::LengthValue,
            ))
        })?;
        let (remain, (enc, checksum, encrypted)) =
            (le_u32, take(4_usize), take(payload_len)).parse(input)?;

        // 规范里面好像没有加密这步
        let enc_method = (enc >> 4) & 0xf;
        let comp_method = enc & 0xf;

        let mut md = Ripemd128::new();
        md.update(checksum);
        let key = md.finalize();

        let data: Vec<u8> = match enc_method {
            0 => Vec::from(encrypted),
            1 => fast_decrypt(encrypted, key.as_slice()),
            2 => {
                tracing::error!("unsupported enc method: {enc_method}");
                return Err(nom::Err::Failure(nom::error::Error::new(
                    input,
                    nom::error::ErrorKind::Verify,
                )));
            }
            _ => {
                tracing::error!("unknown enc method: {enc_method}");
                return Err(nom::Err::Failure(nom::error::Error::new(
                    input,
                    nom::error::ErrorKind::Verify,
                )));
            }
        };

        let decompressed = match comp_method {
            0 => data,
            1 => {
                let lzo = minilzo_rs::LZO::init().map_err(|e| {
                    tracing::error!("LZO init failed: {:?}", e);
                    nom::Err::Failure(nom::error::Error::new(input, nom::error::ErrorKind::Fail))
                })?;
                lzo.decompress(&data[..], dsize).map_err(|e| {
                    tracing::error!("lzo decompress failed: {:?}", e);
                    nom::Err::Failure(nom::error::Error::new(input, nom::error::ErrorKind::Fail))
                })?
            }
            2 => {
                let limit = u64::try_from(dsize)
                    .ok()
                    .and_then(|v| v.checked_add(1))
                    .unwrap_or(u64::MAX);
                let mut v = Vec::new();
                let mut decoder = ZlibDecoder::new(&data[..]).take(limit);
                decoder.read_to_end(&mut v).map_err(|e| {
                    tracing::error!("zlib decompress failed: {:?}", e);
                    nom::Err::Failure(nom::error::Error::new(input, nom::error::ErrorKind::Fail))
                })?;
                if v.len() > dsize {
                    tracing::error!(
                        "zlib decompressed size {} exceeds expected {}",
                        v.len(),
                        dsize
                    );
                    return Err(nom::Err::Failure(nom::error::Error::new(
                        input,
                        nom::error::ErrorKind::Verify,
                    )));
                }
                v
            }
            _ => {
                tracing::error!("unknown compression method: {comp_method}");
                return Err(nom::Err::Failure(nom::error::Error::new(
                    input,
                    nom::error::ErrorKind::Verify,
                )));
            }
        };

        if decompressed.len() != dsize {
            tracing::error!(
                "record block decompressed size mismatch: got {}, expected {}",
                decompressed.len(),
                dsize
            );
            return Err(nom::Err::Failure(nom::error::Error::new(
                input,
                nom::error::ErrorKind::Verify,
            )));
        }

        Ok((remain, decompressed))
    }
}
