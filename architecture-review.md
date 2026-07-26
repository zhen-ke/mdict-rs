# mdict-rs 架构与交互体验深度分析

## 一、项目定位

一个基于 Rust (Axum) 的 MDict 词典 Web 服务器，将 `.mdx`/`.mdd` 格式词典文件解析、索引后通过 HTTP 提供查询服务，配合轻量级前端 SPA 实现多词典聚合查询。

---

## 二、架构总览

```
┌─────────────────────────────────────────────────────────┐
│  Frontend (SPA)                                         │
│  index.html + index.js + jQuery + lm6.js                │
│  Hash Routing · Debounced Suggest · History · Audio      │
└──────────────┬──────────────────────────────────────────┘
               │ POST /query · GET /suggest · GET /dict/{id}/entry/{word}
               │ GET /dict/{id}/res/{path} · GET /dict/{id}/audio/{path}
┌──────────────▼──────────────────────────────────────────┐
│  Handler Layer (src/handlers/)                           │
│  Axum extractors → Semaphore → spawn_blocking            │
│  3-tier cache: entry / resource / negative               │
│  Static file serving · Path traversal protection         │
└──────────────┬──────────────────────────────────────────┘
               │
┌──────────────▼──────────────────────────────────────────┐
│  Service Layer (src/query/)                              │
│  service.rs   → aggregate/specific/trace query logic     │
│  normalize.rs → lemma candidate generation               │
│  rewrite.rs   → HTML link rewriting (entry://, sound://) │
│  presenter.rs → multi-dict HTML aggregation              │
└──────────────┬──────────────────────────────────────────┘
               │
┌──────────────▼──────────────────────────────────────────┐
│  Repository Layer (src/query/repository.rs)              │
│  SQLite (r2d2 pool) → MDX_INDEX / MDX_FTS lookup         │
│  MdxReader (memmap2) → block decompression → record read │
└──────────────┬──────────────────────────────────────────┘
               │
┌──────────────▼──────────────────────────────────────────┐
│  MDict Parser (src/mdict/)                               │
│  header → keyblock → recordblock → Mdx                   │
│  nom parsers · Adler32/Ripemd128 · Zlib/LZO decompress   │
│  Encryption support · UTF-16LE (MDD) handling            │
└──────────────┬──────────────────────────────────────────┐
               │                                          │
┌──────────────▼────────────┐  ┌─────────────────────────┐│
│  Indexing (src/indexing/)  │  │  Config (src/config/)   ││
│  One-shot parse → SQLite  │  │  TOML per-dict config   ││
│  FTS5 · schema versioning │  │  Custom CSS/JS support  ││
│  Signature-based skip     │  │  @file references       ││
└───────────────────────────┘  └─────────────────────────┘│
└─────────────────────────────────────────────────────────┘
```

---

## 三、架构优点 ✅

### 1. 分层清晰，职责分离做得好
- **Handler → Service → Repository** 三层架构边界明确
- Handler 只管 HTTP 语义 + 缓存 + 并发控制
- Service 负责查询策略（聚合/特定/重定向追踪）
- Repository 封装 SQLite + 二进制文件读取细节

### 2. 性能设计精到
| 机制 | 效果 |
|------|------|
| `memmap2` 内存映射 | GB 级词典文件零堆拷贝，OS 页缓存自动管理 |
| LRU 块缓存（MdxReader 内 64 slot） | 同块连续查询免解压 |
| 3 层应用缓存（entry/resource/negative） | 热词秒回、404 不重查 |
| `r2d2` SQLite 连接池（max_size=10） | 避免反复建连 |
| `Semaphore(64)` 并发控制 | 防止 spawn_blocking 线程池耗尽 |
| FTS5 + BM25 + 手工评分 | 搜索建议质量高 |
| 资源大小分级缓存策略 | 大文件 stream、小文件缓存 |

### 3. 兼容性和鲁棒性
- `@@@LINK` 重定向递归追踪（depth 限 5）
- 多候选词生成（大小写变体、词形还原）
- 路径候选（正斜杠/反斜杠变体 × 有无前缀）
- 支持加密词典（Ripemd128 + fast_decrypt）
- `sound://`、`entry://` 旧协议兼容

### 4. 配置化设计
- Per-dict TOML 配置支持自定义 CSS/JS（`@file` 引用外部文件）
- `container_class` 实现 CSS 作用域隔离
- FTS 开关可按词典配置

---

## 四、架构问题与改进建议 ⚠️

### 1. AppState 过于集中（God Object 倾向）

**现状**：`AppState` 同时管理目录信息、字典目录、连接池、MdxReader 缓存、3 层 LRU 缓存、信号量。约 500 行代码。

**问题**：
- 所有 handler/service 都依赖完整的 `AppState`，单元测试困难
- 新增缓存策略需要修改 AppState

**建议**：将 `RuntimeState` 拆分为独立组件：
```rust
struct CacheManager { entry_cache, resource_cache, negative_cache }
struct ReaderPool { db_pools, mdx_readers }
struct ConcurrencyGuard { blocking_query_slots }
```
通过 trait 注入，使 service 层只依赖 `trait DictRepository` 而非具体 AppState。

### 2. HTML 手工拼接（presenter.rs）

**现状**：`render_aggregate_html` 用 `format!` + `push_str` 手工构建 HTML。

**问题**：
- 维护成本高，难以直观看出最终 HTML 结构
- escape 逻辑可能遗漏（虽然当前做了）
- 无法复用模板，前后端 HTML 结构同步困难

**建议**：引入 `askama` 或 `maud` 模板引擎。当前体量小，不急，但如果要增加更多页面（如词典管理面板），就值得迁移。

### 3. Handler 层逻辑偏重

**现状**：`handlers/mod.rs` 约 700 行，包含：
- 缓存 key 构建（~8 个函数）
- 资源候选路径构建
- 静态文件解析
- 缓存读写编排

**建议**：将缓存编排抽到 middleware 或独立的 `CacheLayer`，handler 只负责参数提取 + 调 service + 返回 response。

### 4. 同步阻塞查询的 Spawn 模式

**现状**：每次查询调用 `spawn_blocking`，传入整个 `AppState` clone。

**隐含问题**：
- `AppState::clone()` 虽然是 Arc clone（轻量），但语义上传了"所有能力"给阻塞任务
- Semaphore `try_acquire` 失败直接返 503，没有排队等待机制

**建议**：
- 对 suggest 请求可以考虑 `acquire_owned().await`（带超时），而非直接拒绝
- 或至少对 query 和 suggest 使用不同的并发槽

### 5. FTS 过滤过于激进

**现状**：`is_suggest_candidate` 过滤掉了所有含空格、数字开头、超过 40 字符的条目。

**遗漏**：
- 短语词典条目（如 "ice cream", "New York"）
- 技术术语（如 "C++", "802.11"）
- CJK 词典的多字词条

**建议**：让过滤策略可配置，或至少按词典类型区分。

### 6. Lucky 功能是静态硬编码

**现状**：`lucky/mod.rs` 从一个 ~100 个英语词的硬编码列表随机选。

**问题**：与用户实际加载的词典无关。

**建议**：从 SQLite `MDX_INDEX` 中 `ORDER BY RANDOM() LIMIT 1` 取真实词条。

---

## 五、交互体验分析

### 当前 UX 评价：**7.5/10 — 实用够用，但离"最优解"有差距**

#### ✅ 做得好的
| 方面 | 实现 |
|------|------|
| **搜索建议** | 200ms debounce + FTS5 + BM25 + 多维评分，质量高 |
| **多词典聚合** | 所有词典结果合并展示，带词典名/序号标签，比 GoldenDict 更直观 |
| **URL 路由** | `#/word/apple` hash 路由，支持浏览器前进后退和分享 |
| **查询历史** | localStorage 存储最近 20 条，输入框聚焦空输入时自动展示 |
| **键盘交互** | Enter 查询、上下键选建议、Esc 关闭、Ctrl+L 清空 |
| **暗色主题** | 完整的深色配色，不刺眼 |
| **响应式** | 600px 断点下搜索框垂直排列 |
| **音频播放** | 拦截 sound:// 链接，动态创建 `<audio>` 播放 |
| **链接重写** | 词典内部链接自动改写为服务器路由，跨词典跳转正常 |

#### ❌ 明显缺失或可优化的
| 问题 | 影响 | 建议 |
|------|------|------|
| **无词典筛选/排序** | 用户加载 10+ 词典时，只能看到全部聚合结果，无法只看某一本 | 添加词典 toggle 或 tab 切换模式 |
| **无折叠/展开** | 多词典结果全部展开，页面可能很长 | 每个词典 section 可折叠，或只展开第一个 |
| **结果无锚点定位** | 用户需要手动滚动到目标词典 | 添加侧边 mini-nav 或顶部词典快速跳转 |
| **查询是 POST** | 刷新页面不会重新触发查询（需要 hash 路由手动触发） | 改为 GET 更 RESTful，或确保 hash 路由覆盖所有场景 |
| **无 loading 骨架屏** | 只有 spinner，没有内容骨架 | 首次加载可用骨架屏减少感知延迟 |
| **搜索建议无词性/来源提示** | 只展示词条文本 | 可附加词典来源或词性标记 |
| **无深/浅色主题切换** | 部分词典 CSS 假设白底（如牛津），暗色主题下可能冲突 | 提供主题切换，或对词典 body 使用白底隔离 |
| **无离线/PWA 支持** | 本质是本地服务，但没有 Service Worker | 添加 manifest + SW 后可做 PWA |
| **jQuery 依赖** | 2026 年新项目用 jQuery 显得过时 | 代码量不大（~700行），可迁移到 vanilla JS 或 Alpine.js |
| **无收藏/生词本** | 词典 app 核心功能之一 | 可用 localStorage 或 SQLite 实现简单收藏 |
| **无发音按钮** | 音频依赖词典内嵌的 speaker 图标链接 | 对有音频的词条自动显示发音按钮 |

---

## 六、与主流词典 App 对比

| 特性 | mdict-rs | GoldenDict | Eudic 欧路 | macOS Dictionary |
|------|----------|------------|------------|-----------------|
| 多词典聚合 | ✅ 聚合卡片 | ✅ Tab 切换 | ✅ 分组折叠 | ✅ 统一滚动 |
| 搜索建议 | ✅ FTS5 | ✅ 前缀匹配 | ✅ 模糊匹配 | ✅ 即时 |
| 词典管理 | ❌ 无 UI | ✅ 完整 | ✅ 完整 | ✅ 偏好设置 |
| 收藏/生词本 | ❌ | ❌ | ✅ | ❌ |
| 发音 | ⚠️ 依赖词典 | ✅ TTS + 词典 | ✅ TTS + 词典 | ✅ 系统 TTS |
| 跨平台 | ✅ Web | ⚠️ Desktop | ⚠️ 桌面+移动 | ❌ macOS only |
| 资源占用 | ✅ <50MB | ❌ 200-500MB | ❌ 100-300MB | ✅ 系统级 |
| 启动速度 | ✅ 秒级 | ❌ 较慢 | ⚠️ 中等 | ✅ 即时 |

**mdict-rs 的核心竞争力**：极致轻量 + 跨平台 Web 访问 + 高性能 Rust 后端。

---

## 七、结论：是不是最优解？

### 后端架构：**接近最优（8.5/10）**
对于"个人/小团队 MDict Web 服务器"这个定位，当前架构**非常合适**：
- Axum + Tokio 异步框架选型正确
- memmap2 + SQLite FTS5 索引策略是性能与复杂度的最佳平衡
- 3 层缓存 + 并发控制是生产级设计
- 代码结构清晰，Rust 特性运用得当

主要改进空间在 AppState 解耦和 handler 瘦身，属于"锦上添花"。

### 前端交互：**功能够用但非最优（6.5/10）**
核心查词流程流畅，但作为一个"词典应用"：
- **缺少词典管理交互**（筛选、排序、启停）—— 最大短板
- **缺少结果导航**（折叠、锚点、侧栏）—— 多词典下体验下降明显
- **缺少个性化功能**（收藏、主题切换）
- 技术栈偏旧（jQuery）

### 推荐优先改进项（ROI 从高到低）

1. **词典筛选 toggle** — 让用户选择查哪几本词典（前端 checkbox + 后端参数）
2. **结果折叠/展开** — 每个词典 section 默认只展开前 2 个
3. **Lucky 改为真实随机词条** — 一行 SQL 搞定
4. **侧边词典快速跳转** — 固定在右侧的 mini 导航
5. **浅色主题支持** — 部分词典在白底下效果更好
6. **去 jQuery** — 迁移到 vanilla JS，减少 87KB 依赖
