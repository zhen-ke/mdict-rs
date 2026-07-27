use std::collections::HashMap;
use std::sync::LazyLock;

use adler32::adler32;
use anyhow::{Context, bail};
use encoding::{Encoding, all::UTF_16LE};
use nom::Parser;
use nom::multi::length_data;
use nom::number::complete::{be_u32, le_u32};
use regex::Regex;
use tracing::{info, warn};

#[derive(Debug)]
pub enum Version {
    V1,
    V2,
}

/// mdx头部信息
#[derive(Debug)]
pub struct Header {
    // 牛津8/汉语词典3/朗文4都是 V2
    pub version: Version,
    /**
     * encryption flag "0"-no encryption, "1"-encrypt record block, "2"-encrypt key info block
     * e.g., 牛津8/汉语词典3 "0" 朗文4 "2"
     */
    pub encrypted: String,
    // record bytes encoding, e.g. "UTF-8"
    pub encoding: String,
}

/// Compiled once on first use; shared across all header parses (each MDX/MDD
/// file parse used to recompile this regex, which is wasteful under parallel
/// indexing with rayon).
static HEADER_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(\w+)="((.|\r\n|[\r\n])*?)""#).expect("header regex"));

/// Parse header using nom, but return anyhow::Result for better error handling
pub fn parse_header(data: &[u8]) -> anyhow::Result<(&[u8], Header)> {
    // Use nom to parse length-prefixed data
    let (data, (header_buf, checksum)) = (length_data(be_u32), le_u32).parse(data).map_err(
        |e: nom::Err<nom::error::Error<&[u8]>>| {
            anyhow::anyhow!("Failed to parse header length/checksum: {:?}", e)
        },
    )?;

    // Verify checksum
    let computed_checksum = adler32(header_buf).context("Failed to compute adler32 checksum")?;
    if computed_checksum != checksum {
        bail!(
            "Header checksum mismatch: computed {} != expected {}",
            computed_checksum,
            checksum
        );
    }

    // Decode UTF-16LE header content
    let info = UTF_16LE
        .decode(header_buf, encoding::DecoderTrap::Strict)
        .map_err(|e| anyhow::anyhow!("Failed to decode header as UTF-16LE: {}", e))?;

    // Parse header attributes
    let mut attrs = HashMap::new();
    for cap in HEADER_REGEX.captures_iter(info.as_str()) {
        attrs.insert(cap[1].to_ascii_lowercase(), cap[2].to_string());
    }

    info!(">>>the header content: {:?}", &attrs);

    // Parse version
    let version_str = attrs
        .get("generatedbyengineversion")
        .context("Missing 'GeneratedByEngineVersion' attribute in header")?;

    let version_char = version_str
        .trim()
        .chars()
        .next()
        .context("Empty 'GeneratedByEngineVersion' value")?;

    let version_num = version_char
        .to_digit(10)
        .context(format!("Invalid version digit: '{}'", version_char))? as u8;

    let version = match version_num {
        1 => Version::V1,
        2 => Version::V2,
        n if n > 2 => {
            // Keep parser permissive for newer generator versions that still use V2 layout.
            warn!(
                "MDX engine version {} detected, treat as V2-compatible parser path",
                n
            );
            Version::V2
        }
        _ => bail!("Unsupported MDX engine version: {}", version_num),
    };

    // "0" "2" "3" - MDD 文件也可能没有 Encrypted 属性
    let encrypted = attrs
        .get("encrypted")
        .map(|s| s.to_string())
        .unwrap_or_else(|| "No".to_string());

    // "UTF-8" - MDD 文件（二进制资源）通常没有 Encoding 属性
    let encoding = attrs
        .get("encoding")
        .map(|s| s.to_string())
        .unwrap_or_else(|| "".to_string());

    Ok((
        data,
        Header {
            version,
            encrypted,
            encoding,
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::{Version, parse_header};
    use adler32::adler32;

    fn header_blob(xml: &str) -> Vec<u8> {
        let mut utf16le = Vec::new();
        for code in xml.encode_utf16() {
            utf16le.extend_from_slice(&code.to_le_bytes());
        }
        let checksum = adler32(&utf16le[..]).expect("adler32");
        let mut out = Vec::new();
        out.extend_from_slice(&(utf16le.len() as u32).to_be_bytes());
        out.extend_from_slice(&utf16le);
        out.extend_from_slice(&checksum.to_le_bytes());
        out
    }

    #[test]
    fn parse_version_3_as_v2_compatible() {
        let raw = header_blob(
            r#"<Dictionary GeneratedByEngineVersion="3.0" Encrypted="0" Encoding="UTF-8"/>"#,
        );
        let (_, header) = parse_header(&raw).expect("parse header");
        assert!(matches!(header.version, Version::V2));
    }

    #[test]
    fn parse_header_attributes_case_insensitive() {
        let raw = header_blob(
            r#"<Dictionary generatedbyengineversion="2.0" encrypted="1" encoding="utf-8"/>"#,
        );
        let (_, header) = parse_header(&raw).expect("parse header");
        assert!(matches!(header.version, Version::V2));
        assert_eq!(header.encrypted, "1");
        assert_eq!(header.encoding, "utf-8");
    }
}
