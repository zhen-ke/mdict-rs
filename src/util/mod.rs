use nom::IResult;
use nom::number::complete::{be_u8, be_u16};

// 解压缩这个地方优化一下
pub fn fast_decrypt(encrypted: &[u8], key: &[u8]) -> Vec<u8> {
    let mut buf = Vec::from(encrypted);
    let mut prev = 0x36;
    for i in 0..buf.len() {
        let mut t = buf[i].rotate_left(4);
        t = t ^ prev ^ (i as u8) ^ key[i % key.len()];
        prev = buf[i];
        buf[i] = t;
    }
    buf
}

/// nom parser for UTF-8 encoding (returns byte count directly)
pub fn text_len_parser_v2(input: &[u8]) -> IResult<&[u8], u16> {
    let (input, len) = be_u16(input)?;
    Ok((input, len + 1))
}

/// nom parser for UTF-16 encoding (length is in 2-byte units, so we multiply by 2 to get bytes)
/// MDD 文件使用 UTF-16LE 编码，长度字段是字符数，需要 * 2 得到字节数
pub fn text_len_parser_v2_utf16(input: &[u8]) -> IResult<&[u8], u16> {
    let (input, len) = be_u16(input)?;
    // 字符数 + 1 (null terminator)，然后 * 2 得到字节数
    Ok((input, (len + 1) * 2))
}

pub fn text_len_parser_v1(input: &[u8]) -> IResult<&[u8], u8> {
    be_u8(input)
}
