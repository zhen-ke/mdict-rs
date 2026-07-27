//! 平台无关核心层的强类型错误。
//!
//! 之前解析层入口（`parse_header` / `Mdx::new` / `MdxReader::read_record`）
//! 直接返回 `anyhow::Result`，错误链尾端是 `nom::Err` 的 Debug 串，既不可
//! 模式匹配也读不懂。这里给出有结构的 [`MdictError`] 枚举，在解析层边界
//! 构造，使损坏文件、不支持加密、块越界等都能被上游（web 层、CLI、FFI）
//! 精确识别并给出"人话"。
//!
//! `MdictError` 实现 `std::error::Error`，因此可无缝 `?` 进 `anyhow::Result`
//! 与上游服务层的 `QueryError::Internal` —— 边界签名只换类型，不改语义。

use std::io;
use thiserror::Error;

/// mdict-core 解析层错误。
#[derive(Debug, Error)]
pub enum MdictError {
    /// adler-32 / zlib 校验和不匹配，数据已损坏。
    #[error("checksum mismatch ({context}): {detail}")]
    ChecksumMismatch {
        context: &'static str,
        detail: String,
    },

    /// 不支持的 MDX 引擎版本（如非数字首字符）。
    #[error("unsupported MDX engine version: {version}")]
    UnsupportedVersion { version: u8 },

    /// 不支持的加密方法（RegCode/商业加密等），无法解码。
    #[error(
        "unsupported encryption method {method} (RegCode/commercial encryption is not supported)"
    )]
    UnsupportedEncryption { method: u8 },

    /// 解压后的块超过安全上限，疑似恶意/损坏文件。
    #[error("block too large: decompressed size {dsize} exceeds safety limit {limit}")]
    BlockTooLarge { dsize: usize, limit: usize },

    /// 块的字节范围超出文件（mmap）边界。
    #[error("block out of bounds: offset {offset} + size {csize} > file size {file_size}")]
    BlockOutOfBounds {
        offset: usize,
        csize: usize,
        file_size: usize,
    },

    /// LZO/zlib 解压失败（含 zlib 尾部 adler-32 校验失败）。
    #[error("decompression failed ({codec}): {message}")]
    DecompressFailed {
        codec: &'static str,
        message: String,
    },

    /// 其它"输入损坏/不合规"错误，附带人话描述。
    #[error("corrupt MDX input: {0}")]
    CorruptInput(String),

    /// 底层 IO 错误（打开/读取/mmap）。
    #[error("io error: {0}")]
    Io(#[from] io::Error),
}
