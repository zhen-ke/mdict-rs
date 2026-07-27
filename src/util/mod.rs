use std::sync::OnceLock;

use nom::IResult;
use nom::number::complete::{be_u8, be_u16};

/// Process-wide LZO instance, initialized once and reused.
///
/// `LZO::init()` runs the C-level `lzo_init()` and allocates a 128 KiB work
/// memory buffer. Calling it per block wastes both. Decompression does not
/// touch the work memory (minilzo passes a null pointer), so a single shared
/// `&'static LZO` is safe across threads.
static LZO: OnceLock<minilzo_rs::LZO> = OnceLock::new();

pub(crate) fn lzo_instance() -> &'static minilzo_rs::LZO {
    LZO.get_or_init(|| minilzo_rs::LZO::init().expect("minilzo LZO init failed"))
}

// MDX "fast" decryption (Ripemd128-key XOR stream).
//
// The reference loop updates `prev = buf[i]` (the *original* input byte)
// after computing each output byte. That creates a false loop-carried
// dependency through `prev`, but since `prev` at index i is just
// `encrypted[i-1]` (0x36 for the first byte), we can read the predecessor
// directly from the input slice. Each output byte then depends only on
// its own input byte and its predecessor — the compiler is free to
// pipeline/vectorize the body.
pub fn fast_decrypt(encrypted: &[u8], key: &[u8]) -> Vec<u8> {
    let len = encrypted.len();
    let key_len = key.len();
    assert!(key_len > 0, "fast_decrypt key must not be empty");

    let mut out = Vec::with_capacity(len);
    if len == 0 {
        return out;
    }

    let mut prev = 0x36u8;
    for (i, &b) in encrypted.iter().enumerate() {
        let t = b.rotate_left(4) ^ prev ^ (i as u8) ^ key[i % key_len];
        prev = b;
        out.push(t);
    }
    out
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
    // Use u32 for intermediate calculation to prevent u16 overflow
    let byte_len = (u32::from(len) + 1) * 2;
    let byte_len = u16::try_from(byte_len).map_err(|_| {
        nom::Err::Failure(nom::error::Error::new(input, nom::error::ErrorKind::Verify))
    })?;
    Ok((input, byte_len))
}

pub fn text_len_parser_v1(input: &[u8]) -> IResult<&[u8], u8> {
    be_u8(input)
}

#[cfg(test)]
mod tests {
    use super::fast_decrypt;

    /// Reference implementation (byte-by-byte, mutable buffer) — used only to
    /// cross-check that the optimized version produces identical output.
    fn fast_decrypt_reference(encrypted: &[u8], key: &[u8]) -> Vec<u8> {
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

    #[test]
    fn fast_decrypt_matches_reference() {
        let key = [0x12u8, 0xab, 0xcd, 0x37, 0x90, 0x55, 0x01, 0xfe];
        for case in [
            &b""[..],
            b"a",
            b"hello",
            b"The quick brown fox",
            &(0u8..=255).collect::<Vec<u8>>(),
        ] {
            let got = fast_decrypt(case, &key);
            let want = fast_decrypt_reference(case, &key);
            assert_eq!(got, want, "mismatch for input len {}", case.len());
        }
    }

    #[test]
    fn fast_decrypt_is_deterministic() {
        let key = [0x77u8; 16];
        let input = b"some encrypted payload";
        let a = fast_decrypt(input, &key);
        let b = fast_decrypt(input, &key);
        assert_eq!(a, b);
    }
}
