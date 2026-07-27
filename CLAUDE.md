# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

mdict-rs is a web-based dictionary application built in Rust that supports MDX format dictionary files. It provides a web interface for querying dictionary entries and serves static files.

## Common Development Tasks

### Running the Application

```bash
cargo run --bin mdict-rs
```

The application runs on `http://localhost:8181` by default.

### Building

```bash
cargo build
```

### Code Formatting

This project uses rustfmt with specific configuration:

```bash
cargo fmt
```

## Architecture

### Core Components

- **MDX File Processing** (`crates/mdict-core/src/mdict/`): Parses MDX dictionary files into structured data
  - `header.rs`: Parses MDX file headers
  - `keyblock.rs`: Handles key blocks containing dictionary entries
  - `recordblock.rs`: Manages record blocks with definitions
  - `mdx.rs`: Main MDX file parser and coordinator

- **Indexing System** (`crates/mdict-core/src/indexing/`): Converts MDX files to SQLite databases for efficient querying
  - Creates `.db` files alongside MDX files
  - Tables:
    - `MDX_INDEX(text, record_offset, record_length, block_offset, block_size, block_dsize)`
    - `MDX_FTS(text)` (optional, when FTS5 is available)

- **Web Server** (`crates/mdict-server/src/main.rs`, `crates/mdict-server/src/handlers/`): Axum-based web server
  - `/query`: POST endpoint for dictionary lookups
  - `/lucky`: GET endpoint for random word lookup
  - Static file serving from detected static directory (see Configuration)

- **Query System** (`crates/mdict-server/src/query/`): Handles dictionary queries across multiple MDX files
  - Searches through SQLite databases in scan order (MDX first; resources prefer MDD first)

### Configuration

- **Dictionary Files**: Scanned at startup from:
  1. `MDX_DICT_DIR` environment variable (if set)
  2. `mdict/` folder next to the binary
  3. `mdict/` folder in current working directory
- **Static Files**: Served from:
  1. `static/` folder next to the binary
  2. `static/` folder in current working directory
  3. `resources/static/` (development)
- **Per-dictionary config**: Optional TOML next to each `.mdx` (same filename, `.toml` extension)

### Data Flow

1. Application starts, scans dictionaries, and ensures SQLite indexes exist
2. Web server accepts queries via `/query` endpoint
3. Query system searches through databases in configured order
4. Results returned as plain text responses

### Dependencies

- **axum**: Web framework
- **rusqlite**: SQLite database operations
- **nom**: Parser combinators for MDX file parsing
- **tracing**: Logging and observability
- **flate2**: Compression/decompression for MDX files

## File Structure

本项目是 Cargo workspace，分为平台无关核心库与 Web 服务两个 crate：

```
crates/
├── mdict-core/          # 平台无关核心（lib，无 HTTP 依赖）
│   └── src/
│       ├── mdict/       # MDX/MDD 文件解析与 mmap 记录读取
│       ├── indexing/    # MDX to SQLite conversion
│       ├── normalize.rs # 查询词归一化与词形候选
│       ├── rewrite.rs   # 词条链接重写
│       ├── presenter.rs # 聚合 HTML 渲染
│       └── util/        # 加解密与基础解析工具
│
└── mdict-server/        # Web 服务（bin: mdict-rs）
    ├── resources/
    │   └── static/      # Static files (CSS, etc.)
    └── src/
        ├── config/      # Application configuration
        ├── handlers/    # HTTP request handlers
        ├── lucky/       # Random word selection
        └── query/       # 查询编排（service/repository/specific）

mdict/             # Dictionary files (default runtime location)
├── xxx.mdx
├── xxx.mdx.db
├── xxx.mdd
└── xxx.mdd.db
```

## Important Notes

- The project uses Rust 2024 edition
- Dictionary files are scanned from `MDX_DICT_DIR` (if set) or `mdict/` near the binary / current working directory
- Indexing happens automatically on startup, creating `.db` files alongside dictionary files
- The application currently supports MDX version 2.0 with encryption levels 0 and 2
