# mdict-rs vs onedict-core 架构对比与借鉴结论

> **来源**：pi session `019fa37d-9dad-7d0a-af5a-349d6c299fba` 的分析报告（msg 47，13KB）+ 后续批判性评估
> **主题**：将参考库 `mdict/core`（实为 `onedict-core`）与 mdict-rs 做同语言架构对照，给出可落地的借鉴清单与演进路线
> **日期**：2026-07-27

---

## 摘要（核心定性）

参考库 `mdict/core` 实为 **onedict-core**——**同样是 Rust 项目**（clean-room 实现，约 4.8k 行，lib crate + CLI + UniFFI FFI）。这让"借鉴"从"跨语言重写"降级为"**直接移植代码**"，成本极低。

两者是"同语言、两种架构哲学"的直接对照：

| | onedict-core | mdict-rs |
| --- | --- | --- |
| **哲学** | 库优先、面向多平台客户端 | 服务优先、面向浏览器聚合查询 |
| **规模** | ~4.8k 行（lib+CLI+UniFFI） | ~5.5k 行（Axum Web 服务） |

---

## 一、架构亮点对比表

> 逐维度对照 onedict-core (`mdict/core`) 与 mdict-rs (`src/` / `crates/`)。**不一边倒，明确各有所长。**

| 维度 | onedict-core (`mdict/core`) | mdict-rs (`src/`) | 评析 |
| --- | --- | --- | --- |
| **定位与分层** | 纯 lib crate：`mdict`(解析) → `index`(SQLite) → `library`(门面) → `server`(渲染) → `ffi`(UniFFI)，CLI 独立 crate | 单二进制：`mdict`(解析) → `indexing` → `query` → `handlers`(Axum)，`AppState` 为中心枢纽 | onedict 分层更解耦、可复用（cdylib/rlib/staticlib 三形态）；mdict-rs 解析层被锁在 binary 内，无法被其他客户端复用 |
| **核心抽象** | `Mdict` 单一类型同时服务"建索引"与"运行时查询"；`Library` 门面聚合多词典+MDD+资产 | `Mdx`（仅建索引时用一次）与 `MdxReader`（运行时 mmap 读取）**两条独立读取路径** | onedict 的"一个类型管全生命周期"更简单；mdict-rs 的双路径意味着 record-block 解析逻辑虽复用，但定位信息要冗余进 DB |
| **Record 读取 I/O** | `Mutex<File>` + `seek+read` 按需读块 | **mmap** + `Bytes` 零拷贝切片 | **mdict-rs 更优**：mmap 无系统调用、OS 页缓存天然共享、并发读无锁；onedict 的 `Mutex<File>` 每词典串行化 I/O 是其明显短板 |
| **块缓存** | **字节预算 LRU**（64MB 上限，`BlockCache{budget, used}`） | **条数预算 LRU**（固定 64 块，不论块大小） | **onedict 更优**：mdict-rs 允许单块 256MB（`MAX_RECORD_BLOCK_DSIZE`），64 块理论峰值 ~16GB 内存；onedict 按字节记账，内存有硬顶 |
| **数据完整性** | **每个解压块都做 adler-32 校验**，损坏→`ChecksumMismatch{block}` 干净报错 | 块解压后只校验长度，不校验内容 | **onedict 显著更稳**：mdict-rs 的 LZO/zlib 解码错误可能静默产出脏数据；onedict 注释明说"解码 bug 只会变成干净的校验错误，绝不静默损坏" |
| **编码处理** | `Encoding` 枚举（UTF-8/16LE/GBK/Big5）+ encoding_rs，MDD 强制 UTF-16LE，声明外编码直接报错 | `encoding` crate + whatwg label 字符串匹配，UTF-16 手写双字节终止符扫描 | 两者功能相当；onedict 的类型化枚举 + `terminator_width()` 更不易错 |
| **LZO** | 纯 Rust LZO1X（180 行，无 C 依赖），adler 兜底 | `minilzo-rs`（C 绑定）+ 全局单例 | onedict 可移植性更好（iOS/WASM 友好）；mdict-rs 引入 C 工具链依赖 |
| **加密兼容** | `Encrypted=2`（key-info RIPEMD-128）支持；`Encrypted&1`（RegCode 商业加密）**前置检测并人话拒绝**；v3(MdxBuilder 4.x) 检测后明确拒绝 | `Encrypted="2"/"3"` 支持；块头按 `(enc<<4)\|comp` 双 nibble 解读（对标准文件等价，但属推测性死代码）；v3 静默当 v2 继续解析 | onedict 的"早失败、说清楚"更符合边界处理规范；mdict-rs 把 v3 当 v2 跑会在后续解析处以莫名错误崩溃 |
| **容错/安全解析** | 全程 checked arithmetic；`num_record_blocks` 先做 `2*nw` 上界校验再分配；`read_bytes` 拒绝超过文件长度的分配（防恶意文件 OOM） | 有 `MAX_*_DSIZE` 上限和 checked_add，但 nom `many0`/`count` 路径防御密度较低 | onedict 按"攻击者可控输入"标准写解析器，明显经过安全审计（注释引用 audit §1/§7/§20） |
| **索引 schema** | 单库 `onedict.db`：`entries(dict_id, headword, normalized, record_offset, record_size)` + **覆盖索引** `(normalized, dict_id, record_offset, record_size, headword)`；注册表含 `enabled/sort_order/fingerprint` | 每词典一库 `<file>.db`：`MDX_INDEX(text, record_offset, record_length, block_offset, block_size, block_dsize)` + 可选 FTS5 虚表 | 哲学对立：onedict 索引时归一化（NFKD→去变音→小写→去标点）单列支撑所有匹配；mdict-rs **查询时展开最多 32 个候选词**逐个查。onedict 每行只存 offset+size（块归属运行时 `partition_point` 算出）；mdict-rs 每行冗余 5 列定位信息（DB 更大但查询自洽） |
| **前缀建议** | 覆盖索引**区间扫描** `normalized >= lo AND < hi`（`prefix_upper()` 进位算上界），`CANDIDATE_CAP=4000` 内部截断，亚 10ms | FTS5 + bm25 排序为主，`LIKE 'prefix%'` 全扫兜底 + 启发式打分合并 | mdict-rs 的 FTS5 bm25 相关性更强、还支持分词全文，但 `LIKE` 兜底是全索引扫描；onedict 的区间扫描有严格延迟上界。两者可互补 |
| **更多查询形态** | 通配符 GLOB（`*`/`?`）、编辑距离≤2 的 did-you-mean（首字符+长度窗预筛 + 早停 Levenshtein）、`neighbors()` 邻近词（翻词典连续性） | 无通配符、无拼写建议、无邻近词 | **mdict-rs 明确缺失的三项** |
| **@@@LINK 重定向** | 跟随重定向后按 **resolved offset 去重**（别名与目标词只渲染一次），深度上限 8 | 跟随重定向（深度 5），跨词典聚合时不去重 | onedict 注释给出真实案例：Cambridge "think" 别名 + "Think" 语法条目会渲染成重复 section——mdict-rs 存在此 bug |
| **MDD 资源** | 打开时构建 `resource_map: HashMap<规范化路径, offset>`（小写、`\` 分隔、前导 `\`），查询 **O(1) 无 SQL**；`foo.mdd`/`foo.1.mdd…foo.32.mdd` 多卷自动关联；**磁盘松散资源兜底**（GoldenDict 式，带路径穿越防护） | 资源也走 SQLite `MDX_INDEX` 查询 + 候选路径尝试；mdx/mdd 按 stem 分组为同一 dict_id | onedict 资源查找快且零 SQL 抖动；其"词典目录下的散装 css/js 直接服务"是 mdict-rs 没有的真实兼容点（如 LDOCE 的 `ldoce6ec.css` 不进 MDD） |
| **词典资产注入** | 同名 `foo.css`/`foo.js` 自动发现（2MB 上限），Shadow-DOM 隔离注入每个词条 | TOML 配置声明 css/js（支持 `@file`），按 `container_class` 作用域 | 殊途同归；onedict 免配置、mdict-rs 更可控 |
| **渲染安全** | 词条 payload **base64 内嵌**（根治词条含 `</script>` 提前截断问题）+ CSP 禁外联 + Shadow DOM 隔离 + 深色智能反色 | regex 剥离 `<script>`/事件属性/`javascript:` URL | mdict-rs 的 sanitize 剥 script 属纵深防御，但不如 base64 内嵌根治 `</script>` 截断；onedict 的 base64+CSP+Shadow-DOM 是更彻底的渲染安全模型（⚠️ 属重大安全架构变更，见 [§六·4](#62-弱项风险) 风险评估） |

### 关键判断速览

| 维度 | 谁更优 |
| --- | --- |
| 分层解耦/可复用 | onedict（解析层被锁在 binary 内是 mdict-rs 的硬伤） |
| Record I/O | **mdict-rs**（mmap 零拷贝无锁 vs onedict 的 `Mutex<File>` 串行化） |
| 块缓存 | onedict（字节预算 64MB vs mdict-rs 条数预算，单块 256MB→峰值 ~16GB） |
| 数据完整性 | onedict（每块 adler-32 vs mdict-rs 只校验长度，解码 bug 可能静默产脏数据） |
| 建索引并行度 | **mdict-rs**（rayon 解压 + 延迟建 B-tree） |
| 常驻内存 | **mdict-rs**（近零 vs onedict 1M 词条 ≈60–100MB） |
| 查询形态 | onedict（通配符/模糊/邻近词；mdict-rs 明确缺失这三项） |

---

## 二、5 项重点借鉴清单（按价值排序）

每项均附代码草稿，可执行而非空谈。

1. **① 字节预算 LRU** —— 块缓存从条数预算改字节预算（moka weigher，理论一行改动；⚠️ 见风险评估）。解决单块 256MB × 64 块 = ~16GB 内存峰值。
2. **② 每块 adler-32 + thiserror 人话错误** —— 全部解压块过 adler-32 校验；解析层错误改 thiserror 枚举；v3/RegCode 前置人话拒绝。杜绝静默脏数据。
3. **③ normalized 覆盖索引 + 区间扫描** —— `MDX_INDEX` 加 `normalized` 列 + 覆盖索引；suggest 改区间扫描；新增通配符/模糊/邻近词。把查询时展开的 32 候选收敛到 ~3。（⚠️ 最高价值同时最高风险，见风险评估）
4. **④ MDD resource_map O(1) 查找 + 散装资源兜底** —— 打开时构建 `resource_map: HashMap<规范化路径, offset>`，资源查询下推为 O(1) 无 SQL；词典目录散装 css/js 直接服务。
5. **⑤ 聚合查询 resolved-offset 去重** —— 跨词典聚合 `@@@LINK` 重定向后按 resolved offset 去重（HashSet），别名与目标词只渲染一次。

---

## 三、演进路线（Phase 0–4）

> Phase 编号在 session 全文与仓库 `mdict-rs-refactor-roadmap.md` 中只出现 0–4，共 5 个阶段。

### Phase 0 — 拆分 core lib crate（架构对齐的最大单步收益）

把 `src/mdict/*` + `src/indexing/*` + 纯查询逻辑抽成 `mdict-core` lib crate，`src/` 只剩 Axum 壳：

```
crates/
  mdict-core/    # 解析 + 索引 + 查询门面（无 HTTP 依赖；thiserror 错误）
  mdict-server/  # axum handlers / app_state / 静态资源
```

**解锁**：① 解析层可单测/fuzz（mdict-rs 解析层几乎无集成测试）；② Tauri/CLI/FFI 复用；③ `cargo publish`。CLI 子命令照抄 onedict（`info/keys/lookup/bench/import/bench-query`）。

### Phase 1 — 健壮性与内存安全（对应借鉴项 ①②）

1. 块缓存改字节预算（moka weigher 一行改动）。
2. 全部解压块过 adler-32；解析层错误改 thiserror 枚举；v3/RegCode 前置人话拒绝。
3. 解析器按"恶意输入"标准补 checked math（分配不得超过文件长度）。
4. （可选）纯 Rust LZO1X 替换 `minilzo-rs` C 绑定——180 行、去 C 工具链、WASM 友好。
5. 补测试金字塔：合成 MDX fixture → 真实词典抽样渲染 → `cargo fuzz` 解析入口。

### Phase 2 — 索引与查询能力升级（对应借鉴项 ③④⑤）

1. `MDX_INDEX` 加 `normalized` 列 + 覆盖索引；`INDEX_SCHEMA_VERSION` 升 4 触发平滑重建。
2. suggest 改区间扫描（保留 FTS5 bm25 做相关性增强）；新增 `/suggest/wildcard`、`/suggest/fuzzy`、`/neighbors`。
3. 资源查询下推 `resource_map`（方案 B）+ 散装资源兜底。
4. 聚合去重（resolved-offset）。
5. 索引时归一化后，`entry_query_candidates` 的 32 候选收敛到 ~3，SQL 次数降一个数量级。

### Phase 3 — 性能深挖（发挥 mdict-rs 已有优势，超越 onedict）

> 前提：mdict-rs 在建索引并行度（rayon 解压 key 块、延迟建 B-tree、多文件并行）与运行时 I/O（mmap 零拷贝）已领先，应放大非回退。

1. **不要**借鉴 onedict 的 `Mutex<File>` seek-read；mmap + 字节预算缓存已是更优解。
2. key 块解压已 rayon 并行，再把 `insert` 改 `par_chunks` 收集后单线程批量写；**保留每词典一库**（场景正确的取舍）。
3. 加 criterion + 真实词典基准，把"keystroke→candidates < 50ms / 1M 词条"写成回归门禁。
4. 常驻内存已近零，注意 Phase 2 的 `resource_map` 只挂 MDD reader 即可。
5. 查询并发：评估把聚合 fan-out 从 `rayon::par_iter` 改为每词典一个 `spawn_blocking`，或固定 rayon 全局池上限。

### Phase 4 — 产品面（按需）

- 词典启用/排序 API（`enabled`/`sort_order` + fingerprint 增量导入）。
- 桌面/移动端：Phase 0 的 core crate 套 UniFFI（onedict `ffi.rs` 的 flat-error + `Mutex<Library>` 模式即模板）。
- 收藏/历史：对 Web 服务形态优先级最低。

---

## 四、3 项不宜照搬

借鉴纪律——以下三点恰好是 onedict 的短板，不可当优点移植过来：

1. **`Mutex<File>` seek-read** —— onedict 每词典串行化 I/O，mdict-rs 的 mmap 零拷贝无锁已是更优解。
2. **全量 keys 常驻** —— onedict 1M 词条 ≈60–100MB 常驻内存；mdict-rs 近零常驻。
3. **单库 `onedict.db`** —— 单库全局写锁，多词典聚合时是瓶颈；mdict-rs 每词典一库是场景正确的取舍。

---

## 五、实施结论（落地情况）

> 截至 2026-07-28，基于 git 历史与代码实测的落地现状：

| Phase | 状态 | Commit |
| --- | --- | --- |
| 0 拆 core lib crate | ✅ 完成 | `5cdb529` |
| 1 健壮性/内存安全 | ✅ 完成 | `40e480f` |
| 2 索引/查询升级（重构半） | ✅ 完成 | `377026e` |
| 2 新功能三项 | ⏸ 主动延后 | — |
| 3 性能深挖 | ✅ 完成（批量插入 + criterion 门禁） | （本 session） |

**Phase 1 实际落地**（commit `40e480f`）：

- 字节预算缓存：在 `lru::LruCache` 上叠 `used` 字节计数（非计划的 moka weigher——批判评估已指出后者偏理想化，此实现更稳）。
- adler-32：header + key-block-info 已校验；`MdictError` 全 thiserror 枚举（`ChecksumMismatch`/`UnsupportedEncryption`/`BlockTooLarge`/`BlockOutOfBounds`/`DecompressFailed`…，人话文案）。
- v3/RegCode 人话拒绝：`UnsupportedVersion`/`UnsupportedEncryption` 前置。
- ⚠️ 纯 Rust LZO 未做（仍 `minilzo-rs`，计划标“可选”）；record-block 的 LZO 解压块未做 adler（LZO 无原生校验和）。

**Phase 2 实际落地**（commit `377026e`）：

- `normalized` 列 + `idx_mdx_normalized` 覆盖索引 + `INDEX_SCHEMA_VERSION` 校验；.mdd 资源存 NULL。
- suggest 区间扫描：`prefix_upper` + `WHERE normalized >= :lo AND < :hi`，取代旧 `LIKE 'prefix%'` 允底；FTS5 bm25 保留做相关性。
- 查询时 32 候选 → 单次 `WHERE normalized = ?`：`repository.rs` 已是单次归一化精确查询。
- ⏸ 三个新端点（/suggest/wildcard、/fuzzy、/neighbors）+ resource_map + resolved-offset 去重：按批判评估“是新产品功能，不该塞进本 Phase”主动延后。

**Phase 3 实际落地**（本 session）：

- 批量多 VALUES 插入（`flush_index_chunk`/`flush_fts_chunk`、`INSERT_CHUNK=100`、`params_from_iter`），减少 SQLite 调用/解析轮次。
- criterion 回归门禁：`benches/normalize.rs`（候选生成热路径，~391ns/词）+ `benches/query_scan.rs`（1M 行合成 DB，精确查询 ~1.2µs、前缀区间扫描 ~6.1µs，远低于 50ms 门禁）。
- 测试 31 全绿。

**resolved-offset 去重（借鉴项 ⑤）调查结论：不实现。**
代码追踪证实“别名与目标词重复渲染”在当前代码不发生——`query_internal` 与 `query_specific_entry_internal` 均首命中即 `return` + SQL `LIMIT 1`，每词典最多一个 section；跨词典是不同文件不同 HTML（用户主动加载多本，去重反而错误）。实现需透传 record_offset（API 改动）却永不触发，属 cargo-cult。比较报告此条为结构推测，未考虑 LIMIT-1 设计。

---

## 六、批判性评估

### 6.1 强项（结论质量高）

1. **"参考库同语言"是承重洞察**——它把借鉴成本量化对了，后续所有"直接照抄代码"的建议才有依据。
2. **对比不滑向"参考库全面更优"**——mmap/rayon/近零常驻/FTS5 服务化基建被明确判给 mdict-rs，避免了 cargo-cult。
3. **5 项借鉴都附代码草稿**（moka weigher、thiserror 枚举、ALTER TABLE+prefix_upper、resource_map、resolved-offset HashSet），可执行而非空谈。
4. **"3 项不照搬"是有益的纪律**——防止把 onedict 的 `Mutex<File>` 串行化、60–100MB 常驻、单库全局写锁当优点移植过来。
5. **Phase 排序正确**：先做结构解锁（Phase 0 拆 lib）才能给解析层加 fuzz/测试，依赖关系理顺了。

### 6.2 弱项/风险（结论里值得存疑的）

1. **意图与交付严重错配**：用户要 Phase 0-3，实际只交付 Phase 0。Phase 1-3 是**未经验证的设计假设**，不是已验证的实现。"每 Phase 一次 commit"的约束很可能正是 Phase 0 之后就停下的原因——Phase 0 是机械搬运（单 commit 合理），Phase 1-3 远不止单 commit 体量。
2. **③（normalized 覆盖索引 + 3 个新端点）是最高风险项，且被低估**：
   - 它把两类**本质不同**的归一化策略混为一谈：onedict 是**索引时**归一化（查一列）；mdict-rs 是**查询时**展开 32 候选（正是因为没有 normalized 列）。"索引时归一化 + 查询时一次性 lemma 回退"的混合方案没解决——lemma 候选自身要不要再归一化？否则又把多候选问题请回来了。
   - "32 → 3"过于乐观：英文 lemma + nocase + accent-fold 单词常产生 5-8 形，加 Latin/Greek 复数规则更多。
   - `/suggest/wildcard`、`/suggest/fuzzy`、`/neighbors` 是**新产品功能**（含 GLOB/模糊匹配的 DoS 面与 UX），不是重构，塞进"Phase 2 查询升级"低估了范围。
   - "INDEX_SCHEMA_VERSION 升 4 触发平滑重建"需验证 freshness 机制真的支持**加列**，而不只是检测缺文件。
3. **① 的 moka weigher "一行改动"偏理想化**：从 `Mutex` 下的单租户缓存切到 moka 的并发语义，线程安全模型变了（并发读 vs 串行），不止换 API。
4. **base64+Shadow-DOM+CSP 被标"低成本高感知"名不副实**：信任词典 JS + Shadow-DOM 隔离是**重大安全架构变更**，有已知绕过（CSS 侧信道、原型链继承），与 mdict-rs 当前"sanitize 剥 script"的纵深防御相悖。应归 Phase 4+ 或明确排除。
5. **对比漏掉一个 mdict-rs 的正确性加分项**：onedict 常驻 `keys: Vec` + 运行时 `partition_point` 推块归属，意味着**部分索引后 keys vec 过期是静默正确性 bug**；mdict-rs "DB 即真相"模型天然免疫此类。报告只说 mdict-rs 的 5 列冗余让 DB 更大，没把这点说透。
6. **Phase 1-3 缺工作量/风险评级**：Phase 0 实际是文件搬迁（相似度 96-100% 为证），而 Phase 1-3 是新错误体系、schema 迁移、新查询算法、criterion 基准——体量不在一个量级，但报告按"每 Phase 一段"平铺，掩盖了陡增的复杂度。

### 6.3 总判断

- **分析结论本身是高质量的**（平衡、具体、排序正确、有不照搬纪律）。
- **但它是"规划态"而非"完成态"**：5 项借鉴里只有支撑性最强、最机械的 Phase 0 落地；高价值的 ①②③④⑤ 全是未验证设计。
- **最高价值的 ③ 同时是最高风险的**——它把"加列+区间扫描（重构）"、"三个新端点（新功能）"、"lemma 候选收敛（需先解决归一化合并）"三件事捆进一个 Phase，应拆开并先做 spike 验证 schema 迁移与归一化合并可行性。

### 6.4 建议下一步

1. 把 Phase 1-3 当作**各自需多 commit 的独立工作项**重排，别再套"单 commit/Phase"。
2. 优先单独落地 **① moka weigher** 和 **② adler+thiserror**（低本高值），以及 **⑤ resolved-offset 去重**（独立 bugfix，可现在就做）。
3. ③ 先拆三件：normalized 列+覆盖索引+区间扫描（重构）、三个新端点（新功能）、lemma 收敛（先验证归一化合并），分别估时。
4. base64/Shadow-DOM 那条单独开 Phase 4+，按"信任+隔离"安全模型正经做，别当低成本项。
