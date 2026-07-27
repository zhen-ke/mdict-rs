//! # mdict-core
//!
//! mdict-rs 的平台无关核心：MDX/MDD 解析（v1/v2；none/LZO/zlib）、
//! SQLite 索引构建、以及不依赖任何 Web 框架的纯查询逻辑
//! （归一化、链接重写、聚合渲染）。
//!
//! 模块划分：
//! - [`mdict`]      — MDX/MDD 容器解析与 mmap 记录读取
//! - [`indexing`]   — 词典文件 → SQLite 索引库（`<file>.db`）
//! - [`normalize`]  — 查询词归一化与词形候选展开
//! - [`rewrite`]    — 词条 HTML 中资源/词条链接的路由重写
//! - [`presenter`]  — 多词典聚合 HTML 渲染与安全清洗
//! - [`util`]       — 加解密与基础解析工具

pub mod error;
pub mod indexing;
pub mod mdict;
pub mod normalize;
pub mod presenter;
pub mod rewrite;
pub mod util;
