# mdict-rs 📚

[![Rust Edition](https://img.shields.io/badge/Rust-2024-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/License-MIT%2FApache--2.0-blue.svg)](LICENSE)
[![Framework](https://img.shields.io/badge/Axum-0.8-purple.svg)](https://github.com/tokio-rs/axum)
[![Database](https://img.shields.io/badge/SQLite-FTS5-green.svg)](https://www.sqlite.org/)

**mdict-rs** 是一个基于 Rust 语言开发的现代、高性能、低资源占用的 **MDX / MDD 电子词典 Web 服务器** 与解析核心引擎。

它能够将常见的 MDX 格式大词典（如朗文、牛津、韦氏、Collins 等）快速索引并转译为高性能 Web 服务，支持多词典联合查询、富文本 HTML 聚合渲染、静态资源/音频流离线重写、前缀与模糊搜索，完美适配个人电脑、NAS (如群晖/Unraid)、树莓派、Armbian 等各类软硬件环境。

---

## 🌟 核心特性

- ⚡ **极致性能与内存优化**：基于 `memmap2` 零拷贝懒加载技术，几 GB 大小的 MDX 文件秒级载入，内存占用极低。
- 📦 **全格式兼容性**：
  - 完整支持 MDX / MDD **V1 与 V2** 格式版本。
  - 支持 **Zlib** 与 **LZO** 数据压缩块解压。
  - 支持 Encrypted Key Block **Flag 0 / 1 / 2 / 3** 加密字段解析。
- 🔍 **多维度智能检索**：
  - **前缀补全** (`/suggest`)：实时键入高频词条匹配。
  - **模糊纠错** (`/suggest/fuzzy`)：基于 Levenshtein 编辑距离算法的 "Did you mean?" 相似词推荐。
  - **词形归一化** (Lemmatization)：支持复数、时态、大小写自动折叠与变体还原。
  - **全文检索** (FTS5)：可选择性自动构建 SQLite FTS5 全文索引，支持全局模糊查找。
- 🎨 **多词典聚合与路由重写**：
  - 支持同时勾选/渲染多本词典条目。
  - 自动重写 `entry://`（条目跳转）、`sound://`（发音音频）、`http(s)://` 及本地 `.mdd` 静态图片/CSS 资源路径。
  - 独立的词典样式隔离（CSS Container Class 作用域），防止不同词典全局样式冲突。
- ⚙️ **后台无感索引构建**：
  - 启动时自动扫描词典并异步多线程生成单文件 `.db` 索引。
  - 无需等待全量索引建完即可直接响应 HTTP 检索服务。
- 🐳 **轻量跨平台部署**：
  - 提供无缝的 Alpine 基础 Dockerfile 支持。
  - 提供针对 ARM64 / Armbian 等嵌入式设备的一键交叉编译部署脚本。

---

## 🏗️ 架构设计

本项目采用了标准 Rust **Cargo Workspace** 模块化设计，解耦平台无关解析引擎与 Web 应用服务：

```
mdict-rs/
├── crates/
│   ├── mdict-core/              # 🦀 平台无关的核心解析与索引库 (Library)
│   │   ├── src/
│   │   │   ├── mdict/           # MDX/MDD 文件结构解析 (Header, KeyBlock, RecordBlock, Reader)
│   │   │   ├── indexing/        # SQLite 索引器构建 (.db / MDX_INDEX / MDX_FTS)
│   │   │   ├── normalize.rs     # 词条归一化与变体还原算法
│   │   │   ├── fuzzy.rs         # 模糊编辑距离匹配器
│   │   │   ├── rewrite.rs       # 词条 HTML 内链与资源路径重写器
│   │   │   └── presenter.rs     # 多词典 HTML 结果渲染与 DOM 隔离
│   │   └── Cargo.toml
│   │
│   └── mdict-server/            # 🌐 Axum 驱动的高性能 HTTP Web 服务 (Binary: mdict-rs)
│       ├── resources/static/    # Web 前端 UI 静态资源 (CSS, JS, Icons)
│       ├── src/
│       │   ├── config/          # 环境变量与单词典 .toml 配置解析器
│       │   ├── handlers/        # Axum 路由 handlers (查询/建议/资源/API)
│       │   ├── query/           # 多词典并行检索编排与存储仓储层
│       │   ├── lucky/           # 随机词条抽取器
│       │   └── main.rs          # 服务启动入口与索引后台任务调度
│       └── Cargo.toml
│
├── mdict/                       # 📁 默认词典放置目录 (.mdx / .mdd / .toml / .db)
├── Dockerfile                   # 🐳 容器化多阶段构建文件
├── deploy.sh                    # 🚀 Armbian / Linux 远程部署脚本
└── Cargo.toml                   # Workspace 根配置文件
```

---

## 🚀 快速上手

### 1. 准备词典文件

在项目根目录下创建 `mdict/` 文件夹（或指定路径），将你的 `.mdx` 和 `.mdd` 词典文件放入其中：

```
mdict/
├── 朗文当代高级英语辞典.mdx
├── 朗文当代高级英语辞典.mdd
├── 牛津高阶英汉双解词典.mdx
└── 牛津高阶英汉双解词典.mdd
```

### 2. 本地直接运行

```bash
# 克隆仓库
git clone https://github.com/zhen-ke/mdict-rs.git
cd mdict-rs

# 编译并运行 Release 版本
cargo run --release
```

启动成功后，浏览器打开 `http://localhost:8181` 即可体验。

> 💡 **提示**：首次启动时，程序会自动扫描 `mdict/` 目录并在后台为所有 `.mdx`/`.mdd` 建立 SQLite 索引库（生成同名 `.db` 文件）。建立完成后后续启动将达到毫秒级极速响应。

---

## ⚙️ 高级配置

### 环境变量

可通过以下环境变量对服务行为进行定制：

| 环境变量 | 默认值 | 说明 |
| :--- | :--- | :--- |
| `MDX_DICT_DIR` | `./mdict` | 词典文件 (`.mdx`/`.mdd`) 所在的目录路径 |
| `BIND_ADDR` | `127.0.0.1` | HTTP 服务监听的主机地址 (如需外网/局域网访问可设为 `0.0.0.0`) |
| `PORT` | `8181` | HTTP 服务端口 |
| `RUST_LOG` | `info` | 日志输出级别 (`error`, `warn`, `info`, `debug`, `trace`) |

### 单词典个性化配置 (`.toml`)

你可以为任意 `.mdx` 词典创建同名的 `.toml` 配置文件（如 `mdict/Oxford.mdx` 对应 `mdict/Oxford.toml`），以支持独立的样式定制与参数开关：

```toml
# mdict/Oxford.toml

# 词典显示名称（覆盖内部文件名）
name = "牛津高阶英汉双解词典 (第10版)"

# 词典简介
description = "Oxford Advanced Learner's Dictionary 10th Edition"

# 是否启用 SQLite FTS5 全文索引 (默认: true)
fts = true

# 自定义 CSS 插入 (支持内联代码或通过 @filename 引用同目录文件)
css = "@oxford_custom.css"

# 自定义 JavaScript 脚本
js = "console.log('Oxford dictionary loaded');"

# 隔离的容器 CSS 类名 (防止样式污染)
container_class = "oxford-dict-wrapper"
```

---

## 🔌 API 接口规范

`mdict-rs` 提供了丰富且清晰的 HTTP / RESTful 接口供前端 UI 及第三方客户端调用：

| 路由 / 路径 | HTTP 方法 | 功能描述 |
| :--- | :--- | :--- |
| `/query` | `POST` | 核心检索接口。查询一个或多个词条，返回各词典聚合渲染后的 HTML 内容 |
| `/suggest?q={word}` | `GET` | 前缀自动补全建议接口，返回匹配的词条候选列表 |
| `/suggest/fuzzy?q={word}`| `GET` | 模糊拼写纠错建议接口 ("Did you mean?") |
| `/lucky` | `GET` | 手气不错 / 随机获取词条条目 |
| `/trace?q={word}` | `GET` | 调试诊断接口，返回查询处理耗时与命中的词典分布 |
| `/api/dicts` | `GET` | 获取当前已加载的所有词典列表与元数据信息 |
| `/api/index/status` | `GET` | 查验各词典 SQLite 索引构建状态及 FTS 全文索引健康度 |
| `/dict/{id}/entry/{word}`| `GET` | 内部条目跳转专属路由 (重写 `entry://` 链接) |
| `/dict/{id}/res/{path}` | `GET` | 词典内部 `.mdd` 静态资源提取路由 (图片、字体、CSS) |
| `/dict/{id}/audio/{path}`| `GET` | 词典内部音频流读取路由 (支持 `.wav`, `.mp3`, `.spx` 发音文件) |
| `/api/dict/style` | `GET` | 动态获取某词典配置的自定义 CSS 方案 |
| `/api/dict/script` | `GET` | 动态获取某词典配置的自定义 JS 脚本 |

---

## 🐳 部署指南

### 1. Docker 容器部署

根目录下自带高效的 Linux Alpine 多阶段构建 `Dockerfile`：

```bash
# 构建镜像
docker build -t mdict-rs:latest .

# 运行容器 (挂载宿主机的词典目录)
docker run -d \
  --name mdict \
  -p 8181:8181 \
  -v /path/to/your/mdict:/app/mdict \
  --restart unless-stopped \
  mdict-rs:latest
```

使用 **Docker Compose** (`docker-compose.yml`) 部署：

```yaml
version: '3.8'
services:
  mdict-rs:
    build: .
    container_name: mdict-rs
    ports:
      - "8181:8181"
    environment:
      - BIND_ADDR=0.0.0.0
      - PORT=8181
      - RUST_LOG=info
    volumes:
      - ./mdict:/app/mdict
    restart: unless-stopped
```

### 2. ARM64 / Armbian 嵌入式设备一键部署

项目针对 Armbian (如斐讯 N1、树莓派等 ARM64 设备) 提供了自动化交叉编译与远程部署脚本：

1. **环境准备 (以 macOS 为例)**：
   ```bash
   brew install FiloSottile/musl-cross/musl-cross
   rustup target add aarch64-unknown-linux-musl
   ```

2. **远程部署**：
   ```bash
   # 执行部署脚本 (替换为你的远程设备 IP)
   ./deploy.sh root@192.168.1.100

   # 将词典拷贝至远程设备的 mdict 目录
   scp mdict/*.mdx root@192.168.1.100:/DATA/Documents/mdict-server/mdict/
   ```

详细部署细节参见 [DEPLOY.md](file:///Users/ke/Documents/github/mdict-rs/DEPLOY.md)。

---

## 🛠️ 本地开发与测试

```bash
# 检查代码语法与类型
cargo check --workspace

# 格式化代码
cargo fmt

# 运行单元测试
cargo test --workspace

# 启动开发服务器 (支持热重载前端静态资源)
cargo run --bin mdict-rs
```

---

## 📖 参考与致谢

- MDX/MDD 文件格式逆向分析与算法参考：
  - [xwang/mdict-analysis](https://bitbucket.org/xwang/mdict-analysis/src/master/) (Bitbucket)
  - Einverne's Blog: [MDX/MDD 文件格式解析](http.einverne.github.io/post/2018/08/mdx-mdd-file-format.html)

---

## 📄 开源协议

本项目遵守 [MIT License](LICENSE) 开源许可协议。
